use std::env;
use std::io::{self, Stdout, Write};
use std::process::Command;
use std::path::{Path, PathBuf};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{
    self, disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{execute, queue};
use runtime::{
    active_trajectory, backend_catalog, blueprint_summary, build_handoff_text,
    build_memory_recall_text, build_model_handoff_snapshot, build_resume_text, call_mcp_tool,
    create_slash_command_template, detect_provider_key, discover_mcp_servers, discover_skills,
    discover_slash_commands, dismiss_memory_candidate, doctor_report, edit_file, exec_command,
    expand_slash_command, export_backend_jsonl, gather_workspace_context, glob_search, grep_search,
    import_backend_jsonl, init_slash_command_dir, latest_model_handoff, list_mcp_tools,
    list_memory_candidates, list_memory_records, list_recent_trajectories, list_skill_candidates,
    load_config, load_provider_registry, maybe_track_skill_candidate_promotion,
    metadata_legacy_kind, metadata_title, migrate_backend_items, parallel_read_only,
    pending_model_handoff, promote_memory_candidate, promote_skill_candidate, provider_preset,
    provider_presets, read_file, record_session_trajectory, remove_provider_profile,
    render_prompt_context, resolve_selected_memory_backend, resolve_skill, resolve_slash_command,
    run_agent_loop, save_config, save_session_memory_bundle, search_memory_records,
    search_trajectories, slash_command_dir, upsert_provider_profile, validate_slash_command_args,
    write_file, ApprovalAction, ApprovalOutcome, ApprovalPolicy, ApprovalRequest, ConnectionMode,
    LoadedConfig, MemoryBackendKind, MemoryRecord, ModelHandoffSnapshot, PermissionMode,
    SavedProviderProfile, SessionStore, SlashCommandKind, SlashCommandScope, ToolOutput,
    TrajectoryRecord, VerificationPolicy,
};
use serde_json::json;

const APP_NAME: &str = "3122";
const MAX_SLASH_SUGGESTIONS: usize = 7;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let workspace = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let _ = runtime::load_workspace_env(&workspace);
    let config = match load_config(&workspace) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("failed to load config: {err}");
            std::process::exit(2);
        }
    };
    let update_notice = detect_update_notice(&workspace);

    match args.first().map(String::as_str) {
        None | Some("repl") => run_repl(&workspace, &config, update_notice),
        Some("doctor") => {
            maybe_print_update_notice(update_notice.as_deref());
            print!("{}", doctor_report(&workspace, &config).render());
        }
        Some("config") => {
            maybe_print_update_notice(update_notice.as_deref());
            println!("{}", config.render_summary(&workspace));
        }
        Some("providers") => {
            maybe_print_update_notice(update_notice.as_deref());
            handle_providers_command(&workspace, &args[1..]);
        }
        Some("model") => {
            maybe_print_update_notice(update_notice.as_deref());
            handle_model_command(&workspace, &config, &args[1..]);
        }
        Some("memory") => {
            maybe_print_update_notice(update_notice.as_deref());
            handle_memory_command(&workspace, &config, &args[1..]);
        }
        Some("trajectory") => {
            maybe_print_update_notice(update_notice.as_deref());
            handle_trajectory_command(&workspace, &args[1..]);
        }
        Some("commands") => {
            maybe_print_update_notice(update_notice.as_deref());
            handle_commands_command(&workspace, &args[1..]);
        }
        Some("resume") => match build_resume_text(&workspace) {
            Ok(text) => print!("{text}"),
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        },
        Some("handoff") => {
            maybe_print_update_notice(update_notice.as_deref());
            handle_handoff_command(&workspace, &args[1..])
        }
        Some("why-context") => {
            maybe_print_update_notice(update_notice.as_deref());
            print!("{}", build_context_dump(&workspace, &config, None, None));
        }
        Some("blueprint") => {
            maybe_print_update_notice(update_notice.as_deref());
            println!("{}", blueprint_summary());
        }
        Some("prompt") => {
            if args
                .get(1)
                .map(String::as_str)
                .is_some_and(|value| matches!(value, "--help" | "-h" | "help"))
            {
                print_prompt_help();
                return;
            }
            maybe_print_update_notice(update_notice.as_deref());
            let (override_model, prompt) = parse_prompt_args(&args[1..]);
            if prompt.trim().is_empty() {
                eprintln!("usage: {} prompt [--model <spec>] <text...>", APP_NAME);
                std::process::exit(2);
            }
            let mut approval_policy = config.default_approval_policy();
            run_prompt(
                &workspace,
                &config,
                &prompt,
                override_model.as_deref(),
                config.default_permission_mode(),
                &mut approval_policy,
                None,
            );
        }
        Some("skills") => {
            maybe_print_update_notice(update_notice.as_deref());
            handle_skills_command(&workspace, &config, &args[1..]);
        }
        Some("mcp") => {
            maybe_print_update_notice(update_notice.as_deref());
            handle_mcp_command(&workspace, &config, &args[1..]);
        }
        Some("session") => {
            maybe_print_update_notice(update_notice.as_deref());
            handle_session_command(&workspace, &config, &args[1..]);
        }
        Some("tool") => {
            maybe_print_update_notice(update_notice.as_deref());
            handle_tool_command(
                &workspace,
                &args[1..],
                config.default_permission_mode(),
                None,
            );
        }
        Some("help") | Some("--help") | Some("-h") => {
            maybe_print_update_notice(update_notice.as_deref());
            print_help();
        }
        Some(other) => {
            maybe_print_update_notice(update_notice.as_deref());
            eprintln!("unknown command: {other}");
            print_help();
            std::process::exit(2);
        }
    }
}

fn run_repl(workspace: &PathBuf, config: &LoadedConfig, update_notice: Option<String>) {
    let session = match SessionStore::create_in(&config.session_dir(workspace)) {
        Ok(store) => store,
        Err(err) => {
            eprintln!("failed to create session store: {err}");
            return;
        }
    };
    let mut mode = config.default_permission_mode();
    let mut approval_policy = config.default_approval_policy();
    let mut current_model = config.primary_model().map(ToOwned::to_owned);
    let _ = session.append(
        "session_start",
        json!({
            "session_path": session.path().display().to_string(),
            "session_id": runtime::SessionStore::session_id_from_path(session.path()),
            "workspace_root": workspace.display().to_string(),
            "boundary": "current workspace only",
        }),
    );

    let mut ui = TuiState::new();
    ui.push_system("Type /help for commands".to_string());
    if let Some(notice) = update_notice {
        ui.push_system(notice);
    }

    if enable_raw_mode().is_err() {
        eprintln!("failed to enable raw mode");
        autosave_session_memory(workspace, &session);
        return;
    }

    let mut stdout = io::stdout();
    if execute!(stdout, EnterAlternateScreen, Hide).is_err() {
        let _ = disable_raw_mode();
        eprintln!("failed to enter alternate screen");
        autosave_session_memory(workspace, &session);
        return;
    }

    loop {
        if redraw_tui(
            &mut stdout,
            workspace,
            config,
            &session,
            &current_model,
            mode,
            approval_policy,
            &ui,
        )
        .is_err()
        {
            break;
        }

        let Ok(event) = event::read() else {
            ui.push_error("failed to read terminal input".to_string());
            continue;
        };

        let Event::Key(key) = event else {
            continue;
        };

        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => break,
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                ui.input.clear();
                ui.sync_slash_navigation(0);
                ui.clear_history_navigation();
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                ui.input.pop();
                let total = build_slash_suggestions(workspace, &ui.input).len();
                ui.sync_slash_navigation(total);
                if ui.history_selection.is_some() {
                    ui.clear_history_navigation();
                }
            }
            KeyEvent {
                code: KeyCode::Up, ..
            } => {
                let total = build_slash_suggestions(workspace, &ui.input).len();
                if ui.input.starts_with('/') && total > 0 {
                    ui.move_slash_selection(total, -1);
                } else {
                    ui.move_history_selection(-1);
                    let total = build_slash_suggestions(workspace, &ui.input).len();
                    ui.sync_slash_navigation(total);
                }
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            } => {
                let total = build_slash_suggestions(workspace, &ui.input).len();
                if ui.input.starts_with('/') && total > 0 {
                    ui.move_slash_selection(total, 1);
                } else {
                    ui.move_history_selection(1);
                    let total = build_slash_suggestions(workspace, &ui.input).len();
                    ui.sync_slash_navigation(total);
                }
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers,
                ..
            } => {
                if modifiers.contains(KeyModifiers::SHIFT) || modifiers.contains(KeyModifiers::ALT)
                {
                    ui.input.push('\n');
                    continue;
                }
                if maybe_accept_selected_slash_suggestion(workspace, &mut ui) {
                    continue;
                }
                let line = ui.input.trim().to_string();
                ui.input.clear();
                ui.sync_slash_navigation(0);
                ui.remember_input(&line);
                if line.is_empty() {
                    continue;
                }
                ui.push_user(line.clone());
                let _ = redraw_tui(
                    &mut stdout,
                    workspace,
                    config,
                    &session,
                    &current_model,
                    mode,
                    approval_policy,
                    &ui,
                );
                match process_repl_input_tui(
                    workspace,
                    config,
                    &session,
                    &line,
                    &mut current_model,
                    &mut mode,
                    &mut approval_policy,
                    &mut ui,
                    0,
                ) {
                    ReplDirective::Continue => {}
                    ReplDirective::Exit => break,
                }
            }
            KeyEvent {
                code: KeyCode::Char('j'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                ui.input.push('\n');
            }
            KeyEvent {
                code: KeyCode::Char(ch),
                modifiers,
                ..
            } if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                ui.input.push(ch);
                let total = build_slash_suggestions(workspace, &ui.input).len();
                ui.sync_slash_navigation(total);
                if ui.history_selection.is_some() {
                    ui.clear_history_navigation();
                }
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                let suggestions = build_slash_suggestions(workspace, &ui.input);
                if !suggestions.is_empty() {
                    ui.sync_slash_navigation(suggestions.len());
                    let selected = suggestions[ui.slash_selection].clone();
                    apply_slash_suggestion(&mut ui, &selected);
                    ui.sync_slash_navigation(suggestions.len());
                }
            }
            _ => {}
        }
    }

    let _ = execute!(stdout, Show, LeaveAlternateScreen);
    let _ = disable_raw_mode();
    autosave_session_memory(workspace, &session);
}

fn maybe_print_update_notice(notice: Option<&str>) {
    if let Some(notice) = notice {
        eprintln!("{notice}");
    }
}

fn detect_update_notice(workspace: &Path) -> Option<String> {
    let branch = git_stdout(workspace, ["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "HEAD" {
        return None;
    }
    let upstream = git_stdout(
        workspace,
        ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
    )?;
    let local_head = git_stdout(workspace, ["rev-parse", "HEAD"])?;
    let upstream_head = git_stdout(workspace, ["rev-parse", upstream.as_str()])?;
    if local_head == upstream_head {
        return None;
    }
    let behind = git_stdout(
        workspace,
        ["rev-list", "--count", format!("HEAD..{upstream}").as_str()],
    )?
    .parse::<usize>()
    .ok()?;
    build_update_notice(&branch, &upstream, behind)
}

fn build_update_notice(branch: &str, upstream: &str, behind: usize) -> Option<String> {
    if behind == 0 {
        return None;
    }
    Some(format!(
        "update available: `{branch}` is behind `{upstream}` by {behind} commit(s); run `git pull`"
    ))
}

fn git_stdout<const N: usize>(workspace: &Path, args: [&str; N]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

struct TuiState {
    transcript: Vec<String>,
    input: String,
    model_choices: Vec<String>,
    slash_selection: usize,
    slash_scroll: usize,
    input_history: Vec<String>,
    history_selection: Option<usize>,
    history_draft: String,
}

impl TuiState {
    fn new() -> Self {
        Self {
            transcript: Vec::new(),
            input: String::new(),
            model_choices: Vec::new(),
            slash_selection: 0,
            slash_scroll: 0,
            input_history: Vec::new(),
            history_selection: None,
            history_draft: String::new(),
        }
    }

    fn push_user(&mut self, line: String) {
        self.transcript.push(format!("> {line}"));
    }

    fn push_system(&mut self, line: String) {
        self.push_block(&line);
    }

    fn push_error(&mut self, line: String) {
        self.push_block(&format!("error: {line}"));
    }

    fn push_block(&mut self, block: &str) {
        for line in block.lines() {
            self.transcript.push(line.to_string());
        }
        if block.trim().is_empty() {
            self.transcript.push(String::new());
        }
    }

    fn push_result(&mut self, result: Result<String, String>) {
        match result {
            Ok(text) => self.push_block(&text),
            Err(err) => self.push_error(err),
        }
    }

    fn sync_slash_navigation(&mut self, total: usize) {
        if !self.input.starts_with('/') || total == 0 {
            self.slash_selection = 0;
            self.slash_scroll = 0;
            return;
        }
        if self.slash_selection >= total {
            self.slash_selection = total.saturating_sub(1);
        }
        if self.slash_selection < self.slash_scroll {
            self.slash_scroll = self.slash_selection;
        }
        let window = MAX_SLASH_SUGGESTIONS.max(1);
        if self.slash_selection >= self.slash_scroll + window {
            self.slash_scroll = self.slash_selection + 1 - window;
        }
    }

    fn move_slash_selection(&mut self, total: usize, direction: isize) {
        if total == 0 {
            self.sync_slash_navigation(0);
            return;
        }
        self.sync_slash_navigation(total);
        let last = total.saturating_sub(1);
        self.slash_selection = if direction < 0 {
            self.slash_selection.saturating_sub(1)
        } else {
            (self.slash_selection + 1).min(last)
        };
        self.sync_slash_navigation(total);
    }

    fn remember_input(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            self.history_selection = None;
            self.history_draft.clear();
            return;
        }
        if self.input_history.last().map(String::as_str) != Some(trimmed) {
            self.input_history.push(trimmed.to_string());
        }
        self.history_selection = None;
        self.history_draft.clear();
    }

    fn move_history_selection(&mut self, direction: isize) {
        if self.input_history.is_empty() {
            return;
        }

        if self.history_selection.is_none() {
            self.history_draft = self.input.clone();
        }

        match direction {
            d if d < 0 => {
                let next = match self.history_selection {
                    Some(index) => index.saturating_sub(1),
                    None => self.input_history.len().saturating_sub(1),
                };
                self.history_selection = Some(next);
                self.input = self.input_history[next].clone();
            }
            _ => match self.history_selection {
                Some(index) if index + 1 < self.input_history.len() => {
                    let next = index + 1;
                    self.history_selection = Some(next);
                    self.input = self.input_history[next].clone();
                }
                Some(_) => {
                    self.history_selection = None;
                    self.input = self.history_draft.clone();
                }
                None => {}
            },
        }
    }

    fn clear_history_navigation(&mut self) {
        self.history_selection = None;
        self.history_draft.clear();
    }
}

#[derive(Debug, Clone)]
struct SlashSuggestion {
    name: String,
    description: String,
}

fn redraw_tui(
    stdout: &mut Stdout,
    workspace: &Path,
    config: &LoadedConfig,
    session: &SessionStore,
    current_model: &Option<String>,
    mode: PermissionMode,
    approval_policy: ApprovalPolicy,
    ui: &TuiState,
) -> Result<(), String> {
    let (width, height) = terminal::size().map_err(|err| err.to_string())?;
    let width = width.max(20);
    let height = height.max(8);
    let suggestions = build_slash_suggestions(workspace, &ui.input);
    let suggestion_lines = render_suggestion_lines(
        &suggestions,
        width as usize,
        ui.slash_selection,
        ui.slash_scroll,
    );
    let footer_rows = 2usize + suggestion_lines.len();
    let workspace_name = workspace
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_else(|| workspace.to_str().unwrap_or("-"));
    let header = vec![
        format!(
            "{APP_NAME} | {} | {} | {}",
            current_model
                .as_deref()
                .or_else(|| config.primary_model())
                .unwrap_or("-"),
            mode,
            approval_policy,
        ),
        format!(
            "{} | conn={} | session={}",
            workspace_name,
            config.interactive_connection_mode(),
            runtime::SessionStore::session_id_from_path(session.path())
                .unwrap_or_else(|| "-".to_string())
        ),
    ];

    let transcript_height = (height as usize)
        .saturating_sub(header.len())
        .saturating_sub(footer_rows);
    let mut wrapped = Vec::new();
    for line in &ui.transcript {
        wrapped.extend(wrap_for_terminal(line, width as usize));
    }
    let start = wrapped.len().saturating_sub(transcript_height);
    let visible = &wrapped[start..];

    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All)).map_err(|err| err.to_string())?;
    for (index, line) in header.iter().enumerate() {
        queue!(
            stdout,
            MoveTo(0, index as u16),
            SetAttribute(Attribute::Bold),
            Print(truncate_for_terminal(line, width as usize)),
            SetAttribute(Attribute::Reset)
        )
        .map_err(|err| err.to_string())?;
    }

    let mut row = header.len() as u16;
    for line in visible {
        queue!(
            stdout,
            MoveTo(0, row),
            Print(truncate_for_terminal(line, width as usize))
        )
        .map_err(|err| err.to_string())?;
        row += 1;
    }

    let suggestion_start = height as usize - footer_rows;
    for (offset, line) in suggestion_lines.iter().enumerate() {
        queue!(
            stdout,
            MoveTo(0, (suggestion_start + offset) as u16),
            Print(truncate_for_terminal(line, width as usize))
        )
        .map_err(|err| err.to_string())?;
    }

    let input_row = height.saturating_sub(1);
    queue!(
        stdout,
        MoveTo(0, input_row.saturating_sub(1)),
        SetAttribute(Attribute::Dim),
        Print("Enter send/select | Up/Down browse | Shift/Alt-Enter or Ctrl-J newline | Tab complete | Ctrl-C exit"),
        SetAttribute(Attribute::Reset),
        MoveTo(0, input_row),
        Print(truncate_for_terminal(&format!("> {}", ui.input), width as usize)),
        MoveTo((2 + ui.input.chars().count()) as u16, input_row)
    )
    .map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())
}

fn build_slash_suggestions(workspace: &Path, input: &str) -> Vec<SlashSuggestion> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    let prefix = input
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    let mut suggestions = core_slash_suggestions();
    for command in discover_slash_commands(workspace) {
        suggestions.push(SlashSuggestion {
            name: command.name,
            description: if command.description.trim().is_empty() {
                command.kind.as_str().to_string()
            } else {
                command.description
            },
        });
    }
    suggestions.sort_by(|left, right| left.name.cmp(&right.name));
    suggestions.dedup_by(|left, right| left.name == right.name);
    if prefix.is_empty() {
        return suggestions;
    }
    let filtered = suggestions
        .into_iter()
        .filter(|item| item.name.starts_with(&prefix))
        .collect::<Vec<_>>();
    filtered
}

