use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::SessionEvent;
use crate::SessionStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryKind {
    Summary,
    Decision,
    Task,
    Error,
    Note,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Decision => "decision",
            Self::Task => "task",
            Self::Error => "error",
            Self::Note => "note",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::Summary => "summaries.jsonl",
            Self::Decision => "decisions.jsonl",
            Self::Task => "tasks.jsonl",
            Self::Error => "errors.jsonl",
            Self::Note => "notes.jsonl",
        }
    }

    fn all() -> [Self; 5] {
        [
            Self::Summary,
            Self::Decision,
            Self::Task,
            Self::Error,
            Self::Note,
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub ts_ms: u128,
    pub kind: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub session_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionDigest {
    pub title: String,
    pub summary: String,
    pub goals: Vec<String>,
    pub tools: Vec<String>,
    pub errors: Vec<String>,
    pub last_assistant_text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SavedMemoryBundle {
    pub saved_records: Vec<MemoryRecord>,
}

pub fn memory_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".harness").join("memory")
}

pub fn append_memory_record(
    workspace_root: &Path,
    kind: MemoryKind,
    title: &str,
    body: &str,
    tags: &[String],
    session_path: Option<&Path>,
) -> Result<PathBuf, String> {
    let dir = memory_dir(workspace_root);
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let path = dir.join(kind.file_name());
    let record = MemoryRecord {
        ts_ms: now_ms(),
        kind: kind.as_str().to_string(),
        title: title.to_string(),
        body: body.to_string(),
        tags: tags.to_vec(),
        session_path: session_path.map(|path| path.display().to_string()),
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| err.to_string())?;
    let line = serde_json::to_string(&record).map_err(|err| err.to_string())?;
    writeln!(file, "{line}").map_err(|err| err.to_string())?;
    Ok(path)
}

pub fn list_memory_records(workspace_root: &Path) -> Result<Vec<MemoryRecord>, String> {
    let dir = memory_dir(workspace_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut records = Vec::new();
    for kind in MemoryKind::all() {
        let path = dir.join(kind.file_name());
        if !path.is_file() {
            continue;
        }
        let contents = fs::read_to_string(&path).map_err(|err| err.to_string())?;
        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            records
                .push(serde_json::from_str::<MemoryRecord>(line).map_err(|err| err.to_string())?);
        }
    }

    records.sort_by(|left, right| right.ts_ms.cmp(&left.ts_ms));
    Ok(records)
}

pub fn search_memory_records(
    workspace_root: &Path,
    query: &str,
) -> Result<Vec<MemoryRecord>, String> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    Ok(list_memory_records(workspace_root)?
        .into_iter()
        .filter(|record| {
            record.title.to_ascii_lowercase().contains(&query)
                || record.body.to_ascii_lowercase().contains(&query)
                || record
                    .tags
                    .iter()
                    .any(|tag| tag.to_ascii_lowercase().contains(&query))
        })
        .collect())
}

pub fn save_session_summary(
    workspace_root: &Path,
    session_path: &Path,
) -> Result<(PathBuf, MemoryRecord), String> {
    let events = SessionStore::read_events(session_path)?;
    let digest = summarize_session_events(&events);
    let tags = digest.tools.clone();
    let path = append_memory_record(
        workspace_root,
        MemoryKind::Summary,
        &digest.title,
        &digest.summary,
        &tags,
        Some(session_path),
    )?;
    let record = list_memory_records(workspace_root)?
        .into_iter()
        .find(|record| {
            record.kind == MemoryKind::Summary.as_str()
                && record.session_path.as_deref() == Some(&session_path.display().to_string())
        })
        .ok_or_else(|| "failed to reload saved session summary".to_string())?;
    Ok((path, record))
}

