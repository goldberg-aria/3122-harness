mod agent;
mod approvals;
mod commands;
mod config;
mod context;
mod discovery;
mod envfile;
mod mcp;
mod memory;
mod permissions;
mod profiles;
mod provider;
mod session;
mod skills;
mod tools;
mod verifier;

use std::env;
use std::path::{Path, PathBuf};

pub use agent::{
    run_agent_loop, AgentOptions, AgentReply, AgentToolEvent, ApprovalOutcome, ApprovalRequest,
};
pub use approvals::{
    approval_action_for_policy, classify_approval_request, ApprovalAction, ApprovalPolicy,
    ApprovalRisk, VerificationPolicy,
};
pub use commands::{
    create_slash_command_template, discover_slash_commands, expand_slash_command,
    init_slash_command_dir, resolve_slash_command, slash_command_dir, SlashCommand,
    SlashCommandKind, SlashCommandScope,
};
pub use config::{load_config, save_config, ConnectionMode, HarnessConfig, LoadedConfig};
pub use context::{
    gather_workspace_context, render_prompt_context, GitContext, InstructionContext,
    WorkspaceContext,
};
pub use discovery::{discover_mcp_servers, discover_skills, McpServerEntry, SkillEntry};
pub use envfile::load_workspace_env;
pub use mcp::{call_tool as call_mcp_tool, list_tools as list_mcp_tools, McpToolInfo};
pub use memory::{
    append_memory_record, build_handoff_text, build_memory_recall_text,
    build_model_handoff_snapshot, build_resume_text, latest_model_handoff, list_memory_records,
    memory_dir, pending_model_handoff, render_model_handoff_text, save_session_memory_bundle,
    save_session_summary, search_memory_records, summarize_session_events, MemoryKind,
    MemoryRecord, ModelHandoffSnapshot, SavedMemoryBundle, SessionDigest, StoredModelHandoff,
};
pub use permissions::{can_exec, can_read, can_write, PermissionDecision, PermissionMode};
pub use profiles::{
    detect_provider_key, find_provider_profile, load_provider_registry, provider_preset,
    provider_presets, provider_registry_path, remove_provider_profile, save_provider_registry,
    upsert_provider_profile, DetectionConfidence, ProviderDetection, ProviderPreset,
    ProviderRegistry, SavedProviderProfile,
};
pub use provider::{
    parse_model_target, resolve_model_target, resolve_model_target_with_mode, send_prompt,
    ProviderReply, ProviderRoute, ProviderTarget, ProviderToolCall,
};
pub use session::SessionStore;
pub use skills::{build_skill_packet, resolve_skill, ResolvedSkill, SkillPacket};
pub use tools::{
    edit_file, exec_command, glob_search, grep_search, parallel_read_only, read_file, write_file,
    ToolOutput,
};
pub use verifier::{assess_verification, VerificationAssessment, VerificationEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Anthropic,
    OpenAiCompat,
    Ollama,
    ClaudeCode,
    Codex,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCompat => "openai-compat",
            Self::Ollama => "ollama",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendLane {
    ByokApi,
    LocalRuntime,
    ExternalAdapter,
}

impl BackendLane {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ByokApi => "byok-api",
            Self::LocalRuntime => "local-runtime",
            Self::ExternalAdapter => "external-adapter",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendSpec {
    pub kind: BackendKind,
    pub lane: BackendLane,
    pub auth_hint: &'static str,
    pub availability_hint: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub workspace_root: PathBuf,
    pub config_source: Option<PathBuf>,
    pub default_permission_mode: PermissionMode,
    pub primary_model: Option<String>,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("Harness doctor\n");
        out.push_str(&format!("workspace: {}\n", self.workspace_root.display()));
        out.push_str(&format!(
            "config: {}\n",
            self.config_source
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "default (no config file found)".to_string())
        ));
        out.push_str(&format!(
            "default_permission_mode: {}\n",
            self.default_permission_mode
        ));
        out.push_str(&format!(
            "primary_model: {}\n",
            self.primary_model.as_deref().unwrap_or("-")
        ));
        for check in &self.checks {
            let status = if check.ok { "ok" } else { "missing" };
            out.push_str(&format!(
                "- [{}] {}: {}\n",
                status, check.name, check.detail
            ));
        }
        out
    }
}

