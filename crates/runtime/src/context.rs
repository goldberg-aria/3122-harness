use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::{build_memory_recall_text, LoadedConfig, PermissionMode, SessionStore};

const MAX_INSTRUCTION_CHARS: usize = 8000;
const MAX_RECENT_HISTORY_EVENTS: usize = 8;
const MAX_RECENT_HISTORY_CHARS: usize = 2000;
const MAX_MEMORY_RECALL_CHARS: usize = 1800;
const MAX_CONVERSATION_RECALL_LINES: usize = 6;
const MAX_CONVERSATION_RECALL_CHARS: usize = 1800;
const MAX_PROMPT_CONTEXT_CHARS: usize = 12000;
const BUDGETED_INSTRUCTION_CHARS: usize = 4000;
const BUDGETED_RECENT_HISTORY_CHARS: usize = 1200;
const BUDGETED_MEMORY_RECALL_CHARS: usize = 1200;
const BUDGETED_CONVERSATION_RECALL_CHARS: usize = 900;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceContext {
    pub workspace_root: PathBuf,
    pub config_source: Option<PathBuf>,
    pub permission_mode: PermissionMode,
    pub active_model: Option<String>,
    pub configured_primary_model: Option<String>,
    pub session_id: Option<String>,
    pub session_path: Option<PathBuf>,
    pub git: GitContext,
    pub instructions: Vec<InstructionContext>,
    pub recent_history: String,
    pub memory_recall: String,
    pub conversation_recall: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitContext {
    pub repo_root: Option<PathBuf>,
    pub branch: Option<String>,
    pub dirty: bool,
    pub status_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionContext {
    pub name: String,
    pub path: PathBuf,
    pub contents: String,
}

pub fn gather_workspace_context(
    workspace_root: &Path,
    config: &LoadedConfig,
    permission_mode: PermissionMode,
    active_model: Option<&str>,
    user_query: Option<&str>,
) -> WorkspaceContext {
    let latest_session_path = SessionStore::latest_in(&config.session_dir(workspace_root))
        .ok()
        .flatten();
    WorkspaceContext {
        workspace_root: workspace_root.to_path_buf(),
        config_source: config.source.clone(),
        permission_mode,
        active_model: active_model.map(ToOwned::to_owned),
        configured_primary_model: config.primary_model().map(ToOwned::to_owned),
        session_id: latest_session_path
            .as_deref()
            .and_then(SessionStore::session_id_from_path),
        session_path: latest_session_path,
        git: detect_git_context(workspace_root),
        instructions: discover_instruction_contexts(workspace_root),
        recent_history: build_recent_history(config, workspace_root)
            .unwrap_or_else(|_| "none".to_string()),
        memory_recall: build_memory_recall_text(workspace_root, 5)
            .map(|text| truncate_text(&text, MAX_MEMORY_RECALL_CHARS))
            .unwrap_or_else(|_| "none".to_string()),
        conversation_recall: build_conversation_recall(config, workspace_root, user_query)
            .unwrap_or_else(|_| "none".to_string()),
    }
}

pub fn render_prompt_context(context: &WorkspaceContext) -> String {
    let mut instructions = render_instructions_block(&context.instructions);
    let mut recent_history = context.recent_history.clone();
    let mut memory_recall = context.memory_recall.clone();
    let mut conversation_recall = context.conversation_recall.clone();

    let mut out = render_prompt_context_sections(
        context,
        &instructions,
        &recent_history,
        &memory_recall,
        &conversation_recall,
    );
    if out.chars().count() <= MAX_PROMPT_CONTEXT_CHARS {
        return out;
    }

    conversation_recall = budget_section(&conversation_recall, BUDGETED_CONVERSATION_RECALL_CHARS);
    out = render_prompt_context_sections(
        context,
        &instructions,
        &recent_history,
        &memory_recall,
        &conversation_recall,
    );
    if out.chars().count() <= MAX_PROMPT_CONTEXT_CHARS {
        return out;
    }

    memory_recall = budget_section(&memory_recall, BUDGETED_MEMORY_RECALL_CHARS);
    out = render_prompt_context_sections(
        context,
        &instructions,
        &recent_history,
        &memory_recall,
        &conversation_recall,
    );
    if out.chars().count() <= MAX_PROMPT_CONTEXT_CHARS {
        return out;
    }

    recent_history = budget_section(&recent_history, BUDGETED_RECENT_HISTORY_CHARS);
    out = render_prompt_context_sections(
        context,
        &instructions,
        &recent_history,
        &memory_recall,
        &conversation_recall,
    );
    if out.chars().count() <= MAX_PROMPT_CONTEXT_CHARS {
        return out;
    }

    instructions = budget_section(&instructions, BUDGETED_INSTRUCTION_CHARS);
    out = render_prompt_context_sections(
        context,
        &instructions,
        &recent_history,
        &memory_recall,
        &conversation_recall,
    );
    if out.chars().count() <= MAX_PROMPT_CONTEXT_CHARS {
        return out;
    }

    truncate_text(&out, MAX_PROMPT_CONTEXT_CHARS)
}

fn render_prompt_context_sections(
    context: &WorkspaceContext,
    instructions: &str,
    recent_history: &str,
    memory_recall: &str,
    conversation_recall: &str,
) -> String {
    let mut out = String::new();
    out.push_str("Runtime context:\n");
    out.push_str(&format!(
        "workspace_root: {}\n",
        context.workspace_root.display()
    ));
    out.push_str(&format!(
        "config_source: {}\n",
        context
            .config_source
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "default".to_string())
    ));
    out.push_str(&format!("permission_mode: {}\n", context.permission_mode));
    out.push_str(&format!(
        "active_model: {}\n",
        context.active_model.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "configured_primary_model: {}\n",
        context.configured_primary_model.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "session_id: {}\n",
        context.session_id.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "session_path: {}\n",
        context
            .session_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_string())
    ));
    out.push_str("workspace_boundary: current workspace only\n");

    match &context.git.repo_root {
        Some(repo_root) => {
            out.push_str(&format!("git_repo_root: {}\n", repo_root.display()));
            out.push_str(&format!(
                "git_branch: {}\n",
                context.git.branch.as_deref().unwrap_or("(detached)")
            ));
            out.push_str(&format!("git_dirty: {}\n", context.git.dirty));
            if !context.git.status_lines.is_empty() {
                out.push_str("git_status:\n");
                for line in &context.git.status_lines {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        None => out.push_str("git: not a repository\n"),
    }

    if context.instructions.is_empty() {
        out.push_str("instructions: none found\n");
    } else {
        out.push_str("instructions:\n");
        out.push_str(instructions);
    }

    out.push_str("recent_working_history:\n");
    out.push_str(recent_history);
    out.push('\n');

    out.push_str("local_lite_recall:\n");
    out.push_str(memory_recall);
    out.push('\n');

    out.push_str("conversation_recall:\n");
    out.push_str(conversation_recall);
    out.push('\n');

    out
}

fn render_instructions_block(instructions: &[InstructionContext]) -> String {
    let mut out = String::new();
    for instruction in instructions {
        out.push_str(&format!(
            "[{}] {}\n{}\n\n",
            instruction.name,
            instruction.path.display(),
            instruction.contents
        ));
    }
    out
}

fn budget_section(input: &str, max_chars: usize) -> String {
    if input == "none" {
        return "none".to_string();
    }
    truncate_text(input, max_chars)
}

fn discover_instruction_contexts(workspace_root: &Path) -> Vec<InstructionContext> {
    ["AGENTS.md", "CLAUDE.md"]
        .into_iter()
        .filter_map(|name| load_instruction_context(workspace_root, name))
        .collect()
}

fn load_instruction_context(workspace_root: &Path, name: &str) -> Option<InstructionContext> {
    let path = find_nearest_ancestor_file(workspace_root, name)?;
    let contents = fs::read_to_string(&path).ok()?;
    Some(InstructionContext {
        name: name.to_string(),
        path,
        contents: truncate_chars(contents, MAX_INSTRUCTION_CHARS),
    })
}

fn find_nearest_ancestor_file(workspace_root: &Path, file_name: &str) -> Option<PathBuf> {
    for dir in workspace_root.ancestors() {
        let candidate = dir.join(file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn truncate_chars(contents: String, max_chars: usize) -> String {
    if contents.chars().count() <= max_chars {
        return contents;
    }
    let truncated = contents.chars().take(max_chars).collect::<String>();
    format!("{truncated}\n\n[truncated]")
}

fn truncate_text(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let truncated = input.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

fn build_recent_history(config: &LoadedConfig, workspace_root: &Path) -> Result<String, String> {
    let Some(path) = SessionStore::latest_in(&config.session_dir(workspace_root))? else {
        return Ok("none".to_string());
    };
    let events = SessionStore::read_events(&path)?;
    let mut rendered = Vec::new();

    for event in events.iter().rev() {
        if let Some(line) = summarize_recent_event(event) {
            rendered.push(line);
            if rendered.len() >= MAX_RECENT_HISTORY_EVENTS {
                break;
            }
        }
    }

    if rendered.is_empty() {
        return Ok("none".to_string());
    }

    rendered.reverse();
    Ok(truncate_text(
        &rendered.join("\n"),
        MAX_RECENT_HISTORY_CHARS,
    ))
}

fn build_conversation_recall(
    config: &LoadedConfig,
    workspace_root: &Path,
    user_query: Option<&str>,
) -> Result<String, String> {
    let Some(query) = user_query.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok("none".to_string());
    };
    let tokens = extract_query_tokens(query);
    if tokens.is_empty() {
        return Ok("none".to_string());
    }

    let latest_path = SessionStore::latest_in(&config.session_dir(workspace_root))?;
    let mut matches = Vec::new();
    for path in SessionStore::list_in(&config.session_dir(workspace_root))? {
        if latest_path.as_ref() == Some(&path) {
            continue;
        }
        let events = SessionStore::read_events(&path)?;
        for event in events.iter().rev() {
            if let Some(line) = summarize_conversation_match(event, &tokens) {
                if !matches.iter().any(|existing| existing == &line) {
                    matches.push(line);
                }
                if matches.len() >= MAX_CONVERSATION_RECALL_LINES {
                    break;
                }
            }
        }
        if matches.len() >= MAX_CONVERSATION_RECALL_LINES {
            break;
        }
    }

    if matches.is_empty() {
        return Ok("none".to_string());
    }

    matches.reverse();
    Ok(truncate_text(
        &matches.join("\n"),
        MAX_CONVERSATION_RECALL_CHARS,
    ))
}

fn extract_query_tokens(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for token in query
        .split(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    ',' | '.'
                        | ':'
                        | ';'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '"'
                        | '\''
                        | '/'
                        | '\\'
                )
        })
        .map(str::trim)
        .filter(|token| token.chars().count() >= 2)
    {
        let normalized = token.to_ascii_lowercase();
        if !tokens.iter().any(|existing| existing == &normalized) {
            tokens.push(normalized);
        }
        if tokens.len() >= 6 {
            break;
        }
    }
    tokens
}

fn summarize_conversation_match(
    event: &crate::session::SessionEvent,
    tokens: &[String],
) -> Option<String> {
    let (label, text) = match event.kind.as_str() {
        "user_input" | "prompt_start" => ("user", payload_text(&event.payload, "text")?),
        "agent_result" | "prompt_result" => ("assistant", payload_text(&event.payload, "text")?),
        "agent_tool" => ("tool", payload_text(&event.payload, "summary")?),
        _ => return None,
    };
    let haystack = text.to_ascii_lowercase();
    if !tokens.iter().any(|token| haystack.contains(token)) {
        return None;
    }
    Some(format!("{}: {}", label, truncate_text(text.trim(), 160)))
}

fn summarize_recent_event(event: &crate::session::SessionEvent) -> Option<String> {
    match event.kind.as_str() {
        "user_input" | "prompt_start" => event
            .payload
            .get("text")
            .and_then(|value| value.as_str())
            .map(|text| format!("user: {}", truncate_text(text.trim(), 160))),
        "agent_tool" => {
            let name = event.payload.get("name").and_then(|value| value.as_str())?;
            let summary = event
                .payload
                .get("summary")
                .and_then(|value| value.as_str())
                .unwrap_or("-");
            Some(format!(
                "tool: {} | {}",
                name,
                truncate_text(summary.trim(), 160)
            ))
        }
        "agent_result" | "prompt_result" => event
            .payload
            .get("text")
            .and_then(|value| value.as_str())
            .map(|text| format!("assistant: {}", truncate_text(text.trim(), 160))),
        "tool_error" | "prompt_error" | "mcp_error" => event
            .payload
            .get("error")
            .and_then(|value| value.as_str())
            .map(|text| format!("error: {}", truncate_text(text.trim(), 160))),
        "approval_change" => event
            .payload
            .get("policy")
            .and_then(|value| value.as_str())
            .map(|policy| format!("approval: {}", policy)),
        "model_change" => event
            .payload
            .get("model")
            .and_then(|value| value.as_str())
            .map(|model| format!("model: {}", model)),
        "verification_warning" => event
            .payload
            .get("reason")
            .and_then(|value| value.as_str())
            .map(|reason| format!("verification: {}", truncate_text(reason.trim(), 160))),
        _ => None,
    }
}

fn payload_text<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

fn detect_git_context(workspace_root: &Path) -> GitContext {
    let repo_root_output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output();

    let Ok(repo_root_output) = repo_root_output else {
        return GitContext {
            repo_root: None,
            branch: None,
            dirty: false,
            status_lines: Vec::new(),
        };
    };

    if !repo_root_output.status.success() {
        return GitContext {
            repo_root: None,
            branch: None,
            dirty: false,
            status_lines: Vec::new(),
        };
    }

    let repo_root = String::from_utf8_lossy(&repo_root_output.stdout)
        .trim()
        .to_string();
    let status_output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("status")
        .arg("--short")
        .arg("--branch")
        .output();

    let Ok(status_output) = status_output else {
        return GitContext {
            repo_root: Some(PathBuf::from(repo_root)),
            branch: None,
            dirty: false,
            status_lines: Vec::new(),
        };
    };

    let status_text = String::from_utf8_lossy(&status_output.stdout);
    let (branch, dirty, status_lines) = parse_git_status_output(&status_text);

    GitContext {
        repo_root: Some(PathBuf::from(repo_root)),
        branch,
        dirty,
        status_lines,
    }
}

fn parse_git_status_output(output: &str) -> (Option<String>, bool, Vec<String>) {
    let mut lines = output
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    let branch = lines
        .first()
        .and_then(|line| line.strip_prefix("## "))
        .and_then(parse_git_branch_line);

    let status_lines = if lines.is_empty() {
        Vec::new()
    } else {
        lines.drain(1..).collect::<Vec<_>>()
    };

    (branch, !status_lines.is_empty(), status_lines)
}

fn parse_git_branch_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.starts_with("HEAD") {
        return None;
    }
    let branch = trimmed
        .split_once("...")
        .map(|(left, _)| left)
        .unwrap_or(trimmed);
    let branch = branch
        .split_once(" [")
        .map(|(left, _)| left)
        .unwrap_or(branch)
        .trim();

    if branch.is_empty() {
        None
    } else if let Some(value) = branch.strip_prefix("No commits yet on ") {
        Some(value.to_string())
    } else {
        Some(branch.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use crate::{append_memory_record, GitContext, LoadedConfig, MemoryKind, PermissionMode};

    use super::{
        gather_workspace_context, parse_git_status_output, render_prompt_context, WorkspaceContext,
    };

    fn temp_dir(prefix: &str) -> PathBuf {
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
    fn parses_git_status_branch_and_dirty_state() {
        let (branch, dirty, status_lines) = parse_git_status_output(
            "## feat/harness...origin/feat/harness [ahead 1]\n M src/main.rs\n?? tmp/test.txt\n",
        );
        assert_eq!(branch.as_deref(), Some("feat/harness"));
        assert!(dirty);
        assert_eq!(status_lines.len(), 2);
        assert_eq!(status_lines[0], " M src/main.rs");
    }

    #[test]
    fn gathers_nearest_instruction_files() {
        let root = temp_dir("context-instructions");
        let child = root.join("nested/project");
        fs::create_dir_all(&child).unwrap();
        fs::write(root.join("AGENTS.md"), "parent agents").unwrap();
        fs::write(child.join("AGENTS.md"), "local agents").unwrap();
        fs::write(root.join("CLAUDE.md"), "parent claude").unwrap();

        let context = gather_workspace_context(
            &child,
            &LoadedConfig::default(),
            PermissionMode::WorkspaceWrite,
            Some("ollama/test"),
            None,
        );

        assert_eq!(context.instructions.len(), 2);
        assert_eq!(context.instructions[0].name, "AGENTS.md");
        assert_eq!(context.instructions[0].contents, "local agents");
        assert_eq!(context.instructions[1].name, "CLAUDE.md");
        assert_eq!(context.instructions[1].contents, "parent claude");

        cleanup(&root);
    }

    #[test]
    fn renders_context_summary_for_prompt() {
        let root = temp_dir("context-render");
        fs::write(root.join("AGENTS.md"), "boss rules").unwrap();
        fs::create_dir_all(root.join(".harness/sessions")).unwrap();
        fs::write(
            root.join(".harness/sessions/session-older.jsonl"),
            [
                serde_json::to_string(&crate::session::SessionEvent {
                    ts_ms: 1,
                    kind: "user_input".to_string(),
                    payload: json!({ "text": "previous provider integration notes" }),
                })
                .unwrap(),
                serde_json::to_string(&crate::session::SessionEvent {
                    ts_ms: 2,
                    kind: "agent_result".to_string(),
                    payload: json!({ "text": "provider integration used openrouter fallback" }),
                })
                .unwrap(),
            ]
            .join("\n"),
        )
        .unwrap();
        fs::write(
            root.join(".harness/sessions/session-test.jsonl"),
            [
                serde_json::to_string(&crate::session::SessionEvent {
                    ts_ms: 1,
                    kind: "user_input".to_string(),
                    payload: json!({ "text": "continue provider integration" }),
                })
                .unwrap(),
                serde_json::to_string(&crate::session::SessionEvent {
                    ts_ms: 2,
                    kind: "agent_result".to_string(),
                    payload: json!({ "text": "provider wiring complete" }),
                })
                .unwrap(),
            ]
            .join("\n"),
        )
        .unwrap();
        append_memory_record(
            &root,
            MemoryKind::Note,
            "Memory title",
            "Recall body",
            &[],
            None,
        )
        .unwrap();

        let mut config = LoadedConfig::default();
        config.data.model.primary = Some("ollama/qwen2.5-coder:7b".to_string());
        let context = gather_workspace_context(
            &root,
            &config,
            PermissionMode::ReadOnly,
            Some("openai/gpt-4.1-mini"),
            Some("continue provider integration"),
        );
        let rendered = render_prompt_context(&context);

        assert!(rendered.contains("workspace_root:"));
        assert!(rendered.contains("permission_mode: read-only"));
        assert!(rendered.contains("active_model: openai/gpt-4.1-mini"));
        assert!(rendered.contains("configured_primary_model: ollama/qwen2.5-coder:7b"));
        assert!(rendered.contains("workspace_boundary: current workspace only"));
        assert!(rendered.contains("boss rules"));
        assert!(rendered.contains("recent_working_history:"));
        assert!(rendered.contains("continue provider integration"));
        assert!(rendered.contains("Memory title"));
        assert!(rendered.contains("conversation_recall:"));
        assert!(rendered.contains("openrouter fallback"));

        cleanup(&root);
    }

    #[test]
    fn keeps_rendered_context_within_budget() {
        let huge = "x".repeat(9000);
        let context = WorkspaceContext {
            workspace_root: PathBuf::from("/tmp/workspace"),
            config_source: None,
            permission_mode: PermissionMode::WorkspaceWrite,
            active_model: Some("anthropic/claude-sonnet-4-6".to_string()),
            configured_primary_model: Some("anthropic/claude-sonnet-4-6".to_string()),
            session_id: Some("session-1".to_string()),
            session_path: Some(PathBuf::from(
                "/tmp/workspace/.harness/sessions/session-1.jsonl",
            )),
            git: GitContext {
                repo_root: None,
                branch: None,
                dirty: false,
                status_lines: Vec::new(),
            },
            instructions: vec![crate::InstructionContext {
                name: "AGENTS.md".to_string(),
                path: PathBuf::from("/tmp/workspace/AGENTS.md"),
                contents: huge.clone(),
            }],
            recent_history: huge.clone(),
            memory_recall: huge.clone(),
            conversation_recall: huge,
        };

        let rendered = render_prompt_context(&context);
        assert!(rendered.chars().count() <= 12_003);
        assert!(rendered.contains("conversation_recall:"));
    }
}
