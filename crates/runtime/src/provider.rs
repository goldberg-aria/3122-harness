use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{find_provider_profile, ConnectionMode, ProviderRegistry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRoute {
    Anthropic,
    OpenAiCompat,
    Ollama,
    ClaudeCode,
    Codex,
}

impl ProviderRoute {
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

#[derive(Debug, Clone)]
pub struct ProviderTarget {
    pub route: ProviderRoute,
    pub model: String,
    pub base_url_override: Option<String>,
    pub api_key_override: Option<String>,
    pub profile_alias: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderReply {
    pub route: ProviderRoute,
    pub model: String,
    pub text: String,
    pub tool_calls: Vec<ProviderToolCall>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments: Value,
}

pub fn parse_model_target(input: &str) -> Result<ProviderTarget, String> {
    let trimmed = input.trim();
    if let Some(model) = trimmed.strip_prefix("anthropic/") {
        return Ok(ProviderTarget {
            route: ProviderRoute::Anthropic,
            model: model.to_string(),
            base_url_override: None,
            api_key_override: None,
            profile_alias: None,
        });
    }
    if let Some(model) = trimmed.strip_prefix("openai/") {
        return Ok(ProviderTarget {
            route: ProviderRoute::OpenAiCompat,
            model: model.to_string(),
            base_url_override: None,
            api_key_override: None,
            profile_alias: None,
        });
    }
    if let Some(model) = trimmed.strip_prefix("ollama/") {
        return Ok(ProviderTarget {
            route: ProviderRoute::Ollama,
            model: model.to_string(),
            base_url_override: None,
            api_key_override: None,
            profile_alias: None,
        });
    }
    if let Some(model) = trimmed.strip_prefix("claude-code/") {
        return Ok(ProviderTarget {
            route: ProviderRoute::ClaudeCode,
            model: model.to_string(),
            base_url_override: None,
            api_key_override: None,
            profile_alias: None,
        });
    }
    if let Some(model) = trimmed.strip_prefix("codex/") {
        return Ok(ProviderTarget {
            route: ProviderRoute::Codex,
            model: model.to_string(),
            base_url_override: None,
            api_key_override: None,
            profile_alias: None,
        });
    }
    if trimmed.starts_with("claude-") {
        return Ok(ProviderTarget {
            route: ProviderRoute::Anthropic,
            model: trimmed.to_string(),
            base_url_override: None,
            api_key_override: None,
            profile_alias: None,
        });
    }
    if trimmed.starts_with("gpt-") || trimmed.starts_with("o1") || trimmed.starts_with("o3") {
        return Ok(ProviderTarget {
            route: ProviderRoute::OpenAiCompat,
            model: trimmed.to_string(),
            base_url_override: None,
            api_key_override: None,
            profile_alias: None,
        });
    }
    Err(format!("unsupported model target: {trimmed}"))
}

pub fn resolve_model_target(
    input: &str,
    registry: &ProviderRegistry,
) -> Result<ProviderTarget, String> {
    let trimmed = input.trim();
    if let Some(rest) = trimmed.strip_prefix("profile/") {
        let (alias, model) = rest
            .split_once('/')
            .ok_or_else(|| "profile targets must be profile/<alias>/<model>".to_string())?;
        let profile = find_provider_profile(registry, alias)
            .ok_or_else(|| format!("saved provider profile not found: {alias}"))?;
        let route = match profile.route.as_str() {
            "openai-compat" => ProviderRoute::OpenAiCompat,
            "anthropic" => ProviderRoute::Anthropic,
            "ollama" => ProviderRoute::Ollama,
            other => return Err(format!("unsupported saved provider route: {other}")),
        };
        return Ok(ProviderTarget {
            route,
            model: model.to_string(),
            base_url_override: Some(profile.base_url.clone()),
            api_key_override: Some(profile.api_key.clone()),
            profile_alias: Some(profile.alias.clone()),
        });
    }
    parse_model_target(trimmed)
}

pub fn resolve_model_target_with_mode(
    input: &str,
    registry: &ProviderRegistry,
    mode: ConnectionMode,
) -> Result<ProviderTarget, String> {
    let trimmed = input.trim();
    let target = resolve_model_target(trimmed, registry)?;

    if matches!(
        target.route,
        ProviderRoute::ClaudeCode | ProviderRoute::Codex
    ) {
        return Ok(target);
    }
    if trimmed.starts_with("profile/") || target.route == ProviderRoute::Ollama {
        return Ok(target);
    }

    match mode {
        ConnectionMode::Api => Ok(target),
        ConnectionMode::Auth => auth_adapter_target(&target).ok_or_else(|| {
            format!(
                "auth mode is not supported for route `{}`; use an API/profile target instead",
                target.route.as_str()
            )
        }),
        ConnectionMode::Auto => {
            if api_lane_available(&target) {
                return Ok(target);
            }
            auth_adapter_target(&target).ok_or_else(|| {
                format!(
                    "no API credentials found and no auth adapter is available for route `{}`",
                    target.route.as_str()
                )
            })
        }
    }
}

fn auth_adapter_target(target: &ProviderTarget) -> Option<ProviderTarget> {
    let route = match target.route {
        ProviderRoute::Anthropic => ProviderRoute::ClaudeCode,
        ProviderRoute::OpenAiCompat => ProviderRoute::Codex,
        _ => return None,
    };

    Some(ProviderTarget {
        route,
        model: target.model.clone(),
        base_url_override: None,
        api_key_override: None,
        profile_alias: target.profile_alias.clone(),
    })
}

fn api_lane_available(target: &ProviderTarget) -> bool {
    if target.api_key_override.is_some() || target.base_url_override.is_some() {
        return true;
    }

    match target.route {
        ProviderRoute::Anthropic => std::env::var("ANTHROPIC_API_KEY").is_ok(),
        ProviderRoute::OpenAiCompat => std::env::var("OPENAI_API_KEY").is_ok(),
        ProviderRoute::Ollama => true,
        ProviderRoute::ClaudeCode | ProviderRoute::Codex => false,
    }
}

pub fn send_prompt(target: &ProviderTarget, prompt: &str) -> Result<ProviderReply, String> {
    match target.route {
        ProviderRoute::Anthropic => anthropic_prompt_with_overrides(
            &target.model,
            prompt,
            target.base_url_override.as_deref(),
            target.api_key_override.as_deref(),
        ),
        ProviderRoute::OpenAiCompat => openai_compat_prompt_with_overrides(
            &target.model,
            prompt,
            target.base_url_override.as_deref(),
            target.api_key_override.as_deref(),
        ),
        ProviderRoute::Ollama => ollama_prompt(&target.model, prompt),
        ProviderRoute::ClaudeCode => claude_code_prompt(&target.model, prompt),
        ProviderRoute::Codex => codex_prompt(&target.model, prompt),
    }
}

fn anthropic_prompt_with_overrides(
    model: &str,
    prompt: &str,
    base_override: Option<&str>,
    api_key_override: Option<&str>,
) -> Result<ProviderReply, String> {
    let api_key = api_key_override
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
        .ok_or_else(|| "ANTHROPIC_API_KEY is not set".to_string())?;
    let base = base_override
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
        .unwrap_or_else(|| "https://api.anthropic.com".to_string());
    let url = join_url(&base, "/v1/messages");

    let response = Client::new()
        .post(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({
            "model": model,
            "max_tokens": 2048,
            "tools": anthropic_tool_definitions(),
            "messages": [
                { "role": "user", "content": prompt }
            ]
        }))
        .send()
        .map_err(|err| err.to_string())?;

    let status = response.status();
    let body = response.text().map_err(|err| err.to_string())?;
    if !status.is_success() {
        return Err(format!("anthropic error {status}: {body}"));
    }

    let parsed: AnthropicResponse = serde_json::from_str(&body).map_err(|err| err.to_string())?;
    let tool_calls = parse_anthropic_tool_calls(&parsed.content);
    let text = parsed
        .content
        .iter()
        .filter_map(|block| match block {
            AnthropicContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(ProviderReply {
        route: ProviderRoute::Anthropic,
        model: parsed.model,
        text,
        tool_calls,
    })
}

fn openai_compat_prompt_with_overrides(
    model: &str,
    prompt: &str,
    base_override: Option<&str>,
    api_key_override: Option<&str>,
) -> Result<ProviderReply, String> {
    let api_key = api_key_override
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .ok_or_else(|| "OPENAI_API_KEY is not set".to_string())?;
    let base = base_override
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let url = join_url(&base, "/chat/completions");

    let client = Client::new();
    let response = client
        .post(url)
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({
            "model": model,
            "tool_choice": "auto",
            "tools": native_tool_definitions(),
            "messages": [
                { "role": "user", "content": prompt }
            ]
        }))
        .send()
        .map_err(|err| err.to_string())?;

    let status = response.status();
    let body = response.text().map_err(|err| err.to_string())?;
    if !status.is_success() {
        if looks_like_tool_capability_error(&body) {
            return openai_compat_prompt_without_tools(
                model,
                prompt,
                base_override,
                api_key_override,
            );
        }
        return Err(format!("openai-compatible error {status}: {body}"));
    }

    let parsed: OpenAiResponse = serde_json::from_str(&body).map_err(|err| err.to_string())?;
    let choice = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| "openai-compatible response contained no choices".to_string())?;

    Ok(ProviderReply {
        route: ProviderRoute::OpenAiCompat,
        model: parsed.model,
        text: choice.message.content.unwrap_or_default(),
        tool_calls: parse_openai_tool_calls(choice.message.tool_calls)?,
    })
}

fn openai_compat_prompt_without_tools(
    model: &str,
    prompt: &str,
    base_override: Option<&str>,
    api_key_override: Option<&str>,
) -> Result<ProviderReply, String> {
    let api_key = api_key_override
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .ok_or_else(|| "OPENAI_API_KEY is not set".to_string())?;
    let base = base_override
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let url = join_url(&base, "/chat/completions");

    let response = Client::new()
        .post(url)
        .header(AUTHORIZATION, format!("Bearer {api_key}"))
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({
            "model": model,
            "messages": [
                { "role": "user", "content": prompt }
            ]
        }))
        .send()
        .map_err(|err| err.to_string())?;

    let status = response.status();
    let body = response.text().map_err(|err| err.to_string())?;
    if !status.is_success() {
        return Err(format!("openai-compatible error {status}: {body}"));
    }

    let parsed: OpenAiResponse = serde_json::from_str(&body).map_err(|err| err.to_string())?;
    let choice = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| "openai-compatible response contained no choices".to_string())?;

    Ok(ProviderReply {
        route: ProviderRoute::OpenAiCompat,
        model: parsed.model,
        text: choice.message.content.unwrap_or_default(),
        tool_calls: Vec::new(),
    })
}

fn ollama_prompt(model: &str, prompt: &str) -> Result<ProviderReply, String> {
    let base =
        std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    let url = join_url(&base, "/api/chat");

    let response = Client::new()
        .post(url)
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({
            "model": model,
            "stream": false,
            "tools": native_tool_definitions(),
            "messages": [
                { "role": "user", "content": prompt }
            ]
        }))
        .send()
        .map_err(|err| err.to_string())?;

    let status = response.status();
    let body = response.text().map_err(|err| err.to_string())?;
    if !status.is_success() {
        return Err(format!("ollama error {status}: {body}"));
    }

    let parsed: OllamaResponse = serde_json::from_str(&body).map_err(|err| err.to_string())?;
    Ok(ProviderReply {
        route: ProviderRoute::Ollama,
        model: parsed.model,
        text: parsed.message.content,
        tool_calls: parse_ollama_tool_calls(parsed.message.tool_calls),
    })
}