fn core_slash_suggestions() -> Vec<SlashSuggestion> {
    [
        ("help", "Show commands"),
        ("status", "Show current session state"),
        ("model", "Show or set the active model"),
        ("init", "Show the current project summary"),
        ("resume", "Show latest session resume summary"),
        ("handoff", "Print the latest handoff block"),
        ("why-context", "Show the current prompt context"),
        (
            "memory",
            "List, inspect sessions, save, delete, export, import, or migrate portable memory",
        ),
        ("trajectory", "Inspect active and recent trajectories"),
        ("commands", "List custom slash commands"),
        ("login", "Show provider setup hints"),
        ("mode", "Show or set permission mode"),
        ("approval", "Show or set approval policy"),
        ("doctor", "Inspect local environment"),
        ("providers", "List saved providers"),
        ("skills", "List skills, suggestions, and promotion"),
        ("mcp", "List discovered MCP servers"),
        ("session", "Show latest session path"),
        ("parallel-read", "Run read/grep/glob in one batch"),
        ("q", "Leave the session"),
        ("exit", "Leave the session"),
    ]
    .into_iter()
    .map(|(name, description)| SlashSuggestion {
        name: name.to_string(),
        description: description.to_string(),
    })
    .collect()
}

fn render_suggestion_lines(
    suggestions: &[SlashSuggestion],
    width: usize,
    selected: usize,
    scroll: usize,
) -> Vec<String> {
    if suggestions.is_empty() {
        return Vec::new();
    }
    let window = MAX_SLASH_SUGGESTIONS.max(1);
    let start = scroll.min(suggestions.len().saturating_sub(1));
    let end = (start + window).min(suggestions.len());
    let mut lines = vec![format!(
        "commands {}/{}",
        selected.saturating_add(1).min(suggestions.len()),
        suggestions.len()
    )];
    for (offset, suggestion) in suggestions[start..end].iter().enumerate() {
        let absolute_index = start + offset;
        let marker = if absolute_index == selected { ">" } else { " " };
        lines.push(truncate_for_terminal(
            &format!(
                "{marker} /{:<14} {}",
                suggestion.name, suggestion.description
            ),
            width,
        ));
    }
    lines
}

fn apply_slash_suggestion(ui: &mut TuiState, suggestion: &SlashSuggestion) {
    let suffix = ui
        .input
        .strip_prefix('/')
        .and_then(|rest| rest.find(char::is_whitespace).map(|index| &rest[index..]))
        .unwrap_or("");
    ui.input = format!("/{}{}", suggestion.name, suffix);
}

fn maybe_accept_selected_slash_suggestion(workspace: &Path, ui: &mut TuiState) -> bool {
    if !ui.input.starts_with('/') {
        return false;
    }
    let suggestions = build_slash_suggestions(workspace, &ui.input);
    if suggestions.is_empty() {
        ui.sync_slash_navigation(0);
        return false;
    }
    ui.sync_slash_navigation(suggestions.len());
    let selected = &suggestions[ui.slash_selection];
    let current = ui
        .input
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("");
    if current != selected.name {
        apply_slash_suggestion(ui, selected);
        ui.sync_slash_navigation(suggestions.len());
        return true;
    }
    false
}

fn truncate_for_terminal(line: &str, width: usize) -> String {
    line.chars().take(width).collect()
}

