use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommandKind {
    Alias,
    Macro,
    PromptTemplate,
}

impl SlashCommandKind {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "alias" => Some(Self::Alias),
            "macro" => Some(Self::Macro),
            "prompt-template" | "prompt_template" => Some(Self::PromptTemplate),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::Macro => "macro",
            Self::PromptTemplate => "prompt-template",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub kind: SlashCommandKind,
    pub source: String,
    pub path: PathBuf,
    pub target: Option<String>,
    pub steps: Vec<String>,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommandScope {
    Global,
    Workspace,
}

impl SlashCommandScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
        }
    }
}

#[derive(Debug, Deserialize)]
struct SlashCommandConfig {
    name: Option<String>,
    description: Option<String>,
    kind: String,
    target: Option<String>,
    steps: Option<Vec<String>>,
    prompt: Option<String>,
}

pub fn discover_slash_commands(workspace_root: &Path) -> Vec<SlashCommand> {
    let mut merged = BTreeMap::new();
    for command in built_in_commands() {
        merged.insert(command.name.clone(), command);
    }

    for (scope, dir) in command_sources(workspace_root) {
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }
            let Ok(command) = parse_slash_command(&path, &scope) else {
                continue;
            };
            merged.insert(command.name.clone(), command);
        }
    }

    merged.into_values().collect()
}

pub fn slash_command_dir(workspace_root: &Path, scope: SlashCommandScope) -> Result<PathBuf, String> {
    match scope {
        SlashCommandScope::Global => {
            let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
            Ok(PathBuf::from(home).join(".harness").join("commands"))
        }
        SlashCommandScope::Workspace => Ok(workspace_root.join(".harness").join("commands")),
    }
}

pub fn init_slash_command_dir(
    workspace_root: &Path,
    scope: SlashCommandScope,
) -> Result<PathBuf, String> {
    let dir = slash_command_dir(workspace_root, scope)?;
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir)
}

pub fn create_slash_command_template(
    workspace_root: &Path,
    scope: SlashCommandScope,
    name: &str,
    kind: SlashCommandKind,
) -> Result<PathBuf, String> {
    let normalized = normalize_slash_command_name(name)?;
    let dir = init_slash_command_dir(workspace_root, scope)?;
    let path = dir.join(format!("{normalized}.toml"));
    if path.exists() {
        return Err(format!("command already exists: {}", path.display()));
    }
    let contents = render_slash_command_template(&normalized, kind);
    fs::write(&path, contents).map_err(|err| err.to_string())?;
    Ok(path)
}

pub fn resolve_slash_command<'a>(
    commands: &'a [SlashCommand],
    name: &str,
) -> Option<&'a SlashCommand> {
    commands.iter().find(|command| command.name == name.trim())
}

pub fn expand_slash_command(command: &SlashCommand, args_raw: &str) -> Vec<String> {
    let args_raw = args_raw.trim();
    let args = args_raw.split_whitespace().collect::<Vec<_>>();
    match command.kind {
        SlashCommandKind::Alias => command
            .target
            .as_deref()
            .map(|target| {
                let mut expanded = apply_template(target, args_raw, &args);
                if !args_raw.is_empty() && !target.contains("{args}") {
                    expanded = format!("{expanded} {args_raw}");
                }
                expanded.trim().to_string()
            })
            .into_iter()
            .collect(),
        SlashCommandKind::Macro => command
            .steps
            .iter()
            .map(|step| apply_template(step, args_raw, &args).trim().to_string())
            .filter(|step| !step.is_empty())
            .collect(),
        SlashCommandKind::PromptTemplate => command
            .prompt
            .as_deref()
            .map(|prompt| apply_template(prompt, args_raw, &args).trim().to_string())
            .into_iter()
            .collect(),
    }
}

fn command_sources(workspace_root: &Path) -> Vec<(String, PathBuf)> {
    let mut sources = Vec::new();
    if let Ok(dir) = slash_command_dir(workspace_root, SlashCommandScope::Global) {
        sources.push(("global".to_string(), dir));
    }
    sources.push((
        "workspace".to_string(),
        slash_command_dir(workspace_root, SlashCommandScope::Workspace)
            .unwrap_or_else(|_| workspace_root.join(".harness").join("commands")),
    ));
    sources
}

fn built_in_commands() -> Vec<SlashCommand> {
    vec![
        built_in_alias("ctx", "Show the current prompt context", "/why-context"),
        built_in_alias("mem", "List local memory records", "/memory"),
        built_in_alias("sum", "Show the latest resume summary", "/resume"),
        built_in_alias("hf", "Show the latest handoff block", "/handoff"),
        built_in_alias("models", "Show the active and saved model state", "/model"),
        built_in_alias("check", "Run doctor checks", "/doctor"),
        built_in_alias("notes", "List saved local memory", "/memory"),
        built_in_macro(
            "checkpoint",
            "Save memory, show resume, and print the current handoff",
            &["/memory save", "/resume", "/handoff"],
        ),
    ]
}

fn built_in_alias(name: &str, description: &str, target: &str) -> SlashCommand {
    SlashCommand {
        name: name.to_string(),
        description: description.to_string(),
        kind: SlashCommandKind::Alias,
        source: "builtin".to_string(),
        path: PathBuf::from(format!("<builtin:{name}>")),
        target: Some(target.to_string()),
        steps: Vec::new(),
        prompt: None,
    }
}

fn built_in_macro(name: &str, description: &str, steps: &[&str]) -> SlashCommand {
    SlashCommand {
        name: name.to_string(),
        description: description.to_string(),
        kind: SlashCommandKind::Macro,
        source: "builtin".to_string(),
        path: PathBuf::from(format!("<builtin:{name}>")),
        target: None,
        steps: steps.iter().map(|step| (*step).to_string()).collect(),
        prompt: None,
    }
}