fn join_url(base: &str, suffix: &str) -> String {
    let base = base.trim_end_matches('/');
    let suffix = suffix.trim_start_matches('/');
    if base.ends_with("/v1") && suffix.starts_with("v1/") {
        return format!("{base}/{}", suffix.trim_start_matches("v1/"));
    }
    if base.ends_with("/api") && suffix.starts_with("api/") {
        return format!("{base}/{}", suffix.trim_start_matches("api/"));
    }
    format!("{base}/{suffix}")
}

fn claude_code_prompt(model: &str, prompt: &str) -> Result<ProviderReply, String> {
    let output = std::process::Command::new("claude")
        .arg("-p")
        .arg("--output-format")
        .arg("text")
        .arg("--model")
        .arg(model)
        .arg(prompt)
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "claude-code adapter failed: {}{}",
            stderr.trim(),
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!(" | stdout: {}", stdout.trim())
            }
        ));
    }

    Ok(ProviderReply {
        route: ProviderRoute::ClaudeCode,
        model: model.to_string(),
        text: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        tool_calls: Vec::new(),
    })
}

fn codex_prompt(model: &str, prompt: &str) -> Result<ProviderReply, String> {
    let output_file = std::env::temp_dir().join(format!(
        "harness-codex-last-message-{}.txt",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0)
    ));

    let mut child = std::process::Command::new("codex")
        .arg("exec")
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--model")
        .arg(model)
        .arg("--output-last-message")
        .arg(&output_file)
        .arg(prompt)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let status = loop {
        match child.try_wait().map_err(|err| err.to_string())? {
            Some(status) => break status,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&output_file);
                return Err("codex adapter timed out after 15 seconds".to_string());
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    };

    let output = child.wait_with_output().map_err(|err| err.to_string())?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let _ = std::fs::remove_file(&output_file);
        return Err(format!(
            "codex adapter failed: {}{}",
            stderr.trim(),
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!(" | stdout: {}", stdout.trim())
            }
        ));
    }

    let text = std::fs::read_to_string(&output_file).map_err(|err| err.to_string())?;
    let _ = std::fs::remove_file(&output_file);

    Ok(ProviderReply {
        route: ProviderRoute::Codex,
        model: model.to_string(),
        text: text.trim().to_string(),
        tool_calls: Vec::new(),
    })
}