pub fn save_session_memory_bundle(
    workspace_root: &Path,
    session_path: &Path,
) -> Result<SavedMemoryBundle, String> {
    let events = SessionStore::read_events(session_path)?;
    let digest = summarize_session_events(&events);
    let existing = list_memory_records(workspace_root)?;
    let session_path_rendered = session_path.display().to_string();
    let mut saved_records = Vec::new();

    if !record_exists(
        &existing,
        MemoryKind::Summary,
        &digest.title,
        &session_path_rendered,
    ) {
        let (_, record) = save_session_summary(workspace_root, session_path)?;
        saved_records.push(record);
    }

    for goal in &digest.goals {
        if record_exists(&existing, MemoryKind::Task, goal, &session_path_rendered) {
            continue;
        }
        append_memory_record(
            workspace_root,
            MemoryKind::Task,
            goal,
            goal,
            &digest.tools,
            Some(session_path),
        )?;
        saved_records.push(MemoryRecord {
            ts_ms: now_ms(),
            kind: MemoryKind::Task.as_str().to_string(),
            title: goal.clone(),
            body: goal.clone(),
            tags: digest.tools.clone(),
            session_path: Some(session_path_rendered.clone()),
        });
    }

    for error in &digest.errors {
        if record_exists(&existing, MemoryKind::Error, error, &session_path_rendered) {
            continue;
        }
        append_memory_record(
            workspace_root,
            MemoryKind::Error,
            error,
            error,
            &digest.tools,
            Some(session_path),
        )?;
        saved_records.push(MemoryRecord {
            ts_ms: now_ms(),
            kind: MemoryKind::Error.as_str().to_string(),
            title: error.clone(),
            body: error.clone(),
            tags: digest.tools.clone(),
            session_path: Some(session_path_rendered.clone()),
        });
    }

    Ok(SavedMemoryBundle { saved_records })
}

pub fn build_resume_text(workspace_root: &Path) -> Result<String, String> {
    let latest_session = SessionStore::latest(workspace_root)?;
    let memories = list_memory_records(workspace_root)?;
    let latest_summary = memories
        .iter()
        .find(|record| record.kind == MemoryKind::Summary.as_str());
    let latest_tasks = memories
        .iter()
        .filter(|record| record.kind == MemoryKind::Task.as_str())
        .take(3)
        .collect::<Vec<_>>();
    let latest_errors = memories
        .iter()
        .filter(|record| record.kind == MemoryKind::Error.as_str())
        .take(3)
        .collect::<Vec<_>>();

    let mut out = String::new();
    out.push_str("Resume\n");
    out.push_str(&format!(
        "latest_session: {}\n",
        latest_session
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_string())
    ));
    if let Some(summary) = latest_summary {
        out.push_str(&format!("latest_summary: {}\n", summary.title));
        out.push_str(&summary.body);
        out.push('\n');
    } else {
        out.push_str("latest_summary: none\n");
    }
    if !latest_tasks.is_empty() {
        out.push_str("recent_tasks:\n");
        for record in latest_tasks {
            out.push_str("- ");
            out.push_str(&record.title);
            out.push('\n');
        }
    }
    if !latest_errors.is_empty() {
        out.push_str("recent_errors:\n");
        for record in latest_errors {
            out.push_str("- ");
            out.push_str(&record.title);
            out.push('\n');
        }
    }
    Ok(out)
}

