use std::fs;
use std::path::PathBuf;

use serde::Serialize;

use crate::SkillEntry;

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedSkill {
    pub name: String,
    pub path: PathBuf,
    pub source: String,
    pub summary: String,
    pub markdown: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillPacket {
    pub skill: ResolvedSkill,
    pub task: Option<String>,
    pub prompt: String,
}

pub fn resolve_skill(entries: &[SkillEntry], name: &str) -> Result<ResolvedSkill, String> {
    let normalized = name.trim();
    let candidate = entries
        .iter()
        .filter(|entry| entry.name == normalized)
        .max_by_key(|entry| source_rank(&entry.source))
        .ok_or_else(|| format!("skill not found: {normalized}"))?;

    let markdown = fs::read_to_string(&candidate.path).map_err(|err| err.to_string())?;
    Ok(ResolvedSkill {
        name: candidate.name.clone(),
        path: candidate.path.clone(),
        source: candidate.source.clone(),
        summary: candidate.summary.clone(),
        markdown,
    })
}

pub fn build_skill_packet(skill: ResolvedSkill, task: Option<&str>) -> SkillPacket {
    let task = task
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let prompt = compose_prompt(&skill, task.as_deref());
    SkillPacket {
        skill,
        task,
        prompt,
    }
}

fn compose_prompt(skill: &ResolvedSkill, task: Option<&str>) -> String {
    let mut prompt = String::new();
    prompt.push_str("You must follow this skill.\n\n");
    prompt.push_str("Skill name: ");
    prompt.push_str(&skill.name);
    prompt.push_str("\nSource: ");
    prompt.push_str(&skill.source);
    prompt.push_str("\nPath: ");
    prompt.push_str(&skill.path.display().to_string());
    prompt.push_str("\nSummary: ");
    prompt.push_str(&skill.summary);
    prompt.push_str("\n\nSkill contents:\n");
    prompt.push_str(&skill.markdown);

    if let Some(task) = task {
        prompt.push_str("\n\nUser task:\n");
        prompt.push_str(task);
    }

    prompt
}

fn source_rank(source: &str) -> u8 {
    match source {
        "workspace" => 2,
        "user" => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::SkillEntry;

    use super::{build_skill_packet, resolve_skill};

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
    fn resolve_skill_prefers_workspace_source() {
        let workspace = temp_dir("skill-workspace");
        let user = temp_dir("skill-user");

        let workspace_skill = workspace.join("SKILL.md");
        let user_skill = user.join("SKILL.md");
        fs::write(&workspace_skill, "# Workspace\npreferred").unwrap();
        fs::write(&user_skill, "# User\nfallback").unwrap();

        let entries = vec![
            SkillEntry {
                name: "bootstrap".to_string(),
                path: user_skill,
                source: "user".to_string(),
                summary: "fallback summary".to_string(),
            },
            SkillEntry {
                name: "bootstrap".to_string(),
                path: workspace_skill,
                source: "workspace".to_string(),
                summary: "workspace summary".to_string(),
            },
        ];

        let resolved = resolve_skill(&entries, "bootstrap").unwrap();
        assert_eq!(resolved.source, "workspace");
        assert!(resolved.markdown.contains("preferred"));

        cleanup(&workspace);
        cleanup(&user);
    }

    #[test]
    fn build_skill_packet_includes_task_and_contents() {
        let skill = super::ResolvedSkill {
            name: "bootstrap".to_string(),
            path: PathBuf::from("/tmp/bootstrap/SKILL.md"),
            source: "workspace".to_string(),
            summary: "Builds the first runtime slice.".to_string(),
            markdown: "# Skill\nDo the thing".to_string(),
        };

        let packet = build_skill_packet(skill, Some("wire the provider"));
        assert_eq!(packet.task.as_deref(), Some("wire the provider"));
        assert!(packet.prompt.contains("Skill name: bootstrap"));
        assert!(packet
            .prompt
            .contains("Summary: Builds the first runtime slice."));
        assert!(packet.prompt.contains("Do the thing"));
        assert!(packet.prompt.contains("wire the provider"));
    }
}