fn wrap_for_terminal(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in line.chars() {
        current.push(ch);
        if current.chars().count() >= width {
            out.push(current);
            current = String::new();
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

enum ReplDirective {
    Continue,
    Exit,
}

fn process_repl_input_tui(
    workspace: &Path,
    config: &LoadedConfig,
    session: &SessionStore,
    trimmed: &str,
    current_model: &mut Option<String>,
    mode: &mut PermissionMode,
    approval_policy: &mut ApprovalPolicy,
    ui: &mut TuiState,
    custom_depth: usize,
) -> ReplDirective {
    let should_log_input = !(trimmed.parse::<usize>().is_ok() && !ui.model_choices.is_empty());
    if should_log_input {
        let _ = session.append("user_input", json!({ "text": trimmed }));
    }

    match trimmed {
        "" => return ReplDirective::Continue,
        "/exit" | "/quit" | "/q" => return ReplDirective::Exit,
        "/help" => {
            ui.push_block(&render_repl_help_text());
            return ReplDirective::Continue;
        }
        "/status" => {
            ui.push_block(&render_repl_status_text(
                workspace,
                session,
                current_model.as_deref(),
                *mode,
                *approval_policy,
                config.interactive_connection_mode(),
                config.default_verification_policy(),
            ));
            return ReplDirective::Continue;
        }
        "/model" => {
            let (text, choices) =
                render_model_status_text(workspace, config, current_model.as_deref());
            ui.model_choices = choices;
            ui.push_block(&text);
            return ReplDirective::Continue;
        }
        "/init" => {
            ui.push_result(render_init_text(
                workspace,
                config,
                current_model.as_deref(),
            ));
            return ReplDirective::Continue;
        }
        "/resume" => {
            ui.push_result(build_resume_text(workspace));
            return ReplDirective::Continue;
        }
        "/handoff" => {
            ui.push_result(build_handoff_text(workspace));
            return ReplDirective::Continue;
        }
        "/why-context" => {
            ui.push_block(&build_context_dump(
                workspace,
                config,
                current_model.as_deref(),
                Some(*mode),
            ));
            return ReplDirective::Continue;
        }
        "/memory" => {
            ui.push_result(render_memory_list_text(workspace));
            return ReplDirective::Continue;
        }
        "/trajectory" => {
            ui.push_result(render_trajectory_list_text(workspace, 6));
            return ReplDirective::Continue;
        }
        "/commands" => {
            ui.push_block(&render_slash_commands_text(workspace));
            return ReplDirective::Continue;
        }
        "/login" => {
            ui.push_block(&render_login_status_text(workspace));
            return ReplDirective::Continue;
        }
        "/mode" => {
            ui.push_system(mode.to_string());
            return ReplDirective::Continue;
        }
        "/approval" => {
            ui.push_block(&format_approval_status(*approval_policy));
            return ReplDirective::Continue;
        }
        "/doctor" => {
            ui.push_block(&doctor_report(workspace, config).render());
            return ReplDirective::Continue;
        }
        "/config" => {
            ui.push_block(&config.render_summary(workspace));
            return ReplDirective::Continue;
        }
        "/skills" => {
            ui.push_block(&render_skills_text(workspace, config));
            return ReplDirective::Continue;
        }
        "/mcp" => {
            ui.push_block(&render_mcp_text(workspace, config));
            return ReplDirective::Continue;
        }
        "/providers" => {
            ui.push_block(&render_saved_providers_text(workspace));
            return ReplDirective::Continue;
        }
        "/blueprint" => {
            ui.push_block(&blueprint_summary());
            return ReplDirective::Continue;
        }
        "/session" => {
            ui.push_system(session.path().display().to_string());
            return ReplDirective::Continue;
        }
        _ => {}
    }

    if let Ok(index) = trimmed.parse::<usize>() {
        if !ui.model_choices.is_empty() {
            if let Some(model) = ui.model_choices.get(index.saturating_sub(1)).cloned() {
                let previous_model = current_model
                    .as_deref()
                    .or_else(|| config.primary_model())
                    .map(ToOwned::to_owned);
                *current_model = Some(model.clone());
                let _ = session.append(
                    "model_change",
                    json!({ "from": previous_model, "model": model }),
                );
                match build_model_handoff_snapshot(
                    workspace,
                    session.path(),
                    previous_model.as_deref(),
                    &model,
                ) {
                    Ok(snapshot) => {
                        let _ = session.append(
                            "model_handoff",
                            serde_json::to_value(&snapshot).unwrap_or_else(|_| json!({})),
                        );
                        ui.push_block(&render_model_switch_summary_text(&snapshot));
                    }
                    Err(err) => ui.push_error(format!(
                        "model set to {model}; failed to build handoff: {err}"
                    )),
                }
                ui.model_choices.clear();
                return ReplDirective::Continue;
            }
            ui.push_error(format!("unknown model selection: {index}"));
            return ReplDirective::Continue;
        }
    }

    if let Some(next_mode) = trimmed
        .strip_prefix("/mode ")
        .and_then(PermissionMode::parse)
    {
        *mode = next_mode;
        let _ = session.append("mode_change", json!({ "mode": mode.as_str() }));
        ui.push_system(format!("mode set to {mode}"));
        return ReplDirective::Continue;
    }
    if let Some(model) = trimmed.strip_prefix("/model ").map(str::trim) {
        if model.is_empty() {
            ui.push_error("usage: /model <provider/model | profile/alias/model>".to_string());
            return ReplDirective::Continue;
        }
        ui.model_choices.clear();
        let previous_model = current_model
            .as_deref()
            .or_else(|| config.primary_model())
            .map(ToOwned::to_owned);
        *current_model = Some(model.to_string());
        let _ = session.append(
            "model_change",
            json!({ "from": previous_model, "model": model }),
        );
        match build_model_handoff_snapshot(
            workspace,
            session.path(),
            previous_model.as_deref(),
            model,
        ) {
            Ok(snapshot) => {
                let _ = session.append(
                    "model_handoff",
                    serde_json::to_value(&snapshot).unwrap_or_else(|_| json!({})),
                );
                ui.push_block(&render_model_switch_summary_text(&snapshot));
            }
            Err(err) => ui.push_error(format!(
                "model set to {model}; failed to build handoff: {err}"
            )),
        }
        return ReplDirective::Continue;
    }
    if trimmed == "/memory save" {
        ui.model_choices.clear();
        ui.push_result(render_save_latest_session_memory(workspace, session));
        return ReplDirective::Continue;
    }
    if let Some(index) = trimmed.strip_prefix("/memory show ").map(str::trim) {
        match parse_positive_index(index) {
            Ok(index) => ui.push_result(render_memory_show_text(workspace, index)),
            Err(err) => ui.push_error(err),
        }
        return ReplDirective::Continue;
    }
    if let Some(limit) = trimmed.strip_prefix("/memory recall").map(str::trim) {
        match parse_optional_limit(limit, 6) {
            Ok(limit) => ui.push_result(build_memory_recall_text(workspace, limit)),
            Err(err) => ui.push_error(err),
        }
        return ReplDirective::Continue;
    }
    if let Some(query) = trimmed.strip_prefix("/memory search ").map(str::trim) {
        ui.push_result(render_memory_search_text(workspace, query));
        return ReplDirective::Continue;
    }
    if trimmed == "/memory sessions" {
        ui.push_result(render_memory_sessions_text(workspace, config));
        return ReplDirective::Continue;
    }
    if trimmed == "/memory candidates" {
        ui.push_result(render_memory_candidates_text(workspace, 8));
        return ReplDirective::Continue;
    }
    if let Some(index) = trimmed.strip_prefix("/memory promote ").map(str::trim) {
        match parse_positive_index(index) {
            Ok(index) => ui.push_result(run_memory_candidate_promotion_text(workspace, index)),
            Err(err) => ui.push_error(err),
        }
        return ReplDirective::Continue;
    }
    if let Some(index) = trimmed.strip_prefix("/memory dismiss ").map(str::trim) {
        match parse_positive_index(index) {
            Ok(index) => ui.push_result(run_memory_candidate_dismiss_text(workspace, index)),
            Err(err) => ui.push_error(err),
        }
        return ReplDirective::Continue;
    }
    if let Some(session_key) = trimmed.strip_prefix("/memory session ").map(str::trim) {
        ui.push_result(render_memory_session_text(workspace, config, session_key));
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/memory delete").map(str::trim) {
        let args = rest
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        match parse_memory_delete_args(&args) {
            Ok(ids) => ui.push_result(render_memory_delete_text(workspace, config, &ids)),
            Err(err) => ui.push_error(err),
        }
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/memory export").map(str::trim) {
        let args = rest
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        match parse_memory_export_args(&args) {
            Ok((_format, output)) => ui.push_result(render_memory_export_text(
                workspace,
                config,
                output.as_deref(),
            )),
            Err(err) => ui.push_error(err),
        }
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/memory import").map(str::trim) {
        let args = rest
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        match parse_memory_import_args(&args) {
            Ok((_format, input)) => {
                ui.push_result(render_memory_import_text(workspace, config, &input))
            }
            Err(err) => ui.push_error(err),
        }
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/memory migrate").map(str::trim) {
        let args = rest
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        match parse_memory_migrate_args(&args) {
            Ok((from, to)) => {
                ui.push_result(render_memory_migrate_text(workspace, config, from, to))
            }
            Err(err) => ui.push_error(err),
        }
        return ReplDirective::Continue;
    }
    if trimmed == "/trajectory active" {
        ui.push_result(render_active_trajectory_text(workspace));
        return ReplDirective::Continue;
    }
    if let Some(index) = trimmed.strip_prefix("/trajectory show ").map(str::trim) {
        match parse_positive_index(index) {
            Ok(index) => ui.push_result(render_trajectory_show_text(workspace, index)),
            Err(err) => ui.push_error(err),
        }
        return ReplDirective::Continue;
    }
    if let Some(query) = trimmed.strip_prefix("/trajectory search ").map(str::trim) {
        ui.push_result(render_trajectory_search_text(workspace, query));
        return ReplDirective::Continue;
    }
    if trimmed == "/skills suggest" {
        ui.push_result(render_skill_candidates_text(workspace, 8));
        return ReplDirective::Continue;
    }
    if let Some(index) = trimmed.strip_prefix("/skills promote ").map(str::trim) {
        match parse_positive_index(index) {
            Ok(index) => ui.push_result(run_skill_candidate_promotion_text(workspace, index)),
            Err(err) => ui.push_error(err),
        }
        return ReplDirective::Continue;
    }
    if let Some(name) = trimmed.strip_prefix("/commands show ").map(str::trim) {
        ui.push_result(render_slash_command_show_text(workspace, name));
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/commands init").map(str::trim) {
        ui.push_result(run_commands_init_text(workspace, parse_scope_flag(rest)));
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/commands new ").map(str::trim) {
        match parse_new_command_args(rest) {
            Ok((name, kind, scope)) => {
                ui.push_result(run_commands_new_text(workspace, name, kind, scope))
            }
            Err(err) => ui.push_error(err),
        }
        return ReplDirective::Continue;
    }
    if trimmed == "/handoff debug" {
        ui.push_result(render_handoff_debug_text(workspace));
        return ReplDirective::Continue;
    }
    if let Some(next_policy) = trimmed
        .strip_prefix("/approval ")
        .and_then(ApprovalPolicy::parse)
    {
        *approval_policy = next_policy;
        let _ = session.append(
            "approval_change",
            json!({ "policy": approval_policy.as_str() }),
        );
        ui.push_system(format!("approval set to {approval_policy}"));
        return ReplDirective::Continue;
    }
    if let Some(body) = trimmed.strip_prefix("/skill ") {
        let Some((name, task)) = split_skill_command(body) else {
            ui.push_error("usage: /skill <name> [task...]".to_string());
            return ReplDirective::Continue;
        };
        ui.push_result(render_skill_text(workspace, config, name, task, session));
        return ReplDirective::Continue;
    }
    if let Some(body) = trimmed.strip_prefix("/mcp-tools ") {
        let name = body.trim();
        if name.is_empty() {
            ui.push_error("usage: /mcp-tools <server>".to_string());
            return ReplDirective::Continue;
        }
        ui.push_result(render_mcp_tools_text(workspace, config, name));
        return ReplDirective::Continue;
    }
    if let Some(body) = trimmed.strip_prefix("/mcp-call ") {
        match split_mcp_call_command(body) {
            Some((server, tool, arguments)) => {
                ui.push_result(render_mcp_call_text(
                    workspace, config, server, tool, arguments, session,
                ));
            }
            None => ui.push_error("usage: /mcp-call <server> <tool> [json-args]".to_string()),
        }
        return ReplDirective::Continue;
    }

    if trimmed.starts_with('/') && custom_depth < 4 {
        match maybe_run_custom_slash_command_tui(
            workspace,
            config,
            session,
            trimmed,
            current_model,
            mode,
            approval_policy,
            ui,
            custom_depth + 1,
        ) {
            Ok(true) => return ReplDirective::Continue,
            Ok(false) => {}
            Err(err) => {
                ui.push_error(err);
                return ReplDirective::Continue;
            }
        }
    }

    if trimmed.starts_with('/') {
        ui.model_choices.clear();
        ui.push_result(render_repl_tool_output(workspace, trimmed, *mode, session));
    } else {
        ui.model_choices.clear();
        let _ = session.append("prompt_start", json!({ "text": trimmed }));
        let result = run_prompt_capture(
            workspace,
            config,
            trimmed,
            current_model.as_deref(),
            *mode,
            approval_policy,
            session,
            ui,
        );
        ui.push_result(result);
    }
    ReplDirective::Continue
}

#[allow(dead_code)]
fn handle_repl_line(
    workspace: &Path,
    config: &LoadedConfig,
    session: &SessionStore,
    trimmed: &str,
    current_model: &mut Option<String>,
    mode: &mut PermissionMode,
    approval_policy: &mut ApprovalPolicy,
    custom_depth: usize,
) -> ReplDirective {
    match trimmed {
        "" => return ReplDirective::Continue,
        "/exit" | "/quit" | "/q" => return ReplDirective::Exit,
        "/help" => {
            print_repl_help();
            return ReplDirective::Continue;
        }
        "/status" => {
            print_repl_status(
                workspace,
                session,
                current_model.as_deref(),
                *mode,
                *approval_policy,
                config.interactive_connection_mode(),
                config.default_verification_policy(),
            );
            return ReplDirective::Continue;
        }
        "/model" => {
            print_model_status(workspace, config, current_model.as_deref());
            return ReplDirective::Continue;
        }
        "/resume" => {
            match build_resume_text(workspace) {
                Ok(text) => println!("{text}"),
                Err(err) => eprintln!("{err}"),
            }
            return ReplDirective::Continue;
        }
        "/handoff" => {
            match build_handoff_text(workspace) {
                Ok(text) => println!("{text}"),
                Err(err) => eprintln!("{err}"),
            }
            return ReplDirective::Continue;
        }
        "/why-context" => {
            print!(
                "{}",
                build_context_dump(workspace, config, current_model.as_deref(), Some(*mode))
            );
            return ReplDirective::Continue;
        }
        "/memory" => {
            print_memory_list(workspace);
            return ReplDirective::Continue;
        }
        "/trajectory" => {
            print_trajectory_list(workspace, 6);
            return ReplDirective::Continue;
        }
        "/commands" => {
            print_slash_commands(workspace);
            return ReplDirective::Continue;
        }
        "/login" => {
            print_login_status(workspace);
            return ReplDirective::Continue;
        }
        "/mode" => {
            println!("{mode}");
            return ReplDirective::Continue;
        }
        "/approval" => {
            print_approval_status(*approval_policy);
            return ReplDirective::Continue;
        }
        "/doctor" => {
            print!("{}", doctor_report(workspace, config).render());
            return ReplDirective::Continue;
        }
        "/config" => {
            println!("{}", config.render_summary(workspace));
            return ReplDirective::Continue;
        }
        "/skills" => {
            print_skills(workspace, config, false);
            return ReplDirective::Continue;
        }
        "/mcp" => {
            print_mcp(workspace, config);
            return ReplDirective::Continue;
        }
        "/providers" => {
            print_saved_providers(workspace);
            return ReplDirective::Continue;
        }
        "/blueprint" => {
            println!("{}", blueprint_summary());
            return ReplDirective::Continue;
        }
        "/session" => {
            println!("{}", session.path().display());
            return ReplDirective::Continue;
        }
        _ => {}
    }

    if let Some(next_mode) = trimmed
        .strip_prefix("/mode ")
        .and_then(PermissionMode::parse)
    {
        *mode = next_mode;
        let _ = session.append("mode_change", json!({ "mode": mode.as_str() }));
        println!("mode set to {mode}");
        return ReplDirective::Continue;
    }
    if let Some(model) = trimmed.strip_prefix("/model ").map(str::trim) {
        if model.is_empty() {
            eprintln!("usage: /model <provider/model | profile/alias/model>");
            return ReplDirective::Continue;
        }
        let previous_model = current_model
            .as_deref()
            .or_else(|| config.primary_model())
            .map(ToOwned::to_owned);
        *current_model = Some(model.to_string());
        let _ = session.append(
            "model_change",
            json!({ "from": previous_model, "model": model }),
        );
        match build_model_handoff_snapshot(
            workspace,
            session.path(),
            previous_model.as_deref(),
            model,
        ) {
            Ok(snapshot) => {
                let _ = session.append(
                    "model_handoff",
                    serde_json::to_value(&snapshot).unwrap_or_else(|_| json!({})),
                );
                print_model_switch_summary(&snapshot);
            }
            Err(err) => {
                eprintln!("model set to {model}");
                eprintln!("warning: failed to build handoff: {err}");
            }
        }
        return ReplDirective::Continue;
    }
    if trimmed == "/memory save" {
        save_latest_session_memory(workspace, session);
        return ReplDirective::Continue;
    }
    if let Some(index) = trimmed.strip_prefix("/memory show ").map(str::trim) {
        match parse_positive_index(index) {
            Ok(index) => print_memory_show(workspace, index),
            Err(err) => eprintln!("{err}"),
        }
        return ReplDirective::Continue;
    }
    if let Some(limit) = trimmed.strip_prefix("/memory recall").map(str::trim) {
        match parse_optional_limit(limit, 6) {
            Ok(limit) => print_memory_recall(workspace, limit),
            Err(err) => eprintln!("{err}"),
        }
        return ReplDirective::Continue;
    }
    if let Some(query) = trimmed.strip_prefix("/memory search ").map(str::trim) {
        print_memory_search(workspace, query);
        return ReplDirective::Continue;
    }
    if trimmed == "/memory sessions" {
        print_memory_sessions(workspace, config);
        return ReplDirective::Continue;
    }
    if trimmed == "/memory candidates" {
        print_memory_candidates(workspace, 8);
        return ReplDirective::Continue;
    }
    if let Some(index) = trimmed.strip_prefix("/memory promote ").map(str::trim) {
        match parse_positive_index(index) {
            Ok(index) => match run_memory_candidate_promotion_text(workspace, index) {
                Ok(text) => println!("{text}"),
                Err(err) => eprintln!("{err}"),
            },
            Err(err) => eprintln!("{err}"),
        }
        return ReplDirective::Continue;
    }
    if let Some(index) = trimmed.strip_prefix("/memory dismiss ").map(str::trim) {
        match parse_positive_index(index) {
            Ok(index) => match run_memory_candidate_dismiss_text(workspace, index) {
                Ok(text) => println!("{text}"),
                Err(err) => eprintln!("{err}"),
            },
            Err(err) => eprintln!("{err}"),
        }
        return ReplDirective::Continue;
    }
    if let Some(session_key) = trimmed.strip_prefix("/memory session ").map(str::trim) {
        print_memory_session(workspace, config, session_key);
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/memory delete").map(str::trim) {
        let args = rest
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        match parse_memory_delete_args(&args) {
            Ok(ids) => print_memory_delete(workspace, config, &ids),
            Err(err) => eprintln!("{err}"),
        }
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/memory export").map(str::trim) {
        let args = rest
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        match parse_memory_export_args(&args) {
            Ok((_format, output)) => print_memory_export(workspace, config, output.as_deref()),
            Err(err) => eprintln!("{err}"),
        }
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/memory import").map(str::trim) {
        let args = rest
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        match parse_memory_import_args(&args) {
            Ok((_format, input)) => print_memory_import(workspace, config, &input),
            Err(err) => eprintln!("{err}"),
        }
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/memory migrate").map(str::trim) {
        let args = rest
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        match parse_memory_migrate_args(&args) {
            Ok((from, to)) => print_memory_migrate(workspace, config, from, to),
            Err(err) => eprintln!("{err}"),
        }
        return ReplDirective::Continue;
    }
    if trimmed == "/trajectory active" {
        print_active_trajectory(workspace);
        return ReplDirective::Continue;
    }
    if let Some(index) = trimmed.strip_prefix("/trajectory show ").map(str::trim) {
        match parse_positive_index(index) {
            Ok(index) => print_trajectory_show(workspace, index),
            Err(err) => eprintln!("{err}"),
        }
        return ReplDirective::Continue;
    }
    if let Some(query) = trimmed.strip_prefix("/trajectory search ").map(str::trim) {
        print_trajectory_search(workspace, query);
        return ReplDirective::Continue;
    }
    if trimmed == "/skills suggest" {
        print_skill_candidates(workspace, 8);
        return ReplDirective::Continue;
    }
    if let Some(index) = trimmed.strip_prefix("/skills promote ").map(str::trim) {
        match parse_positive_index(index) {
            Ok(index) => run_skill_candidate_promotion(workspace, index),
            Err(err) => eprintln!("{err}"),
        }
        return ReplDirective::Continue;
    }
    if let Some(name) = trimmed.strip_prefix("/commands show ").map(str::trim) {
        print_slash_command_show(workspace, name);
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/commands init").map(str::trim) {
        run_commands_init(workspace, parse_scope_flag(rest));
        return ReplDirective::Continue;
    }
    if let Some(rest) = trimmed.strip_prefix("/commands new ").map(str::trim) {
        match parse_new_command_args(rest) {
            Ok((name, kind, scope)) => run_commands_new(workspace, name, kind, scope),
            Err(err) => eprintln!("{err}"),
        }
        return ReplDirective::Continue;
    }
    if trimmed == "/handoff debug" {
        print_handoff_debug(workspace);
        return ReplDirective::Continue;
    }
    if let Some(next_policy) = trimmed
        .strip_prefix("/approval ")
        .and_then(ApprovalPolicy::parse)
    {
        *approval_policy = next_policy;
        let _ = session.append(
            "approval_change",
            json!({ "policy": approval_policy.as_str() }),
        );
        println!("approval set to {approval_policy}");
        return ReplDirective::Continue;
    }
    if let Some(body) = trimmed.strip_prefix("/skill ") {
        let Some((name, task)) = split_skill_command(body) else {
            eprintln!("usage: /skill <name> [task...]");
            return ReplDirective::Continue;
        };
        run_skill(workspace, config, name, task, Some(session));
        return ReplDirective::Continue;
    }
    if let Some(body) = trimmed.strip_prefix("/mcp-tools ") {
        let name = body.trim();
        if name.is_empty() {
            eprintln!("usage: /mcp-tools <server>");
            return ReplDirective::Continue;
        }
        print_mcp_tools(workspace, config, name);
        return ReplDirective::Continue;
    }
    if let Some(body) = trimmed.strip_prefix("/mcp-call ") {
        match split_mcp_call_command(body) {
            Some((server, tool, arguments)) => {
                run_mcp_call(workspace, config, server, tool, arguments, Some(session));
            }
            None => eprintln!("usage: /mcp-call <server> <tool> [json-args]"),
        }
        return ReplDirective::Continue;
    }

    if trimmed.starts_with('/') && custom_depth < 4 {
        match maybe_run_custom_slash_command(
            workspace,
            config,
            session,
            trimmed,
            current_model,
            mode,
            approval_policy,
            custom_depth + 1,
        ) {
            Ok(true) => return ReplDirective::Continue,
            Ok(false) => {}
            Err(err) => {
                eprintln!("{err}");
                return ReplDirective::Continue;
            }
        }
    }

    if trimmed.starts_with('/') {
        handle_repl_tool_command(workspace, trimmed, *mode, session);
    } else {
        let _ = session.append("prompt_start", json!({ "text": trimmed }));
        run_prompt(
            workspace,
            config,
            trimmed,
            current_model.as_deref(),
            *mode,
            approval_policy,
            Some(session),
        );
    }
    ReplDirective::Continue
}

fn render_repl_help_text() -> String {
    [
        "/help       show commands",
        "/status     show current session state",
        "/model      show or set the active model",
        "/init       show the current project summary",
        "/resume     show latest session resume summary",
        "/handoff    print a handoff block",
        "/handoff debug inspect the latest handoff state",
        "/why-context show the current prompt context",
        "/memory     list/search/candidates/sessions/save/delete/export/import/migrate portable memory",
        "/trajectory inspect active and recent trajectories",
        "/memory show inspect one recent memory record",
        "/memory candidates list pending auto-promotion candidates",
        "/memory promote save one pending candidate into portable memory",
        "/memory dismiss drop one pending candidate",
        "/memory session inspect one portable memory session",
        "/memory recall print rendered portable recall text",
        "/skills suggest list repeated workflow candidates",
        "/skills promote create a prompt-template from a candidate",
        "/commands   list custom slash commands",
        "/commands show inspect one custom slash command",
        "/commands init create a commands directory",
        "/commands new create a command template",
        "/login      show provider setup hints",
        "/mode       show or set permission mode",
        "/approval   show or set approval policy",
        "/doctor     inspect local environment",
        "/config     show resolved config",
        "/providers  list saved providers",
        "/blueprint  print architecture summary",
        "/skills     list discovered skills",
        "/skill      show or run a specific skill",
        "/mcp        list discovered MCP servers",
        "/mcp-tools  list tools from one MCP server",
        "/mcp-call   call a tool on one MCP server",
        "/session    show latest session path",
        "/read       read a file",
        "/write      write a file",
        "/edit       replace first occurrence",
        "/grep       search text",
        "/glob       find paths",
        "/exec       run a shell command",
        "/parallel-read run read/grep/glob in one batch",
        "/q          leave the repl",
        "/exit       leave the repl",
    ]
    .join("\n")
}

fn render_repl_status_text(
    workspace: &Path,
    session: &SessionStore,
    current_model: Option<&str>,
    mode: PermissionMode,
    approval_policy: ApprovalPolicy,
    connection_mode: ConnectionMode,
    verification_policy: VerificationPolicy,
) -> String {
    let provider_count = load_provider_registry(workspace)
        .map(|registry| registry.profiles.len())
        .unwrap_or(0);
    let memory_records = list_memory_records(workspace).unwrap_or_default();
    let mut lines = vec![
        format!("workspace: {}", workspace.display()),
        format!("session: {}", session.path().display()),
        format!("model: {}", current_model.unwrap_or("-")),
    ];
    if let Ok(Some(handoff)) = latest_model_handoff(session.path()) {
        if let Some(previous) = handoff.snapshot.from_model.as_deref() {
            lines.push(format!("previous_model: {previous}"));
        }
        lines.push(format!(
            "next_step: {}",
            handoff.snapshot.suggested_next_step
        ));
    }
    if let Ok(Some(handoff)) = pending_model_handoff(session.path()) {
        lines.push(format!(
            "handoff_boost: pending for {}",
            handoff.snapshot.to_model
        ));
    }
    lines.push(format!("mode: {mode}"));
    lines.push(format!("connection: {connection_mode}"));
    lines.push(format!(
        "connection_behavior: {}",
        connection_mode_hint(connection_mode)
    ));
    lines.push(format!("approval: {approval_policy}"));
    lines.push(format!(
        "approval_behavior: {}",
        approval_policy_hint(approval_policy)
    ));
    lines.push(format!("verification: {verification_policy}"));
    lines.push(format!(
        "verification_behavior: {}",
        verification_policy_hint(verification_policy)
    ));
    lines.push(format!(
        "memory_backend: {}",
        config_memory_backend_label(workspace)
    ));
    lines.push(format!("saved_providers: {provider_count}"));
    lines.push(format!("memory_records: {}", memory_records.len()));
    lines.join("\n")
}

fn render_model_status_text(
    workspace: &Path,
    config: &LoadedConfig,
    current_model: Option<&str>,
) -> (String, Vec<String>) {
    let choices = build_model_choices(workspace, config, current_model);
    let mut lines = vec![
        format!("active: {}", current_model.unwrap_or("-")),
        format!("default: {}", config.primary_model().unwrap_or("-")),
    ];
    if let Ok(Some(session_path)) = SessionStore::latest(workspace) {
        if let Ok(Some(handoff)) = latest_model_handoff(&session_path) {
            if let Some(previous) = handoff.snapshot.from_model.as_deref() {
                lines.push(format!("previous: {previous}"));
            }
            lines.push(format!("current_goal: {}", handoff.snapshot.current_goal));
            lines.push(format!("next: {}", handoff.snapshot.suggested_next_step));
        }
    }
    if !config.data.model.fallback.is_empty() {
        lines.push(format!(
            "fallback: {}",
            config.data.model.fallback.join(", ")
        ));
    }
    if !choices.is_empty() {
        lines.push("switch:".to_string());
        for (index, model) in choices.iter().enumerate() {
            lines.push(format!("{}. {}", index + 1, model));
        }
        lines.push("tip: type the number to switch".to_string());
    }
    let registry = load_provider_registry(workspace).unwrap_or_default();
    if registry.profiles.is_empty() {
        lines.push("saved_profiles: none".to_string());
    } else {
        lines.push("saved_profiles:".to_string());
        for profile in registry.profiles {
            lines.push(format!(
                "- {} | {} | use: profile/{}/<model>",
                profile.alias, profile.base_url, profile.alias
            ));
        }
    }
    (lines.join("\n"), choices)
}

fn render_login_status_text(workspace: &Path) -> String {
    let registry = load_provider_registry(workspace).unwrap_or_default();
    let mut lines = vec!["BYOK:".to_string()];
    if registry.profiles.is_empty() {
        lines.push("- no saved provider profiles".to_string());
    } else {
        lines.push(format!(
            "- saved provider profiles: {}",
            registry.profiles.len()
        ));
        for profile in registry.profiles {
            lines.push(format!("- {} | {}", profile.alias, profile.base_url));
        }
    }
    lines.push("Next steps:".to_string());
    lines.push(format!("- `{APP_NAME} providers presets`"));
    lines.push(format!(
        "- `{APP_NAME} providers add <alias> --api-key <key>`"
    ));
    lines.push("- use `profile/<alias>/<model>` in `/model`".to_string());
    lines.push("External CLIs:".to_string());
    lines.push(format!(
        "- run `{APP_NAME} doctor` to inspect `claude` and `codex`"
    ));
    lines.join("\n")
}

fn build_model_choices(
    workspace: &Path,
    config: &LoadedConfig,
    current_model: Option<&str>,
) -> Vec<String> {
    let mut choices = Vec::new();
    if let Some(model) = current_model {
        push_unique_choice(&mut choices, model.to_string());
    }
    if let Some(model) = config.primary_model() {
        push_unique_choice(&mut choices, model.to_string());
    }
    for model in &config.data.model.fallback {
        push_unique_choice(&mut choices, model.clone());
    }

    let registry = load_provider_registry(workspace).unwrap_or_default();
    for profile in registry.profiles {
        if let Some(model) = default_model_for_profile(&profile.alias, &profile.route) {
            push_unique_choice(&mut choices, format!("profile/{}/{}", profile.alias, model));
        }
    }
    choices
}

fn push_unique_choice(choices: &mut Vec<String>, model: String) {
    if !choices.iter().any(|existing| existing == &model) {
        choices.push(model);
    }
}

fn default_model_for_profile(alias: &str, route: &str) -> Option<String> {
    let env_key = match alias {
        "groq" => Some("HARNESS_TEST_GROQ_MODEL"),
        "qwen-api" => Some("HARNESS_TEST_QWEN_API_MODEL"),
        "zai" => Some("HARNESS_TEST_ZAI_MODEL"),
        "minimax" => Some("HARNESS_TEST_MINIMAX_MODEL"),
        "deepinfra" => Some("HARNESS_TEST_DEEPINFRA_MODEL"),
        "openai-api" => Some("OPENAI_DEFAULT_MODEL"),
        "anthropic-api" => Some("ANTHROPIC_DEFAULT_MODEL"),
        _ => None,
    };
    if let Some(key) = env_key {
        if let Ok(value) = env::var(key) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    match (alias, route) {
        ("anthropic-api", "anthropic") => Some("claude-sonnet-4-6".to_string()),
        ("openai-api", "openai-compat") => Some("gpt-4.1-mini".to_string()),
        ("groq", "openai-compat") => Some("openai/gpt-oss-20b".to_string()),
        ("qwen-api", "openai-compat") => Some("qwen/qwen3.6-plus".to_string()),
        ("zai", "openai-compat") => Some("glm-5".to_string()),
        ("minimax", "openai-compat") => Some("MiniMax-M2.7".to_string()),
        ("deepinfra", "openai-compat") => Some("nvidia/Nemotron-3-Nano-30B-A3B".to_string()),
        _ => None,
    }
}

fn render_slash_commands_text(workspace: &Path) -> String {
    let commands = discover_slash_commands(workspace);
    if commands.is_empty() {
        return "no custom slash commands".to_string();
    }
    let mut lines = Vec::new();
    if let Ok(global_dir) = slash_command_dir(workspace, SlashCommandScope::Global) {
        lines.push(format!("global_dir: {}", global_dir.display()));
    }
    if let Ok(workspace_dir) = slash_command_dir(workspace, SlashCommandScope::Workspace) {
        lines.push(format!("workspace_dir: {}", workspace_dir.display()));
    }
    lines.push(format!("custom_commands: {}", commands.len()));
    for command in commands {
        let summary = if command.description.trim().is_empty() {
            "-".to_string()
        } else {
            compact_line(command.description.trim(), 96)
        };
        lines.push(format!(
            "/{} | {} | {} | {}",
            command.name,
            command.kind.as_str(),
            command.source,
            summary
        ));
    }
    lines.join("\n")
}

fn run_commands_init_text(workspace: &Path, scope: SlashCommandScope) -> Result<String, String> {
    init_slash_command_dir(workspace, scope).map(|path| {
        format!(
            "initialized {} commands dir\npath: {}",
            scope.as_str(),
            path.display()
        )
    })
}

fn run_commands_new_text(
    workspace: &Path,
    name: &str,
    kind: SlashCommandKind,
    scope: SlashCommandScope,
) -> Result<String, String> {
    let kind_label = kind.as_str().to_string();
    create_slash_command_template(workspace, scope, name, kind).map(|path| {
        format!(
            "created /{} ({})\nscope: {}\npath: {}",
            name.trim().trim_start_matches('/'),
            kind_label,
            scope.as_str(),
            path.display()
        )
    })
}

fn render_slash_command_show_text(workspace: &Path, name: &str) -> Result<String, String> {
    let commands = discover_slash_commands(workspace);
    let command = resolve_slash_command(&commands, name)
        .ok_or_else(|| format!("unknown custom command: {name}"))?;
    let usage = command
        .usage
        .clone()
        .unwrap_or_else(|| format!("/{}", command.name));
    let mut lines = vec![
        format!("name: /{}", command.name),
        format!("kind: {}", command.kind.as_str()),
        format!("source: {}", command.source),
        format!("path: {}", command.path.display()),
        format!(
            "description: {}",
            if command.description.trim().is_empty() {
                "-"
            } else {
                command.description.trim()
            }
        ),
        format!("usage: {usage}"),
        format!(
            "args: min={} max={}",
            command
                .min_args
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            command
                .max_args
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        ),
    ];
    match command.kind {
        SlashCommandKind::Alias => {
            lines.push(format!(
                "target: {}",
                command.target.as_deref().unwrap_or("-")
            ));
        }
        SlashCommandKind::Macro => {
            lines.push("steps:".to_string());
            for step in &command.steps {
                lines.push(format!("- {step}"));
            }
        }
        SlashCommandKind::PromptTemplate => {
            lines.push(format!(
                "prompt: {}",
                command.prompt.as_deref().unwrap_or("-")
            ));
        }
    }
    Ok(lines.join("\n"))
}

fn render_saved_providers_text(workspace: &Path) -> String {
    let registry = load_provider_registry(workspace).unwrap_or_default();
    if registry.profiles.is_empty() {
        return "no saved provider profiles".to_string();
    }
    registry
        .profiles
        .into_iter()
        .map(|profile| {
            format!(
                "{} | {} | {} | use: profile/{}/<model>",
                profile.alias, profile.route, profile.base_url, profile.alias
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_init_text(
    workspace: &Path,
    config: &LoadedConfig,
    current_model: Option<&str>,
) -> Result<String, String> {
    let mut entries = std::fs::read_dir(workspace)
        .map_err(|err| err.to_string())?
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries.truncate(12);

    let mut lines = vec![
        format!("project: {}", workspace.display()),
        format!(
            "active_model: {}",
            current_model.unwrap_or_else(|| config.primary_model().unwrap_or("-"))
        ),
        format!(
            "config: {}",
            config
                .source
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string())
        ),
    ];
    if !entries.is_empty() {
        lines.push("top_level:".to_string());
        for entry in entries {
            lines.push(format!("- {entry}"));
        }
    }
    lines.push("tips: /model, /trajectory, /memory, /commands".to_string());
    Ok(lines.join("\n"))
}

fn config_memory_backend_label(workspace: &Path) -> String {
    load_config(workspace)
        .map(|config| config.memory_backend().to_string())
        .unwrap_or_else(|_| "local-amcp".to_string())
}

fn render_memory_list_text(workspace: &Path) -> Result<String, String> {
    let records = list_memory_records(workspace)?;
    if records.is_empty() {
        return Ok(format!(
            "memory_backend: {}\nno memory records",
            config_memory_backend_label(workspace)
        ));
    }
    let mut lines = vec![
        format!("memory_backend: {}", config_memory_backend_label(workspace)),
        format!("memory_records: {}", records.len()),
    ];
    let pending_candidates = list_memory_candidates(workspace, 32)
        .map(|items| items.len())
        .unwrap_or(0);
    lines.push(format!("pending_candidates: {pending_candidates}"));
    let (summaries, decisions, tasks, errors, notes) = memory_kind_counts(&records);
    lines.push(format!(
        "kinds: summary={} decision={} task={} error={} note={}",
        summaries, decisions, tasks, errors, notes
    ));
    lines.push("recent:".to_string());
    for (index, record) in records.into_iter().take(5).enumerate() {
        lines.push(format!(
            "- #{} | {} | {} | {}",
            index + 1,
            record.kind,
            record.title,
            record.ts_ms
        ));
    }
    lines.push("hint: use `memory show <index>` or `memory recall [limit]`".to_string());
    Ok(lines.join("\n"))
}

fn render_trajectory_list_text(workspace: &Path, limit: usize) -> Result<String, String> {
    let trajectories = list_recent_trajectories(workspace, limit)?;
    if trajectories.is_empty() {
        return Ok("no trajectories recorded".to_string());
    }
    let mut lines = vec![format!("trajectories: {}", trajectories.len())];
    for (index, trajectory) in trajectories.into_iter().enumerate() {
        lines.push(format!(
            "- #{} | {} | goal={} | next={}",
            index + 1,
            trajectory.title,
            compact_line(&trajectory.current_goal, 56),
            compact_line(&trajectory.next_step, 56)
        ));
    }
    lines.push(
        "hint: use `trajectory active`, `trajectory show <index>`, or `trajectory search <query>`"
            .to_string(),
    );
    Ok(lines.join("\n"))
}

fn render_active_trajectory_text(workspace: &Path) -> Result<String, String> {
    match active_trajectory(workspace)? {
        Some(trajectory) => Ok(render_trajectory_detail(&trajectory)),
        None => Ok("no active trajectory".to_string()),
    }
}

fn render_trajectory_show_text(workspace: &Path, index: usize) -> Result<String, String> {
    let trajectories = list_recent_trajectories(workspace, index)?;
    let Some(trajectory) = trajectories.get(index.saturating_sub(1)) else {
        return Err(format!("trajectory index out of range: {index}"));
    };
    Ok(render_trajectory_detail(trajectory))
}

fn render_trajectory_search_text(workspace: &Path, query: &str) -> Result<String, String> {
    let results = search_trajectories(workspace, query, 6)?;
    if results.is_empty() {
        return Ok("no trajectory matches".to_string());
    }
    let mut lines = vec![format!("trajectory_matches: {}", results.len())];
    for (index, trajectory) in results.into_iter().enumerate() {
        lines.push(format!(
            "- #{} | {} | goal={} | next={}",
            index + 1,
            trajectory.title,
            compact_line(&trajectory.current_goal, 52),
            compact_line(&trajectory.next_step, 52)
        ));
    }
    Ok(lines.join("\n"))
}

fn render_trajectory_detail(trajectory: &TrajectoryRecord) -> String {
    let mut lines = vec![
        format!("title: {}", trajectory.title),
        format!("session: {}", trajectory.session_path),
        format!("goal: {}", trajectory.current_goal),
        format!("next: {}", trajectory.next_step),
    ];
    if let Some(model) = trajectory.active_model.as_deref() {
        lines.push(format!("active_model: {model}"));
    }
    if let Some(previous) = trajectory.previous_model.as_deref() {
        lines.push(format!("previous_model: {previous}"));
    }
    if !trajectory.active_files.is_empty() {
        lines.push(format!(
            "active_files: {}",
            trajectory.active_files.join(", ")
        ));
    }
    if let Some(attempt) = trajectory.latest_attempt.as_deref() {
        lines.push(format!("latest_attempt: {attempt}"));
    }
    if let Some(failure) = trajectory.latest_failure.as_deref() {
        lines.push(format!("latest_failure: {failure}"));
    }
    if let Some(verification) = trajectory.last_verification.as_deref() {
        lines.push(format!("last_verification: {verification}"));
    }
    if !trajectory.open_tasks.is_empty() {
        lines.push("open_tasks:".to_string());
        for item in &trajectory.open_tasks {
            lines.push(format!("- {item}"));
        }
    }
    if !trajectory.recent_errors.is_empty() {
        lines.push("recent_errors:".to_string());
        for item in &trajectory.recent_errors {
            lines.push(format!("- {item}"));
        }
    }
    if !trajectory.verification_hints.is_empty() {
        lines.push("verification_hints:".to_string());
        for item in &trajectory.verification_hints {
            lines.push(format!("- {item}"));
        }
    }
    lines.push("recent_summary:".to_string());
    lines.push(trajectory.recent_work_summary.clone());
    lines.join("\n")
}

fn render_skill_candidates_text(workspace: &Path, limit: usize) -> Result<String, String> {
    let candidates = list_skill_candidates(workspace, limit)?;
    if candidates.is_empty() {
        return Ok("no promoted skill candidates yet".to_string());
    }
    let mut lines = vec![format!("skill_candidates: {}", candidates.len())];
    for (index, candidate) in candidates.into_iter().enumerate() {
        lines.push(format!(
            "- #{} | /{} | uses={} | {}",
            index + 1,
            candidate.command_name,
            candidate.occurrence_count,
            compact_line(&candidate.description, 72)
        ));
    }
    lines
        .push("hint: use `skills promote <index>` to create a prompt-template command".to_string());
    Ok(lines.join("\n"))
}

fn run_skill_candidate_promotion_text(workspace: &Path, index: usize) -> Result<String, String> {
    let candidates = list_skill_candidates(workspace, index)?;
    let Some(candidate) = candidates.get(index.saturating_sub(1)) else {
        return Err(format!("skill candidate index out of range: {index}"));
    };
    let path = promote_skill_candidate(workspace, candidate.id)?;
    let memory_result = maybe_track_skill_candidate_promotion(workspace, candidate.id)?;
    let mut lines = vec![format!(
        "promoted skill candidate\nname: /{}\npath: {}",
        candidate.command_name,
        path.display()
    )];
    if let Some(record) = memory_result {
        lines.push(format!(
            "portable_memory: saved {} | {}",
            record.kind, record.title
        ));
    }
    Ok(lines.join("\n"))
}

fn render_memory_candidates_text(workspace: &Path, limit: usize) -> Result<String, String> {
    let candidates = list_memory_candidates(workspace, limit)?;
    if candidates.is_empty() {
        return Ok("no pending promotion candidates".to_string());
    }
    let mut lines = vec![format!("memory_candidates: {}", candidates.len())];
    for (index, candidate) in candidates.into_iter().enumerate() {
        lines.push(format!(
            "- #{} | {} | {} | {}",
            index + 1,
            candidate.trigger,
            metadata_legacy_kind(&candidate.item),
            compact_line(&candidate.summary, 72)
        ));
    }
    lines.push("hint: use `memory promote <index>` or `memory dismiss <index>`".to_string());
    Ok(lines.join("\n"))
}

fn run_memory_candidate_promotion_text(workspace: &Path, index: usize) -> Result<String, String> {
    let candidates = list_memory_candidates(workspace, index)?;
    let Some(candidate) = candidates.get(index.saturating_sub(1)) else {
        return Err(format!("memory candidate index out of range: {index}"));
    };
    match promote_memory_candidate(workspace, candidate.id)? {
        Some(record) => Ok(format!(
            "promoted memory candidate\ntrigger: {}\nrecord: {} | {}",
            candidate.trigger, record.kind, record.title
        )),
        None => Ok(format!(
            "promoted memory candidate\ntrigger: {}\nrecord: already existed",
            candidate.trigger
        )),
    }
}

fn run_memory_candidate_dismiss_text(workspace: &Path, index: usize) -> Result<String, String> {
    let candidates = list_memory_candidates(workspace, index)?;
    let Some(candidate) = candidates.get(index.saturating_sub(1)) else {
        return Err(format!("memory candidate index out of range: {index}"));
    };
    let dismissed = dismiss_memory_candidate(workspace, candidate.id)?;
    if dismissed {
        Ok(format!(
            "dismissed memory candidate\ntrigger: {}",
            candidate.trigger
        ))
    } else {
        Ok("memory candidate already dismissed".to_string())
    }
}

fn render_memory_show_text(workspace: &Path, index: usize) -> Result<String, String> {
    let records = list_memory_records(workspace)?;
    if records.is_empty() {
        return Ok("no memory records".to_string());
    }
    let Some(record) = records.get(index - 1) else {
        return Err(format!("memory index out of range: {index}"));
    };
    let mut lines = vec![
        format!("index: {index}"),
        format!("id: {}", record.id.as_deref().unwrap_or("-")),
        format!("kind: {}", record.kind),
        format!("title: {}", record.title),
        format!("ts_ms: {}", record.ts_ms),
        format!("session: {}", record.session_path.as_deref().unwrap_or("-")),
        format!(
            "tags: {}",
            if record.tags.is_empty() {
                "-".to_string()
            } else {
                record.tags.join(", ")
            }
        ),
        "body:".to_string(),
        record.body.trim().to_string(),
    ];
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    Ok(lines.join("\n"))
}

fn render_memory_search_text(workspace: &Path, query: &str) -> Result<String, String> {
    let records = search_memory_records(workspace, query)?;
    if records.is_empty() {
        return Ok("no memory matches".to_string());
    }
    let mut lines = vec![format!("matches: {}", records.len())];
    for record in records {
        lines.push(format!("{} | {}", record.kind, record.title));
        lines.push(record.body.trim().to_string());
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

fn render_memory_sessions_text(workspace: &Path, config: &LoadedConfig) -> Result<String, String> {
    let backend = resolve_selected_memory_backend(workspace, config)?;
    let sessions = backend.sessions()?;
    if sessions.is_empty() {
        return Ok("no portable memory sessions".to_string());
    }
    let mut lines = vec![
        format!("memory_backend: {}", config.memory_backend()),
        format!("sessions: {}", sessions.len()),
    ];
    for (index, session) in sessions.into_iter().enumerate() {
        lines.push(format!(
            "- #{} | {} | items={} | updated_at={}",
            index + 1,
            compact_line(&session.key, 72),
            session.item_count,
            session.updated_at
        ));
    }
    lines.push(
        "hint: use `memory session <key>` to inspect one portable memory session".to_string(),
    );
    Ok(lines.join("\n"))
}

fn render_memory_session_text(
    workspace: &Path,
    config: &LoadedConfig,
    session_key: &str,
) -> Result<String, String> {
    let backend = resolve_selected_memory_backend(workspace, config)?;
    let items = backend.session(session_key)?;
    if items.is_empty() {
        return Ok("no portable memory items for that session".to_string());
    }
    let mut lines = vec![
        format!("memory_backend: {}", config.memory_backend()),
        format!("session: {session_key}"),
        format!("items: {}", items.len()),
    ];
    for item in items {
        lines.push(format!(
            "- {} | {} | {} | {}",
            item.id,
            metadata_legacy_kind(&item),
            metadata_title(&item),
            item.updated_at
        ));
    }
    Ok(lines.join("\n"))
}

fn render_memory_delete_text(
    workspace: &Path,
    config: &LoadedConfig,
    ids: &[String],
) -> Result<String, String> {
    let backend = resolve_selected_memory_backend(workspace, config)?;
    let deleted = backend.delete(ids)?;
    Ok(format!(
        "deleted portable memory\nbackend: {}\nrequested: {}\ndeleted: {}",
        config.memory_backend(),
        ids.len(),
        deleted
    ))
}

fn render_memory_export_text(
    workspace: &Path,
    config: &LoadedConfig,
    output: Option<&Path>,
) -> Result<String, String> {
    let rendered = export_backend_jsonl(
        workspace,
        config,
        parse_memory_backend_kind(config.memory_backend())?,
    )?;
    if let Some(path) = output {
        std::fs::write(path, &rendered).map_err(|err| err.to_string())?;
        return Ok(format!(
            "exported portable memory\nbackend: {}\nformat: amcp-jsonl\npath: {}",
            config.memory_backend(),
            path.display()
        ));
    }
    Ok(rendered)
}

fn render_memory_import_text(
    workspace: &Path,
    config: &LoadedConfig,
    input: &Path,
) -> Result<String, String> {
    let contents = std::fs::read_to_string(input).map_err(|err| err.to_string())?;
    let imported = import_backend_jsonl(
        workspace,
        config,
        parse_memory_backend_kind(config.memory_backend())?,
        &contents,
    )?;
    Ok(format!(
        "imported portable memory\nbackend: {}\nformat: amcp-jsonl\ncount: {}\npath: {}",
        config.memory_backend(),
        imported,
        input.display()
    ))
}

fn render_memory_migrate_text(
    workspace: &Path,
    config: &LoadedConfig,
    from: MemoryBackendKind,
    to: MemoryBackendKind,
) -> Result<String, String> {
    let migrated = migrate_backend_items(workspace, config, from, to)?;
    Ok(format!(
        "migrated portable memory\nfrom: {}\nto: {}\ncount: {}",
        from, to, migrated
    ))
}

fn render_save_latest_session_memory(
    workspace: &Path,
    session: &SessionStore,
) -> Result<String, String> {
    let bundle = save_session_memory_bundle(workspace, session.path())?;
    if bundle.saved_records.is_empty() {
        return Ok(format!(
            "memory unchanged\npending_candidates: {}",
            bundle.pending_candidates
        ));
    }
    let mut lines = vec![format!(
        "saved {} memory record(s)",
        bundle.saved_records.len()
    )];
    for record in bundle.saved_records {
        lines.push(format!("{} | {}", record.kind, record.title));
    }
    lines.push(format!("pending_candidates: {}", bundle.pending_candidates));
    Ok(lines.join("\n"))
}

fn render_model_switch_summary_text(snapshot: &ModelHandoffSnapshot) -> String {
    let mut lines = vec![
        format!("active: {}", snapshot.to_model),
        format!(
            "previous: {}",
            snapshot.from_model.as_deref().unwrap_or("-")
        ),
        format!("handoff: {}", compact_line(&snapshot.current_goal, 96)),
        format!("next: {}", compact_line(&snapshot.suggested_next_step, 96)),
    ];
    if !snapshot.open_tasks.is_empty() {
        lines.push(format!(
            "warning: {} open task(s) carried forward",
            snapshot.open_tasks.len()
        ));
    }
    lines.join("\n")
}

fn render_handoff_debug_text(workspace: &Path) -> Result<String, String> {
    let latest_session = SessionStore::latest(workspace)?;
    let Some(session_path) = latest_session else {
        return Ok("no sessions found".to_string());
    };

    let mut lines = vec![format!("session: {}", session_path.display())];
    match latest_model_handoff(&session_path)? {
        Some(handoff) => {
            lines.push(format!("latest_handoff_ts_ms: {}", handoff.ts_ms));
            lines.push(format!(
                "from_model: {}",
                handoff.snapshot.from_model.as_deref().unwrap_or("-")
            ));
            lines.push(format!("to_model: {}", handoff.snapshot.to_model));
            lines.push(format!("current_goal: {}", handoff.snapshot.current_goal));
            lines.push(format!(
                "recent_work_summary: {}",
                handoff.snapshot.recent_work_summary
            ));
            if handoff.snapshot.open_tasks.is_empty() {
                lines.push("open_tasks: -".to_string());
            } else {
                lines.push("open_tasks:".to_string());
                for task in &handoff.snapshot.open_tasks {
                    lines.push(format!("- {task}"));
                }
            }
            if handoff.snapshot.recent_errors.is_empty() {
                lines.push("recent_errors: -".to_string());
            } else {
                lines.push("recent_errors:".to_string());
                for error in &handoff.snapshot.recent_errors {
                    lines.push(format!("- {error}"));
                }
            }
            lines.push(format!(
                "suggested_next_step: {}",
                handoff.snapshot.suggested_next_step
            ));
        }
        None => lines.push("latest_handoff: -".to_string()),
    }
    match pending_model_handoff(&session_path)? {
        Some(handoff) => {
            lines.push("pending_handoff: yes".to_string());
            lines.push(format!("pending_to_model: {}", handoff.snapshot.to_model));
        }
        None => lines.push("pending_handoff: no".to_string()),
    }
    Ok(lines.join("\n"))
}

fn render_repl_tool_output(
    workspace: &Path,
    line: &str,
    mode: PermissionMode,
    session: &SessionStore,
) -> Result<String, String> {
    let tokens: Vec<String> = line.split_whitespace().map(ToOwned::to_owned).collect();
    if tokens.is_empty() {
        return Ok(String::new());
    }
    let command = tokens[0].trim_start_matches('/');
    let args = &tokens[1..];
    match run_tool_command(workspace, command, args, line, mode) {
        Ok(output) => {
            let _ = session.append(
                "tool_result",
                json!({ "command": command, "summary": output.summary, "content": output.content }),
            );
            Ok(format_tool_output(&output))
        }
        Err(err) => {
            let _ = session.append("tool_error", json!({ "command": command, "error": err }));
            Err(err)
        }
    }
}

fn format_tool_output(output: &ToolOutput) -> String {
    if output.content.is_empty() {
        output.summary.clone()
    } else {
        format!("{}\n{}", output.summary, output.content)
    }
}

fn format_approval_status(policy: ApprovalPolicy) -> String {
    format!(
        "approval: {policy}\nbehavior: {}",
        approval_policy_hint(policy)
    )
}

fn render_skills_text(workspace: &Path, config: &LoadedConfig) -> String {
    let skills = discover_skills(&config.skill_sources(workspace));
    if skills.is_empty() {
        return "no skills found".to_string();
    }
    render_skill_list(&skills, false)
}

fn render_skill_text(
    workspace: &Path,
    config: &LoadedConfig,
    name: &str,
    task: Option<&str>,
    session: &SessionStore,
) -> Result<String, String> {
    let skills = discover_skills(&config.skill_sources(workspace));
    let skill = resolve_skill(&skills, name)?;
    let packet = runtime::build_skill_packet(skill, task);
    let _ = session.append(
        "skill_invocation",
        json!({
            "name": packet.skill.name,
            "source": packet.skill.source,
            "path": packet.skill.path.display().to_string(),
            "task": packet.task,
        }),
    );
    Ok(format!(
        "skill: {}\nsource: {}\npath: {}\nsummary: {}\ntask: {}\n\n{}",
        packet.skill.name,
        packet.skill.source,
        packet.skill.path.display(),
        packet.skill.summary,
        packet.task.as_deref().unwrap_or("-"),
        packet.prompt
    ))
}

fn render_mcp_text(workspace: &Path, config: &LoadedConfig) -> String {
    let servers = discover_mcp_servers(&config.mcp_sources(workspace));
    if servers.is_empty() {
        return "no mcp servers configured\nexpected config file shape: .harness/mcp.json"
            .to_string();
    }
    servers
        .into_iter()
        .map(|server| {
            let location = server
                .command
                .as_ref()
                .map(ToOwned::to_owned)
                .or(server.url.as_ref().map(ToOwned::to_owned))
                .unwrap_or_else(|| "-".to_string());
            format!(
                "{} | {} | enabled={} | {} | {}",
                server.name, server.transport, server.enabled, location, server.source
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_mcp_tools_text(
    workspace: &Path,
    config: &LoadedConfig,
    server_name: &str,
) -> Result<String, String> {
    let servers = discover_mcp_servers(&config.mcp_sources(workspace));
    let tools = list_mcp_tools(&servers, server_name)?;
    if tools.is_empty() {
        return Ok("no tools reported".to_string());
    }
    Ok(tools
        .into_iter()
        .map(|tool| {
            format!(
                "{} | {}",
                tool.name,
                tool.description.unwrap_or_else(|| "-".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn render_mcp_call_text(
    workspace: &Path,
    config: &LoadedConfig,
    server_name: &str,
    tool_name: &str,
    arguments: Option<&str>,
    session: &SessionStore,
) -> Result<String, String> {
    let servers = discover_mcp_servers(&config.mcp_sources(workspace));
    let parsed_arguments = match arguments {
        Some(raw) => serde_json::from_str::<serde_json::Value>(raw)
            .map_err(|err| format!("invalid JSON arguments: {err}"))?,
        None => json!({}),
    };
    let result = call_mcp_tool(&servers, server_name, tool_name, parsed_arguments.clone())?;
    let _ = session.append(
        "mcp_call",
        json!({
            "server": server_name,
            "tool": tool_name,
            "arguments": parsed_arguments,
            "result": result,
        }),
    );
    Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string()))
}

fn maybe_run_custom_slash_command_tui(
    workspace: &Path,
    config: &LoadedConfig,
    session: &SessionStore,
    trimmed: &str,
    current_model: &mut Option<String>,
    mode: &mut PermissionMode,
    approval_policy: &mut ApprovalPolicy,
    ui: &mut TuiState,
    custom_depth: usize,
) -> Result<bool, String> {
    let Some((name, args_raw)) = parse_slash_invocation(trimmed) else {
        return Ok(false);
    };
    let commands = discover_slash_commands(workspace);
    let Some(command) = resolve_slash_command(&commands, name) else {
        return Ok(false);
    };
    validate_slash_command_args(command, args_raw)?;
    let expanded = expand_slash_command(command, args_raw);
    if expanded.is_empty() {
        return Err(format!(
            "custom command /{} expanded to no steps",
            command.name
        ));
    }
    ui.push_system(format!(
        "custom: /{} ({}, {})",
        command.name,
        command.kind.as_str(),
        command.source
    ));
    let _ = session.append(
        "custom_command",
        json!({
            "name": command.name,
            "kind": command.kind.as_str(),
            "source": command.source,
            "args": args_raw,
            "expanded_steps": expanded,
        }),
    );
    for step in expanded {
        ui.push_system(format!("step: {step}"));
        let _ = session.append("custom_command_step", json!({ "command": step }));
        match process_repl_input_tui(
            workspace,
            config,
            session,
            &step,
            current_model,
            mode,
            approval_policy,
            ui,
            custom_depth,
        ) {
            ReplDirective::Continue => {}
            ReplDirective::Exit => return Ok(true),
        }
    }
    Ok(true)
}

fn run_prompt_capture(
    workspace: &Path,
    config: &LoadedConfig,
    prompt: &str,
    override_model: Option<&str>,
    permission_mode: PermissionMode,
    approval_policy: &mut ApprovalPolicy,
    session: &SessionStore,
    ui: &mut TuiState,
) -> Result<String, String> {
    match run_agent_loop(
        config,
        workspace,
        prompt,
        override_model,
        permission_mode,
        Some(session),
        |request| approval_for_request_tui(request, approval_policy, session, ui),
    ) {
        Ok(reply) => {
            let mut lines = vec![
                format!("provider: {}", reply.provider.route.as_str()),
                format!("model: {}", reply.provider.model),
            ];
            if reply.provider.text.starts_with("Not verified:") {
                lines.push("verification: not verified".to_string());
            }
            if !reply.tool_events.is_empty() {
                lines.push(String::new());
                for (index, event) in reply.tool_events.iter().enumerate() {
                    lines.push(format!(
                        "tool[{index}] {} {}",
                        event.name,
                        serde_json::to_string(&event.arguments)
                            .unwrap_or_else(|_| "{}".to_string())
                    ));
                    lines.push(event.summary.clone());
                }
            }
            lines.push(String::new());
            lines.push(reply.provider.text);
            Ok(lines.join("\n"))
        }
        Err(errors) => {
            let rendered = errors.join("\n");
            let _ = session.append("prompt_error", json!({ "errors": errors }));
            if let Ok(Some(handoff)) = pending_model_handoff(session.path()) {
                let _ = session.append(
                    "model_probe_failed",
                    json!({
                        "model": handoff.snapshot.to_model,
                        "error": rendered,
                    }),
                );
            }
            Err(rendered)
        }
    }
}

fn approval_for_request_tui(
    request: &ApprovalRequest,
    approval_policy: &mut ApprovalPolicy,
    session: &SessionStore,
    _ui: &mut TuiState,
) -> Result<ApprovalOutcome, String> {
    let action = runtime::approval_action_for_policy(*approval_policy, request.risk);
    match action {
        ApprovalAction::AutoApprove => {
            let _ = session.append(
                "approval_result",
                json!({
                    "tool": request.tool,
                    "risk": request.risk.as_str(),
                    "decision": "auto-approve",
                    "reason": request.reason,
                }),
            );
            return Ok(ApprovalOutcome::Approve);
        }
        ApprovalAction::Deny => {
            let reason = format!(
                "blocked {}-risk tool `{}`: {}",
                request.risk, request.tool, request.reason
            );
            let _ = session.append(
                "approval_result",
                json!({
                    "tool": request.tool,
                    "risk": request.risk.as_str(),
                    "decision": "deny",
                    "reason": reason,
                }),
            );
            return Ok(ApprovalOutcome::Reject { reason });
        }
        ApprovalAction::Prompt => {}
    }

    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen, Show);
    println!();
    println!("approval required");
    println!("tool: {}", request.tool);
    println!("risk: {}", request.risk);
    println!("why: {}", request.reason);
    println!(
        "arguments: {}",
        serde_json::to_string_pretty(&request.arguments).unwrap_or_else(|_| "{}".to_string())
    );
    print!("approve? [y]es / [n]o / [a]uto: ");
    let _ = io::stdout().flush();

    let mut line = String::new();
    let read_result = io::stdin()
        .read_line(&mut line)
        .map_err(|err| err.to_string());
    let restore_result = restore_tui_terminal(&mut stdout);
    if let Err(err) = read_result {
        let _ = restore_result;
        return Err(err);
    }
    restore_result?;

    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => {
            let _ = session.append(
                "approval_result",
                json!({
                    "tool": request.tool,
                    "risk": request.risk.as_str(),
                    "decision": "approve",
                    "reason": request.reason,
                }),
            );
            Ok(ApprovalOutcome::Approve)
        }
        "a" | "auto" => {
            *approval_policy = ApprovalPolicy::Auto;
            let _ = session.append(
                "approval_change",
                json!({ "policy": approval_policy.as_str(), "via": "interactive" }),
            );
            let _ = session.append(
                "approval_result",
                json!({
                    "tool": request.tool,
                    "risk": request.risk.as_str(),
                    "decision": "approve",
                    "reason": request.reason,
                }),
            );
            Ok(ApprovalOutcome::Approve)
        }
        _ => {
            let _ = session.append(
                "approval_result",
                json!({
                    "tool": request.tool,
                    "risk": request.risk.as_str(),
                    "decision": "reject",
                    "reason": request.reason,
                }),
            );
            Ok(ApprovalOutcome::Reject {
                reason: format!(
                    "rejected by user for {}-risk tool `{}`",
                    request.risk, request.tool
                ),
            })
        }
    }
}

fn restore_tui_terminal(stdout: &mut Stdout) -> Result<(), String> {
    enable_raw_mode().map_err(|err| err.to_string())?;
    execute!(stdout, EnterAlternateScreen, Hide).map_err(|err| err.to_string())?;
    Ok(())
}

#[allow(dead_code)]
fn print_repl_status(
    workspace: &Path,
    session: &SessionStore,
    current_model: Option<&str>,
    mode: PermissionMode,
    approval_policy: ApprovalPolicy,
    connection_mode: ConnectionMode,
    verification_policy: VerificationPolicy,
) {
    let provider_count = load_provider_registry(workspace)
        .map(|registry| registry.profiles.len())
        .unwrap_or(0);
    let memory_records = list_memory_records(workspace).unwrap_or_default();
    println!("workspace: {}", workspace.display());
    println!("session: {}", session.path().display());
    println!("model: {}", current_model.unwrap_or("-"));
    if let Ok(Some(handoff)) = latest_model_handoff(session.path()) {
        if let Some(previous) = handoff.snapshot.from_model.as_deref() {
            println!("previous_model: {previous}");
        }
        println!("next_step: {}", handoff.snapshot.suggested_next_step);
    }
    if let Ok(Some(handoff)) = pending_model_handoff(session.path()) {
        println!("handoff_boost: pending for {}", handoff.snapshot.to_model);
    }
    println!("mode: {mode}");
    println!("connection: {connection_mode}");
    println!(
        "connection_behavior: {}",
        connection_mode_hint(connection_mode)
    );
    println!("approval: {}", approval_policy);
    println!(
        "approval_behavior: {}",
        approval_policy_hint(approval_policy)
    );
    println!("verification: {}", verification_policy);
    println!(
        "verification_behavior: {}",
        verification_policy_hint(verification_policy)
    );
    println!("saved_providers: {provider_count}");
    println!("memory_records: {}", memory_records.len());
}

fn print_model_status(workspace: &Path, config: &LoadedConfig, current_model: Option<&str>) {
    println!("active: {}", current_model.unwrap_or("-"));
    println!("default: {}", config.primary_model().unwrap_or("-"));
    if let Ok(Some(session_path)) = SessionStore::latest(workspace) {
        if let Ok(Some(handoff)) = latest_model_handoff(&session_path) {
            if let Some(previous) = handoff.snapshot.from_model.as_deref() {
                println!("previous: {previous}");
            }
            println!("current_goal: {}", handoff.snapshot.current_goal);
            println!("next: {}", handoff.snapshot.suggested_next_step);
        }
    }
    if !config.data.model.fallback.is_empty() {
        println!("fallback: {}", config.data.model.fallback.join(", "));
    }
    let registry = load_provider_registry(workspace).unwrap_or_default();
    if registry.profiles.is_empty() {
        println!("saved_profiles: none");
    } else {
        println!("saved_profiles:");
        for profile in registry.profiles {
            println!(
                "- {} | {} | use: profile/{}/<model>",
                profile.alias, profile.base_url, profile.alias
            );
        }
    }
}

#[allow(dead_code)]
fn print_login_status(workspace: &Path) {
    let registry = load_provider_registry(workspace).unwrap_or_default();
    println!("BYOK:");
    if registry.profiles.is_empty() {
        println!("- no saved provider profiles");
    } else {
        println!("- saved provider profiles: {}", registry.profiles.len());
        for profile in registry.profiles {
            println!("- {} | {}", profile.alias, profile.base_url);
        }
    }
    println!("Next steps:");
    println!("- `{} providers presets`", APP_NAME);
    println!("- `{} providers add <alias> --api-key <key>`", APP_NAME);
    println!("- use `profile/<alias>/<model>` in `/model` or `prompt --model`");
    println!("External CLIs:");
    println!(
        "- run `{} doctor` to inspect `claude` and `codex` availability",
        APP_NAME
    );
}

#[allow(dead_code)]
fn print_repl_help() {
    println!("/help       show commands");
    println!("/status     show current session state");
    println!("/model      show or set the active model");
    println!("/resume     show latest session resume summary");
    println!("/handoff    print a handoff block");
    println!("/handoff debug inspect the latest handoff state");
    println!("/why-context show the current prompt context");
    println!("/memory     list/search/candidates/sessions/save/delete/export/import/migrate portable memory");
    println!("/trajectory inspect active and recent trajectories");
    println!("/memory show inspect one recent memory record");
    println!("/memory candidates list pending auto-promotion candidates");
    println!("/memory promote save one pending candidate into portable memory");
    println!("/memory dismiss drop one pending candidate");
    println!("/memory session inspect one portable memory session");
    println!("/memory recall print rendered portable recall text");
    println!("/skills suggest list repeated workflow candidates");
    println!("/skills promote create a prompt-template from a candidate");
    println!("/commands   list custom slash commands");
    println!("/commands show inspect one custom slash command");
    println!("/commands init create a commands directory");
    println!("/commands new create a command template");
    println!("/login      show provider setup hints");
    println!("/mode       show or set permission mode");
    println!("/approval   show or set approval policy");
    println!("/doctor     inspect local environment");
    println!("/config     show resolved config");
    println!("/providers  list saved providers");
    println!("/blueprint  print architecture summary");
    println!("/prompt     run the harness loop against the configured model chain");
    println!("/skills     list discovered skills");
    println!("/skill      show or run a specific skill");
    println!("/mcp        list discovered MCP servers");
    println!("/mcp-tools  list tools from one MCP server");
    println!("/mcp-call   call a tool on one MCP server");
    println!("/session    show latest session path");
    println!("/read       read a file");
    println!("/write      write a file");
    println!("/edit       replace first occurrence");
    println!("/grep       search text");
    println!("/glob       find paths");
    println!("/exec       run a shell command");
    println!("/parallel-read run read/grep/glob in one batch");
    println!("/exit       leave the repl");
}

fn print_slash_commands(workspace: &Path) {
    let commands = discover_slash_commands(workspace);
    if commands.is_empty() {
        println!("no custom slash commands");
        return;
    }
    if let Ok(global_dir) = slash_command_dir(workspace, SlashCommandScope::Global) {
        println!("global_dir: {}", global_dir.display());
    }
    if let Ok(workspace_dir) = slash_command_dir(workspace, SlashCommandScope::Workspace) {
        println!("workspace_dir: {}", workspace_dir.display());
    }
    println!("custom_commands: {}", commands.len());
    for command in commands {
        let summary = if command.description.trim().is_empty() {
            "-".to_string()
        } else {
            compact_line(command.description.trim(), 96)
        };
        println!(
            "/{} | {} | {} | {}",
            command.name,
            command.kind.as_str(),
            command.source,
            summary
        );
    }
}

fn run_commands_init(workspace: &Path, scope: SlashCommandScope) {
    match init_slash_command_dir(workspace, scope) {
        Ok(path) => {
            println!("initialized {} commands dir", scope.as_str());
            println!("path: {}", path.display());
        }
        Err(err) => eprintln!("{err}"),
    }
}

fn run_commands_new(
    workspace: &Path,
    name: &str,
    kind: SlashCommandKind,
    scope: SlashCommandScope,
) {
    let kind_label = kind.as_str().to_string();
    match create_slash_command_template(workspace, scope, name, kind) {
        Ok(path) => {
            println!(
                "created /{} ({})",
                name.trim().trim_start_matches('/'),
                kind_label
            );
            println!("scope: {}", scope.as_str());
            println!("path: {}", path.display());
        }
        Err(err) => eprintln!("{err}"),
    }
}

fn print_slash_command_show(workspace: &Path, name: &str) {
    let commands = discover_slash_commands(workspace);
    let Some(command) = resolve_slash_command(&commands, name) else {
        eprintln!("unknown custom command: {name}");
        return;
    };
    println!("name: /{}", command.name);
    println!("kind: {}", command.kind.as_str());
    println!("source: {}", command.source);
    println!("path: {}", command.path.display());
    println!(
        "description: {}",
        if command.description.trim().is_empty() {
            "-"
        } else {
            command.description.trim()
        }
    );
    let usage = command
        .usage
        .clone()
        .unwrap_or_else(|| format!("/{}", command.name));
    println!("usage: {usage}");
    println!(
        "args: min={} max={}",
        command
            .min_args
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        command
            .max_args
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    match command.kind {
        SlashCommandKind::Alias => {
            println!("target: {}", command.target.as_deref().unwrap_or("-"));
        }
        SlashCommandKind::Macro => {
            println!("steps:");
            for step in &command.steps {
                println!("- {step}");
            }
        }
        SlashCommandKind::PromptTemplate => {
            println!("prompt: {}", command.prompt.as_deref().unwrap_or("-"));
        }
    }
}

#[allow(dead_code)]
fn print_saved_providers(workspace: &Path) {
    let registry = load_provider_registry(workspace).unwrap_or_default();
    if registry.profiles.is_empty() {
        println!("no saved provider profiles");
        return;
    }
    for profile in registry.profiles {
        println!(
            "{} | {} | {} | use: profile/{}/<model>",
            profile.alias, profile.route, profile.base_url, profile.alias
        );
    }
}

#[allow(dead_code)]
fn maybe_run_custom_slash_command(
    workspace: &Path,
    config: &LoadedConfig,
    session: &SessionStore,
    trimmed: &str,
    current_model: &mut Option<String>,
    mode: &mut PermissionMode,
    approval_policy: &mut ApprovalPolicy,
    custom_depth: usize,
) -> Result<bool, String> {
    let Some((name, args_raw)) = parse_slash_invocation(trimmed) else {
        return Ok(false);
    };
    let commands = discover_slash_commands(workspace);
    let Some(command) = resolve_slash_command(&commands, name) else {
        return Ok(false);
    };
    validate_slash_command_args(command, args_raw)?;
    let expanded = expand_slash_command(command, args_raw);
    if expanded.is_empty() {
        return Err(format!(
            "custom command /{} expanded to no steps",
            command.name
        ));
    }
    println!(
        "custom: /{} ({}, {})",
        command.name,
        command.kind.as_str(),
        command.source
    );
    let _ = session.append(
        "custom_command",
        json!({
            "name": command.name,
            "kind": command.kind.as_str(),
            "source": command.source,
            "args": args_raw,
            "expanded_steps": expanded,
        }),
    );
    for step in expanded {
        println!("step: {step}");
        let _ = session.append("custom_command_step", json!({ "command": step }));
        match handle_repl_line(
            workspace,
            config,
            session,
            &step,
            current_model,
            mode,
            approval_policy,
            custom_depth,
        ) {
            ReplDirective::Continue => {}
            ReplDirective::Exit => return Ok(true),
        }
    }
    Ok(true)
}

fn print_memory_list(workspace: &Path) {
    match list_memory_records(workspace) {
        Ok(records) => {
            if records.is_empty() {
                println!("memory_backend: {}", config_memory_backend_label(workspace));
                println!("no memory records");
                return;
            }
            println!("memory_backend: {}", config_memory_backend_label(workspace));
            println!("memory_records: {}", records.len());
            let (summaries, decisions, tasks, errors, notes) = memory_kind_counts(&records);
            println!(
                "kinds: summary={} decision={} task={} error={} note={}",
                summaries, decisions, tasks, errors, notes
            );
            println!("recent:");
            for (index, record) in records.into_iter().take(5).enumerate() {
                println!(
                    "- #{} | {} | {} | {}",
                    index + 1,
                    record.kind,
                    record.title,
                    record.ts_ms
                );
            }
            println!("hint: use `memory show <index>` or `memory recall [limit]`");
        }
        Err(err) => eprintln!("{err}"),
    }
}

fn print_trajectory_list(workspace: &Path, limit: usize) {
    match render_trajectory_list_text(workspace, limit) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_active_trajectory(workspace: &Path) {
    match render_active_trajectory_text(workspace) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_trajectory_show(workspace: &Path, index: usize) {
    match render_trajectory_show_text(workspace, index) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_trajectory_search(workspace: &Path, query: &str) {
    match render_trajectory_search_text(workspace, query) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_skill_candidates(workspace: &Path, limit: usize) {
    match render_skill_candidates_text(workspace, limit) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn run_skill_candidate_promotion(workspace: &Path, index: usize) {
    match run_skill_candidate_promotion_text(workspace, index) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_memory_show(workspace: &Path, index: usize) {
    match list_memory_records(workspace) {
        Ok(records) => {
            if records.is_empty() {
                println!("no memory records");
                return;
            }
            let Some(record) = records.get(index - 1) else {
                eprintln!("memory index out of range: {index}");
                return;
            };
            println!("index: {index}");
            println!("id: {}", record.id.as_deref().unwrap_or("-"));
            println!("kind: {}", record.kind);
            println!("title: {}", record.title);
            println!("ts_ms: {}", record.ts_ms);
            println!("session: {}", record.session_path.as_deref().unwrap_or("-"));
            if record.tags.is_empty() {
                println!("tags: -");
            } else {
                println!("tags: {}", record.tags.join(", "));
            }
            println!("body:");
            println!("{}", record.body.trim());
        }
        Err(err) => eprintln!("{err}"),
    }
}

fn print_memory_recall(workspace: &Path, limit: usize) {
    match build_memory_recall_text(workspace, limit) {
        Ok(text) => print!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_handoff_debug(workspace: &Path) {
    let latest_session = match SessionStore::latest(workspace) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("{err}");
            return;
        }
    };

    let Some(session_path) = latest_session else {
        println!("no sessions found");
        return;
    };

    println!("session: {}", session_path.display());

    match latest_model_handoff(&session_path) {
        Ok(Some(handoff)) => {
            println!("latest_handoff_ts_ms: {}", handoff.ts_ms);
            println!(
                "from_model: {}",
                handoff.snapshot.from_model.as_deref().unwrap_or("-")
            );
            println!("to_model: {}", handoff.snapshot.to_model);
            println!("current_goal: {}", handoff.snapshot.current_goal);
            println!(
                "recent_work_summary: {}",
                handoff.snapshot.recent_work_summary
            );
            if handoff.snapshot.open_tasks.is_empty() {
                println!("open_tasks: -");
            } else {
                println!("open_tasks:");
                for task in &handoff.snapshot.open_tasks {
                    println!("- {task}");
                }
            }
            if handoff.snapshot.recent_errors.is_empty() {
                println!("recent_errors: -");
            } else {
                println!("recent_errors:");
                for error in &handoff.snapshot.recent_errors {
                    println!("- {error}");
                }
            }
            println!(
                "suggested_next_step: {}",
                handoff.snapshot.suggested_next_step
            );
        }
        Ok(None) => println!("latest_handoff: -"),
        Err(err) => {
            eprintln!("{err}");
            return;
        }
    }

    match pending_model_handoff(&session_path) {
        Ok(Some(handoff)) => {
            println!("pending_handoff: yes");
            println!("pending_to_model: {}", handoff.snapshot.to_model);
        }
        Ok(None) => println!("pending_handoff: no"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_memory_search(workspace: &Path, query: &str) {
    match search_memory_records(workspace, query) {
        Ok(records) => {
            if records.is_empty() {
                println!("no memory matches");
                return;
            }
            println!("matches: {}", records.len());
            for record in records {
                println!("{} | {}", record.kind, record.title);
                println!("{}", record.body.trim());
                println!();
            }
        }
        Err(err) => eprintln!("{err}"),
    }
}

fn print_memory_sessions(workspace: &Path, config: &LoadedConfig) {
    match render_memory_sessions_text(workspace, config) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_memory_candidates(workspace: &Path, limit: usize) {
    match render_memory_candidates_text(workspace, limit) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_memory_session(workspace: &Path, config: &LoadedConfig, session_key: &str) {
    match render_memory_session_text(workspace, config, session_key) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_memory_delete(workspace: &Path, config: &LoadedConfig, ids: &[String]) {
    match render_memory_delete_text(workspace, config, ids) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_memory_export(workspace: &Path, config: &LoadedConfig, output: Option<&Path>) {
    match render_memory_export_text(workspace, config, output) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_memory_import(workspace: &Path, config: &LoadedConfig, input: &Path) {
    match render_memory_import_text(workspace, config, input) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

fn print_memory_migrate(
    workspace: &Path,
    config: &LoadedConfig,
    from: MemoryBackendKind,
    to: MemoryBackendKind,
) {
    match render_memory_migrate_text(workspace, config, from, to) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("{err}"),
    }
}

#[allow(dead_code)]
fn save_latest_session_memory(workspace: &Path, session: &SessionStore) {
    match save_session_memory_bundle(workspace, session.path()) {
        Ok(bundle) => {
            if bundle.saved_records.is_empty() {
                println!("memory unchanged");
                return;
            }
            println!("saved {} memory record(s)", bundle.saved_records.len());
            for record in bundle.saved_records {
                println!("{} | {}", record.kind, record.title);
            }
        }
        Err(err) => eprintln!("{err}"),
    }
}

#[allow(dead_code)]
fn print_model_switch_summary(snapshot: &ModelHandoffSnapshot) {
    println!("active: {}", snapshot.to_model);
    println!(
        "previous: {}",
        snapshot.from_model.as_deref().unwrap_or("-")
    );
    println!("handoff: {}", compact_line(&snapshot.current_goal, 96));
    println!("next: {}", compact_line(&snapshot.suggested_next_step, 96));
    if !snapshot.recent_errors.is_empty() {
        println!(
            "warning: {}",
            compact_line(snapshot.recent_errors[0].as_str(), 96)
        );
    }
}

fn compact_line(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let truncated = input.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

fn autosave_session_memory(workspace: &Path, session: &SessionStore) {
    match save_session_memory_bundle(workspace, session.path()) {
        Ok(bundle) if !bundle.saved_records.is_empty() => {
            println!("memory autosaved: {} record(s)", bundle.saved_records.len());
        }
        Ok(_) => {}
        Err(err) => eprintln!("memory autosave failed: {err}"),
    }
}

fn build_context_dump(
    workspace: &Path,
    config: &LoadedConfig,
    active_model: Option<&str>,
    mode: Option<PermissionMode>,
) -> String {
    let context = gather_workspace_context(
        workspace,
        config,
        mode.unwrap_or_else(|| config.default_permission_mode()),
        active_model.or_else(|| config.primary_model()),
        None,
    );
    render_prompt_context(&context)
}

#[allow(dead_code)]
fn handle_repl_tool_command(
    workspace: &Path,
    line: &str,
    mode: PermissionMode,
    session: &SessionStore,
) {
    let tokens: Vec<String> = line.split_whitespace().map(ToOwned::to_owned).collect();
    if tokens.is_empty() {
        return;
    }
    let command = tokens[0].trim_start_matches('/');
    let args = &tokens[1..];
    let result = run_tool_command(workspace, command, args, line, mode);
    match result {
        Ok(output) => {
            let _ = session.append(
                "tool_result",
                json!({ "command": command, "summary": output.summary, "content": output.content }),
            );
            print_tool_output(&output);
        }
        Err(err) => {
            let _ = session.append("tool_error", json!({ "command": command, "error": err }));
            eprintln!("{err}");
        }
    }
}

fn handle_session_command(workspace: &Path, config: &LoadedConfig, args: &[String]) {
    match args.first().map(String::as_str) {
        Some("latest") | None => match SessionStore::latest_in(&config.session_dir(workspace)) {
            Ok(Some(path)) => println!("{}", path.display()),
            Ok(None) => println!("no sessions found"),
            Err(err) => eprintln!("{err}"),
        },
        Some(other) => {
            eprintln!("unknown session command: {other}");
            std::process::exit(2);
        }
    }
}

fn handle_model_command(workspace: &Path, config: &LoadedConfig, args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("show") => print_model_status(workspace, config, config.primary_model()),
        Some("set-primary") => {
            let Some(model) = args.get(1) else {
                eprintln!("usage: {} model set-primary <spec>", APP_NAME);
                std::process::exit(2);
            };
            let mut next = config.data.clone();
            next.model.primary = Some(model.clone());
            match save_config(workspace, &next) {
                Ok(path) => {
                    println!("primary model set to {model}");
                    println!("path: {}", path.display());
                }
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(1);
                }
            }
        }
        Some(other) => {
            eprintln!("unknown model command: {other}");
            std::process::exit(2);
        }
    }
}

fn handle_memory_command(workspace: &Path, config: &LoadedConfig, args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("list") => print_memory_list(workspace),
        Some("show") => {
            let Some(index) = args.get(1) else {
                eprintln!("usage: {} memory show <index>", APP_NAME);
                std::process::exit(2);
            };
            match parse_positive_index(index) {
                Ok(index) => print_memory_show(workspace, index),
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(2);
                }
            }
        }
        Some("recall") => {
            let limit = match parse_optional_limit(args.get(1).map(String::as_str).unwrap_or(""), 6)
            {
                Ok(limit) => limit,
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(2);
                }
            };
            print_memory_recall(workspace, limit);
        }
        Some("search") => {
            let Some(query) = args.get(1) else {
                eprintln!("usage: {} memory search <query>", APP_NAME);
                std::process::exit(2);
            };
            print_memory_search(workspace, query);
        }
        Some("candidates") => print_memory_candidates(workspace, 12),
        Some("promote") => {
            let Some(index) = args.get(1) else {
                eprintln!("usage: {} memory promote <index>", APP_NAME);
                std::process::exit(2);
            };
            match parse_positive_index(index) {
                Ok(index) => match run_memory_candidate_promotion_text(workspace, index) {
                    Ok(text) => println!("{text}"),
                    Err(err) => {
                        eprintln!("{err}");
                        std::process::exit(1);
                    }
                },
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(2);
                }
            }
        }
        Some("dismiss") => {
            let Some(index) = args.get(1) else {
                eprintln!("usage: {} memory dismiss <index>", APP_NAME);
                std::process::exit(2);
            };
            match parse_positive_index(index) {
                Ok(index) => match run_memory_candidate_dismiss_text(workspace, index) {
                    Ok(text) => println!("{text}"),
                    Err(err) => {
                        eprintln!("{err}");
                        std::process::exit(1);
                    }
                },
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(2);
                }
            }
        }
        Some("sessions") => print_memory_sessions(workspace, config),
        Some("session") => {
            let Some(session_key) = args.get(1) else {
                eprintln!("usage: {} memory session <session-key>", APP_NAME);
                std::process::exit(2);
            };
            print_memory_session(workspace, config, session_key);
        }
        Some("save") => {
            let session_path = if let Some(path) = args.get(1) {
                PathBuf::from(path)
            } else {
                match SessionStore::latest(workspace) {
                    Ok(Some(path)) => path,
                    Ok(None) => {
                        eprintln!("no sessions found");
                        std::process::exit(1);
                    }
                    Err(err) => {
                        eprintln!("{err}");
                        std::process::exit(1);
                    }
                }
            };
            match save_session_memory_bundle(workspace, &session_path) {
                Ok(bundle) => {
                    if bundle.saved_records.is_empty() {
                        println!("memory unchanged");
                        println!("pending_candidates: {}", bundle.pending_candidates);
                        return;
                    }
                    println!("saved {} memory record(s)", bundle.saved_records.len());
                    for record in bundle.saved_records {
                        println!("{} | {}", record.kind, record.title);
                    }
                    println!("pending_candidates: {}", bundle.pending_candidates);
                }
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(1);
                }
            }
        }
        Some("delete") => match parse_memory_delete_args(&args[1..]) {
            Ok(ids) => print_memory_delete(workspace, config, &ids),
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(2);
            }
        },
        Some("export") => match parse_memory_export_args(&args[1..]) {
            Ok((_format, output)) => print_memory_export(workspace, config, output.as_deref()),
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(2);
            }
        },
        Some("import") => match parse_memory_import_args(&args[1..]) {
            Ok((_format, input)) => print_memory_import(workspace, config, &input),
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(2);
            }
        },
        Some("migrate") => match parse_memory_migrate_args(&args[1..]) {
            Ok((from, to)) => print_memory_migrate(workspace, config, from, to),
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(2);
            }
        },
        Some(other) => {
            eprintln!("unknown memory command: {other}");
            std::process::exit(2);
        }
    }
}

fn handle_commands_command(workspace: &Path, args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("list") => print_slash_commands(workspace),
        Some("init") => {
            let scope = parse_commands_scope(args);
            run_commands_init(workspace, scope);
        }
        Some("new") => {
            let Some(name) = args.get(1) else {
                eprintln!(
                    "usage: {} commands new <name> [alias|macro|prompt-template] [--global]",
                    APP_NAME
                );
                std::process::exit(2);
            };
            let kind = args
                .get(2)
                .and_then(|value| parse_command_kind(value))
                .unwrap_or(SlashCommandKind::Macro);
            let scope = parse_commands_scope(args);
            run_commands_new(workspace, name, kind, scope);
        }
        Some("show") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: {} commands show <name>", APP_NAME);
                std::process::exit(2);
            };
            print_slash_command_show(workspace, name);
        }
        Some(other) => {
            eprintln!("unknown commands command: {other}");
            std::process::exit(2);
        }
    }
}

fn parse_commands_scope(args: &[String]) -> SlashCommandScope {
    if args.iter().any(|arg| arg == "--global") {
        SlashCommandScope::Global
    } else {
        SlashCommandScope::Workspace
    }
}

fn handle_providers_command(workspace: &Path, args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("catalog") => {
            for backend in backend_catalog() {
                println!(
                    "{} | {} | {} | {}",
                    backend.kind.as_str(),
                    backend.lane.as_str(),
                    backend.auth_hint,
                    backend.availability_hint
                );
            }
        }
        Some("presets") => {
            for preset in provider_presets() {
                println!(
                    "{} | {} | {} | {}",
                    preset.name,
                    preset.route.as_str(),
                    preset.base_url,
                    preset.description
                );
            }
        }
        Some("saved") => match load_provider_registry(workspace) {
            Ok(registry) => {
                if registry.profiles.is_empty() {
                    println!("no saved provider profiles");
                    return;
                }
                for profile in registry.profiles {
                    println!(
                        "{} | {} | {} | {} | use: profile/{}/<model>",
                        profile.alias,
                        profile.route,
                        profile.base_url,
                        profile.source,
                        profile.alias
                    );
                }
            }
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        },
        Some("sync-env") => sync_provider_profiles_from_env(workspace),
        Some("detect-key") => {
            let Some(api_key) = args.get(1) else {
                eprintln!("usage: {} providers detect-key <api-key>", APP_NAME);
                std::process::exit(2);
            };
            match detect_provider_key(api_key) {
                Some(detection) => {
                    println!(
                        "{} | {} | {} | confidence={:?}",
                        detection.provider_name,
                        detection.route.as_str(),
                        detection.base_url,
                        detection.confidence
                    );
                }
                None => println!("unknown | provide --preset or --base-url manually"),
            }
        }
        Some("add") => add_provider_profile_command(workspace, &args[1..]),
        Some("remove") => {
            let Some(alias) = args.get(1) else {
                eprintln!("usage: {} providers remove <alias>", APP_NAME);
                std::process::exit(2);
            };
            match remove_provider_profile(workspace, alias) {
                Ok(true) => println!("removed provider profile `{alias}`"),
                Ok(false) => {
                    eprintln!("provider profile not found: {alias}");
                    std::process::exit(1);
                }
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(1);
                }
            }
        }
        Some(other) => {
            eprintln!("unknown providers command: {other}");
            std::process::exit(2);
        }
    }
}

fn parse_positive_index(input: &str) -> Result<usize, String> {
    let value = input
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("invalid index: {input}"))?;
    if value == 0 {
        return Err("index must be >= 1".to_string());
    }
    Ok(value)
}

fn parse_slash_invocation(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim();
    let body = trimmed.strip_prefix('/')?;
    let (name, rest) = body
        .split_once(char::is_whitespace)
        .map(|(name, rest)| (name, rest.trim()))
        .unwrap_or((body, ""));
    if name.is_empty() {
        return None;
    }
    Some((name, rest))
}

fn parse_scope_flag(input: &str) -> SlashCommandScope {
    if input.split_whitespace().any(|part| part == "--global") {
        SlashCommandScope::Global
    } else {
        SlashCommandScope::Workspace
    }
}

fn parse_new_command_args(
    input: &str,
) -> Result<(&str, SlashCommandKind, SlashCommandScope), String> {
    let tokens = input.split_whitespace().collect::<Vec<_>>();
    let Some(name) = tokens.first().copied() else {
        return Err(
            "usage: /commands new <name> [alias|macro|prompt-template] [--global]".to_string(),
        );
    };
    let mut kind = SlashCommandKind::Macro;
    for token in tokens.iter().skip(1) {
        if *token == "--global" {
            continue;
        }
        kind = parse_command_kind(token).ok_or_else(|| format!("unknown command kind: {token}"))?;
    }
    Ok((name, kind, parse_scope_flag(input)))
}

fn parse_command_kind(raw: &str) -> Option<SlashCommandKind> {
    match raw.trim() {
        "alias" => Some(SlashCommandKind::Alias),
        "macro" => Some(SlashCommandKind::Macro),
        "prompt-template" | "prompt_template" => Some(SlashCommandKind::PromptTemplate),
        _ => None,
    }
}

fn parse_optional_limit(input: &str, default: usize) -> Result<usize, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(default);
    }
    let value = trimmed
        .parse::<usize>()
        .map_err(|_| format!("invalid limit: {trimmed}"))?;
    if value == 0 {
        return Err("limit must be >= 1".to_string());
    }
    Ok(value)
}

fn parse_memory_backend_kind(input: &str) -> Result<MemoryBackendKind, String> {
    MemoryBackendKind::parse(input).ok_or_else(|| format!("unknown memory backend: {input}"))
}

fn parse_flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2).find_map(|window| {
        if window[0] == flag {
            Some(window[1].as_str())
        } else {
            None
        }
    })
}

fn parse_memory_export_args(args: &[String]) -> Result<(String, Option<PathBuf>), String> {
    let format = parse_flag_value(args, "--format").unwrap_or("amcp-jsonl");
    if format != "amcp-jsonl" {
        return Err(format!("unsupported memory export format: {format}"));
    }
    let output = parse_flag_value(args, "--output").map(PathBuf::from);
    Ok((format.to_string(), output))
}

fn parse_memory_import_args(args: &[String]) -> Result<(String, PathBuf), String> {
    let format = parse_flag_value(args, "--format").unwrap_or("amcp-jsonl");
    if format != "amcp-jsonl" {
        return Err(format!("unsupported memory import format: {format}"));
    }
    let input = parse_flag_value(args, "--input").ok_or_else(|| {
        "usage: 3122 memory import --format amcp-jsonl --input <path>".to_string()
    })?;
    Ok((format.to_string(), PathBuf::from(input)))
}

fn parse_memory_migrate_args(
    args: &[String],
) -> Result<(MemoryBackendKind, MemoryBackendKind), String> {
    let from = parse_flag_value(args, "--from")
        .ok_or_else(|| "usage: 3122 memory migrate --from <backend> --to <backend>".to_string())
        .and_then(parse_memory_backend_kind)?;
    let to = parse_flag_value(args, "--to")
        .ok_or_else(|| "usage: 3122 memory migrate --from <backend> --to <backend>".to_string())
        .and_then(parse_memory_backend_kind)?;
    Ok((from, to))
}

fn parse_memory_delete_args(args: &[String]) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--id" => {
                index += 1;
                let Some(id) = args.get(index) else {
                    return Err(
                        "usage: 3122 memory delete --id <memory-id> [--id <memory-id> ...]"
                            .to_string(),
                    );
                };
                ids.push(id.clone());
            }
            other if !other.trim().is_empty() => ids.push(other.to_string()),
            _ => {}
        }
        index += 1;
    }
    if ids.is_empty() {
        return Err(
            "usage: 3122 memory delete --id <memory-id> [--id <memory-id> ...]".to_string(),
        );
    }
    Ok(ids)
}

fn handle_handoff_command(workspace: &Path, args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("show") => match build_handoff_text(workspace) {
            Ok(text) => print!("{text}"),
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        },
        Some("debug") => print_handoff_debug(workspace),
        Some(other) => {
            eprintln!("unknown handoff command: {other}");
            std::process::exit(2);
        }
    }
}

fn add_provider_profile_command(workspace: &Path, args: &[String]) {
    let Some(alias) = args.first() else {
        eprintln!(
            "usage: {} providers add <alias> --api-key <key> [--preset <name>] [--base-url <url>] [--route <openai-compat|anthropic|ollama>]",
            APP_NAME
        );
        std::process::exit(2);
    };

    let mut api_key = None;
    let mut preset_name = None;
    let mut base_url = None;
    let mut route = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--api-key" => {
                index += 1;
                api_key = args.get(index).cloned();
            }
            "--preset" => {
                index += 1;
                preset_name = args.get(index).cloned();
            }
            "--base-url" => {
                index += 1;
                base_url = args.get(index).cloned();
            }
            "--route" => {
                index += 1;
                route = args.get(index).cloned();
            }
            other => {
                eprintln!("unknown flag: {other}");
                std::process::exit(2);
            }
        }
        index += 1;
    }

    let Some(api_key) = api_key else {
        eprintln!("missing required flag: --api-key");
        std::process::exit(2);
    };

    let (route, base_url, source) = if let Some(preset_name) = preset_name {
        match provider_preset(&preset_name) {
            Some(preset) => (
                preset.route.as_str().to_string(),
                preset.base_url.to_string(),
                format!("preset:{preset_name}"),
            ),
            None => {
                eprintln!("unknown preset: {preset_name}");
                std::process::exit(2);
            }
        }
    } else if let Some(detection) = detect_provider_key(&api_key) {
        (
            detection.route.as_str().to_string(),
            detection.base_url,
            format!("detected:{}", detection.provider_name),
        )
    } else if let Some(base_url) = base_url {
        (
            route.unwrap_or_else(|| "openai-compat".to_string()),
            base_url,
            "manual".to_string(),
        )
    } else {
        eprintln!(
            "could not detect provider from this key; pass --preset <name> or --base-url <url>"
        );
        std::process::exit(2);
    };

    let profile = SavedProviderProfile {
        alias: alias.clone(),
        route,
        base_url,
        api_key,
        source,
    };

    match upsert_provider_profile(workspace, profile.clone()) {
        Ok(path) => {
            println!("saved provider profile `{}`", profile.alias);
            println!("route: {}", profile.route);
            println!("base_url: {}", profile.base_url);
            println!("source: {}", profile.source);
            println!("path: {}", path.display());
            println!("use: profile/{}/<model>", profile.alias);
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

fn sync_provider_profiles_from_env(workspace: &Path) {
    let specs = [
        (
            "anthropic-api",
            "anthropic",
            "ANTHROPIC_API_KEY",
            Some("ANTHROPIC_BASE_URL"),
            Some("anthropic"),
            "env:anthropic",
        ),
        (
            "openai-api",
            "openai-compat",
            "OPENAI_API_KEY",
            Some("OPENAI_BASE_URL"),
            Some("openai"),
            "env:openai",
        ),
        (
            "groq",
            "openai-compat",
            "GROQ_API_KEY",
            Some("GROQ_BASE_URL"),
            Some("groq"),
            "env:groq",
        ),
        (
            "qwen-api",
            "openai-compat",
            "QWEN_API_KEY",
            Some("QWEN_BASE_URL"),
            Some("openrouter"),
            "env:qwen-api",
        ),
        (
            "zai",
            "openai-compat",
            "ZAI_API_KEY",
            Some("ZAI_BASE_URL"),
            Some("zai-coding"),
            "env:zai",
        ),
        (
            "minimax",
            "openai-compat",
            "MINIMAX_API_KEY",
            Some("MINIMAX_BASE_URL"),
            Some("minimax"),
            "env:minimax",
        ),
        (
            "deepinfra",
            "openai-compat",
            "DEEPINFRA_API_KEY",
            Some("DEEPINFRA_BASE_URL"),
            Some("deepinfra"),
            "env:deepinfra",
        ),
    ];

    let mut saved = Vec::new();
    for (alias, route, api_key_env, base_url_env, preset_name, source) in specs {
        let Some(api_key) = env::var(api_key_env)
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };

        let base_url = base_url_env
            .and_then(|name| env::var(name).ok())
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                preset_name
                    .and_then(provider_preset)
                    .map(|preset| preset.base_url.to_string())
            });

        let Some(base_url) = base_url else {
            eprintln!("skipping {alias}: no base URL configured");
            continue;
        };

        let profile = SavedProviderProfile {
            alias: alias.to_string(),
            route: route.to_string(),
            base_url,
            api_key,
            source: source.to_string(),
        };
        match upsert_provider_profile(workspace, profile) {
            Ok(_) => saved.push(alias.to_string()),
            Err(err) => {
                eprintln!("failed to save {alias}: {err}");
                std::process::exit(1);
            }
        }
    }

    if saved.is_empty() {
        println!("no env-backed provider profiles found");
        return;
    }

    println!("saved {} provider profile(s)", saved.len());
    for alias in saved {
        println!("- {alias}");
    }
}

fn handle_skills_command(workspace: &Path, config: &LoadedConfig, args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("list") => print_skills(workspace, config, false),
        Some("all") => print_skills(workspace, config, true),
        Some("suggest") => print_skill_candidates(workspace, 8),
        Some("promote") => {
            let Some(index) = args.get(1) else {
                eprintln!("usage: {} skills promote <index>", APP_NAME);
                std::process::exit(2);
            };
            match parse_positive_index(index) {
                Ok(index) => run_skill_candidate_promotion(workspace, index),
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(2);
                }
            }
        }
        Some("show") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: {} skills show <name>", APP_NAME);
                std::process::exit(2);
            };
            show_skill(workspace, config, name);
        }
        Some("run") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: {} skills run <name> [task...]", APP_NAME);
                std::process::exit(2);
            };
            let task = if args.len() > 2 {
                Some(args[2..].join(" "))
            } else {
                None
            };
            run_skill(workspace, config, name, task.as_deref(), None);
        }
        Some(other) => {
            eprintln!("unknown skills command: {other}");
            std::process::exit(2);
        }
    }
}

