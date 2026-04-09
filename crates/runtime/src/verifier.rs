use std::path::Path;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationEvent {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationAssessment {
    pub requires_verification: bool,
    pub has_verification_after_last_mutation: bool,
    pub reason: Option<String>,
    pub suggestions: Vec<String>,
}

impl VerificationAssessment {
    pub fn none() -> Self {
        Self {
            requires_verification: false,
            has_verification_after_last_mutation: false,
            reason: None,
            suggestions: Vec::new(),
        }
    }

    pub fn guidance(&self) -> String {
        let Some(reason) = self.reason.as_deref() else {
            return "verification was not required".to_string();
        };
        if self.suggestions.is_empty() {
            return format!(
                "{reason}; run a verification step or answer with `Not verified` and the reason"
            );
        }
        format!(
            "{reason}; try {} or answer with `Not verified` and the reason",
            self.suggestions.join(" / ")
        )
    }
}

pub fn assess_verification(
    workspace_root: &Path,
    events: &[VerificationEvent],
) -> VerificationAssessment {
    let mutation_events = events
        .iter()
        .filter(|event| event_mutates_workspace(event))
        .collect::<Vec<_>>();
    if mutation_events.is_empty() {
        return VerificationAssessment::none();
    }

    if mutation_events
        .iter()
        .all(|event| event_is_docs_only(event))
    {
        return VerificationAssessment::none();
    }

    VerificationAssessment {
        requires_verification: true,
        has_verification_after_last_mutation: has_verification_after_last_mutation(events),
        reason: Some(verification_reason(&mutation_events)),
        suggestions: verification_suggestions(workspace_root, &mutation_events),
    }
}

fn has_verification_after_last_mutation(events: &[VerificationEvent]) -> bool {
    let Some(last_mutation_index) = events.iter().rposition(event_mutates_workspace) else {
        return false;
    };
    events
        .iter()
        .skip(last_mutation_index + 1)
        .any(event_is_verification)
}

fn event_mutates_workspace(event: &VerificationEvent) -> bool {
    match event.name.as_str() {
        "write" | "edit" => true,
        "exec" => exec_command_mutates_workspace(event),
        _ => false,
    }
}

fn exec_command_mutates_workspace(event: &VerificationEvent) -> bool {
    if event_is_verification(event) {
        return false;
    }
    let Some(command) = event.arguments.get("command").and_then(Value::as_str) else {
        return true;
    };
    let command = command.trim().to_ascii_lowercase();
    let read_only_prefixes = [
        "ls",
        "pwd",
        "cat ",
        "head ",
        "tail ",
        "sed ",
        "grep ",
        "rg ",
        "find ",
        "tree",
        "git status",
        "git diff",
        "git log",
        "git show",
        "cargo tree",
        "cargo metadata",
        "ollama list",
    ];
    !read_only_prefixes
        .iter()
        .any(|prefix| command == *prefix || command.starts_with(prefix))
}

fn event_is_verification(event: &VerificationEvent) -> bool {
    if event.name != "exec" {
        return false;
    }

    let Some(command) = event.arguments.get("command").and_then(Value::as_str) else {
        return false;
    };
    let command = command.to_ascii_lowercase();
    [
        " test",
        "cargo test",
        "cargo check",
        "cargo build",
        "npm test",
        "npm run test",
        "npm run build",
        "pnpm test",
        "pnpm build",
        "pytest",
        "vitest",
        "jest",
        "lint",
        "verify",
        "check",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

fn event_is_docs_only(event: &VerificationEvent) -> bool {
    if !matches!(event.name.as_str(), "write" | "edit") {
        return false;
    }
    let Some(path) = event.arguments.get("path").and_then(Value::as_str) else {
        return false;
    };
    path_is_docs_only(path)
}

fn path_is_docs_only(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let path = Path::new(&normalized);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if matches!(
        file_name.as_str(),
        "readme.md" | "changelog.md" | "license" | "license.md" | "agents.md" | "claude.md"
    ) {
        return true;
    }

    if normalized.starts_with("docs/") {
        return true;
    }

    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("md" | "mdx" | "txt" | "rst" | "adoc")
    )
}

fn verification_reason(events: &[&VerificationEvent]) -> String {
    let touched_paths = events
        .iter()
        .filter_map(|event| event.arguments.get("path").and_then(Value::as_str))
        .map(|path| path.replace('\\', "/"))
        .collect::<Vec<_>>();

    if touched_paths.is_empty() {
        return "no verification step was recorded after workspace changes".to_string();
    }

    let joined = touched_paths.join(", ");
    format!("no verification step was recorded after changes to {joined}")
}

fn verification_suggestions(workspace_root: &Path, events: &[&VerificationEvent]) -> Vec<String> {
    let mut rust = false;
    let mut node = false;
    let mut python = false;
    let mut go = false;
    let mut ruby = false;

    for path in events
        .iter()
        .filter_map(|event| event.arguments.get("path").and_then(Value::as_str))
    {
        match Path::new(path).extension().and_then(|value| value.to_str()) {
            Some("rs") => rust = true,
            Some("js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs") => node = true,
            Some("py") => python = true,
            Some("go") => go = true,
            Some("rb") => ruby = true,
            _ => {}
        }
    }

    let mut suggestions = project_verification_suggestions(workspace_root);

    if rust {
        push_unique(&mut suggestions, "cargo test --workspace");
        push_unique(&mut suggestions, "cargo check --workspace");
    }
    if node {
        push_unique(&mut suggestions, "npm test");
        push_unique(&mut suggestions, "npm run build");
    }
    if python {
        push_unique(&mut suggestions, "pytest");
    }
    if go {
        push_unique(&mut suggestions, "go test ./...");
    }
    if ruby {
        push_unique(&mut suggestions, "bundle exec rspec");
        push_unique(&mut suggestions, "bin/rails test");
    }
    if suggestions.is_empty() {
        suggestions.push("run at least one relevant test, build, or check command".to_string());
    }

    suggestions
}

fn project_verification_suggestions(workspace_root: &Path) -> Vec<String> {
    let mut suggestions = Vec::new();

    if workspace_root.join("Cargo.toml").is_file() {
        push_unique(&mut suggestions, "cargo test --workspace");
        push_unique(&mut suggestions, "cargo check --workspace");
        push_unique(&mut suggestions, "cargo build --workspace");
    }

    if workspace_root.join("package.json").is_file() {
        if workspace_root.join("pnpm-lock.yaml").is_file() {
            push_unique(&mut suggestions, "pnpm test");
            push_unique(&mut suggestions, "pnpm build");
        } else if workspace_root.join("yarn.lock").is_file() {
            push_unique(&mut suggestions, "yarn test");
            push_unique(&mut suggestions, "yarn build");
        } else {
            push_unique(&mut suggestions, "npm test");
            push_unique(&mut suggestions, "npm run build");
        }
    }

    if workspace_root.join("pyproject.toml").is_file()
        || workspace_root.join("pytest.ini").is_file()
        || workspace_root.join("requirements.txt").is_file()
    {
        push_unique(&mut suggestions, "pytest");
    }

    if workspace_root.join("go.mod").is_file() {
        push_unique(&mut suggestions, "go test ./...");
    }

    if workspace_root.join("Gemfile").is_file() {
        push_unique(&mut suggestions, "bundle exec rspec");
        push_unique(&mut suggestions, "bin/rails test");
    }

    suggestions
}

fn push_unique(suggestions: &mut Vec<String>, value: &str) {
    if !suggestions.iter().any(|existing| existing == value) {
        suggestions.push(value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{assess_verification, VerificationEvent};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

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
    fn docs_only_changes_do_not_require_verification() {
        let workspace = temp_workspace("verifier-docs-only");
        let assessment = assess_verification(
            &workspace,
            &[VerificationEvent {
                name: "write".to_string(),
                arguments: json!({ "path": "docs/ROADMAP.md" }),
            }],
        );
        assert!(!assessment.requires_verification);
        cleanup(&workspace);
    }

    #[test]
    fn verification_must_follow_last_mutation() {
        let workspace = temp_workspace("verifier-mutation-order");
        fs::write(workspace.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let assessment = assess_verification(
            &workspace,
            &[
                VerificationEvent {
                    name: "exec".to_string(),
                    arguments: json!({ "command": "cargo test --workspace" }),
                },
                VerificationEvent {
                    name: "write".to_string(),
                    arguments: json!({ "path": "src/main.rs" }),
                },
            ],
        );
        assert!(assessment.requires_verification);
        assert!(!assessment.has_verification_after_last_mutation);
        assert!(assessment.guidance().contains("cargo test --workspace"));
        cleanup(&workspace);
    }

    #[test]
    fn read_only_exec_does_not_force_verification() {
        let workspace = temp_workspace("verifier-readonly-exec");
        let assessment = assess_verification(
            &workspace,
            &[VerificationEvent {
                name: "exec".to_string(),
                arguments: json!({ "command": "ls -la" }),
            }],
        );
        assert!(!assessment.requires_verification);
        cleanup(&workspace);
    }

    #[test]
    fn uses_project_manifest_to_suggest_verification_commands() {
        let workspace = temp_workspace("verifier-project-hints");
        fs::write(workspace.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();

        let assessment = assess_verification(
            &workspace,
            &[VerificationEvent {
                name: "edit".to_string(),
                arguments: json!({ "path": "README-ish", "needle": "a", "replacement": "b" }),
            }],
        );

        assert!(assessment.requires_verification);
        assert!(assessment.guidance().contains("cargo test --workspace"));
        assert!(assessment.guidance().contains("cargo check --workspace"));
        cleanup(&workspace);
    }
}
