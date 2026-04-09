use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    assess_verification, call_mcp_tool, classify_approval_request, discover_mcp_servers,
    discover_skills, gather_workspace_context, list_mcp_tools, load_provider_registry,
    parallel_read_only, render_prompt_context, resolve_model_target_with_mode, resolve_skill,
    send_prompt, write_file, ApprovalRisk, ConnectionMode, LoadedConfig, PermissionMode,
    ProviderReply, ProviderRoute, ProviderTarget, ProviderToolCall, SessionStore, ToolOutput,
    VerificationEvent, VerificationPolicy,
};
use crate::{build_skill_packet, edit_file, exec_command, glob_search, grep_search, read_file};

#[derive(Debug, Clone)]
pub struct AgentOptions {
    pub max_steps: usize,
    pub permission_mode: PermissionMode,
    pub verification_policy: VerificationPolicy,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            max_steps: 8,
            permission_mode: PermissionMode::WorkspaceWrite,
            verification_policy: VerificationPolicy::Annotate,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentToolEvent {
    pub name: String,
    pub arguments: Value,
    pub summary: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct AgentReply {
    pub provider: ProviderReply,
    pub tool_events: Vec<AgentToolEvent>,
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub tool: String,
    pub arguments: Value,
    pub risk: ApprovalRisk,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Approve,
    Reject { reason: String },
}

#[derive(Debug, Clone)]
enum AgentEvent {
    User(String),
    Assistant(String),
    ToolCall {
        name: String,
        arguments: Value,
    },
    ToolResult {
        name: String,
        summary: String,
        content: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct ToolCallEnvelope {
    tool: String,
    #[serde(default)]
    arguments: Value,
}

pub fn run_agent_loop(
    config: &LoadedConfig,
    workspace_root: &Path,
    prompt: &str,
    override_model: Option<&str>,
    permission_mode: PermissionMode,
    session: Option<&SessionStore>,
    mut approval_handler: impl FnMut(&ApprovalRequest) -> Result<ApprovalOutcome, String>,
) -> Result<AgentReply, Vec<String>> {
    let mut chain = Vec::new();
    if let Some(model) = override_model {
        chain.push(model.to_string());
    } else if let Some(primary) = config.primary_model() {
        chain.push(primary.to_string());
        chain.extend(config.data.model.fallback.clone());
    }

    if chain.is_empty() {
        return Err(vec![
            "no model configured; set [model].primary in harness.toml".to_string(),
        ]);
    }

    let options = AgentOptions {
        permission_mode,
        verification_policy: config.default_verification_policy(),
        ..AgentOptions::default()
    };
    let connection_mode = if session.is_some() {
        config.interactive_connection_mode()
    } else {
        config.default_connection_mode()
    };
    let skill_sources = config.skill_sources(workspace_root);
    let mcp_sources = config.mcp_sources(workspace_root);
    let registry = match load_provider_registry(workspace_root) {
        Ok(registry) => registry,
        Err(err) => return Err(vec![format!("failed to load provider registry: {err}")]),
    };
    let mut errors = Vec::new();
    for model in chain {
        let target = match resolve_model_target_with_mode(&model, &registry, connection_mode) {
            Ok(target) => target,
            Err(err) => {
                errors.push(format!("{model}: {err}"));
                continue;
            }
        };
        let prompt_context = render_prompt_context(&gather_workspace_context(
            workspace_root,
            config,
            permission_mode,
            Some(&display_model(&model, &target, connection_mode)),
            Some(prompt),
        ));

        match run_agent_loop_with_runner(
            &target,
            workspace_root,
            &skill_sources,
            &mcp_sources,
            &prompt_context,
            prompt,
            &options,
            session,
            |target, composed_prompt| send_prompt(target, composed_prompt),
            &mut approval_handler,
        ) {
            Ok(reply) => return Ok(reply),
            Err(err) => errors.push(format!("{model}: {err}")),
        }
    }

    Err(errors)
}

fn display_model(requested_model: &str, target: &ProviderTarget, mode: ConnectionMode) -> String {
    if matches!(mode, ConnectionMode::Api) || requested_model.starts_with(target.route.as_str()) {
        return requested_model.to_string();
    }
    format!("{}/{}", target.route.as_str(), target.model)
}

pub fn run_agent_loop_with_runner<F, G>(
    target: &ProviderTarget,
    workspace_root: &Path,
    skill_sources: &[(PathBuf, String)],
    mcp_sources: &[(PathBuf, String)],
    prompt_context: &str,
    prompt: &str,
    options: &AgentOptions,
    session: Option<&SessionStore>,
    mut runner: F,
    mut approval_handler: G,
) -> Result<AgentReply, String>
where
    F: FnMut(&ProviderTarget, &str) -> Result<ProviderReply, String>,
    G: FnMut(&ApprovalRequest) -> Result<ApprovalOutcome, String>,
{
    let mut events = vec![AgentEvent::User(prompt.trim().to_string())];
    let mut tool_events = Vec::new();

    for step in 0..options.max_steps {
        let composed_prompt = compose_prompt(target, prompt_context, &events);
        if let Some(store) = session {
            let _ = store.append(
                "agent_turn",
                json!({
                    "step": step + 1,
                    "provider": target.route.as_str(),
                    "model": target.model,
                    "prompt": composed_prompt,
                }),
            );
        }

        let reply = runner(target, &composed_prompt)?;
        let text = strip_thinking_blocks(&reply.text);
        let sanitized_reply = ProviderReply {
            route: reply.route,
            model: reply.model.clone(),
            text: text.clone(),
            tool_calls: reply.tool_calls.clone(),
        };

        if !reply.tool_calls.is_empty() {
            execute_provider_tool_calls(
                &reply.tool_calls,
                workspace_root,
                skill_sources,
                mcp_sources,
                options.permission_mode,
                step + 1,
                session,
                &mut tool_events,
                &mut events,
                &mut approval_handler,
            )?;
            continue;
        }

        if let Some(request) = parse_tool_call(&text)? {
            execute_model_tool_call(
                request.tool,
                request.arguments,
                workspace_root,
                skill_sources,
                mcp_sources,
                options.permission_mode,
                step + 1,
                session,
                &mut tool_events,
                &mut events,
                &mut approval_handler,
                "text",
            )?;
            continue;
        }

        let final_text = enforce_verification_policy(
            workspace_root,
            &text,
            &tool_events,
            options.verification_policy,
        )?;
        if final_text != text && final_text.starts_with("Not verified:") {
            if let Some(store) = session {
                let _ = store.append(
                    "verification_warning",
                    json!({
                        "reason": "no verification step recorded after workspace changes",
                    }),
                );
            }
        }

        events.push(AgentEvent::Assistant(final_text.clone()));
        if let Some(store) = session {
            let _ = store.append(
                "agent_result",
                json!({
                    "step": step + 1,
                    "provider": sanitized_reply.route.as_str(),
                    "model": sanitized_reply.model.clone(),
                    "text": final_text,
                }),
            );
        }

        return Ok(AgentReply {
            provider: ProviderReply {
                text: final_text,
                ..sanitized_reply
            },
            tool_events,
        });
    }

    Err(format!(
        "agent loop exceeded {} steps without a final answer",
        options.max_steps
    ))
}

fn execute_provider_tool_calls<G>(
    tool_calls: &[ProviderToolCall],
    workspace_root: &Path,
    skill_sources: &[(PathBuf, String)],
    mcp_sources: &[(PathBuf, String)],
    permission_mode: PermissionMode,
    step: usize,
    session: Option<&SessionStore>,
    tool_events: &mut Vec<AgentToolEvent>,
    events: &mut Vec<AgentEvent>,
    approval_handler: &mut G,
) -> Result<(), String>
where
    G: FnMut(&ApprovalRequest) -> Result<ApprovalOutcome, String>,
{
    for tool_call in tool_calls {
        execute_model_tool_call(
            tool_call.name.clone(),
            tool_call.arguments.clone(),
            workspace_root,
            skill_sources,
            mcp_sources,
            permission_mode,
            step,
            session,
            tool_events,
            events,
            approval_handler,
            "native",
        )?;
    }
    Ok(())
}

fn execute_model_tool_call<G>(
    tool_name: String,
    arguments: Value,
    workspace_root: &Path,
    skill_sources: &[(PathBuf, String)],
    mcp_sources: &[(PathBuf, String)],
    permission_mode: PermissionMode,
    step: usize,
    session: Option<&SessionStore>,
    tool_events: &mut Vec<AgentToolEvent>,
    events: &mut Vec<AgentEvent>,
    approval_handler: &mut G,
    source: &str,
) -> Result<(), String>
where
    G: FnMut(&ApprovalRequest) -> Result<ApprovalOutcome, String>,
{
    let (risk, reason) = classify_approval_request(&tool_name, &arguments);
    let approval = approval_handler(&ApprovalRequest {
        tool: tool_name.clone(),
        arguments: arguments.clone(),
        risk,
        reason,
    })?;
    if let ApprovalOutcome::Reject { reason } = approval {
        return Err(format!("tool request rejected: {reason}"));
    }

    let request = ToolCallEnvelope {
        tool: tool_name.clone(),
        arguments: arguments.clone(),
    };
    let output = match execute_tool_request(
        workspace_root,
        skill_sources,
        mcp_sources,
        &request,
        permission_mode,
    ) {
        Ok(output) => output,
        Err(err) => {
            events.push(AgentEvent::ToolCall {
                name: tool_name.clone(),
                arguments: arguments.clone(),
            });
            events.push(AgentEvent::ToolResult {
                name: tool_name.clone(),
                summary: format!("tool error: {err}"),
                content: err.clone(),
            });
            if let Some(store) = session {
                let _ = store.append(
                    "agent_tool_error",
                    json!({
                        "step": step,
                        "source": source,
                        "name": tool_name,
                        "arguments": arguments,
                        "error": err,
                    }),
                );
            }
            return Ok(());
        }
    };

    let event = AgentToolEvent {
        name: tool_name.clone(),
        arguments: arguments.clone(),
        summary: output.summary.clone(),
        content: output.content.clone(),
    };
    tool_events.push(event.clone());

    events.push(AgentEvent::ToolCall {
        name: tool_name.clone(),
        arguments: arguments.clone(),
    });
    events.push(AgentEvent::ToolResult {
        name: tool_name,
        summary: output.summary,
        content: output.content,
    });

    if let Some(store) = session {
        let _ = store.append(
            "agent_tool",
            json!({
                "step": step,
                "source": source,
                "name": event.name,
                "arguments": event.arguments,
                "summary": event.summary,
                "content": event.content,
            }),
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptShape {
    Default,
    Compact,
}

#[derive(Debug, Clone, Copy)]
struct PromptProfile {
    shape: PromptShape,
    answer_line_limit: usize,
}

fn compose_prompt(target: &ProviderTarget, prompt_context: &str, events: &[AgentEvent]) -> String {
    let profile = prompt_profile(target);
    let mut prompt = String::new();
    let header = match profile.shape {
        PromptShape::Default => {
            let mut header = String::new();
            header.push_str(
                "You are a terminal coding agent running inside a harness.\n\
Critical rules:\n\
- Verify before you claim success. Run at least 1 concrete check when a check is possible.\n\
- If you cannot verify, say so explicitly in 1 sentence.\n",
            );
            header.push_str(&format!(
                "- Keep the final answer short by default, usually within {} lines unless the user asks for depth.\n",
                profile.answer_line_limit
            ));
            header.push_str(
                "- Treat this session as isolated to the current workspace and prompt context only.\n\
- If the user asked a question, summary, explanation, or analysis, answer directly.\n\
- Do not create or edit files unless the user explicitly asked for implementation or file changes.\n\
- The current User request overrides older memory, recall, and prior session tasks.\n\
- Do not continue an older file-writing request unless the current User request repeats it.\n\
\n\
Do:\n\
- use read, grep, and glob before write, edit, or exec when more context is needed\n\
- use parallel_read to batch several safe discovery operations into one turn when helpful\n\
- request exactly 1 tool call at a time\n\
- use tool results as evidence for the next step\n\
- verify before saying the task is done\n\
\n\
Don't:\n\
- do not claim completion without verification\n\
- do not emit chain-of-thought or `<thinking>` blocks\n\
- do not mix other tasks, repos, or assumptions into this session\n\
- do not return prose alongside a tool block\n\
\n\
You may either answer normally, or request exactly one tool call.\n\
If you need a tool, respond with only this XML block and nothing else:\n\
<tool_call>\n\
{\"tool\":\"read\",\"arguments\":{\"path\":\"README.md\"}}\n\
</tool_call>\n\n\
Available built-in tools:\n\
- read { path }\n\
- write { path, contents }\n\
- edit { path, needle, replacement }\n\
- grep { query, path? }\n\
- glob { pattern, path? }\n\
- exec { command }\n\
- parallel_read { operations[{ tool, path?/query?/pattern? }] }\n\
- skill { name, task? }\n\
- mcp_list_tools { server }\n\
- mcp_call { server, tool, arguments? }\n\n\
Verification reminder:\n\
- before a final answer, prefer at least 1 verification step\n\
- if verification is impossible, say `Not verified` and give the reason\n\
\n\
When a tool result appears, use it to decide the next step. When you are done, answer normally without a tool block.\n\n",
            );
            header
        }
        PromptShape::Compact => {
            let mut header = String::new();
            header.push_str(
                "You are a terminal coding agent running inside a harness.\n\
Compact rules for this model:\n\
- Stay inside the current workspace and prompt context only.\n\
- Prefer the shortest path to evidence: grep, glob, read, then mutate.\n\
- Request exactly 1 tool call at a time.\n\
- After code edits, run 1 concrete verification step before claiming success.\n\
- If verification is impossible, say `Not verified` in 1 sentence.\n",
            );
            header.push_str(&format!(
                "- Keep the final answer within about {} lines unless the user asks for more.\n",
                profile.answer_line_limit
            ));
            header.push_str(
                "- If the user asked for explanation, summary, or analysis, answer directly.\n\
- Do not create or edit files unless the user clearly asked for file changes.\n\
- The current User request overrides older recall and prior file-writing tasks.\n\
- Do not emit chain-of-thought or `<thinking>`.\n\
\n\
Tool request format:\n\
<tool_call>\n\
{\"tool\":\"read\",\"arguments\":{\"path\":\"README.md\"}}\n\
</tool_call>\n\
\n\
Tools:\n\
- read { path }\n\
- write { path, contents }\n\
- edit { path, needle, replacement }\n\
- grep { query, path? }\n\
- glob { pattern, path? }\n\
- exec { command }\n\
- parallel_read { operations[{ tool, path?/query?/pattern? }] }\n\
- skill { name, task? }\n\
- mcp_list_tools { server }\n\
- mcp_call { server, tool, arguments? }\n\
\n\
When a tool result appears, use it to decide the next step. Keep each step atomic.\n\n",
            );
            header
        }
    };
    prompt.push_str(&header);
    prompt.push_str(prompt_context);
    let reminder = match profile.shape {
        PromptShape::Default => format!(
            "\nReminder:\n\
- verify before claiming success\n\
- request exactly one tool call if needed\n\
- keep the final answer concise, usually within {} lines\n\
- stay within the current workspace boundary\n\n",
            profile.answer_line_limit
        ),
        PromptShape::Compact => format!(
            "\nReminder:\n\
- use the shortest next step\n\
- request exactly one tool call if needed\n\
- verify after edits\n\
- keep the final answer within about {} lines\n\n",
            profile.answer_line_limit
        ),
    };
    prompt.push_str(&reminder);

    for event in events {
        match event {
            AgentEvent::User(text) => {
                prompt.push_str("User:\n");
                prompt.push_str(text);
                prompt.push_str("\n\n");
            }
            AgentEvent::Assistant(text) => {
                prompt.push_str("Assistant:\n");
                prompt.push_str(text);
                prompt.push_str("\n\n");
            }
            AgentEvent::ToolCall { name, arguments } => {
                prompt.push_str("Tool call:\n");
                prompt.push_str(&format!("{name} {}\n\n", arguments));
            }
            AgentEvent::ToolResult {
                name,
                summary,
                content,
            } => {
                prompt.push_str("Tool result:\n");
                prompt.push_str(&format!(
                    "name: {name}\nsummary: {summary}\ncontent:\n{content}\n\n"
                ));
            }
        }
    }

    let final_reminder = match profile.shape {
        PromptShape::Default => {
            "Final reminder:\n\
- verify before claiming completion\n\
- do not emit `<thinking>`\n\
- either output one tool block or a normal answer\n"
        }
        PromptShape::Compact => {
            "Final reminder:\n\
- verify before claiming completion\n\
- either output one tool block or one short normal answer\n\
- do not emit `<thinking>`\n"
        }
    };
    prompt.push_str(final_reminder);

    prompt
}

fn prompt_profile(target: &ProviderTarget) -> PromptProfile {
    if uses_compact_prompt(target) {
        return PromptProfile {
            shape: PromptShape::Compact,
            answer_line_limit: 8,
        };
    }

    PromptProfile {
        shape: PromptShape::Default,
        answer_line_limit: 12,
    }
}

fn uses_compact_prompt(target: &ProviderTarget) -> bool {
    if target.route == ProviderRoute::Ollama {
        return true;
    }

    let model = target.model.to_ascii_lowercase();
    [
        "qwen", "llama", "mistral", "gemma", "phi", "deepseek", "yi", "minicpm",
    ]
    .iter()
    .any(|family| model.contains(family))
}

fn strip_thinking_blocks(text: &str) -> String {
    let mut remaining = text;
    let mut cleaned = String::new();

    loop {
        let Some(start) = remaining.find("<thinking>") else {
            cleaned.push_str(remaining);
            break;
        };
        cleaned.push_str(&remaining[..start]);
        let after_start = &remaining[start + "<thinking>".len()..];
        let Some(end) = after_start.find("</thinking>") else {
            break;
        };
        remaining = &after_start[end + "</thinking>".len()..];
    }

    cleaned.trim().to_string()
}

fn enforce_verification_policy(
    workspace_root: &Path,
    text: &str,
    tool_events: &[AgentToolEvent],
    policy: VerificationPolicy,
) -> Result<String, String> {
    let assessment = assess_verification(
        workspace_root,
        &tool_events
            .iter()
            .map(|event| VerificationEvent {
                name: event.name.clone(),
                arguments: event.arguments.clone(),
            })
            .collect::<Vec<_>>(),
    );
    if !assessment.requires_verification || assessment.has_verification_after_last_mutation {
        return Ok(text.to_string());
    }

    if text.to_ascii_lowercase().contains("not verified") {
        return Ok(text.to_string());
    }

    let guidance = assessment.guidance();

    match policy {
        VerificationPolicy::Off => Ok(text.to_string()),
        VerificationPolicy::Annotate => Ok(format!("Not verified: {guidance}\n\n{text}")),
        VerificationPolicy::Require => Err(format!(
            "verification required after workspace changes; {guidance}"
        )),
    }
}

fn parse_tool_call(text: &str) -> Result<Option<ToolCallEnvelope>, String> {
    let trimmed = text.trim();
    if !trimmed.contains("<tool_call>") {
        return Ok(None);
    }
    let body = extract_tool_call_body(trimmed).ok_or_else(|| "invalid tool_call wrapper".to_string())?;
    let repaired = repair_text_tool_call_body(&body);
    let parsed = serde_json::from_str::<ToolCallEnvelope>(&repaired).map_err(|err| {
        format!(
            "tool call arguments were not valid JSON: {err}; raw={body}; repaired={repaired}"
        )
    })?;
    if parsed.tool.trim().is_empty() {
        return Err("tool call is missing tool name".to_string());
    }
    Ok(Some(parsed))
}

fn extract_tool_call_body(text: &str) -> Option<String> {
    let start = text.find("<tool_call>")?;
    let rest = &text[start + "<tool_call>".len()..];
    let end = rest.find("</tool_call>")?;
    Some(rest[..end].trim().to_string())
}

fn repair_text_tool_call_body(body: &str) -> String {
    let mut repaired = body.trim().replace("</parameter>", "");
    repaired = repaired.replace("<parameter>", "");
    repaired = repaired.replace("```json", "");
    repaired = repaired.replace("```", "");
    repaired = repaired.trim().to_string();
    if repaired.ends_with(',') {
        repaired.pop();
    }
    close_unbalanced_quotes(&mut repaired);
    close_unbalanced_pairs(&mut repaired, '{', '}');
    close_unbalanced_pairs(&mut repaired, '[', ']');
    repaired
}

fn close_unbalanced_quotes(text: &mut String) {
    let mut quote_count = 0;
    let mut escaped = false;
    for ch in text.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            quote_count += 1;
        }
    }
    if quote_count % 2 == 1 {
        text.push('"');
    }
}

fn close_unbalanced_pairs(text: &mut String, open: char, close: char) {
    let opens = text.chars().filter(|ch| *ch == open).count();
    let closes = text.chars().filter(|ch| *ch == close).count();
    for _ in 0..opens.saturating_sub(closes) {
        text.push(close);
    }
}

fn execute_tool_request(
    workspace_root: &Path,
    skill_sources: &[(PathBuf, String)],
    mcp_sources: &[(PathBuf, String)],
    request: &ToolCallEnvelope,
    permission_mode: PermissionMode,
) -> Result<ToolOutput, String> {
    match request.tool.as_str() {
        "read" => {
            let path = required_string_aliases(
                &request.arguments,
                "path",
                &["filepath", "file_path", "file", "filename", "target"],
            )?;
            read_file(Path::new(&path), workspace_root, permission_mode)
        }
        "write" => {
            let path = required_string_aliases(
                &request.arguments,
                "path",
                &["filepath", "file_path", "file", "filename", "target"],
            )?;
            let contents = required_string(&request.arguments, "contents")?;
            write_file(Path::new(&path), &contents, workspace_root, permission_mode)
        }
        "edit" => {
            let path = required_string_aliases(
                &request.arguments,
                "path",
                &["filepath", "file_path", "file", "filename", "target"],
            )?;
            let needle = required_string(&request.arguments, "needle")?;
            let replacement = required_string(&request.arguments, "replacement")?;
            edit_file(
                Path::new(&path),
                &needle,
                &replacement,
                workspace_root,
                permission_mode,
            )
        }
        "grep" => {
            let query = required_string(&request.arguments, "query")?;
            let scope = optional_string_aliases(
                &request.arguments,
                "path",
                &["filepath", "file_path", "file", "directory", "dir", "target"],
            );
            grep_search(
                &query,
                scope.as_deref().map(Path::new),
                workspace_root,
                permission_mode,
            )
        }
        "glob" => {
            let pattern = required_string(&request.arguments, "pattern")?;
            let scope = optional_string_aliases(
                &request.arguments,
                "path",
                &["filepath", "file_path", "file", "directory", "dir", "target"],
            );
            glob_search(
                &pattern,
                scope.as_deref().map(Path::new),
                workspace_root,
                permission_mode,
            )
        }
        "exec" => {
            let command = required_string(&request.arguments, "command")?;
            exec_command(&command, workspace_root, permission_mode)
        }
        "parallel_read" => {
            let operations = request
                .arguments
                .get("operations")
                .and_then(Value::as_array)
                .ok_or_else(|| "parallel_read requires an `operations` array".to_string())?;
            parallel_read_only(operations, workspace_root, permission_mode)
        }
        "skill" => {
            let name = required_string(&request.arguments, "name")?;
            let task = optional_string(&request.arguments, "task");
            let skills = discover_skills(skill_sources);
            let skill = resolve_skill(&skills, &name)?;
            let packet = build_skill_packet(skill, task.as_deref());
            Ok(ToolOutput {
                summary: format!("loaded skill {}", packet.skill.name),
                content: packet.prompt,
            })
        }
        "mcp_list_tools" => {
            let server = required_string(&request.arguments, "server")?;
            let servers = discover_mcp_servers(mcp_sources);
            let tools = list_mcp_tools(&servers, &server)?;
            Ok(ToolOutput {
                summary: format!("listed {} MCP tools on {}", tools.len(), server),
                content: serde_json::to_string_pretty(&tools_to_json(&tools))
                    .map_err(|err| err.to_string())?,
            })
        }
        "mcp_call" => {
            let server = required_string(&request.arguments, "server")?;
            let tool = required_string(&request.arguments, "tool")?;
            let arguments = request
                .arguments
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let servers = discover_mcp_servers(mcp_sources);
            let result = call_mcp_tool(&servers, &server, &tool, arguments)?;
            Ok(ToolOutput {
                summary: format!("called MCP tool {} on {}", tool, server),
                content: serde_json::to_string_pretty(&result).map_err(|err| err.to_string())?,
            })
        }
        other => Err(format!("unknown tool requested by model: {other}")),
    }
}

fn tools_to_json(tools: &[crate::McpToolInfo]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                })
            })
            .collect(),
    )
}

