use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use serde_json::{json, Value};

use crate::permissions::{can_exec, can_read, can_write, PermissionMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub summary: String,
    pub content: String,
}

const MAX_PARALLEL_READ_OPS: usize = 8;

pub fn read_file(
    path: &Path,
    workspace_root: &Path,
    mode: PermissionMode,
) -> Result<ToolOutput, String> {
    can_read(path, workspace_root, mode).into_result()?;
    let resolved = resolve_path(path, workspace_root);
    let content = fs::read_to_string(&resolved).map_err(|err| err.to_string())?;
    Ok(ToolOutput {
        summary: format!("read {}", resolved.display()),
        content,
    })
}

pub fn write_file(
    path: &Path,
    contents: &str,
    workspace_root: &Path,
    mode: PermissionMode,
) -> Result<ToolOutput, String> {
    can_write(path, workspace_root, mode).into_result()?;
    let resolved = resolve_path(path, workspace_root);
    if let Some(parent) = resolved.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    fs::write(&resolved, contents).map_err(|err| err.to_string())?;
    Ok(ToolOutput {
        summary: format!("wrote {}", resolved.display()),
        content: format!("{} bytes", contents.len()),
    })
}

pub fn edit_file(
    path: &Path,
    needle: &str,
    replacement: &str,
    workspace_root: &Path,
    mode: PermissionMode,
) -> Result<ToolOutput, String> {
    can_write(path, workspace_root, mode).into_result()?;
    let resolved = resolve_path(path, workspace_root);
    let current = fs::read_to_string(&resolved).map_err(|err| err.to_string())?;
    let Some(index) = current.find(needle) else {
        return Err("needle not found".to_string());
    };

    let mut next = String::with_capacity(current.len() + replacement.len());
    next.push_str(&current[..index]);
    next.push_str(replacement);
    next.push_str(&current[index + needle.len()..]);
    fs::write(&resolved, &next).map_err(|err| err.to_string())?;
    Ok(ToolOutput {
        summary: format!("edited {}", resolved.display()),
        content: format!("replaced first occurrence of {:?}", needle),
    })
}

pub fn grep_search(
    query: &str,
    scope: Option<&Path>,
    workspace_root: &Path,
    mode: PermissionMode,
) -> Result<ToolOutput, String> {
    let root = scope.map_or_else(
        || workspace_root.to_path_buf(),
        |path| resolve_path(path, workspace_root),
    );
    can_read(&root, workspace_root, mode).into_result()?;

    let mut matches = Vec::new();
    visit_files(&root, &mut |path| {
        if matches.len() >= 200 || should_skip(path) {
            return;
        }
        if let Ok(content) = fs::read_to_string(path) {
            for (line_no, line) in content.lines().enumerate() {
                if line.contains(query) {
                    matches.push(format!("{}:{}:{}", path.display(), line_no + 1, line));
                    if matches.len() >= 200 {
                        break;
                    }
                }
            }
        }
    })?;

    Ok(ToolOutput {
        summary: format!("{} matches for {:?}", matches.len(), query),
        content: matches.join("\n"),
    })
}

pub fn glob_search(
    pattern: &str,
    scope: Option<&Path>,
    workspace_root: &Path,
    mode: PermissionMode,
) -> Result<ToolOutput, String> {
    let root = scope.map_or_else(
        || workspace_root.to_path_buf(),
        |path| resolve_path(path, workspace_root),
    );
    can_read(&root, workspace_root, mode).into_result()?;

    let mut matches = Vec::new();
    visit_files(&root, &mut |path| {
        if matches.len() >= 200 || should_skip(path) {
            return;
        }
        let relative = path.strip_prefix(workspace_root).unwrap_or(path);
        let rendered = relative.display().to_string();
        if wildcard_match(pattern, &rendered) {
            matches.push(rendered);
        }
    })?;

    Ok(ToolOutput {
        summary: format!("{} paths matched {:?}", matches.len(), pattern),
        content: matches.join("\n"),
    })
}

pub fn exec_command(
    command: &str,
    workspace_root: &Path,
    mode: PermissionMode,
) -> Result<ToolOutput, String> {
    can_exec(command, mode).into_result()?;
    let output = Command::new("zsh")
        .arg("-lc")
        .arg(command)
        .current_dir(workspace_root)
        .output()
        .map_err(|err| err.to_string())?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let status = output.status.code().unwrap_or_default();

    Ok(ToolOutput {
        summary: format!("exit status {status}"),
        content: format!("stdout:\n{stdout}\n\nstderr:\n{stderr}"),
    })
}

