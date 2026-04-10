use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{ApprovalPolicy, PermissionMode, VerificationPolicy};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HarnessConfig {
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub providers: ProvidersConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub permissions: PermissionsConfig,
    #[serde(default)]
    pub approvals: ApprovalsConfig,
    #[serde(default)]
    pub verification: VerificationConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub session: SessionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelConfig {
    pub primary: Option<String>,
    #[serde(default)]
    pub fallback: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProvidersConfig {
    pub default_connection_mode: Option<String>,
    pub interactive_connection_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryConfig {
    pub backend: Option<String>,
    pub dual_write_legacy_jsonl: Option<bool>,
    #[serde(default)]
    pub nexus_cloud: NexusCloudMemoryConfig,
    #[serde(default)]
    pub third_party_amcp: ThirdPartyAmcpMemoryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NexusCloudMemoryConfig {
    pub endpoint: Option<String>,
    pub api_key_env: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ThirdPartyAmcpMemoryConfig {
    pub endpoint: Option<String>,
    pub api_key_env: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PermissionsConfig {
    pub default_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApprovalsConfig {
    pub policy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VerificationConfig {
    pub policy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillsConfig {
    #[serde(default)]
    pub extra_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub config_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionConfig {
    pub directory: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LoadedConfig {
    pub source: Option<PathBuf>,
    pub data: HarnessConfig,
}

impl LoadedConfig {
    pub fn memory_backend(&self) -> &str {
        self.data
            .memory
            .backend
            .as_deref()
            .unwrap_or("local-amcp")
    }

    pub fn dual_write_legacy_jsonl(&self) -> bool {
        self.data.memory.dual_write_legacy_jsonl.unwrap_or(true)
    }

    pub fn default_connection_mode(&self) -> ConnectionMode {
        self.data
            .providers
            .default_connection_mode
            .as_deref()
            .and_then(ConnectionMode::parse)
            .unwrap_or(ConnectionMode::Api)
    }

    pub fn interactive_connection_mode(&self) -> ConnectionMode {
        self.data
            .providers
            .interactive_connection_mode
            .as_deref()
            .and_then(ConnectionMode::parse)
            .unwrap_or(ConnectionMode::Auto)
    }

    pub fn default_permission_mode(&self) -> PermissionMode {
        self.data
            .permissions
            .default_mode
            .as_deref()
            .and_then(PermissionMode::parse)
            .unwrap_or(PermissionMode::WorkspaceWrite)
    }

    pub fn default_approval_policy(&self) -> ApprovalPolicy {
        self.data
            .approvals
            .policy
            .as_deref()
            .and_then(ApprovalPolicy::parse)
            .unwrap_or(ApprovalPolicy::Prompt)
    }

    pub fn primary_model(&self) -> Option<&str> {
        self.data.model.primary.as_deref()
    }

    pub fn default_verification_policy(&self) -> VerificationPolicy {
        self.data
            .verification
            .policy
            .as_deref()
            .and_then(VerificationPolicy::parse)
            .unwrap_or(VerificationPolicy::Annotate)
    }

    pub fn session_dir(&self, workspace_root: &Path) -> PathBuf {
        let configured = self
            .data
            .session
            .directory
            .as_deref()
            .unwrap_or(".harness/sessions");
        resolve_path_string(configured, workspace_root)
    }

    pub fn skill_dirs(&self, workspace_root: &Path) -> Vec<PathBuf> {
        self.skill_sources(workspace_root)
            .into_iter()
            .map(|(path, _)| path)
            .collect()
    }

    pub fn skill_sources(&self, workspace_root: &Path) -> Vec<(PathBuf, String)> {
        let mut dirs = vec![
            (
                workspace_root.join(".harness").join("skills"),
                "workspace".to_string(),
            ),
            (
                workspace_root.join(".agents").join("skills"),
                "workspace".to_string(),
            ),
            (
                workspace_root.join(".codex").join("skills"),
                "workspace".to_string(),
            ),
            (
                workspace_root.join(".claude").join("skills"),
                "workspace".to_string(),
            ),
        ];

        if let Some(home) = env::var_os("HOME") {
            let home = PathBuf::from(home);
            dirs.push((home.join(".harness").join("skills"), "user".to_string()));
            dirs.push((home.join(".agents").join("skills"), "user".to_string()));
            dirs.push((home.join(".codex").join("skills"), "user".to_string()));
            dirs.push((home.join(".claude").join("skills"), "user".to_string()));
        }

        for dir in &self.data.skills.extra_dirs {
            dirs.push((
                resolve_path_string(dir, workspace_root),
                "workspace".to_string(),
            ));
        }

        dedupe_sources(dirs)
    }

    pub fn mcp_config_files(&self, workspace_root: &Path) -> Vec<PathBuf> {
        self.mcp_sources(workspace_root)
            .into_iter()
            .map(|(path, _)| path)
            .collect()
    }

    pub fn mcp_sources(&self, workspace_root: &Path) -> Vec<(PathBuf, String)> {
        let mut files = vec![
            (
                workspace_root.join(".harness").join("mcp.json"),
                "workspace".to_string(),
            ),
            (
                workspace_root.join(".agents").join("mcp.json"),
                "workspace".to_string(),
            ),
        ];

        if let Some(home) = env::var_os("HOME") {
            let home = PathBuf::from(home);
            files.push((home.join(".harness").join("mcp.json"), "user".to_string()));
            files.push((home.join(".agents").join("mcp.json"), "user".to_string()));
        }

        for file in &self.data.mcp.config_files {
            files.push((
                resolve_path_string(file, workspace_root),
                "workspace".to_string(),
            ));
        }

        dedupe_sources(files)
    }

    pub fn render_summary(&self, workspace_root: &Path) -> String {
        let source = self
            .source
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "default (no config file found)".to_string());

        [
            format!("source: {source}"),
            format!(
                "default_permission_mode: {}",
                self.default_permission_mode()
            ),
            format!(
                "default_approval_policy: {}",
                self.default_approval_policy()
            ),
            format!(
                "default_verification_policy: {}",
                self.default_verification_policy()
            ),
            format!("primary_model: {}", self.primary_model().unwrap_or("-")),
            format!(
                "default_connection_mode: {}",
                self.default_connection_mode()
            ),
            format!(
                "interactive_connection_mode: {}",
                self.interactive_connection_mode()
            ),
            format!("memory_backend: {}", self.memory_backend()),
            format!(
                "memory_dual_write_legacy_jsonl: {}",
                self.dual_write_legacy_jsonl()
            ),
            format!(
                "session_dir: {}",
                self.session_dir(workspace_root).display()
            ),
            format!("skill_roots: {}", self.skill_dirs(workspace_root).len()),
            format!(
                "mcp_config_files: {}",
                self.mcp_config_files(workspace_root).len()
            ),
        ]
        .join("\n")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    Api,
    Auth,
    Auto,
}

impl ConnectionMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "api" => Some(Self::Api),
            "auth" => Some(Self::Auth),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Auth => "auth",
            Self::Auto => "auto",
        }
    }
}

impl std::fmt::Display for ConnectionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn load_config(workspace_root: &Path) -> Result<LoadedConfig, String> {
    for candidate in config_candidates(workspace_root) {
        if candidate.is_file() {
            let contents = fs::read_to_string(&candidate).map_err(|err| err.to_string())?;
            let data = toml::from_str::<HarnessConfig>(&contents).map_err(|err| err.to_string())?;
            return Ok(LoadedConfig {
                source: Some(candidate),
                data,
            });
        }
    }

    Ok(LoadedConfig::default())
}

pub fn save_config(workspace_root: &Path, data: &HarnessConfig) -> Result<PathBuf, String> {
    let path = workspace_root.join("harness.toml");
    let rendered = toml::to_string_pretty(data).map_err(|err| err.to_string())?;
    fs::write(&path, rendered).map_err(|err| err.to_string())?;
    Ok(path)
}

fn config_candidates(workspace_root: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![
        workspace_root.join("harness.toml"),
        workspace_root.join(".harness").join("harness.toml"),
    ];

    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".harness").join("config.toml"));
    }

    candidates
}

fn resolve_path_string(value: &str, workspace_root: &Path) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

fn dedupe_sources(entries: Vec<(PathBuf, String)>) -> Vec<(PathBuf, String)> {
    let mut deduped = Vec::new();
    for entry in entries {
        if !deduped.iter().any(|(existing, _)| existing == &entry.0) {
            deduped.push(entry);
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{load_config, save_config};
    use crate::{ApprovalPolicy, ConnectionMode, PermissionMode, VerificationPolicy};

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
    fn loads_workspace_harness_toml() {
        let workspace = temp_workspace("harness-config-load");
        fs::write(
            workspace.join("harness.toml"),
            r#"
[model]
primary = "ollama/qwen2.5-coder:7b"
fallback = ["openai/gpt-4.1-mini"]

[providers]
default_connection_mode = "api"
interactive_connection_mode = "auto"

[permissions]
default_mode = "read-only"

[approvals]
policy = "auto"

[verification]
policy = "require"

[session]
directory = ".state/sessions"
"#,
        )
        .unwrap();

        let loaded = load_config(&workspace).unwrap();
        assert_eq!(loaded.primary_model(), Some("ollama/qwen2.5-coder:7b"));
        assert_eq!(loaded.default_connection_mode(), ConnectionMode::Api);
        assert_eq!(loaded.interactive_connection_mode(), ConnectionMode::Auto);
        assert_eq!(loaded.default_permission_mode(), PermissionMode::ReadOnly);
        assert_eq!(loaded.default_approval_policy(), ApprovalPolicy::Auto);
        assert_eq!(
            loaded.default_verification_policy(),
            VerificationPolicy::Require
        );
        assert_eq!(
            loaded.session_dir(&workspace),
            workspace.join(".state/sessions")
        );
        assert_eq!(loaded.data.model.fallback, vec!["openai/gpt-4.1-mini"]);
        assert_eq!(loaded.source, Some(workspace.join("harness.toml")));

        cleanup(&workspace);
    }

    #[test]
    fn falls_back_to_defaults_when_config_is_missing() {
        let workspace = temp_workspace("harness-config-default");
        let loaded = load_config(&workspace).unwrap();

        assert!(loaded.source.is_none());
        assert_eq!(
            loaded.default_permission_mode(),
            PermissionMode::WorkspaceWrite
        );
        assert_eq!(loaded.default_approval_policy(), ApprovalPolicy::Prompt);
        assert_eq!(loaded.default_connection_mode(), ConnectionMode::Api);
        assert_eq!(loaded.interactive_connection_mode(), ConnectionMode::Auto);
        assert_eq!(
            loaded.default_verification_policy(),
            VerificationPolicy::Annotate
        );
        assert_eq!(loaded.primary_model(), None);
        assert_eq!(
            loaded.session_dir(&workspace),
            workspace.join(".harness/sessions")
        );

        cleanup(&workspace);
    }

    #[test]
    fn saves_config_round_trip() {
        let workspace = temp_workspace("harness-config-save");
        let mut loaded = load_config(&workspace).unwrap();
        loaded.data.model.primary = Some("openai/gpt-4.1-mini".to_string());
        save_config(&workspace, &loaded.data).unwrap();

        let loaded = load_config(&workspace).unwrap();
        assert_eq!(loaded.primary_model(), Some("openai/gpt-4.1-mini"));

        cleanup(&workspace);
    }
}