fn required_string(arguments: &Value, key: &str) -> Result<String, String> {
    required_string_aliases(arguments, key, &[])
}

fn optional_string(arguments: &Value, key: &str) -> Option<String> {
    optional_string_aliases(arguments, key, &[])
}

fn required_string_aliases(
    arguments: &Value,
    primary: &str,
    aliases: &[&str],
) -> Result<String, String> {
    optional_string_aliases(arguments, primary, aliases)
        .ok_or_else(|| format!("tool argument `{primary}` must be a string"))
}

fn optional_string_aliases(arguments: &Value, primary: &str, aliases: &[&str]) -> Option<String> {
    if let Some(value) = arguments.get(primary).and_then(coerce_string_value) {
        return Some(value);
    }
    for alias in aliases {
        if let Some(value) = arguments.get(*alias).and_then(coerce_string_value) {
            return Some(value);
        }
    }
    if aliases.iter().any(|alias| *alias == primary) {
        return None;
    }
    if let Value::String(_) | Value::Array(_) | Value::Object(_) = arguments {
        return coerce_string_value(arguments);
    }
    None
}

fn coerce_string_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Array(items) if items.len() == 1 => items.first().and_then(coerce_string_value),
        Value::Object(map) => {
            for key in ["value", "path", "file", "name", "text"] {
                if let Some(next) = map.get(key).and_then(coerce_string_value) {
                    return Some(next);
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use crate::{
        gather_workspace_context, parse_model_target, render_prompt_context, LoadedConfig,
        PermissionMode, VerificationPolicy,
    };

    use super::{
        coerce_string_value, enforce_verification_policy, parse_tool_call, run_agent_loop_with_runner,
        strip_thinking_blocks, AgentOptions, ApprovalOutcome, ApprovalRequest,
    };

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
    fn parses_tool_call_blocks() {
        let parsed = parse_tool_call(
            "<tool_call>{\"tool\":\"read\",\"arguments\":{\"path\":\"README.md\"}}</tool_call>",
        )
        .unwrap()
        .unwrap();
        assert_eq!(parsed.tool, "read");
        assert_eq!(parsed.arguments["path"], "README.md");
    }

    #[test]
    fn parses_tool_call_blocks_with_wrappers_and_noise() {
        let parsed = parse_tool_call(
            "알겠습니다.\n<tool_call>\n{\"tool\":\"exec\",\"arguments\":{\"command\":\"ls -la\"}}</parameter>\n</tool_call>\n진행하겠습니다.",
        )
        .unwrap()
        .unwrap();
        assert_eq!(parsed.tool, "exec");
        assert_eq!(parsed.arguments["command"], "ls -la");
    }

    #[test]
    fn strips_thinking_blocks_from_provider_text() {
        let cleaned =
            strip_thinking_blocks("<thinking>private reasoning</thinking>\nFinal answer only");
        assert_eq!(cleaned, "Final answer only");
    }

    #[test]
    fn annotates_unverified_completion_after_mutation() {
        let workspace = temp_workspace("verification-annotate");
        let text = enforce_verification_policy(
            &workspace,
            "Implemented the fix.",
            &[crate::AgentToolEvent {
                name: "write".to_string(),
                arguments: json!({ "path": "src/main.rs", "contents": "fn main() {}" }),
                summary: "wrote src/main.rs".to_string(),
                content: "12 bytes".to_string(),
            }],
            VerificationPolicy::Annotate,
        )
        .unwrap();
        assert!(text.starts_with("Not verified:"));
        assert!(text.contains("src/main.rs"));
        assert!(text.contains("cargo test --workspace"));
        cleanup(&workspace);
    }

    #[test]
    fn docs_only_edits_do_not_require_verification() {
        let workspace = temp_workspace("verification-docs-only");
        let text = enforce_verification_policy(
            &workspace,
            "Updated the docs.",
            &[crate::AgentToolEvent {
                name: "write".to_string(),
                arguments: json!({ "path": "docs/ROADMAP.md", "contents": "# updated" }),
                summary: "wrote docs/ROADMAP.md".to_string(),
                content: "9 bytes".to_string(),
            }],
            VerificationPolicy::Require,
        )
        .unwrap();
        assert_eq!(text, "Updated the docs.");
        cleanup(&workspace);
    }

    #[test]
    fn verification_must_happen_after_last_mutation() {
        let workspace = temp_workspace("verification-order");
        fs::write(workspace.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let error = enforce_verification_policy(
            &workspace,
            "Implemented the fix.",
            &[
                crate::AgentToolEvent {
                    name: "exec".to_string(),
                    arguments: json!({ "command": "cargo test --workspace" }),
                    summary: "cargo test".to_string(),
                    content: "ok".to_string(),
                },
                crate::AgentToolEvent {
                    name: "write".to_string(),
                    arguments: json!({ "path": "src/main.rs", "contents": "fn main() {}" }),
                    summary: "wrote src/main.rs".to_string(),
                    content: "12 bytes".to_string(),
                },
            ],
            VerificationPolicy::Require,
        )
        .unwrap_err();
        assert!(error.contains("src/main.rs"));
        cleanup(&workspace);
    }

    #[test]
    fn runs_loop_with_builtin_read_tool() {
        let workspace = temp_workspace("agent-loop-read");
        fs::write(workspace.join("README.md"), "hello from tool").unwrap();
        fs::write(workspace.join("AGENTS.md"), "workspace instructions").unwrap();
        let target = parse_model_target("anthropic/claude-sonnet-4-6").unwrap();
        let options = AgentOptions {
            max_steps: 4,
            permission_mode: PermissionMode::WorkspaceWrite,
            verification_policy: VerificationPolicy::Annotate,
        };
        let skill_sources: Vec<(PathBuf, String)> = Vec::new();
        let mcp_sources: Vec<(PathBuf, String)> = Vec::new();
        let prompt_context = render_prompt_context(&gather_workspace_context(
            &workspace,
            &LoadedConfig::default(),
            options.permission_mode,
            Some(&format!("{}/{}", target.route.as_str(), target.model)),
            Some("read the readme and summarize it"),
        ));
        let mut prompts = Vec::new();
        let mut call_count = 0;

        let reply = run_agent_loop_with_runner(
            &target,
            &workspace,
            &skill_sources,
            &mcp_sources,
            &prompt_context,
            "read the readme and summarize it",
            &options,
            None,
            |_, prompt| {
                prompts.push(prompt.to_string());
                call_count += 1;
                if call_count == 1 {
                    Ok(crate::ProviderReply {
                        route: target.route,
                        model: target.model.clone(),
                        text: "<tool_call>{\"tool\":\"read\",\"arguments\":{\"path\":\"README.md\"}}</tool_call>"
                            .to_string(),
                        tool_calls: Vec::new(),
                    })
                } else {
                    Ok(crate::ProviderReply {
                        route: target.route,
                        model: target.model.clone(),
                        text: "README says: hello from tool".to_string(),
                        tool_calls: Vec::new(),
                    })
                }
            },
            |_| Ok(ApprovalOutcome::Approve),
        )
        .unwrap();

        assert_eq!(reply.tool_events.len(), 1);
        assert_eq!(reply.tool_events[0].name, "read");
        assert_eq!(reply.provider.text, "README says: hello from tool");
        assert!(prompts[0].contains("workspace instructions"));
        assert!(prompts[0].contains("permission_mode: workspace-write"));
        assert!(prompts[0].contains("Critical rules:"));
        assert!(prompts[0].contains("Do:"));
        assert!(prompts[0].contains("Don't:"));
        assert!(prompts[0].contains("Final reminder:"));
        assert!(prompts[1].contains("hello from tool"));

        cleanup(&workspace);
    }

    #[test]
    fn runs_loop_with_skill_and_mcp_tools() {
        let workspace = temp_workspace("agent-loop-skill-mcp");
        let skill_dir = workspace.join(".harness/skills/bootstrap");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "# Bootstrap\nDo staged setup.").unwrap();
        fs::create_dir_all(workspace.join(".harness")).unwrap();

        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("scripts/mock_mcp_echo.js");
        fs::write(
            workspace.join(".harness/mcp.json"),
            format!(
                r#"{{
  "servers": [
    {{
      "name": "mock-echo",
      "transport": "stdio",
      "command": "node {}",
      "enabled": true
    }}
  ]
}}"#,
                script.display()
            ),
        )
        .unwrap();

        let target = parse_model_target("ollama/test-model").unwrap();
        let options = AgentOptions {
            max_steps: 5,
            permission_mode: PermissionMode::WorkspaceWrite,
            verification_policy: VerificationPolicy::Annotate,
        };
        let skill_sources = vec![(workspace.join(".harness/skills"), "workspace".to_string())];
        let mcp_sources = vec![(workspace.join(".harness/mcp.json"), "workspace".to_string())];
        let prompt_context = render_prompt_context(&gather_workspace_context(
            &workspace,
            &LoadedConfig::default(),
            options.permission_mode,
            Some(&format!("{}/{}", target.route.as_str(), target.model)),
            Some("use the local skill and then call MCP"),
        ));
        let mut prompts = Vec::new();
        let replies = [
            "<tool_call>{\"tool\":\"skill\",\"arguments\":{\"name\":\"bootstrap\",\"task\":\"set up providers\"}}</tool_call>",
            "<tool_call>{\"tool\":\"mcp_call\",\"arguments\":{\"server\":\"mock-echo\",\"tool\":\"echo\",\"arguments\":{\"text\":\"hello\"}}}</tool_call>",
            "Skill and MCP both worked.",
        ];
        let mut index = 0;

        let reply = run_agent_loop_with_runner(
            &target,
            &workspace,
            &skill_sources,
            &mcp_sources,
            &prompt_context,
            "use the local skill and then call MCP",
            &options,
            None,
            |_, prompt| {
                prompts.push(prompt.to_string());
                let text = replies[index].to_string();
                index += 1;
                Ok(crate::ProviderReply {
                    route: target.route,
                    model: target.model.clone(),
                    text,
                    tool_calls: Vec::new(),
                })
            },
            |_| Ok(ApprovalOutcome::Approve),
        )
        .unwrap();

        assert_eq!(reply.tool_events.len(), 2);
        assert_eq!(reply.tool_events[0].name, "skill");
        assert_eq!(reply.tool_events[1].name, "mcp_call");
        assert!(prompts[1].contains("Do staged setup."));
        assert!(prompts[2].contains("hello"));
        assert!(prompts[2].contains("mock-echo"));
        assert_eq!(reply.provider.text, "Skill and MCP both worked.");

        cleanup(&workspace);
    }

    #[test]
    fn uses_compact_prompt_shape_for_ollama_models() {
        let workspace = temp_workspace("agent-loop-compact-prompt");
        fs::write(workspace.join("README.md"), "hello").unwrap();

        let target = parse_model_target("ollama/qwen2.5-coder:7b").unwrap();
        let options = AgentOptions {
            max_steps: 2,
            permission_mode: PermissionMode::WorkspaceWrite,
            verification_policy: VerificationPolicy::Annotate,
        };
        let skill_sources = vec![(workspace.join(".harness/skills"), "workspace".to_string())];
        let mcp_sources = vec![(workspace.join(".harness/mcp.json"), "workspace".to_string())];
        let prompt_context = render_prompt_context(&gather_workspace_context(
            &workspace,
            &LoadedConfig::default(),
            PermissionMode::WorkspaceWrite,
            Some("ollama/qwen2.5-coder:7b"),
            None,
        ));
        let mut prompts = Vec::new();

        let reply = run_agent_loop_with_runner(
            &target,
            &workspace,
            &skill_sources,
            &mcp_sources,
            &prompt_context,
            "read the readme and summarize it",
            &options,
            None,
            |_, prompt| {
                prompts.push(prompt.to_string());
                Ok(crate::ProviderReply {
                    route: target.route,
                    model: target.model.clone(),
                    text: "Short answer.".to_string(),
                    tool_calls: Vec::new(),
                })
            },
            |_| Ok(ApprovalOutcome::Approve),
        )
        .unwrap();

        assert_eq!(reply.provider.text, "Short answer.");
        assert!(prompts[0].contains("Compact rules for this model:"));
        assert!(prompts[0].contains("Keep the final answer within about 8 lines"));
        assert!(prompts[0].contains("Keep each step atomic."));
        assert!(!prompts[0].contains("Do:\n"));

        cleanup(&workspace);
    }

    #[test]
    fn coerces_nested_string_like_tool_arguments() {
        assert_eq!(
            coerce_string_value(&json!({"value":"README.md"})).as_deref(),
            Some("README.md")
        );
        assert_eq!(
            coerce_string_value(&json!(["src/main.rs"])).as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            coerce_string_value(&json!({"path":{"value":"Cargo.toml"}})).as_deref(),
            Some("Cargo.toml")
        );
    }

    #[test]
    fn runs_loop_with_native_tool_calls() {
        let workspace = temp_workspace("agent-loop-native-tool");
        fs::write(workspace.join("README.md"), "hello from native tool").unwrap();
        let target = parse_model_target("openai/gpt-4.1-mini").unwrap();
        let options = AgentOptions {
            max_steps: 3,
            permission_mode: PermissionMode::WorkspaceWrite,
            verification_policy: VerificationPolicy::Annotate,
        };
        let prompt_context = render_prompt_context(&gather_workspace_context(
            &workspace,
            &LoadedConfig::default(),
            options.permission_mode,
            Some(&format!("{}/{}", target.route.as_str(), target.model)),
            Some("read the readme using native tools"),
        ));
        let mut call_count = 0;

        let reply = run_agent_loop_with_runner(
            &target,
            &workspace,
            &[],
            &[],
            &prompt_context,
            "read the readme using native tools",
            &options,
            None,
            |_, _| {
                call_count += 1;
                if call_count == 1 {
                    Ok(crate::ProviderReply {
                        route: target.route,
                        model: target.model.clone(),
                        text: String::new(),
                        tool_calls: vec![crate::ProviderToolCall {
                            id: Some("call_1".to_string()),
                            name: "read".to_string(),
                            arguments: json!({ "path": "README.md" }),
                        }],
                    })
                } else {
                    Ok(crate::ProviderReply {
                        route: target.route,
                        model: target.model.clone(),
                        text: "Native read says hello from native tool".to_string(),
                        tool_calls: Vec::new(),
                    })
                }
            },
            |_| Ok(ApprovalOutcome::Approve),
        )
        .unwrap();

        assert_eq!(reply.tool_events.len(), 1);
        assert_eq!(reply.tool_events[0].name, "read");
        assert_eq!(
            reply.provider.text,
            "Native read says hello from native tool"
        );
        cleanup(&workspace);
    }

    #[test]
    fn stops_after_max_steps_without_final_answer() {
        let workspace = temp_workspace("agent-loop-max-steps");
        let target = parse_model_target("ollama/test-model").unwrap();
        let options = AgentOptions {
            max_steps: 1,
            permission_mode: PermissionMode::WorkspaceWrite,
            verification_policy: VerificationPolicy::Annotate,
        };
        let prompt_context = render_prompt_context(&gather_workspace_context(
            &workspace,
            &LoadedConfig::default(),
            options.permission_mode,
            Some(&format!("{}/{}", target.route.as_str(), target.model)),
            Some("loop forever"),
        ));

        let error = run_agent_loop_with_runner(
            &target,
            &workspace,
            &[],
            &[],
            &prompt_context,
            "loop forever",
            &options,
            None,
            |_, _| {
                Ok(crate::ProviderReply {
                    route: target.route,
                    model: target.model.clone(),
                    text: "<tool_call>{\"tool\":\"glob\",\"arguments\":{\"pattern\":\"*\"}}</tool_call>"
                        .to_string(),
                    tool_calls: Vec::new(),
                })
            },
            |_| Ok(ApprovalOutcome::Approve),
        )
        .unwrap_err();

        assert!(error.contains("exceeded 1 steps"));
        cleanup(&workspace);
    }

    #[test]
    fn stops_when_tool_request_is_rejected() {
        let workspace = temp_workspace("agent-loop-reject");
        let target = parse_model_target("ollama/test-model").unwrap();
        let options = AgentOptions {
            max_steps: 2,
            permission_mode: PermissionMode::WorkspaceWrite,
            verification_policy: VerificationPolicy::Annotate,
        };
        let prompt_context = render_prompt_context(&gather_workspace_context(
            &workspace,
            &LoadedConfig::default(),
            options.permission_mode,
            Some(&format!("{}/{}", target.route.as_str(), target.model)),
            Some("do a read"),
        ));

        let error = run_agent_loop_with_runner(
            &target,
            &workspace,
            &[],
            &[],
            &prompt_context,
            "do a read",
            &options,
            None,
            |_, _| {
                Ok(crate::ProviderReply {
                    route: target.route,
                    model: target.model.clone(),
                    text: "<tool_call>{\"tool\":\"read\",\"arguments\":{\"path\":\"README.md\"}}</tool_call>"
                        .to_string(),
                    tool_calls: Vec::new(),
                })
            },
            |request: &ApprovalRequest| {
                assert_eq!(request.tool, "read");
                Ok(ApprovalOutcome::Reject {
                    reason: "boss rejected".to_string(),
                })
            },
        )
        .unwrap_err();

        assert!(error.contains("boss rejected"));
        cleanup(&workspace);
    }

    #[test]
    fn returns_sanitized_final_answer_when_provider_emits_thinking() {
        let workspace = temp_workspace("agent-loop-thinking");
        let target = parse_model_target("ollama/test-model").unwrap();
        let options = AgentOptions {
            max_steps: 1,
            permission_mode: PermissionMode::WorkspaceWrite,
            verification_policy: VerificationPolicy::Annotate,
        };
        let prompt_context = render_prompt_context(&gather_workspace_context(
            &workspace,
            &LoadedConfig::default(),
            options.permission_mode,
            Some(&format!("{}/{}", target.route.as_str(), target.model)),
            Some("say hello"),
        ));

        let reply = run_agent_loop_with_runner(
            &target,
            &workspace,
            &[],
            &[],
            &prompt_context,
            "say hello",
            &options,
            None,
            |_, _| {
                Ok(crate::ProviderReply {
                    route: target.route,
                    model: target.model.clone(),
                    text: "<thinking>internal</thinking>\nhello boss".to_string(),
                    tool_calls: Vec::new(),
                })
            },
            |_| Ok(ApprovalOutcome::Approve),
        )
        .unwrap();

        assert_eq!(reply.provider.text, "hello boss");
        cleanup(&workspace);
    }

    #[test]
    fn keeps_verified_completion_clean() {
        let workspace = temp_workspace("agent-loop-verified");
        let target = parse_model_target("ollama/test-model").unwrap();
        let options = AgentOptions {
            max_steps: 3,
            permission_mode: PermissionMode::WorkspaceWrite,
            verification_policy: VerificationPolicy::Require,
        };
        let prompt_context = render_prompt_context(&gather_workspace_context(
            &workspace,
            &LoadedConfig::default(),
            options.permission_mode,
            Some(&format!("{}/{}", target.route.as_str(), target.model)),
            Some("make and verify a change"),
        ));
        let replies = [
            "<tool_call>{\"tool\":\"write\",\"arguments\":{\"path\":\"README.md\",\"contents\":\"done\"}}</tool_call>",
            "<tool_call>{\"tool\":\"exec\",\"arguments\":{\"command\":\"cargo test\"}}</tool_call>",
            "Implemented and verified.",
        ];
        let mut index = 0;

        let reply = run_agent_loop_with_runner(
            &target,
            &workspace,
            &[],
            &[],
            &prompt_context,
            "make and verify a change",
            &options,
            None,
            |_, _| {
                let text = replies[index].to_string();
                index += 1;
                Ok(crate::ProviderReply {
                    route: target.route,
                    model: target.model.clone(),
                    text,
                    tool_calls: Vec::new(),
                })
            },
            |_| Ok(ApprovalOutcome::Approve),
        )
        .unwrap();

        assert_eq!(reply.provider.text, "Implemented and verified.");
        cleanup(&workspace);
    }

    #[test]
    fn warns_when_mutation_has_no_verification_step() {
        let workspace = temp_workspace("agent-loop-unverified");
        let target = parse_model_target("ollama/test-model").unwrap();
        let options = AgentOptions {
            max_steps: 2,
            permission_mode: PermissionMode::WorkspaceWrite,
            verification_policy: VerificationPolicy::Annotate,
        };
        let prompt_context = render_prompt_context(&gather_workspace_context(
            &workspace,
            &LoadedConfig::default(),
            options.permission_mode,
            Some(&format!("{}/{}", target.route.as_str(), target.model)),
            Some("make a change"),
        ));
        let replies = [
            "<tool_call>{\"tool\":\"write\",\"arguments\":{\"path\":\"src/main.rs\",\"contents\":\"fn main() {}\"}}</tool_call>",
            "Implemented the change.",
        ];
        let mut index = 0;

        let reply = run_agent_loop_with_runner(
            &target,
            &workspace,
            &[],
            &[],
            &prompt_context,
            "make a change",
            &options,
            None,
            |_, _| {
                let text = replies[index].to_string();
                index += 1;
                Ok(crate::ProviderReply {
                    route: target.route,
                    model: target.model.clone(),
                    text,
                    tool_calls: Vec::new(),
                })
            },
            |_| Ok(ApprovalOutcome::Approve),
        )
        .unwrap();

        assert!(reply.provider.text.starts_with("Not verified:"));
        cleanup(&workspace);
    }

    #[test]
    fn requires_verification_when_policy_is_require() {
        let workspace = temp_workspace("agent-loop-require-verification");
        let target = parse_model_target("ollama/test-model").unwrap();
        let options = AgentOptions {
            max_steps: 2,
            permission_mode: PermissionMode::WorkspaceWrite,
            verification_policy: VerificationPolicy::Require,
        };
        let prompt_context = render_prompt_context(&gather_workspace_context(
            &workspace,
            &LoadedConfig::default(),
            options.permission_mode,
            Some(&format!("{}/{}", target.route.as_str(), target.model)),
            Some("make a change"),
        ));
        let replies = [
            "<tool_call>{\"tool\":\"write\",\"arguments\":{\"path\":\"src/main.rs\",\"contents\":\"fn main() {}\"}}</tool_call>",
            "Implemented the change.",
        ];
        let mut index = 0;

        let error = run_agent_loop_with_runner(
            &target,
            &workspace,
            &[],
            &[],
            &prompt_context,
            "make a change",
            &options,
            None,
            |_, _| {
                let text = replies[index].to_string();
                index += 1;
                Ok(crate::ProviderReply {
                    route: target.route,
                    model: target.model.clone(),
                    text,
                    tool_calls: Vec::new(),
                })
            },
            |_| Ok(ApprovalOutcome::Approve),
        )
        .unwrap_err();

        assert!(error.contains("verification required"));
        cleanup(&workspace);
    }
}