fn native_tool_definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a UTF-8 text file from the current workspace.",
                "parameters": {
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": { "type": "string", "description": "Workspace-relative or absolute file path." }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write",
                "description": "Write a UTF-8 text file inside the current workspace.",
                "parameters": {
                    "type": "object",
                    "required": ["path", "contents"],
                    "properties": {
                        "path": { "type": "string" },
                        "contents": { "type": "string" }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Replace the first occurrence of a string in a file.",
                "parameters": {
                    "type": "object",
                    "required": ["path", "needle", "replacement"],
                    "properties": {
                        "path": { "type": "string" },
                        "needle": { "type": "string" },
                        "replacement": { "type": "string" }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search for matching text in files in the current workspace.",
                "parameters": {
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": { "type": "string" },
                        "path": { "type": "string" }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "glob",
                "description": "Find files matching a wildcard pattern in the current workspace.",
                "parameters": {
                    "type": "object",
                    "required": ["pattern"],
                    "properties": {
                        "pattern": { "type": "string" },
                        "path": { "type": "string" }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "exec",
                "description": "Run a shell command inside the current workspace.",
                "parameters": {
                    "type": "object",
                    "required": ["command"],
                    "properties": {
                        "command": { "type": "string" }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "parallel_read",
                "description": "Batch multiple safe read-only discovery operations in one turn.",
                "parameters": {
                    "type": "object",
                    "required": ["operations"],
                    "properties": {
                        "operations": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["tool"],
                                "properties": {
                                    "tool": { "type": "string", "enum": ["read", "grep", "glob"] },
                                    "path": { "type": "string" },
                                    "query": { "type": "string" },
                                    "pattern": { "type": "string" }
                                }
                            }
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "skill",
                "description": "Load a named skill packet for the current task.",
                "parameters": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": { "type": "string" },
                        "task": { "type": "string" }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "mcp_list_tools",
                "description": "List tools exposed by a configured MCP server.",
                "parameters": {
                    "type": "object",
                    "required": ["server"],
                    "properties": {
                        "server": { "type": "string" }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "mcp_call",
                "description": "Call a tool on a configured MCP server.",
                "parameters": {
                    "type": "object",
                    "required": ["server", "tool"],
                    "properties": {
                        "server": { "type": "string" },
                        "tool": { "type": "string" },
                        "arguments": { "type": "object" }
                    }
                }
            }
        }
    ])
}

fn anthropic_tool_definitions() -> Value {
    json!([
        {
            "name": "read",
            "description": "Read a UTF-8 text file from the current workspace.",
            "input_schema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "Workspace-relative or absolute file path." }
                }
            }
        },
        {
            "name": "write",
            "description": "Write a UTF-8 text file inside the current workspace.",
            "input_schema": {
                "type": "object",
                "required": ["path", "contents"],
                "properties": {
                    "path": { "type": "string" },
                    "contents": { "type": "string" }
                }
            }
        },
        {
            "name": "edit",
            "description": "Replace the first occurrence of a string in a file.",
            "input_schema": {
                "type": "object",
                "required": ["path", "needle", "replacement"],
                "properties": {
                    "path": { "type": "string" },
                    "needle": { "type": "string" },
                    "replacement": { "type": "string" }
                }
            }
        },
        {
            "name": "grep",
            "description": "Search for matching text in files in the current workspace.",
            "input_schema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string" },
                    "path": { "type": "string" }
                }
            }
        },
        {
            "name": "glob",
            "description": "Find files matching a wildcard pattern in the current workspace.",
            "input_schema": {
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" }
                }
            }
        },
        {
            "name": "exec",
            "description": "Run a shell command inside the current workspace.",
            "input_schema": {
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": { "type": "string" }
                }
            }
        },
        {
            "name": "parallel_read",
            "description": "Batch multiple safe read-only discovery operations in one turn.",
            "input_schema": {
                "type": "object",
                "required": ["operations"],
                "properties": {
                    "operations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["tool"],
                            "properties": {
                                "tool": { "type": "string", "enum": ["read", "grep", "glob"] },
                                "path": { "type": "string" },
                                "query": { "type": "string" },
                                "pattern": { "type": "string" }
                            }
                        }
                    }
                }
            }
        },
        {
            "name": "skill",
            "description": "Load a named skill packet for the current task.",
            "input_schema": {
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": { "type": "string" },
                    "task": { "type": "string" }
                }
            }
        },
        {
            "name": "mcp_list_tools",
            "description": "List tools exposed by a configured MCP server.",
            "input_schema": {
                "type": "object",
                "required": ["server"],
                "properties": {
                    "server": { "type": "string" }
                }
            }
        },
        {
            "name": "mcp_call",
            "description": "Call a tool on a configured MCP server.",
            "input_schema": {
                "type": "object",
                "required": ["server", "tool"],
                "properties": {
                    "server": { "type": "string" },
                    "tool": { "type": "string" },
                    "arguments": { "type": "object" }
                }
            }
        }
    ])
}