fn handle_trajectory_command(workspace: &Path, args: &[String]) {
    let _ = SessionStore::latest(workspace)
        .ok()
        .flatten()
        .map(|path| record_session_trajectory(workspace, &path));
    match args.first().map(String::as_str) {
        None | Some("list") => print_trajectory_list(workspace, 8),
        Some("active") => print_active_trajectory(workspace),
        Some("show") => {
            let Some(index) = args.get(1) else {
                eprintln!("usage: {} trajectory show <index>", APP_NAME);
                std::process::exit(2);
            };
            match parse_positive_index(index) {
                Ok(index) => print_trajectory_show(workspace, index),
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(2);
                }
            }
        }
        Some("search") => {
            let Some(_query) = args.get(1) else {
                eprintln!("usage: {} trajectory search <query>", APP_NAME);
                std::process::exit(2);
            };
            print_trajectory_search(workspace, &args[1..].join(" "));
        }
        Some(other) => {
            eprintln!("unknown trajectory command: {other}");
            std::process::exit(2);
        }
    }
}

fn handle_mcp_command(workspace: &Path, config: &LoadedConfig, args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("list") => print_mcp(workspace, config),
        Some("tools") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: {} mcp tools <server>", APP_NAME);
                std::process::exit(2);
            };
            print_mcp_tools(workspace, config, name);
        }
        Some("call") => {
            let Some(server) = args.get(1) else {
                eprintln!("usage: {} mcp call <server> <tool> [json-args]", APP_NAME);
                std::process::exit(2);
            };
            let Some(tool) = args.get(2) else {
                eprintln!("usage: {} mcp call <server> <tool> [json-args]", APP_NAME);
                std::process::exit(2);
            };
            let arguments = if args.len() > 3 {
                Some(args[3..].join(" "))
            } else {
                None
            };
            run_mcp_call(workspace, config, server, tool, arguments.as_deref(), None);
        }
        Some(other) => {
            eprintln!("unknown mcp command: {other}");
            std::process::exit(2);
        }
    }
}

