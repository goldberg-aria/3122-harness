use std::fmt;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim() {
            "read-only" => Some(Self::ReadOnly),
            "workspace-write" => Some(Self::WorkspaceWrite),
            "danger-full-access" => Some(Self::DangerFullAccess),
            _ => None,
        }
    }
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny { reason: String },
}

impl PermissionDecision {
    pub fn allow() -> Self {
        Self::Allow
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
        }
    }

    pub fn into_result(self) -> Result<(), String> {
        match self {
            Self::Allow => Ok(()),
            Self::Deny { reason } => Err(reason),
        }
    }
}

pub fn can_read(_path: &Path, _workspace_root: &Path, _mode: PermissionMode) -> PermissionDecision {
    PermissionDecision::allow()
}

pub fn can_write(path: &Path, workspace_root: &Path, mode: PermissionMode) -> PermissionDecision {
    match mode {
        PermissionMode::ReadOnly => {
            PermissionDecision::deny("writes are blocked in read-only mode")
        }
        PermissionMode::WorkspaceWrite => {
            if is_within_workspace(path, workspace_root) {
                PermissionDecision::allow()
            } else {
                PermissionDecision::deny("write path is outside the workspace")
            }
        }
        PermissionMode::DangerFullAccess => PermissionDecision::allow(),
    }
}

pub fn can_exec(command: &str, mode: PermissionMode) -> PermissionDecision {
    match mode {
        PermissionMode::DangerFullAccess => PermissionDecision::allow(),
        PermissionMode::ReadOnly => {
            if is_read_only_command(command) {
                PermissionDecision::allow()
            } else {
                PermissionDecision::deny("command may mutate state; blocked in read-only mode")
            }
        }
        PermissionMode::WorkspaceWrite => {
            if is_dangerous_command(command) {
                PermissionDecision::deny("command is classified as destructive")
            } else {
                PermissionDecision::allow()
            }
        }
    }
}

fn is_within_workspace(path: &Path, workspace_root: &Path) -> bool {
    let root = normalize_path(workspace_root);
    let joined = if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&workspace_root.join(path))
    };
    joined.starts_with(&root)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn is_read_only_command(command: &str) -> bool {
    let token = command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default();

    if token == "git" {
        return is_safe_git_command(command);
    }

    matches!(
        token,
        "cat"
            | "head"
            | "tail"
            | "ls"
            | "find"
            | "grep"
            | "rg"
            | "pwd"
            | "env"
            | "printenv"
            | "which"
            | "where"
            | "whoami"
            | "stat"
            | "file"
            | "wc"
            | "sort"
            | "uniq"
            | "cut"
            | "sed"
            | "awk"
    ) && !contains_write_flags(command)
}

fn contains_write_flags(command: &str) -> bool {
    let lowered = command.to_ascii_lowercase();
    lowered.contains(" -i")
        || lowered.contains(" >")
        || lowered.contains(" >>")
        || lowered.contains(" rm ")
        || lowered.starts_with("rm ")
}

fn is_safe_git_command(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    let Some(first) = tokens.next() else {
        return false;
    };
    if first.rsplit('/').next().unwrap_or_default() != "git" {
        return false;
    }

    while let Some(token) = tokens.next() {
        match token {
            "-C" | "-c" | "--git-dir" | "--work-tree" => {
                let _ = tokens.next();
            }
            "--no-pager" | "--paginate" => {}
            value if value.starts_with('-') => {}
            subcommand => {
                return matches!(
                    subcommand,
                    "status" | "diff" | "show" | "log" | "rev-parse" | "branch"
                );
            }
        }
    }

    false
}

fn is_dangerous_command(command: &str) -> bool {
    let lowered = command.to_ascii_lowercase();
    const DANGEROUS_SNIPPETS: &[&str] = &[
        "rm -rf",
        "rm -fr",
        "mkfs",
        "dd if=",
        "shutdown",
        "reboot",
        "poweroff",
        "sudo ",
        "chown ",
        "chmod 777",
        "git reset --hard",
        "git clean -fd",
        "killall ",
    ];

    DANGEROUS_SNIPPETS
        .iter()
        .any(|snippet| lowered.contains(snippet))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{can_exec, can_write, PermissionDecision, PermissionMode};

    #[test]
    fn workspace_write_allows_paths_inside_workspace() {
        let decision = can_write(
            Path::new("src/main.rs"),
            Path::new("/tmp/harness-workspace"),
            PermissionMode::WorkspaceWrite,
        );
        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[test]
    fn workspace_write_blocks_paths_outside_workspace() {
        let decision = can_write(
            Path::new("/etc/passwd"),
            Path::new("/tmp/harness-workspace"),
            PermissionMode::WorkspaceWrite,
        );
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                reason: "write path is outside the workspace".to_string(),
            }
        );
    }

    #[test]
    fn read_only_blocks_mutating_commands() {
        let decision = can_exec("touch /tmp/file.txt", PermissionMode::ReadOnly);
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                reason: "command may mutate state; blocked in read-only mode".to_string(),
            }
        );
    }

    #[test]
    fn workspace_write_blocks_destructive_commands() {
        let decision = can_exec("rm -rf target", PermissionMode::WorkspaceWrite);
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                reason: "command is classified as destructive".to_string(),
            }
        );
    }

    #[test]
    fn read_only_allows_safe_read_commands() {
        let decision = can_exec("rg harness src", PermissionMode::ReadOnly);
        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[test]
    fn read_only_allows_safe_git_subcommands_only() {
        let decision = can_exec("git -C repo status", PermissionMode::ReadOnly);
        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[test]
    fn read_only_blocks_destructive_git_subcommands() {
        let decision = can_exec("git reset --hard", PermissionMode::ReadOnly);
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                reason: "command may mutate state; blocked in read-only mode".to_string(),
            }
        );
    }

    #[test]
    fn read_only_blocks_sed_in_place_edits() {
        let decision = can_exec("sed -i 's/foo/bar/' README.md", PermissionMode::ReadOnly);
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                reason: "command may mutate state; blocked in read-only mode".to_string(),
            }
        );
    }
}
