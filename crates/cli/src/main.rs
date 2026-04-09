use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use runtime::{
    backend_catalog, blueprint_summary, build_handoff_text, build_resume_text, call_mcp_tool,
    detect_provider_key, discover_mcp_servers, discover_skills, doctor_report, edit_file,
    exec_command, gather_workspace_context, glob_search, grep_search, list_mcp_tools,
    list_memory_records, load_config, load_provider_registry, parallel_read_only, provider_preset,
    provider_presets, read_file, remove_provider_profile, render_prompt_context, resolve_skill,
    run_agent_loop, save_config, save_session_memory_bundle, search_memory_records,
    upsert_provider_profile, write_file, ApprovalOutcome, ApprovalPolicy, ApprovalRequest,
    LoadedConfig, PermissionMode, SavedProviderProfile, SessionStore, ToolOutput,
};
use serde_json::json;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let workspace = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let config = match load_config(&workspace) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("failed to load config: {err}");
            std::process::exit(2);
        }
    };

    match args.first().map(String::as_str) {
        None | Some("repl") => run_repl(&workspace, &config),
        Some("doctor") => {
            print!("{}", doctor_report(&workspace, &config).render());
        }
        Some("config") => {
            println!("{}", config.render_summary(&workspace));
        }
        Some("providers") => {
            handle_providers_command(&workspace, &args[1..]);
        }
        Some("model") => {
            handle_model_command(&workspace, &config, &args[1..]);
        }
        Some("memory") => {
            handle_memory_command(&workspace, &config, &args[1..]);
        }
        Some("resume") => match build_resume_text(&workspace) {
            Ok(text) => print!("{text}"),
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        },
        Some("handoff") => match build_handoff_text(&workspace) {
            Ok(text) => print!("{text}"),
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        },
        Some("why-context") => {
            print!("{}", build_context_dump(&workspace, &config, None, None));
        }
        Some("blueprint") => {
            println!("{}", blueprint_summary());
        }
        Some("prompt") => {
            let (override_model, prompt) = parse_prompt_args(&args[1..]);
            if prompt.trim().is_empty() {
                eprintln!("usage: harness prompt [--model <spec>] <text...>");
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
            handle_skills_command(&workspace, &config, &args[1..]);
        }
        Some("mcp") => {
            handle_mcp_command(&workspace, &config, &args[1..]);
        }
        Some("session") => {
            handle_session_command(&workspace, &config, &args[1..]);
        }
        Some("tool") => {
            handle_tool_command(
                &workspace,
                &args[1..],
                config.default_permission_mode(),
                None,
            );
        }
        Some("help") | Some("--help") | Some("-h") => {
            print_help();
        }
        Some(other) => {
            eprintln!("unknown command: {other}");
            print_help();
            std::process::exit(2);
        }
    }
}