fn handle_tool_command(
    workspace: &Path,
    args: &[String],
    mode: PermissionMode,
    session: Option<&SessionStore>,
) {
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("missing tool command");
        std::process::exit(2);
    };
    let input = args.join(" ");
    match run_tool_command(workspace, command, &args[1..], &input, mode) {
        Ok(output) => {
            if let Some(store) = session {
                let _ = store.append(
                    "tool_result",
                    json!({ "command": command, "summary": output.summary, "content": output.content }),
                );
            }
            print_tool_output(&output);
        }
        Err(err) => {
            if let Some(store) = session {
                let _ = store.append("tool_error", json!({ "command": command, "error": err }));
            }
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

fn run_tool_command(
    workspace: &Path,
    command: &str,
    args: &[String],
    raw_line: &str,
    mode: PermissionMode,
) -> Result<ToolOutput, String> {
    match command {
        "read" => {
            let path = args
                .first()
                .ok_or_else(|| "usage: /read <path>".to_string())?;
            read_file(Path::new(path), workspace, mode)
        }
        "write" => {
            let path = args
                .first()
                .ok_or_else(|| "usage: /write <path> <text...>".to_string())?;
            let slash_prefix = format!("/{command} {path}");
            let bare_prefix = format!("{command} {path}");
            let contents = raw_line
                .strip_prefix(&slash_prefix)
                .or_else(|| raw_line.strip_prefix(&bare_prefix))
                .map(str::trim_start)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "usage: /write <path> <text...>".to_string())?;
            write_file(Path::new(path), contents, workspace, mode)
        }
        "edit" => {
            let body = raw_line
                .strip_prefix("/edit ")
                .or_else(|| raw_line.strip_prefix("edit "))
                .ok_or_else(|| "usage: /edit <path> <needle> => <replacement>".to_string())?;
            let Some((left, replacement)) = body.split_once(" => ") else {
                return Err("usage: /edit <path> <needle> => <replacement>".to_string());
            };
            let Some((path, needle)) = left.split_once(' ') else {
                return Err("usage: /edit <path> <needle> => <replacement>".to_string());
            };
            edit_file(Path::new(path), needle, replacement, workspace, mode)
        }
        "grep" => {
            let query = args
                .first()
                .ok_or_else(|| "usage: /grep <query> [path]".to_string())?;
            let scope = args.get(1).map(|value| Path::new(value.as_str()));
            grep_search(query, scope, workspace, mode)
        }
        "glob" => {
            let pattern = args
                .first()
                .ok_or_else(|| "usage: /glob <pattern> [path]".to_string())?;
            let scope = args.get(1).map(|value| Path::new(value.as_str()));
            glob_search(pattern, scope, workspace, mode)
        }
        "exec" => {
            let command = raw_line
                .strip_prefix("/exec ")
                .or_else(|| raw_line.strip_prefix("exec "))
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "usage: /exec <command...>".to_string())?;
            exec_command(command, workspace, mode)
        }
        "parallel-read" => {
            let raw = raw_line
                .strip_prefix("/parallel-read ")
                .or_else(|| raw_line.strip_prefix("parallel-read "))
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "usage: /parallel-read <json-array>".to_string())?;
            let value =
                serde_json::from_str::<serde_json::Value>(raw).map_err(|err| err.to_string())?;
            let operations = value
                .as_array()
                .ok_or_else(|| "parallel-read expects a JSON array".to_string())?;
            parallel_read_only(operations, workspace, mode)
        }
        other => Err(format!("unknown tool command: {other}")),
    }
}