fn looks_like_tool_capability_error(body: &str) -> bool {
    let lowered = body.to_ascii_lowercase();
    (lowered.contains("tool") || lowered.contains("function"))
        && (lowered.contains("unsupported")
            || lowered.contains("not support")
            || lowered.contains("unknown"))
}

fn parse_openai_tool_calls(
    tool_calls: Option<Vec<OpenAiToolCall>>,
) -> Result<Vec<ProviderToolCall>, String> {
    tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|call| {
            Ok(ProviderToolCall {
                id: Some(call.id),
                name: call.function.name,
                arguments: parse_tool_arguments(&call.function.arguments)?,
            })
        })
        .collect()
}

fn parse_ollama_tool_calls(tool_calls: Option<Vec<OllamaToolCall>>) -> Vec<ProviderToolCall> {
    tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|call| ProviderToolCall {
            id: None,
            name: call.function.name,
            arguments: call.function.arguments,
        })
        .collect()
}

fn parse_anthropic_tool_calls(content: &[AnthropicContentBlock]) -> Vec<ProviderToolCall> {
    content
        .iter()
        .filter_map(|block| match block {
            AnthropicContentBlock::ToolUse { id, name, input } => Some(ProviderToolCall {
                id: Some(id.clone()),
                name: name.clone(),
                arguments: input.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn parse_tool_arguments(arguments: &str) -> Result<Value, String> {
    serde_json::from_str(arguments)
        .map_err(|err| format!("tool call arguments were not valid JSON: {err}"))
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    model: String,
    content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    model: String,
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCall {
    id: String,
    function: OpenAiFunctionCall,
}

#[derive(Debug, Deserialize)]
struct OpenAiFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    model: String,
    message: OllamaMessage,
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    content: String,
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCall {
    function: OllamaFunctionCall,
}

#[derive(Debug, Deserialize)]
struct OllamaFunctionCall {
    name: String,
    arguments: Value,
}

#[cfg(test)]
mod tests {
    use std::env;

    use crate::{send_prompt, ConnectionMode, ProviderRegistry, SavedProviderProfile};

    use super::{
        join_url, parse_anthropic_tool_calls, parse_model_target, parse_ollama_tool_calls,
        parse_openai_tool_calls, resolve_model_target, resolve_model_target_with_mode,
        AnthropicContentBlock, OllamaFunctionCall, OllamaToolCall, OpenAiFunctionCall,
        OpenAiToolCall, ProviderRoute,
    };
    use serde_json::json;

    #[test]
    fn parses_explicit_provider_prefixes() {
        let anthropic = parse_model_target("anthropic/claude-sonnet-4-6").unwrap();
        assert_eq!(anthropic.route, ProviderRoute::Anthropic);
        assert_eq!(anthropic.model, "claude-sonnet-4-6");

        let openai = parse_model_target("openai/gpt-4.1-mini").unwrap();
        assert_eq!(openai.route, ProviderRoute::OpenAiCompat);
        assert_eq!(openai.model, "gpt-4.1-mini");

        let ollama = parse_model_target("ollama/qwen2.5-coder:7b").unwrap();
        assert_eq!(ollama.route, ProviderRoute::Ollama);
        assert_eq!(ollama.model, "qwen2.5-coder:7b");

        let claude_code = parse_model_target("claude-code/sonnet").unwrap();
        assert_eq!(claude_code.route, ProviderRoute::ClaudeCode);
        assert_eq!(claude_code.model, "sonnet");

        let codex = parse_model_target("codex/o3").unwrap();
        assert_eq!(codex.route, ProviderRoute::Codex);
        assert_eq!(codex.model, "o3");
        assert!(codex.base_url_override.is_none());
    }

    #[test]
    fn parses_shortcut_model_names() {
        let anthropic = parse_model_target("claude-3-7-sonnet").unwrap();
        assert_eq!(anthropic.route, ProviderRoute::Anthropic);

        let gpt = parse_model_target("gpt-4.1").unwrap();
        assert_eq!(gpt.route, ProviderRoute::OpenAiCompat);

        let o3 = parse_model_target("o3-mini").unwrap();
        assert_eq!(o3.route, ProviderRoute::OpenAiCompat);
    }

    #[test]
    fn rejects_unknown_model_targets() {
        let err = parse_model_target("gemini/2.5-pro").unwrap_err();
        assert!(err.contains("unsupported model target"));
    }

    #[test]
    fn joins_urls_without_duplicate_version_segments() {
        assert_eq!(
            join_url("https://api.openai.com/v1", "/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            join_url("https://api.anthropic.com", "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            join_url("http://localhost:11434/api", "/api/chat"),
            "http://localhost:11434/api/chat"
        );
    }

    #[test]
    fn resolves_saved_profile_targets() {
        let registry = ProviderRegistry {
            profiles: vec![SavedProviderProfile {
                alias: "router".to_string(),
                route: "openai-compat".to_string(),
                base_url: "https://openrouter.ai/api/v1".to_string(),
                api_key: "sk-or-v1-test".to_string(),
                source: "detected".to_string(),
            }],
        };

        let target = resolve_model_target("profile/router/qwen/qwen3-coder", &registry).unwrap();
        assert_eq!(target.route, ProviderRoute::OpenAiCompat);
        assert_eq!(target.model, "qwen/qwen3-coder");
        assert_eq!(
            target.base_url_override.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(target.profile_alias.as_deref(), Some("router"));
    }

    #[test]
    fn resolves_auth_mode_to_claude_code_for_anthropic() {
        let registry = ProviderRegistry::default();
        let target = resolve_model_target_with_mode(
            "anthropic/claude-sonnet-4-6",
            &registry,
            ConnectionMode::Auth,
        )
        .unwrap();

        assert_eq!(target.route, ProviderRoute::ClaudeCode);
        assert_eq!(target.model, "claude-sonnet-4-6");
    }

    #[test]
    fn resolves_auth_mode_to_codex_for_openai() {
        let registry = ProviderRegistry::default();
        let target =
            resolve_model_target_with_mode("openai/gpt-4.1-mini", &registry, ConnectionMode::Auth)
                .unwrap();

        assert_eq!(target.route, ProviderRoute::Codex);
        assert_eq!(target.model, "gpt-4.1-mini");
    }

    #[test]
    fn keeps_profile_targets_on_api_lane_even_in_auth_mode() {
        let registry = ProviderRegistry {
            profiles: vec![SavedProviderProfile {
                alias: "zai".to_string(),
                route: "openai-compat".to_string(),
                base_url: "https://api.z.ai/api/paas/v4".to_string(),
                api_key: "demo".to_string(),
                source: "manual".to_string(),
            }],
        };

        let target =
            resolve_model_target_with_mode("profile/zai/glm-4.5", &registry, ConnectionMode::Auth)
                .unwrap();

        assert_eq!(target.route, ProviderRoute::OpenAiCompat);
        assert_eq!(target.profile_alias.as_deref(), Some("zai"));
    }

    #[test]
    fn keeps_explicit_adapter_targets_unchanged() {
        let registry = ProviderRegistry::default();
        let target =
            resolve_model_target_with_mode("codex/o3", &registry, ConnectionMode::Api).unwrap();

        assert_eq!(target.route, ProviderRoute::Codex);
        assert_eq!(target.model, "o3");
    }

    #[test]
    fn parses_openai_native_tool_calls() {
        let tool_calls = parse_openai_tool_calls(Some(vec![OpenAiToolCall {
            id: "call_1".to_string(),
            function: OpenAiFunctionCall {
                name: "read".to_string(),
                arguments: "{\"path\":\"README.md\"}".to_string(),
            },
        }]))
        .unwrap();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "read");
        assert_eq!(tool_calls[0].arguments["path"], "README.md");
    }

    #[test]
    fn parses_ollama_native_tool_calls() {
        let tool_calls = parse_ollama_tool_calls(Some(vec![OllamaToolCall {
            function: OllamaFunctionCall {
                name: "grep".to_string(),
                arguments: json!({ "query": "main", "path": "src" }),
            },
        }]));

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "grep");
        assert_eq!(tool_calls[0].arguments["query"], "main");
    }

    #[test]
    fn parses_anthropic_native_tool_calls() {
        let tool_calls = parse_anthropic_tool_calls(&[
            AnthropicContentBlock::Text {
                text: "planning".to_string(),
            },
            AnthropicContentBlock::ToolUse {
                id: "toolu_123".to_string(),
                name: "parallel_read".to_string(),
                input: json!({
                    "operations": [
                        { "tool": "read", "path": "README.md" }
                    ]
                }),
            },
        ]);

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id.as_deref(), Some("toolu_123"));
        assert_eq!(tool_calls[0].name, "parallel_read");
        assert_eq!(tool_calls[0].arguments["operations"][0]["tool"], "read");
    }

    #[test]
    fn live_openai_compatible_prompt_when_enabled() {
        if !live_test_enabled("openai") {
            return;
        }

        let model =
            env::var("HARNESS_TEST_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_string());
        let reply = send_prompt(
            &parse_model_target(&format!("openai/{model}")).unwrap(),
            "Reply with exactly: live-openai-ok",
        )
        .unwrap();

        assert_eq!(reply.route, ProviderRoute::OpenAiCompat);
        assert!(!reply.text.trim().is_empty());
    }

    #[test]
    fn live_anthropic_prompt_when_enabled() {
        if !live_test_enabled("anthropic") {
            return;
        }

        let model = env::var("HARNESS_TEST_ANTHROPIC_MODEL")
            .unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
        let reply = send_prompt(
            &parse_model_target(&format!("anthropic/{model}")).unwrap(),
            "Reply with exactly: live-anthropic-ok",
        )
        .unwrap();

        assert_eq!(reply.route, ProviderRoute::Anthropic);
        assert!(!reply.text.trim().is_empty());
    }

    #[test]
    fn live_ollama_prompt_when_enabled() {
        if !live_test_enabled("ollama") {
            return;
        }

        let model = env::var("HARNESS_TEST_OLLAMA_MODEL")
            .unwrap_or_else(|_| "qwen2.5-coder:7b".to_string());
        let reply = send_prompt(
            &parse_model_target(&format!("ollama/{model}")).unwrap(),
            "Reply with exactly: live-ollama-ok",
        )
        .unwrap();

        assert_eq!(reply.route, ProviderRoute::Ollama);
        assert!(!reply.text.trim().is_empty());
    }

    #[test]
    fn live_saved_profile_prompt_when_enabled() {
        if !saved_profile_live_test_enabled() {
            return;
        }

        let route = env::var("HARNESS_TEST_SAVED_PROFILE_ROUTE")
            .unwrap_or_else(|_| "openai-compat".to_string());
        let model = env::var("HARNESS_TEST_SAVED_PROFILE_MODEL")
            .unwrap_or_else(|_| "gpt-4.1-mini".to_string());
        let alias = env::var("HARNESS_TEST_SAVED_PROFILE_ALIAS")
            .unwrap_or_else(|_| "live-profile".to_string());
        let registry = ProviderRegistry {
            profiles: vec![SavedProviderProfile {
                alias: alias.clone(),
                route: route.clone(),
                base_url: env::var("HARNESS_TEST_SAVED_PROFILE_BASE_URL").unwrap(),
                api_key: env::var("HARNESS_TEST_SAVED_PROFILE_API_KEY").unwrap(),
                source: "env-test".to_string(),
            }],
        };

        let target = resolve_model_target_with_mode(
            &format!("profile/{alias}/{model}"),
            &registry,
            ConnectionMode::Api,
        )
        .unwrap();
        let reply = send_prompt(&target, "Reply with exactly: live-profile-ok").unwrap();

        assert_eq!(reply.route.as_str(), route);
        assert!(!reply.text.trim().is_empty());
    }

    #[test]
    fn live_claude_code_adapter_when_enabled() {
        if !auth_adapter_live_test_enabled("claude") {
            return;
        }

        let model =
            env::var("HARNESS_TEST_CLAUDE_CODE_MODEL").unwrap_or_else(|_| "sonnet".to_string());
        let reply = send_prompt(
            &parse_model_target(&format!("claude-code/{model}")).unwrap(),
            "Reply with exactly: live-claude-code-ok",
        )
        .unwrap();

        assert_eq!(reply.route, ProviderRoute::ClaudeCode);
        assert!(!reply.text.trim().is_empty());
    }

    #[test]
    fn live_codex_adapter_when_enabled() {
        if !auth_adapter_live_test_enabled("codex") {
            return;
        }

        let model = env::var("HARNESS_TEST_CODEX_MODEL").unwrap_or_else(|_| "o3".to_string());
        let reply = send_prompt(
            &parse_model_target(&format!("codex/{model}")).unwrap(),
            "Reply with exactly: live-codex-ok",
        )
        .unwrap();

        assert_eq!(reply.route, ProviderRoute::Codex);
        assert!(!reply.text.trim().is_empty());
    }

    fn live_test_enabled(provider: &str) -> bool {
        let _ = crate::load_workspace_env(&std::env::current_dir().unwrap_or_else(|_| ".".into()));
        matches!(
            env::var("HARNESS_RUN_LIVE_PROVIDER_TESTS").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        ) && match provider {
            "openai" => env::var("OPENAI_API_KEY").is_ok(),
            "anthropic" => env::var("ANTHROPIC_API_KEY").is_ok(),
            "ollama" => {
                env::var("HARNESS_TEST_OLLAMA_MODEL").is_ok() || env::var("OLLAMA_HOST").is_ok()
            }
            _ => false,
        }
    }

    fn saved_profile_live_test_enabled() -> bool {
        let _ = crate::load_workspace_env(&std::env::current_dir().unwrap_or_else(|_| ".".into()));
        matches!(
            env::var("HARNESS_RUN_LIVE_PROVIDER_TESTS").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        ) && env::var("HARNESS_TEST_SAVED_PROFILE_BASE_URL").is_ok()
            && env::var("HARNESS_TEST_SAVED_PROFILE_API_KEY").is_ok()
    }

    fn auth_adapter_live_test_enabled(adapter: &str) -> bool {
        let _ = crate::load_workspace_env(&std::env::current_dir().unwrap_or_else(|_| ".".into()));
        matches!(
            env::var("HARNESS_RUN_AUTH_ADAPTER_TESTS").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        ) && match adapter {
            "claude" => true,
            "codex" => true,
            _ => false,
        }
    }
}