fn run_repl(workspace: &PathBuf, config: &LoadedConfig) {
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

    println!("Harness");
    println!("workspace: {}", workspace.display());
    println!(
        "config: {}",
        config
            .source
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "default (no config file found)".to_string())
    );
    println!("session: {}", session.path().display());
    println!("mode: {mode}");
    println!("approval: {approval_policy}");
    if let Some(model) = current_model.as_deref() {
        println!("model: {model}");
    }
    println!("type /help for commands");

    let stdin = io::stdin();
    loop {
        print!("harness> ");
        let _ = io::stdout().flush();

        let mut line = String::new();
        if stdin.read_line(&mut line).is_err() {
            eprintln!("failed to read input");
            autosave_session_memory(workspace, &session);
            return;
        }
        if line.is_empty() {
            autosave_session_memory(workspace, &session);
            return;
        }

        let trimmed = line.trim();
        let _ = session.append("user_input", json!({ "text": trimmed }));
        match trimmed {
            "" => continue,
            "/exit" | "/quit" => {
                autosave_session_memory(workspace, &session);
                return;
            }
            "/help" => {
                println!("/help       show commands");
                println!("/status     show current session state");
                println!("/model      show or set the active model");
                println!("/resume     show latest session resume summary");
                println!("/handoff    print a handoff block");
                println!("/why-context show the current prompt context");
                println!("/memory     list/search/save local memory");
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
            "/status" => print_repl_status(
                workspace,
                &session,
                current_model.as_deref(),
                mode,
                approval_policy,
            ),
            "/model" => print_model_status(workspace, config, current_model.as_deref()),
            "/resume" => match build_resume_text(workspace) {
                Ok(text) => println!("{text}"),
                Err(err) => eprintln!("{err}"),
            },
            "/handoff" => match build_handoff_text(workspace) {
                Ok(text) => println!("{text}"),
                Err(err) => eprintln!("{err}"),
            },
            "/why-context" => print!(
                "{}",
                build_context_dump(workspace, config, current_model.as_deref(), Some(mode))
            ),
            "/memory" => print_memory_list(workspace),
            "/login" => print_login_status(workspace),
            "/mode" => println!("{mode}"),
            "/approval" => println!("{approval_policy}"),
            "/doctor" => print!("{}", doctor_report(workspace, config).render()),
            "/config" => println!("{}", config.render_summary(workspace)),
            "/skills" => print_skills(workspace, config),
            "/mcp" => print_mcp(workspace, config),
            "/providers" => print_saved_providers(workspace),
            "/blueprint" => println!("{}", blueprint_summary()),
            "/session" => {
                println!("{}", session.path().display());
            }
            _ => {
                if let Some(next_mode) = trimmed
                    .strip_prefix("/mode ")
                    .and_then(PermissionMode::parse)
                {
                    mode = next_mode;
                    let _ = session.append("mode_change", json!({ "mode": mode.as_str() }));
                    println!("mode set to {mode}");
                    continue;
                }
                if let Some(model) = trimmed.strip_prefix("/model ").map(str::trim) {
                    if model.is_empty() {
                        eprintln!("usage: /model <provider/model | profile/alias/model>");
                        continue;
                    }
                    current_model = Some(model.to_string());
                    let _ = session.append("model_change", json!({ "model": model }));
                    println!("model set to {model}");
                    continue;
                }
                if trimmed == "/memory save" {
                    save_latest_session_memory(workspace, &session);
                    continue;
                }
                if let Some(query) = trimmed.strip_prefix("/memory search ").map(str::trim) {
                    print_memory_search(workspace, query);
                    continue;
                }
                if let Some(next_policy) = trimmed
                    .strip_prefix("/approval ")
                    .and_then(ApprovalPolicy::parse)
                {
                    approval_policy = next_policy;
                    let _ = session.append(
                        "approval_change",
                        json!({ "policy": approval_policy.as_str() }),
                    );
                    println!("approval set to {approval_policy}");
                    continue;
                }
                if let Some(body) = trimmed.strip_prefix("/skill ") {
                    let Some((name, task)) = split_skill_command(body) else {
                        eprintln!("usage: /skill <name> [task...]");
                        continue;
                    };
                    run_skill(workspace, config, name, task, Some(&session));
                    continue;
                }
                if let Some(body) = trimmed.strip_prefix("/mcp-tools ") {
                    let name = body.trim();
                    if name.is_empty() {
                        eprintln!("usage: /mcp-tools <server>");
                        continue;
                    }
                    print_mcp_tools(workspace, config, name);
                    continue;
                }
                if let Some(body) = trimmed.strip_prefix("/mcp-call ") {
                    match split_mcp_call_command(body) {
                        Some((server, tool, arguments)) => {
                            run_mcp_call(
                                workspace,
                                config,
                                server,
                                tool,
                                arguments,
                                Some(&session),
                            );
                        }
                        None => eprintln!("usage: /mcp-call <server> <tool> [json-args]"),
                    }
                    continue;
                }
                if trimmed.starts_with('/') {
                    handle_repl_tool_command(workspace, trimmed, mode, &session);
                } else {
                    let _ = session.append("prompt_start", json!({ "text": trimmed }));
                    run_prompt(
                        workspace,
                        config,
                        trimmed,
                        current_model.as_deref(),
                        mode,
                        &mut approval_policy,
                        Some(&session),
                    );
                }
            }
        }
    }
}

fn print_repl_status(
    workspace: &Path,
    session: &SessionStore,
    current_model: Option<&str>,
    mode: PermissionMode,
    approval_policy: ApprovalPolicy,
) {
    let provider_count = load_provider_registry(workspace)
        .map(|registry| registry.profiles.len())
        .unwrap_or(0);
    println!("workspace: {}", workspace.display());
    println!("session: {}", session.path().display());
    println!("model: {}", current_model.unwrap_or("-"));
    println!("mode: {mode}");
    println!("approval: {approval_policy}");
    println!("saved_providers: {provider_count}");
}

fn print_model_status(workspace: &Path, config: &LoadedConfig, current_model: Option<&str>) {
    println!("active: {}", current_model.unwrap_or("-"));
    println!("default: {}", config.primary_model().unwrap_or("-"));
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
    println!("- `harness providers presets`");
    println!("- `harness providers add <alias> --api-key <key>`");
    println!("- use `profile/<alias>/<model>` in `/model` or `prompt --model`");
    println!("External CLIs:");
    println!("- run `harness doctor` to inspect `claude` and `codex` availability");
}

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