fn print_tool_output(output: &ToolOutput) {
    println!("{}", output.summary);
    if !output.content.is_empty() {
        println!("{}", output.content);
    }
}

fn print_skills(workspace: &Path, config: &LoadedConfig, show_all: bool) {
    let skills = discover_skills(&config.skill_sources(workspace));
    if skills.is_empty() {
        println!("no skills found");
        return;
    }
    println!("{}", render_skill_list(&skills, show_all));
}

fn render_skill_list(skills: &[runtime::SkillEntry], show_all: bool) -> String {
    let mut lines = Vec::new();
    let workspace_count = skills
        .iter()
        .filter(|skill| skill.source == "workspace")
        .count();
    let user_count = skills.iter().filter(|skill| skill.source == "user").count();
    lines.push(format!("skills: {}", skills.len()));
    lines.push(format!(
        "sources: workspace={} user={}",
        workspace_count, user_count
    ));
    lines.push("list:".to_string());
    let max_items = if show_all { usize::MAX } else { 20 };
    for skill in skills.iter().take(max_items) {
        lines.push(format!(
            "- {} | {} | {}",
            skill.name,
            skill.source,
            compact_line(&skill.summary, 96)
        ));
    }
    if !show_all && skills.len() > max_items {
        lines.push(format!(
            "… {} more | use `skills list all` or `skills show <name>`",
            skills.len() - max_items
        ));
    } else {
        lines.push("hint: use `skills show <name>`".to_string());
    }
    lines.join("\n")
}

