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

pub fn assess_verification(events: &[VerificationEvent]) -> VerificationAssessment {
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
        suggestions: verification_suggestions(&mutation_events),
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
        "exec" => !event_is_verification(event),
        _ => false,
    }
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

fn verification_suggestions(events: &[&VerificationEvent]) -> Vec<String> {
    let mut rust = false;
    let mut node = false;
    let mut python = false;
    let mut go = false;

    for path in events
        .iter()
        .filter_map(|event| event.arguments.get("path").and_then(Value::as_str))
    {
        match Path::new(path).extension().and_then(|value| value.to_str()) {
            Some("rs") => rust = true,
            Some("js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs") => node = true,
            Some("py") => python = true,
            Some("go") => go = true,
            _ => {}
        }
    }

    if rust {
        return vec![
            "cargo test --workspace".to_string(),
            "cargo check --workspace".to_string(),
        ];
    }
    if node {
        return vec!["npm test".to_string(), "npm run build".to_string()];
    }
    if python {
        return vec!["pytest".to_string()];
    }
    if go {
        return vec!["go test ./...".to_string()];
    }

    vec!["run at least one relevant test, build, or check command".to_string()]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{assess_verification, VerificationEvent};

    #[test]
    fn docs_only_changes_do_not_require_verification() {
        let assessment = assess_verification(&[VerificationEvent {
            name: "write".to_string(),
            arguments: json!({ "path": "docs/ROADMAP.md" }),
        }]);
        assert!(!assessment.requires_verification);
    }

    #[test]
    fn verification_must_follow_last_mutation() {
        let assessment = assess_verification(&[
            VerificationEvent {
                name: "exec".to_string(),
                arguments: json!({ "command": "cargo test --workspace" }),
            },
            VerificationEvent {
                name: "write".to_string(),
                arguments: json!({ "path": "src/main.rs" }),
            },
        ]);
        assert!(assessment.requires_verification);
        assert!(!assessment.has_verification_after_last_mutation);
        assert!(assessment.guidance().contains("cargo test --workspace"));
    }
}