pub fn parallel_read_only(
    operations: &[Value],
    workspace_root: &Path,
    mode: PermissionMode,
) -> Result<ToolOutput, String> {
    if operations.is_empty() {
        return Err("parallel_read requires at least one operation".to_string());
    }
    if operations.len() > MAX_PARALLEL_READ_OPS {
        return Err(format!(
            "parallel_read supports at most {MAX_PARALLEL_READ_OPS} operations"
        ));
    }

    let workspace_root = workspace_root.to_path_buf();
    let mut handles = Vec::new();
    for operation in operations.iter().cloned() {
        let workspace_root = workspace_root.clone();
        handles.push(thread::spawn(move || {
            execute_parallel_read_op(operation, &workspace_root, mode)
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        results.push(
            handle
                .join()
                .map_err(|_| "parallel_read worker panicked".to_string())??,
        );
    }

    Ok(ToolOutput {
        summary: format!("parallel-read completed {} operations", results.len()),
        content: serde_json::to_string_pretty(&Value::Array(results))
            .map_err(|err| err.to_string())?,
    })
}

fn resolve_path(path: &Path, workspace_root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

fn execute_parallel_read_op(
    operation: Value,
    workspace_root: &Path,
    mode: PermissionMode,
) -> Result<Value, String> {
    let tool = operation
        .get("tool")
        .and_then(Value::as_str)
        .ok_or_else(|| "parallel_read operation is missing `tool`".to_string())?;

    let output = match tool {
        "read" => {
            let path = operation
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| "parallel_read read operation is missing `path`".to_string())?;
            read_file(Path::new(path), workspace_root, mode)
        }
        "grep" => {
            let query = operation
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| "parallel_read grep operation is missing `query`".to_string())?;
            let scope = operation
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            grep_search(query, scope.as_deref(), workspace_root, mode)
        }
        "glob" => {
            let pattern = operation
                .get("pattern")
                .and_then(Value::as_str)
                .ok_or_else(|| "parallel_read glob operation is missing `pattern`".to_string())?;
            let scope = operation
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            glob_search(pattern, scope.as_deref(), workspace_root, mode)
        }
        other => {
            return Err(format!(
                "parallel_read only supports read/grep/glob, got `{other}`"
            ))
        }
    }?;

    Ok(json!({
        "tool": tool,
        "summary": output.summary,
        "content": output.content,
    }))
}

fn visit_files(root: &Path, visit: &mut dyn FnMut(&Path)) -> Result<(), String> {
    if root.is_file() {
        visit(root);
        return Ok(());
    }

    for entry in fs::read_dir(root).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            if should_skip(&path) {
                continue;
            }
            visit_files(&path, visit)?;
        } else {
            visit(&path);
        }
    }
    Ok(())
}

fn should_skip(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(name, ".git" | "target" | "references" | ".harness")
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    wildcard_match_bytes(pattern.as_bytes(), text.as_bytes())
}

fn wildcard_match_bytes(pattern: &[u8], text: &[u8]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }

    match pattern[0] {
        b'*' => {
            wildcard_match_bytes(&pattern[1..], text)
                || (!text.is_empty() && wildcard_match_bytes(pattern, &text[1..]))
        }
        b'?' => !text.is_empty() && wildcard_match_bytes(&pattern[1..], &text[1..]),
        value => {
            !text.is_empty() && value == text[0] && wildcard_match_bytes(&pattern[1..], &text[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use crate::PermissionMode;

    use super::parallel_read_only;

    fn temp_workspace(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn parallel_read_batches_safe_read_only_ops() {
        let workspace = temp_workspace("tools-parallel-read");
        fs::write(workspace.join("README.md"), "hello world").unwrap();
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("src/main.rs"), "fn main() {}\n").unwrap();

        let operations = vec![
            json!({ "tool": "read", "path": "README.md" }),
            json!({ "tool": "glob", "pattern": "src/*.rs" }),
            json!({ "tool": "grep", "query": "main", "path": "src" }),
        ];

        let output =
            parallel_read_only(&operations, &workspace, PermissionMode::WorkspaceWrite).unwrap();
        assert!(output.summary.contains("3 operations"));
        assert!(output.content.contains("hello world"));
        assert!(output.content.contains("src/main.rs"));

        cleanup(&workspace);
    }
}
