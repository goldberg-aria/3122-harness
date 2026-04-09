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
    if let Ok(home) = std::env::var("HOME") {
        sources.push((
            "global".to_string(),
            PathBuf::from(home).join(".harness").join("commands"),
        ));
    }
    sources.push((
        "workspace".to_string(),
        workspace_root.join(".harness").join("commands"),
    ));
    sources
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{discover_slash_commands, expand_slash_command, resolve_slash_command};

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
        assert_eq!(commands.len(), 3);

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
}