fn print_memory_list(workspace: &Path) {
    match list_memory_records(workspace) {
        Ok(records) => {
            if records.is_empty() {
                println!("no memory records");
                return;
            }
            for record in records.into_iter().take(10) {
                println!("{} | {} | {}", record.kind, record.title, record.ts_ms);
            }
        }
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
            for record in records {
                println!("{} | {}", record.kind, record.title);
                println!("{}", record.body.trim());
                println!();
            }
        }
        Err(err) => eprintln!("{err}"),
    }
}

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
                eprintln!("usage: harness model set-primary <spec>");
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

fn handle_memory_command(workspace: &Path, _config: &LoadedConfig, args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("list") => print_memory_list(workspace),
        Some("search") => {
            let Some(query) = args.get(1) else {
                eprintln!("usage: harness memory search <query>");
                std::process::exit(2);
            };
            print_memory_search(workspace, query);
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
                        return;
                    }
                    println!("saved {} memory record(s)", bundle.saved_records.len());
                    for record in bundle.saved_records {
                        println!("{} | {}", record.kind, record.title);
                    }
                }
                Err(err) => {
                    eprintln!("{err}");
                    std::process::exit(1);
                }
            }
        }
        Some(other) => {
            eprintln!("unknown memory command: {other}");
            std::process::exit(2);
        }
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
        Some("detect-key") => {
            let Some(api_key) = args.get(1) else {
                eprintln!("usage: harness providers detect-key <api-key>");
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
                eprintln!("usage: harness providers remove <alias>");
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

fn add_provider_profile_command(workspace: &Path, args: &[String]) {
    let Some(alias) = args.first() else {
        eprintln!(
            "usage: harness providers add <alias> --api-key <key> [--preset <name>] [--base-url <url>] [--route <openai-compat|anthropic|ollama>]"
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

fn handle_skills_command(workspace: &Path, config: &LoadedConfig, args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("list") => print_skills(workspace, config),
        Some("show") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: harness skills show <name>");
                std::process::exit(2);
            };
            show_skill(workspace, config, name);
        }
        Some("run") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: harness skills run <name> [task...]");
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

fn handle_mcp_command(workspace: &Path, config: &LoadedConfig, args: &[String]) {
    match args.first().map(String::as_str) {
        None | Some("list") => print_mcp(workspace, config),
        Some("tools") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: harness mcp tools <server>");
                std::process::exit(2);
            };
            print_mcp_tools(workspace, config, name);
        }
        Some("call") => {
            let Some(server) = args.get(1) else {
                eprintln!("usage: harness mcp call <server> <tool> [json-args]");
                std::process::exit(2);
            };
            let Some(tool) = args.get(2) else {
                eprintln!("usage: harness mcp call <server> <tool> [json-args]");
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

fn print_skills(workspace: &Path, config: &LoadedConfig) {
    let skills = discover_skills(&config.skill_sources(workspace));
    if skills.is_empty() {
        println!("no skills found");
        return;
    }
    for skill in skills {
        println!(
            "{} | {} | {} | {}",
            skill.name,
            skill.source,
            skill.path.display(),
            skill.summary
        );
    }
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
    if *approval_policy == ApprovalPolicy::Auto {
        return Ok(ApprovalOutcome::Approve);
    }

    if session.is_none() {
        return Err(format!(
            "approval required for tool `{}` in non-interactive mode; use the REPL or set [approvals].policy = \"auto\"",
            request.tool
        ));
    }

    println!();
    println!("approval required");
    println!("tool: {}", request.tool);
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
                    json!({ "tool": request.tool, "decision": "approve" }),
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
                    json!({ "tool": request.tool, "decision": "approve" }),
                );
            }
            Ok(ApprovalOutcome::Approve)
        }
        _ => {
            if let Some(store) = session {
                let _ = store.append(
                    "approval_result",
                    json!({ "tool": request.tool, "decision": "reject" }),
                );
            }
            Ok(ApprovalOutcome::Reject {
                reason: format!("rejected by user for tool `{}`", request.tool),
            })
        }
    }
}

fn parse_prompt_args(args: &[String]) -> (Option<String>, String) {
    if args.len() >= 3 && args.first().map(String::as_str) == Some("--model") {
        return (args.get(1).cloned(), args[2..].join(" "));
    }
    (None, args.join(" "))
}

fn print_help() {
    println!("harness");
    println!();
    println!("commands:");
    println!("  repl        start interactive shell");
    println!("  doctor      inspect local auth and binary availability");
    println!("  config      show resolved config");
    println!("  model       show or set default model config");
    println!("  memory      list/search/save local memory");
    println!("  resume      show latest session resume summary");
    println!("  handoff     print a handoff block");
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
