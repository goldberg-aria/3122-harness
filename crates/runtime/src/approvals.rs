use std::fmt;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    Prompt,
    Auto,
}

impl ApprovalPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Auto => "auto",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim() {
            "prompt" => Some(Self::Prompt),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

impl fmt::Display for ApprovalPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationPolicy {
    Off,
    Annotate,
    Require,
}

impl VerificationPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Annotate => "annotate",
            Self::Require => "require",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim() {
            "off" => Some(Self::Off),
            "annotate" => Some(Self::Annotate),
            "require" => Some(Self::Require),
            _ => None,
        }
    }
}

impl fmt::Display for VerificationPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRisk {
    Low,
    Medium,
    High,
    Critical,
}

impl ApprovalRisk {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

impl fmt::Display for ApprovalRisk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAction {
    AutoApprove,
    Prompt,
    Deny,
}

impl ApprovalAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoApprove => "auto-approve",
            Self::Prompt => "prompt",
            Self::Deny => "deny",
        }
    }
}

pub fn classify_approval_request(tool: &str, arguments: &Value) -> (ApprovalRisk, String) {
    match tool {
        "read" | "grep" | "glob" | "parallel_read" | "skill" | "mcp_list_tools" => (
            ApprovalRisk::Low,
            "read-only discovery within the workspace".to_string(),
        ),
        "write" | "edit" => (
            ApprovalRisk::Medium,
            format!(
                "workspace file mutation: {}",
                approval_path_label(arguments).unwrap_or_else(|| "unknown path".to_string())
            ),
        ),
        "mcp_call" => (
            ApprovalRisk::High,
            format!(
                "external MCP tool call: {}",
                arguments
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown tool")
            ),
        ),
        "exec" => classify_exec_request(arguments),
        other => (ApprovalRisk::High, format!("unknown tool surface: {other}")),
    }
}

pub fn approval_action_for_policy(policy: ApprovalPolicy, risk: ApprovalRisk) -> ApprovalAction {
    match policy {
        ApprovalPolicy::Auto => match risk {
            ApprovalRisk::Critical => ApprovalAction::Deny,
            _ => ApprovalAction::AutoApprove,
        },
        ApprovalPolicy::Prompt => match risk {
            ApprovalRisk::Low => ApprovalAction::AutoApprove,
            ApprovalRisk::Medium | ApprovalRisk::High => ApprovalAction::Prompt,
            ApprovalRisk::Critical => ApprovalAction::Deny,
        },
    }
}

fn classify_exec_request(arguments: &Value) -> (ApprovalRisk, String) {
    let Some(command) = arguments.get("command").and_then(Value::as_str) else {
        return (
            ApprovalRisk::High,
            "shell command with unknown intent".to_string(),
        );
    };
    let normalized = command.trim().to_ascii_lowercase();

    if matches_dangerous_exec_command(&normalized) {
        return (
            ApprovalRisk::Critical,
            format!("dangerous shell command: {}", truncate_command(command)),
        );
    }

    if matches_read_only_exec_command(&normalized) {
        return (
            ApprovalRisk::Low,
            format!(
                "read-only or verification shell command: {}",
                truncate_command(command)
            ),
        );
    }

    if matches_build_exec_command(&normalized) {
        return (
            ApprovalRisk::Medium,
            format!(
                "project-local build or install command: {}",
                truncate_command(command)
            ),
        );
    }

    (
        ApprovalRisk::High,
        format!(
            "shell command may mutate state: {}",
            truncate_command(command)
        ),
    )
}

fn approval_path_label(arguments: &Value) -> Option<String> {
    arguments
        .get("path")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn matches_dangerous_exec_command(command: &str) -> bool {
    [
        "rm -rf",
        "rm -fr",
        "git reset --hard",
        "git checkout --",
        "git clean -fd",
        "git clean -xdf",
        "sudo ",
        "mkfs",
        "dd if=",
        "shutdown",
        "reboot",
        "kill -9",
        "launchctl unload",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

fn matches_read_only_exec_command(command: &str) -> bool {
    [
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
        "ruff check",
        "go test",
        "cargo fmt --check",
        "git status",
        "git diff",
        "git show",
        "ls",
        "pwd",
        "cat ",
        "sed ",
        "rg ",
        "find ",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

fn matches_build_exec_command(command: &str) -> bool {
    [
        "cargo run",
        "npm install",
        "pnpm install",
        "pnpm dev",
        "npm run dev",
        "uv sync",
        "pip install",
        "go build",
        "make ",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

fn truncate_command(command: &str) -> String {
    let trimmed = command.trim();
    if trimmed.chars().count() <= 80 {
        return trimmed.to_string();
    }
    let truncated = trimmed.chars().take(80).collect::<String>();
    format!("{truncated}...")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        approval_action_for_policy, classify_approval_request, ApprovalAction, ApprovalPolicy,
        ApprovalRisk, VerificationPolicy,
    };

    #[test]
    fn parses_known_approval_policies() {
        assert_eq!(
            ApprovalPolicy::parse("prompt"),
            Some(ApprovalPolicy::Prompt)
        );
        assert_eq!(ApprovalPolicy::parse("auto"), Some(ApprovalPolicy::Auto));
        assert_eq!(ApprovalPolicy::parse("unknown"), None);
    }

    #[test]
    fn parses_known_verification_policies() {
        assert_eq!(
            VerificationPolicy::parse("off"),
            Some(VerificationPolicy::Off)
        );
        assert_eq!(
            VerificationPolicy::parse("annotate"),
            Some(VerificationPolicy::Annotate)
        );
        assert_eq!(
            VerificationPolicy::parse("require"),
            Some(VerificationPolicy::Require)
        );
        assert_eq!(VerificationPolicy::parse("unknown"), None);
    }

    #[test]
    fn classifies_read_and_write_risks() {
        let (read_risk, _) = classify_approval_request("read", &json!({ "path": "README.md" }));
        let (write_risk, write_reason) =
            classify_approval_request("write", &json!({ "path": "src/main.rs" }));

        assert_eq!(read_risk, ApprovalRisk::Low);
        assert_eq!(write_risk, ApprovalRisk::Medium);
        assert!(write_reason.contains("src/main.rs"));
    }

    #[test]
    fn classifies_exec_risks() {
        let (safe_risk, _) =
            classify_approval_request("exec", &json!({ "command": "cargo test --workspace" }));
        let (danger_risk, reason) =
            classify_approval_request("exec", &json!({ "command": "rm -rf target" }));

        assert_eq!(safe_risk, ApprovalRisk::Low);
        assert_eq!(danger_risk, ApprovalRisk::Critical);
        assert!(reason.contains("dangerous"));
    }

    #[test]
    fn maps_policy_and_risk_to_action() {
        assert_eq!(
            approval_action_for_policy(ApprovalPolicy::Prompt, ApprovalRisk::Low),
            ApprovalAction::AutoApprove
        );
        assert_eq!(
            approval_action_for_policy(ApprovalPolicy::Prompt, ApprovalRisk::High),
            ApprovalAction::Prompt
        );
        assert_eq!(
            approval_action_for_policy(ApprovalPolicy::Auto, ApprovalRisk::Critical),
            ApprovalAction::Deny
        );
    }
}