fn show_skill(workspace: &Path, config: &LoadedConfig, name: &str) {
    let skills = discover_skills(&config.skill_sources(workspace));
    match resolve_skill(&skills, name) {
        Ok(skill) => {
            println!("name: {}", skill.name);
            println!("source: {}", skill.source);
            println!("path: {}", skill.path.display());
            println!("summary: {}", skill.summary);
            println!();
            println!("{}", skill.markdown);
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

fn run_skill(
    workspace: &Path,
    config: &LoadedConfig,
    name: &str,
    task: Option<&str>,
    session: Option<&SessionStore>,
) {
    let skills = discover_skills(&config.skill_sources(workspace));
    match resolve_skill(&skills, name) {
        Ok(skill) => {
            let packet = runtime::build_skill_packet(skill, task);
            if let Some(store) = session {
                let _ = store.append(
                    "skill_invocation",
                    json!({
                        "name": packet.skill.name,
                        "source": packet.skill.source,
                        "path": packet.skill.path.display().to_string(),
                        "task": packet.task,
                    }),
                );
            }
            println!("skill: {}", packet.skill.name);
            println!("source: {}", packet.skill.source);
            println!("path: {}", packet.skill.path.display());
            println!("summary: {}", packet.skill.summary);
            println!("task: {}", packet.task.as_deref().unwrap_or("-"));
            println!();
            println!("{}", packet.prompt);
        }
        Err(err) => {
            if let Some(store) = session {
                let _ = store.append("skill_error", json!({ "name": name, "error": err }));
            }
            eprintln!("{err}");
            if session.is_none() {
                std::process::exit(1);
            }
        }
    }
}

fn split_skill_command(input: &str) -> Option<(&str, Option<&str>)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.split_once(' ') {
        Some((name, rest)) => Some((name, Some(rest.trim()).filter(|value| !value.is_empty()))),
        None => Some((trimmed, None)),
    }
}

fn print_mcp(workspace: &Path, config: &LoadedConfig) {
    let servers = discover_mcp_servers(&config.mcp_sources(workspace));
    if servers.is_empty() {
        println!("no mcp servers configured");
        println!("expected config file shape: .harness/mcp.json");
        return;
    }
    for server in servers {
        let location = server
            .command
            .as_ref()
            .map(ToOwned::to_owned)
            .or(server.url.as_ref().map(ToOwned::to_owned))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{} | {} | enabled={} | {} | {}",
            server.name, server.transport, server.enabled, location, server.source
        );
    }
}

fn print_mcp_tools(workspace: &Path, config: &LoadedConfig, server_name: &str) {
    let servers = discover_mcp_servers(&config.mcp_sources(workspace));
    match list_mcp_tools(&servers, server_name) {
        Ok(tools) => {
            if tools.is_empty() {
                println!("no tools reported");
                return;
            }
            for tool in tools {
                println!(
                    "{} | {}",
                    tool.name,
                    tool.description.as_deref().unwrap_or("-")
                );
            }
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

fn run_mcp_call(
    workspace: &Path,
    config: &LoadedConfig,
    server_name: &str,
    tool_name: &str,
    arguments: Option<&str>,
    session: Option<&SessionStore>,
) {
    let servers = discover_mcp_servers(&config.mcp_sources(workspace));
    let parsed_arguments = match arguments {
        Some(raw) => match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("invalid JSON arguments: {err}");
                if session.is_none() {
                    std::process::exit(1);
                }
                return;
            }
        },
        None => json!({}),
    };

    match call_mcp_tool(&servers, server_name, tool_name, parsed_arguments.clone()) {
        Ok(result) => {
            if let Some(store) = session {
                let _ = store.append(
                    "mcp_call",
                    json!({
                        "server": server_name,
                        "tool": tool_name,
                        "arguments": parsed_arguments,
                        "result": result,
                    }),
                );
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string())
            );
        }
        Err(err) => {
            if let Some(store) = session {
                let _ = store.append(
                    "mcp_error",
                    json!({ "server": server_name, "tool": tool_name, "error": err }),
                );
            }
            eprintln!("{err}");
            if session.is_none() {
                std::process::exit(1);
            }
        }
    }
}

fn split_mcp_call_command(input: &str) -> Option<(&str, &str, Option<&str>)> {
    let trimmed = input.trim();
    let (server, rest) = trimmed.split_once(' ')?;
    let (tool, args) = match rest.trim().split_once(' ') {
        Some((tool, args)) => (tool, Some(args.trim()).filter(|value| !value.is_empty())),
        None => (rest.trim(), None),
    };
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool, args))
}

