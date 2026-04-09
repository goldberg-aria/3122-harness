use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

const MAX_SKILL_SUMMARY_CHARS: usize = 250;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    pub path: PathBuf,
    pub source: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub url: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct McpConfigFile {
    #[serde(default)]
    servers: Vec<McpServerEntry>,
}

pub fn discover_skills(skill_roots: &[(PathBuf, String)]) -> Vec<SkillEntry> {
    let mut entries = Vec::new();
    for (root, source) in skill_roots {
        if !root.exists() {
            continue;
        }
        let Ok(read_dir) = fs::read_dir(root) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let skill_file = path.join("SKILL.md");
            if skill_file.is_file() {
                let summary = fs::read_to_string(&skill_file)
                    .ok()
                    .map(|markdown| summarize_skill_markdown(&markdown))
                    .unwrap_or_else(|| "no summary".to_string());
                entries.push(SkillEntry {
                    name: entry.file_name().to_string_lossy().to_string(),
                    path: skill_file,
                    source: source.clone(),
                    summary,
                });
            }
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

fn summarize_skill_markdown(markdown: &str) -> String {
    if let Some(description) = extract_frontmatter_description(markdown) {
        return truncate_summary(description);
    }

    let mut lines = Vec::new();
    let mut started = false;
    for line in markdown.lines().map(str::trim) {
        if line.is_empty() {
            if started {
                break;
            }
            continue;
        }
        if line.starts_with('#') && !started {
            continue;
        }
        started = true;
        lines.push(line);
    }

    let summary = if lines.is_empty() {
        "no summary".to_string()
    } else {
        lines.join(" ")
    };

    truncate_summary(summary)
}

fn extract_frontmatter_description(markdown: &str) -> Option<String> {
    let mut lines = markdown.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }

    let mut description = None;
    let mut in_multiline = false;
    let mut buffer = Vec::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if in_multiline {
            if line.starts_with(' ') || line.starts_with('\t') {
                buffer.push(trimmed.to_string());
                continue;
            }
            break;
        }
        if let Some(value) = trimmed.strip_prefix("description:") {
            let value = value.trim();
            if value == "|" || value == ">" {
                in_multiline = true;
                continue;
            }
            description = Some(value.trim_matches('"').trim_matches('\'').to_string());
            break;
        }
    }

    if !buffer.is_empty() {
        description = Some(buffer.join(" "));
    }

    description.filter(|value| !value.trim().is_empty())
}

fn truncate_summary(summary: String) -> String {
    if summary.chars().count() <= MAX_SKILL_SUMMARY_CHARS {
        summary
    } else {
        let truncated = summary
            .chars()
            .take(MAX_SKILL_SUMMARY_CHARS)
            .collect::<String>();
        format!("{truncated}...")
    }
}

pub fn discover_mcp_servers(config_files: &[(PathBuf, String)]) -> Vec<McpServerEntry> {
    let mut entries = Vec::new();
    for (path, source) in config_files {
        if !path.is_file() {
            continue;
        }
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(config) = serde_json::from_str::<McpConfigFile>(&contents) else {
            continue;
        };
        for mut server in config.servers {
            server.source = source.clone();
            entries.push(server);
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

fn default_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::discover_skills;

    fn temp_dir(prefix: &str) -> PathBuf {
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
    fn discovers_skill_summary_with_budget() {
        let root = temp_dir("discovery-skill-summary");
        let skill_dir = root.join("bootstrap");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "# Bootstrap\nThis skill sets up the first runtime slice and keeps the scope narrow for the initial pass.\n\nMore details here.",
        )
        .unwrap();

        let skills = discover_skills(&[(root.clone(), "workspace".to_string())]);
        assert_eq!(skills.len(), 1);
        assert!(skills[0]
            .summary
            .contains("sets up the first runtime slice"));
        assert!(skills[0].summary.chars().count() <= 253);

        cleanup(&root);
    }

    #[test]
    fn prefers_frontmatter_description_for_summary() {
        let root = temp_dir("discovery-frontmatter-summary");
        let skill_dir = root.join("bootstrap");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: bootstrap\ndescription: Use this skill to bootstrap a runtime quickly.\n---\n\n# Bootstrap\nLonger body",
        )
        .unwrap();

        let skills = discover_skills(&[(root.clone(), "workspace".to_string())]);
        assert_eq!(skills.len(), 1);
        assert_eq!(
            skills[0].summary,
            "Use this skill to bootstrap a runtime quickly."
        );

        cleanup(&root);
    }
}