pub fn build_handoff_text(workspace_root: &Path) -> Result<String, String> {
    let latest_session = SessionStore::latest(workspace_root)?;
    let summary_record = list_memory_records(workspace_root)?
        .into_iter()
        .find(|record| record.kind == MemoryKind::Summary.as_str());

    let mut out = String::new();
    out.push_str("Handoff\n\n");
    if let Some(path) = latest_session {
        out.push_str(&format!("Latest session: {}\n\n", path.display()));
    }
    if let Some(summary) = summary_record {
        out.push_str("Session summary:\n");
        out.push_str(&summary.body);
        out.push_str("\n\n");
    } else {
        out.push_str("Session summary: none\n\n");
    }
    let recent_tasks = list_memory_records(workspace_root)?
        .into_iter()
        .filter(|record| record.kind == MemoryKind::Task.as_str())
        .take(3)
        .collect::<Vec<_>>();
    if !recent_tasks.is_empty() {
        out.push_str("Recent tasks:\n");
        for record in recent_tasks {
            out.push_str("- ");
            out.push_str(&record.title);
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str("Continue from this context. Preserve the current direction, reuse saved memory, and avoid restarting discovery from scratch.\n");
    Ok(out)
}

pub fn build_memory_recall_text(workspace_root: &Path, limit: usize) -> Result<String, String> {
    let records = list_memory_records(workspace_root)?;
    if records.is_empty() {
        return Ok("none".to_string());
    }

    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for record in records {
        let key = format!("{}|{}|{}", record.kind, record.title, record.body);
        if seen.insert(key) {
            deduped.push(record);
        }
        if deduped.len() >= limit {
            break;
        }
    }

    let mut out = String::new();
    for record in deduped {
        out.push_str(&format!("[{}] {}\n", record.kind, record.title));
        out.push_str(&truncate_text(&record.body, 240));
        out.push_str("\n\n");
    }
    Ok(out.trim().to_string())
}

pub fn summarize_session_events(events: &[SessionEvent]) -> SessionDigest {
    let mut goals = Vec::new();
    let mut tools = Vec::new();
    let mut errors = Vec::new();
    let mut last_assistant_text = None;

    for event in events {
        match event.kind.as_str() {
            "user_input" | "prompt_start" => {
                if let Some(text) = event.payload.get("text").and_then(|value| value.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with('/') {
                        push_unique(&mut goals, trimmed.to_string(), 3);
                    }
                }
            }
            "agent_tool" => {
                if let Some(name) = event.payload.get("name").and_then(|value| value.as_str()) {
                    push_unique(&mut tools, name.to_string(), 8);
                }
            }
            "tool_error" | "prompt_error" | "mcp_error" => {
                if let Some(text) = event.payload.get("error").and_then(|value| value.as_str()) {
                    push_unique(&mut errors, truncate_text(text, 160), 5);
                } else if let Some(errors_array) = event
                    .payload
                    .get("errors")
                    .and_then(|value| value.as_array())
                {
                    for value in errors_array {
                        if let Some(text) = value.as_str() {
                            push_unique(&mut errors, truncate_text(text, 160), 5);
                        }
                    }
                }
            }
            "agent_result" | "prompt_result" => {
                if let Some(text) = event.payload.get("text").and_then(|value| value.as_str()) {
                    last_assistant_text = Some(truncate_text(text, 240));
                }
            }
            _ => {}
        }
    }

    let title = goals
        .first()
        .map(|goal| truncate_text(goal, 80))
        .unwrap_or_else(|| "Session summary".to_string());

    let mut summary = String::new();
    if !goals.is_empty() {
        summary.push_str("Goals:\n");
        for goal in &goals {
            summary.push_str("- ");
            summary.push_str(goal);
            summary.push('\n');
        }
    }
    if !tools.is_empty() {
        summary.push_str("Tools used:\n");
        for tool in &tools {
            summary.push_str("- ");
            summary.push_str(tool);
            summary.push('\n');
        }
    }
    if !errors.is_empty() {
        summary.push_str("Errors:\n");
        for error in &errors {
            summary.push_str("- ");
            summary.push_str(error);
            summary.push('\n');
        }
    }
    if let Some(text) = &last_assistant_text {
        summary.push_str("Last assistant output:\n");
        summary.push_str(text);
        summary.push('\n');
    }
    if summary.trim().is_empty() {
        summary.push_str("No significant events captured.\n");
    }

    SessionDigest {
        title,
        summary,
        goals,
        tools,
        errors,
        last_assistant_text,
    }
}

fn push_unique(items: &mut Vec<String>, value: String, max_len: usize) {
    if items.iter().any(|existing| existing == &value) {
        return;
    }
    if items.len() < max_len {
        items.push(value);
    }
}

fn truncate_text(input: &str, max_chars: usize) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let shortened = trimmed.chars().take(max_chars).collect::<String>();
    format!("{shortened}...")
}

fn record_exists(
    existing: &[MemoryRecord],
    kind: MemoryKind,
    title: &str,
    session_path: &str,
) -> bool {
    existing.iter().any(|record| {
        record.kind == kind.as_str()
            && record.title == title
            && record.session_path.as_deref() == Some(session_path)
    })
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use crate::session::SessionEvent;

    use super::{
        append_memory_record, build_handoff_text, build_memory_recall_text, build_resume_text,
        list_memory_records, save_session_memory_bundle, save_session_summary,
        search_memory_records, summarize_session_events, MemoryKind,
    };

    fn temp_workspace(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{unique}"));
        fs::create_dir_all(path.join(".harness/sessions")).unwrap();
        path
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn appends_and_searches_memory_records() {
        let workspace = temp_workspace("memory-append");
        append_memory_record(
            &workspace,
            MemoryKind::Note,
            "Provider setup",
            "Use OpenRouter for broad coverage.",
            &["openrouter".to_string()],
            None,
        )
        .unwrap();

        let results = search_memory_records(&workspace, "openrouter").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Provider setup");

        cleanup(&workspace);
    }

    #[test]
    fn summarizes_session_events_and_saves_summary() {
        let workspace = temp_workspace("memory-summary");
        let session_path = workspace.join(".harness/sessions/session-test.jsonl");
        fs::write(
            &session_path,
            [
                serde_json::to_string(&SessionEvent {
                    ts_ms: 1,
                    kind: "user_input".to_string(),
                    payload: json!({ "text": "implement local memory" }),
                })
                .unwrap(),
                serde_json::to_string(&SessionEvent {
                    ts_ms: 2,
                    kind: "agent_tool".to_string(),
                    payload: json!({ "name": "read" }),
                })
                .unwrap(),
                serde_json::to_string(&SessionEvent {
                    ts_ms: 3,
                    kind: "agent_result".to_string(),
                    payload: json!({ "text": "memory skeleton added" }),
                })
                .unwrap(),
            ]
            .join("\n"),
        )
        .unwrap();

        let (_, record) = save_session_summary(&workspace, &session_path).unwrap();
        assert_eq!(record.kind, "summary");
        assert!(record.body.contains("implement local memory"));
        assert!(record.body.contains("memory skeleton added"));

        let records = list_memory_records(&workspace).unwrap();
        assert_eq!(records.len(), 1);

        cleanup(&workspace);
    }

    #[test]
    fn builds_resume_and_handoff_text() {
        let workspace = temp_workspace("memory-handoff");
        let session_path = workspace.join(".harness/sessions/session-test.jsonl");
        fs::write(
            &session_path,
            serde_json::to_string(&SessionEvent {
                ts_ms: 1,
                kind: "user_input".to_string(),
                payload: json!({ "text": "continue provider integration" }),
            })
            .unwrap(),
        )
        .unwrap();
        let _ = save_session_summary(&workspace, &session_path).unwrap();

        let resume = build_resume_text(&workspace).unwrap();
        let handoff = build_handoff_text(&workspace).unwrap();

        assert!(resume.contains("latest_session:"));
        assert!(handoff.contains("Handoff"));
        assert!(handoff.contains("Continue from this context"));

        cleanup(&workspace);
    }

    #[test]
    fn saves_memory_bundle_and_builds_recall_text() {
        let workspace = temp_workspace("memory-bundle");
        let session_path = workspace.join(".harness/sessions/session-test.jsonl");
        fs::write(
            &session_path,
            [
                serde_json::to_string(&SessionEvent {
                    ts_ms: 1,
                    kind: "user_input".to_string(),
                    payload: json!({ "text": "wire local memory recall" }),
                })
                .unwrap(),
                serde_json::to_string(&SessionEvent {
                    ts_ms: 2,
                    kind: "tool_error".to_string(),
                    payload: json!({ "error": "missing memory file" }),
                })
                .unwrap(),
            ]
            .join("\n"),
        )
        .unwrap();

        let bundle = save_session_memory_bundle(&workspace, &session_path).unwrap();
        assert!(!bundle.saved_records.is_empty());

        let recall = build_memory_recall_text(&workspace, 5).unwrap();
        assert!(recall.contains("wire local memory recall"));
        assert!(recall.contains("missing memory file"));

        cleanup(&workspace);
    }

    #[test]
    fn dedupes_recall_entries() {
        let workspace = temp_workspace("memory-dedupe");
        append_memory_record(
            &workspace,
            MemoryKind::Summary,
            "Same title",
            "Same body",
            &[],
            None,
        )
        .unwrap();
        append_memory_record(
            &workspace,
            MemoryKind::Summary,
            "Same title",
            "Same body",
            &[],
            None,
        )
        .unwrap();

        let recall = build_memory_recall_text(&workspace, 5).unwrap();
        assert_eq!(recall.matches("Same title").count(), 1);

        cleanup(&workspace);
    }

    #[test]
    fn extracts_digest_from_events() {
        let digest = summarize_session_events(&[
            SessionEvent {
                ts_ms: 1,
                kind: "user_input".to_string(),
                payload: json!({ "text": "build memory" }),
            },
            SessionEvent {
                ts_ms: 2,
                kind: "agent_tool".to_string(),
                payload: json!({ "name": "write" }),
            },
            SessionEvent {
                ts_ms: 3,
                kind: "tool_error".to_string(),
                payload: json!({ "error": "permission denied" }),
            },
        ]);

        assert_eq!(digest.goals[0], "build memory");
        assert_eq!(digest.tools[0], "write");
        assert_eq!(digest.errors[0], "permission denied");
    }
}