fn parse_slash_command(path: &Path, scope: &str) -> Result<SlashCommand, String> {
    let contents = fs::read_to_string(path).map_err(|err| err.to_string())?;
    let config = toml::from_str::<SlashCommandConfig>(&contents).map_err(|err| err.to_string())?;
    let kind = SlashCommandKind::parse(&config.kind)
        .ok_or_else(|| format!("unknown slash command kind: {}", config.kind))?;
    let name = config
        .name
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| format!("missing slash command name: {}", path.display()))?;

    match kind {
        SlashCommandKind::Alias if config.target.is_none() => {
            return Err(format!("alias command missing target: {}", path.display()));
        }
        SlashCommandKind::Macro if config.steps.as_ref().is_none_or(Vec::is_empty) => {
            return Err(format!("macro command missing steps: {}", path.display()));
        }
        SlashCommandKind::PromptTemplate if config.prompt.is_none() => {
            return Err(format!(
                "prompt-template command missing prompt: {}",
                path.display()
            ));
        }
        _ => {}
    }

    Ok(SlashCommand {
        name,
        description: config.description.unwrap_or_default(),
        kind,
        source: scope.to_string(),
        path: path.to_path_buf(),
        target: config.target,
        steps: config.steps.unwrap_or_default(),
        prompt: config.prompt,
    })
}

fn apply_template(template: &str, args_raw: &str, args: &[&str]) -> String {
    let mut output = template.replace("{args}", args_raw);
    for (index, value) in args.iter().enumerate() {
        output = output.replace(&format!("{{arg{}}}", index + 1), value);
    }
    output
}

fn normalize_slash_command_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim().trim_start_matches('/').to_ascii_lowercase();
    if trimmed.is_empty() {
        return Err("command name is empty".to_string());
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        return Err(format!(
            "invalid command name: {name} (use lowercase letters, digits, '-' or '_')"
        ));
    }
    Ok(trimmed)
}

fn render_slash_command_template(name: &str, kind: SlashCommandKind) -> String {
    match kind {
        SlashCommandKind::Alias => format!(
            "name = \"{name}\"\ndescription = \"Describe this shortcut\"\nkind = \"alias\"\ntarget = \"/why-context\"\n"
        ),
        SlashCommandKind::Macro => format!(
            "name = \"{name}\"\ndescription = \"Describe this workflow\"\nkind = \"macro\"\nsteps = [\"/memory save\", \"/resume\"]\n"
        ),
        SlashCommandKind::PromptTemplate => format!(
            "name = \"{name}\"\ndescription = \"Describe this prompt template\"\nkind = \"prompt-template\"\nprompt = \"Review {{arg1}} in 3 concise bullets. Extra: {{args}}\"\n"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        create_slash_command_template, discover_slash_commands, expand_slash_command,
        resolve_slash_command, SlashCommandKind, SlashCommandScope,
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
    fn discovers_workspace_commands_and_expands_templates() {
        let workspace = temp_workspace("slash-commands");
        let dir = workspace.join(".harness").join("commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("ctx.toml"),
            r#"
description = "Show prompt context"
kind = "alias"
target = "/why-context"
"#,
        )
        .unwrap();
        fs::write(
            dir.join("checkpoint.toml"),
            r#"
description = "Save memory and print handoff"
kind = "macro"
steps = ["/memory save", "/handoff"]
"#,
        )
        .unwrap();
        fs::write(
            dir.join("review-risk.toml"),
            r#"
description = "Ask the model for a focused risk review"
kind = "prompt-template"
prompt = "Review the risk of {arg1} in 3 bullets. Extra: {args}"
"#,
        )
        .unwrap();

        let commands = discover_slash_commands(&workspace);
        assert!(commands.len() >= 3);

        let ctx = resolve_slash_command(&commands, "ctx").unwrap();
        assert_eq!(expand_slash_command(ctx, ""), vec!["/why-context"]);

        let checkpoint = resolve_slash_command(&commands, "checkpoint").unwrap();
        assert_eq!(
            expand_slash_command(checkpoint, ""),
            vec!["/memory save", "/handoff"]
        );

        let review = resolve_slash_command(&commands, "review-risk").unwrap();
        assert_eq!(
            expand_slash_command(review, "src/main.rs urgent"),
            vec!["Review the risk of src/main.rs in 3 bullets. Extra: src/main.rs urgent"]
        );

        cleanup(&workspace);
    }

    #[test]
    fn creates_new_workspace_command_template() {
        let workspace = temp_workspace("slash-commands-new");
        let path = create_slash_command_template(
            &workspace,
            SlashCommandScope::Workspace,
            "ctx",
            SlashCommandKind::Alias,
        )
        .unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("kind = \"alias\""));
        assert!(contents.contains("target = \"/why-context\""));
        cleanup(&workspace);
    }

    #[test]
    fn builtin_commands_are_available_and_workspace_can_override() {
        let workspace = temp_workspace("slash-commands-override");
        let commands = discover_slash_commands(&workspace);
        let builtin = resolve_slash_command(&commands, "ctx").unwrap();
        assert_eq!(builtin.source, "builtin");
        assert_eq!(expand_slash_command(builtin, ""), vec!["/why-context"]);

        let dir = workspace.join(".harness").join("commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("ctx.toml"),
            r#"
description = "Workspace override"
kind = "alias"
target = "/memory recall 3"
"#,
        )
        .unwrap();

        let commands = discover_slash_commands(&workspace);
        let overridden = resolve_slash_command(&commands, "ctx").unwrap();
        assert_eq!(overridden.source, "workspace");
        assert_eq!(expand_slash_command(overridden, ""), vec!["/memory recall 3"]);
        cleanup(&workspace);
    }
}