pub fn backend_catalog() -> Vec<BackendSpec> {
    vec![
        BackendSpec {
            kind: BackendKind::Anthropic,
            lane: BackendLane::ByokApi,
            auth_hint: "ANTHROPIC_API_KEY",
            availability_hint: "native HTTPS client",
        },
        BackendSpec {
            kind: BackendKind::OpenAiCompat,
            lane: BackendLane::ByokApi,
            auth_hint: "OPENAI_API_KEY + optional OPENAI_BASE_URL",
            availability_hint: "native HTTPS client",
        },
        BackendSpec {
            kind: BackendKind::Ollama,
            lane: BackendLane::LocalRuntime,
            auth_hint: "OLLAMA_HOST optional",
            availability_hint: "requires ollama daemon or reachable host",
        },
        BackendSpec {
            kind: BackendKind::ClaudeCode,
            lane: BackendLane::ExternalAdapter,
            auth_hint: "adapter uses official claude auth or env-backed execution",
            availability_hint: "requires `claude` binary",
        },
        BackendSpec {
            kind: BackendKind::Codex,
            lane: BackendLane::ExternalAdapter,
            auth_hint: "adapter uses official codex auth or env-backed execution",
            availability_hint: "requires `codex` binary",
        },
    ]
}

pub fn blueprint_summary() -> String {
    let lines = [
        "Coding Agent Harness blueprint",
        "",
        "Core loop:",
        "1. user input",
        "2. context assembly",
        "3. provider or adapter call",
        "4. tool intent parsing",
        "5. permission evaluation",
        "6. tool execution",
        "7. transcript append",
        "8. final render",
        "",
        "Default permission mode: workspace-write",
        "Primary backends: anthropic, openai-compatible, ollama",
        "Secondary backends: claude-code adapter, codex adapter",
        "",
        "Design rule: no private session scraping; only official env vars, APIs, or explicit CLI adapters.",
    ];
    lines.join("\n")
}

pub fn doctor_report(workspace_root: &Path, config: &LoadedConfig) -> DoctorReport {
    let checks = vec![
        env_check("ANTHROPIC_API_KEY"),
        env_check("OPENAI_API_KEY"),
        env_check("OLLAMA_HOST"),
        binary_check("ollama"),
        binary_check("claude"),
        binary_check("codex"),
        file_check(workspace_root.join("AGENTS.md"), "AGENTS.md"),
        file_check(workspace_root.join("CLAUDE.md"), "CLAUDE.md"),
        file_check(workspace_root.join(".harness"), ".harness"),
        file_check(workspace_root.join("harness.toml"), "harness.toml"),
    ];

    DoctorReport {
        workspace_root: workspace_root.to_path_buf(),
        config_source: config.source.clone(),
        default_permission_mode: config.default_permission_mode(),
        primary_model: config.primary_model().map(ToOwned::to_owned),
        checks,
    }
}

fn env_check(key: &str) -> DoctorCheck {
    match env::var(key) {
        Ok(value) if !value.trim().is_empty() => DoctorCheck {
            name: key.to_string(),
            ok: true,
            detail: "set".to_string(),
        },
        _ => DoctorCheck {
            name: key.to_string(),
            ok: false,
            detail: "not set".to_string(),
        },
    }
}

fn file_check(path: PathBuf, label: &str) -> DoctorCheck {
    DoctorCheck {
        name: label.to_string(),
        ok: path.exists(),
        detail: path.display().to_string(),
    }
}

fn binary_check(name: &str) -> DoctorCheck {
    let found = find_on_path(name);
    let detail = found
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "not found on PATH".to_string());

    DoctorCheck {
        name: format!("binary:{name}"),
        ok: found.is_some(),
        detail,
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