fn run_prompt(
    workspace: &Path,
    config: &LoadedConfig,
    prompt: &str,
    override_model: Option<&str>,
    permission_mode: PermissionMode,
    approval_policy: &mut ApprovalPolicy,
    session: Option<&SessionStore>,
) {
    match run_agent_loop(
        config,
        workspace,
        prompt,
        override_model,
        permission_mode,
        session,
        |request| approval_for_request(request, approval_policy, session),
    ) {
        Ok(reply) => {
            println!("provider: {}", reply.provider.route.as_str());
            println!("model: {}", reply.provider.model);
            if reply.provider.text.starts_with("Not verified:") {
                println!("verification: not verified");
            }
            if !reply.tool_events.is_empty() {
                println!();
                for (index, event) in reply.tool_events.iter().enumerate() {
                    println!(
                        "tool[{index}] {} {}",
                        event.name,
                        serde_json::to_string(&event.arguments)
                            .unwrap_or_else(|_| "{}".to_string())
                    );
                    println!("{}", event.summary);
                }
            }
            println!();
            println!("{}", reply.provider.text);
        }
        Err(errors) => {
            let rendered = errors.join("\n");
            if let Some(store) = session {
                let _ = store.append("prompt_error", json!({ "errors": errors }));
                if let Ok(Some(handoff)) = pending_model_handoff(store.path()) {
                    let _ = store.append(
                        "model_probe_failed",
                        json!({
                            "model": handoff.snapshot.to_model,
                            "error": rendered,
                        }),
                    );
                }
            }
            eprintln!("{rendered}");
            if session.is_none() {
                std::process::exit(1);
            }
        }
    }
}

fn approval_for_request(
    request: &ApprovalRequest,
    approval_policy: &mut ApprovalPolicy,
    session: Option<&SessionStore>,
) -> Result<ApprovalOutcome, String> {
    let action = runtime::approval_action_for_policy(*approval_policy, request.risk);
    match action {
        ApprovalAction::AutoApprove => {
            if let Some(store) = session {
                let _ = store.append(
                    "approval_result",
                    json!({
                        "tool": request.tool,
                        "risk": request.risk.as_str(),
                        "decision": "auto-approve",
                        "reason": request.reason,
                    }),
                );
            }
            return Ok(ApprovalOutcome::Approve);
        }
        ApprovalAction::Deny => {
            let reason = format!(
                "blocked {}-risk tool `{}`: {}",
                request.risk, request.tool, request.reason
            );
            if let Some(store) = session {
                let _ = store.append(
                    "approval_result",
                    json!({
                        "tool": request.tool,
                        "risk": request.risk.as_str(),
                        "decision": "deny",
                        "reason": reason,
                    }),
                );
            }
            return Ok(ApprovalOutcome::Reject { reason });
        }
        ApprovalAction::Prompt => {}
    }

    if session.is_none() {
        let reason = format!(
            "approval required for {}-risk tool `{}` in non-interactive mode: {}; the harness rejected it and asked the model to continue without that tool",
            request.risk, request.tool, request.reason
        );
        return Ok(ApprovalOutcome::Reject { reason });
    }

    println!();
    println!("approval required");
    println!("tool: {}", request.tool);
    println!("risk: {}", request.risk);
    println!("why: {}", request.reason);
    println!(
        "arguments: {}",
        serde_json::to_string_pretty(&request.arguments).unwrap_or_else(|_| "{}".to_string())
    );
    print!("approve? [y]es / [n]o / [a]uto: ");
    let _ = io::stdout().flush();

    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|err| err.to_string())?;

    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => {
            if let Some(store) = session {
                let _ = store.append(
                    "approval_result",
                    json!({
                        "tool": request.tool,
                        "risk": request.risk.as_str(),
                        "decision": "approve",
                        "reason": request.reason,
                    }),
                );
            }
            Ok(ApprovalOutcome::Approve)
        }
        "a" | "auto" => {
            *approval_policy = ApprovalPolicy::Auto;
            if let Some(store) = session {
                let _ = store.append(
                    "approval_change",
                    json!({ "policy": approval_policy.as_str(), "via": "interactive" }),
                );
                let _ = store.append(
                    "approval_result",
                    json!({
                        "tool": request.tool,
                        "risk": request.risk.as_str(),
                        "decision": "approve",
                        "reason": request.reason,
                    }),
                );
            }
            Ok(ApprovalOutcome::Approve)
        }
        _ => {
            if let Some(store) = session {
                let _ = store.append(
                    "approval_result",
                    json!({
                        "tool": request.tool,
                        "risk": request.risk.as_str(),
                        "decision": "reject",
                        "reason": request.reason,
                    }),
                );
            }
            Ok(ApprovalOutcome::Reject {
                reason: format!(
                    "rejected by user for {}-risk tool `{}`",
                    request.risk, request.tool
                ),
            })
        }
    }
}

fn approval_policy_hint(policy: ApprovalPolicy) -> &'static str {
    match policy {
        ApprovalPolicy::Prompt => "low-risk auto, medium/high prompt, critical deny",
        ApprovalPolicy::Auto => "low/medium/high auto, critical deny",
    }
}

fn verification_policy_hint(policy: VerificationPolicy) -> &'static str {
    match policy {
        VerificationPolicy::Off => "no completion checks",
        VerificationPolicy::Annotate => "warn when completion is unverified",
        VerificationPolicy::Require => "reject unverified completion after code changes",
    }
}

fn connection_mode_hint(mode: ConnectionMode) -> &'static str {
    match mode {
        ConnectionMode::Api => "prefer BYOK/API routes",
        ConnectionMode::Auth => "prefer authenticated adapters when available",
        ConnectionMode::Auto => "prefer API, then fall back to supported auth adapters",
    }
}

#[allow(dead_code)]
fn print_approval_status(policy: ApprovalPolicy) {
    println!("approval: {policy}");
    println!("behavior: {}", approval_policy_hint(policy));
}

fn memory_kind_counts(records: &[MemoryRecord]) -> (usize, usize, usize, usize, usize) {
    let mut summaries = 0;
    let mut decisions = 0;
    let mut tasks = 0;
    let mut errors = 0;
    let mut notes = 0;

    for record in records {
        match record.kind.as_str() {
            "summary" => summaries += 1,
            "decision" => decisions += 1,
            "task" => tasks += 1,
            "error" => errors += 1,
            "note" => notes += 1,
            _ => {}
        }
    }

    (summaries, decisions, tasks, errors, notes)
}

fn parse_prompt_args(args: &[String]) -> (Option<String>, String) {
    if args.len() >= 3 && args.first().map(String::as_str) == Some("--model") {
        return (args.get(1).cloned(), args[2..].join(" "));
    }
    (None, args.join(" "))
}

fn print_help() {
    println!("{APP_NAME}");
    println!();
    println!("commands:");
    println!("  repl        start interactive shell");
    println!("  doctor      inspect local auth and binary availability");
    println!("  config      show resolved config");
    println!("  model       show or set default model config");
    println!("  memory      list/show/search/candidates/sessions/session/recall/save/delete/export/import/migrate portable memory");
    println!("  trajectory  list/show/search active trajectories");
    println!("  commands    list/show custom slash commands");
    println!("  resume      show latest session resume summary");
    println!("  handoff     show or debug the latest handoff block");
    println!("  why-context print the current prompt context");
    println!("  providers   list backend catalog");
    println!("  blueprint   print architecture summary");
    println!("  prompt      send text to the configured provider chain");
    println!("  skills      list/show/run skills");
    println!("  mcp         list/tools/call MCP servers");
    println!("  session     inspect session files");
    println!("  tool        run built-in tools without entering the repl");
    println!("  help        show this text");
}

fn print_prompt_help() {
    println!("{APP_NAME} prompt");
    println!();
    println!("usage:");
    println!("  {APP_NAME} prompt [--model <spec>] <text...>");
    println!();
    println!("examples:");
    println!("  {APP_NAME} prompt \"say hello\"");
    println!("  {APP_NAME} prompt --model openai/gpt-4.1-mini \"summarize this project\"");
    println!("  {APP_NAME} prompt --model profile/groq/openai/gpt-oss-20b \"read README.md and summarize\"");
}

#[cfg(test)]
mod tests {
    use super::{
        apply_slash_suggestion, build_slash_suggestions, build_update_notice,
        maybe_accept_selected_slash_suggestion, render_suggestion_lines, SlashSuggestion,
        TuiState,
    };

    #[test]
    fn slash_suggestions_show_full_catalog_for_bare_slash() {
        let workspace = std::env::temp_dir();
        let suggestions = build_slash_suggestions(&workspace, "/");
        assert!(suggestions.len() > 10);
        assert!(suggestions.iter().any(|item| item.name == "model"));
        assert!(suggestions.iter().any(|item| item.name == "help"));
    }

    #[test]
    fn apply_slash_suggestion_replaces_command_but_keeps_args() {
        let mut ui = TuiState::new();
        ui.input = "/mo extra context".to_string();
        apply_slash_suggestion(
            &mut ui,
            &SlashSuggestion {
                name: "model".to_string(),
                description: "Show model".to_string(),
            },
        );
        assert_eq!(ui.input, "/model extra context");
    }

    #[test]
    fn enter_on_partial_slash_accepts_selected_suggestion_first() {
        let workspace = std::env::temp_dir();
        let mut ui = TuiState::new();
        ui.input = "/mo".to_string();
        let suggestions = build_slash_suggestions(&workspace, &ui.input);
        ui.slash_selection = suggestions
            .iter()
            .position(|item| item.name == "model")
            .unwrap_or(0);
        ui.sync_slash_navigation(suggestions.len());
        assert!(maybe_accept_selected_slash_suggestion(&workspace, &mut ui));
        assert_eq!(ui.input, "/model");
    }

    #[test]
    fn render_suggestion_lines_marks_selected_row() {
        let lines = render_suggestion_lines(
            &[
                SlashSuggestion {
                    name: "help".to_string(),
                    description: "Show commands".to_string(),
                },
                SlashSuggestion {
                    name: "model".to_string(),
                    description: "Show model".to_string(),
                },
            ],
            80,
            1,
            0,
        );
        assert!(lines[1].starts_with("  /help"));
        assert!(lines[2].starts_with("> /model"));
    }

    #[test]
    fn history_navigation_walks_previous_inputs_and_restores_draft() {
        let mut ui = TuiState::new();
        ui.remember_input("first");
        ui.remember_input("second");
        ui.input = "par".to_string();

        ui.move_history_selection(-1);
        assert_eq!(ui.input, "second");

        ui.move_history_selection(-1);
        assert_eq!(ui.input, "first");

        ui.move_history_selection(1);
        assert_eq!(ui.input, "second");

        ui.move_history_selection(1);
        assert_eq!(ui.input, "par");
    }

    #[test]
    fn remember_input_avoids_duplicate_consecutive_entries() {
        let mut ui = TuiState::new();
        ui.remember_input("same");
        ui.remember_input("same");
        ui.remember_input("other");

        assert_eq!(
            ui.input_history,
            vec!["same".to_string(), "other".to_string()]
        );
    }

    #[test]
    fn build_update_notice_returns_none_when_not_behind() {
        assert_eq!(build_update_notice("main", "origin/main", 0), None);
    }

    #[test]
    fn build_update_notice_formats_message_when_behind() {
        let notice = build_update_notice("main", "origin/main", 3).unwrap();
        assert_eq!(
            notice,
            "update available: `main` is behind `origin/main` by 3 commit(s); run `git pull`"
        );
    }
}
