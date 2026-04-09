use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session::SessionEvent;
use crate::SessionStore;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrajectoryRecord {
    pub id: i64,
    pub session_path: String,
    pub session_id: Option<String>,
    pub created_ts_ms: u128,
    pub updated_ts_ms: u128,
    pub title: String,
    pub current_goal: String,
    pub recent_work_summary: String,
    pub active_model: Option<String>,
    pub previous_model: Option<String>,
    pub latest_attempt: Option<String>,
    pub latest_failure: Option<String>,
    pub last_verification: Option<String>,
    pub next_step: String,
    pub active_files: Vec<String>,
    pub open_tasks: Vec<String>,
    pub recent_errors: Vec<String>,
    pub verification_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrajectoryStep {
    pub ts_ms: u128,
    pub kind: String,
    pub summary: String,
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillCandidate {
    pub id: i64,
    pub command_name: String,
    pub description: String,
    pub prompt: String,
    pub tool_sequence: Vec<String>,
    pub occurrence_count: u32,
    pub first_seen_ts_ms: u128,
    pub last_seen_ts_ms: u128,
}

struct DerivedTrajectory {
    record: TrajectoryRecord,
    steps: Vec<TrajectoryStep>,
    tool_sequence: Vec<String>,
}

pub fn memory_db_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".harness").join("memory.db")
}

pub fn record_session_trajectory(
    workspace_root: &Path,
    session_path: &Path,
) -> Result<TrajectoryRecord, String> {
    let events = SessionStore::read_events(session_path)?;
    let derived = derive_trajectory(workspace_root, session_path, &events);
    let connection = open_memory_db(workspace_root)?;
    let trajectory_id = upsert_trajectory(&connection, &derived)?;
    if !derived.tool_sequence.is_empty() {
        upsert_skill_candidate(&connection, session_path, &derived.tool_sequence, trajectory_id)?;
    }
    Ok(active_trajectory(workspace_root)?.unwrap_or(derived.record))
}

pub fn active_trajectory(workspace_root: &Path) -> Result<Option<TrajectoryRecord>, String> {
    let connection = open_memory_db(workspace_root)?;
    connection
        .query_row(
            "SELECT id, session_path, session_id, created_ts_ms, updated_ts_ms, title, current_goal,
                    recent_work_summary, active_model, previous_model, latest_attempt, latest_failure,
                    last_verification, next_step, active_files_json, open_tasks_json, recent_errors_json,
                    verification_hints_json
             FROM trajectories
             ORDER BY updated_ts_ms DESC
             LIMIT 1",
            [],
            row_to_trajectory,
        )
        .optional()
        .map_err(|err| err.to_string())
}

pub fn list_recent_trajectories(
    workspace_root: &Path,
    limit: usize,
) -> Result<Vec<TrajectoryRecord>, String> {
    let connection = open_memory_db(workspace_root)?;
    let mut statement = connection
        .prepare(
            "SELECT id, session_path, session_id, created_ts_ms, updated_ts_ms, title, current_goal,
                    recent_work_summary, active_model, previous_model, latest_attempt, latest_failure,
                    last_verification, next_step, active_files_json, open_tasks_json, recent_errors_json,
                    verification_hints_json
             FROM trajectories
             ORDER BY updated_ts_ms DESC
             LIMIT ?1",
        )
        .map_err(|err| err.to_string())?;
    let rows = statement
        .query_map([limit.max(1) as i64], row_to_trajectory)
        .map_err(|err| err.to_string())?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row.map_err(|err| err.to_string())?);
    }
    Ok(records)
}

pub fn search_trajectories(
    workspace_root: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<TrajectoryRecord>, String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let connection = open_memory_db(workspace_root)?;
    let mut statement = connection
        .prepare(
            "SELECT t.id, t.session_path, t.session_id, t.created_ts_ms, t.updated_ts_ms, t.title,
                    t.current_goal, t.recent_work_summary, t.active_model, t.previous_model,
                    t.latest_attempt, t.latest_failure, t.last_verification, t.next_step,
                    t.active_files_json, t.open_tasks_json, t.recent_errors_json,
                    t.verification_hints_json
             FROM trajectory_fts f
             JOIN trajectories t ON t.id = f.rowid
             WHERE trajectory_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )
        .map_err(|err| err.to_string())?;
    let rows = statement
        .query_map(params![trimmed, limit.max(1) as i64], row_to_trajectory)
        .map_err(|err| err.to_string())?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row.map_err(|err| err.to_string())?);
    }
    Ok(records)
}

pub fn build_trajectory_recall_text(workspace_root: &Path, limit: usize) -> Result<String, String> {
    let mut sections = Vec::new();
    if let Some(active) = active_trajectory(workspace_root)? {
        sections.push(render_active_trajectory_recall(&active));
    }

    for trajectory in list_recent_trajectories(workspace_root, limit.saturating_add(1))? {
        if sections.len() >= limit.max(1) {
            break;
        }
        if sections
            .first()
            .is_some_and(|section| section.contains(&trajectory.current_goal))
        {
            continue;
        }
        sections.push(format!(
            "[trajectory] {}\nGoal: {}\nNext: {}",
            trajectory.title, trajectory.current_goal, trajectory.next_step
        ));
    }

    if sections.is_empty() {
        Ok("none".to_string())
    } else {
        Ok(sections.join("\n\n"))
    }
}

pub fn list_skill_candidates(
    workspace_root: &Path,
    limit: usize,
) -> Result<Vec<SkillCandidate>, String> {
    let connection = open_memory_db(workspace_root)?;
    let mut statement = connection
        .prepare(
            "SELECT id, command_name, description, prompt, tool_sequence_json, occurrence_count,
                    first_seen_ts_ms, last_seen_ts_ms
             FROM skill_candidates
             WHERE occurrence_count >= 2
             ORDER BY occurrence_count DESC, last_seen_ts_ms DESC
             LIMIT ?1",
        )
        .map_err(|err| err.to_string())?;
    let rows = statement
        .query_map([limit.max(1) as i64], |row| {
            Ok(SkillCandidate {
                id: row.get(0)?,
                command_name: row.get(1)?,
                description: row.get(2)?,
                prompt: row.get(3)?,
                tool_sequence: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                occurrence_count: row.get::<_, i64>(5)?.max(0) as u32,
                first_seen_ts_ms: row.get::<_, i64>(6)?.max(0) as u128,
                last_seen_ts_ms: row.get::<_, i64>(7)?.max(0) as u128,
            })
        })
        .map_err(|err| err.to_string())?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row.map_err(|err| err.to_string())?);
    }
    Ok(records)
}

pub fn promote_skill_candidate(
    workspace_root: &Path,
    candidate_id: i64,
) -> Result<PathBuf, String> {
    let connection = open_memory_db(workspace_root)?;
    let candidate = connection
        .query_row(
            "SELECT id, command_name, description, prompt, tool_sequence_json, occurrence_count,
                    first_seen_ts_ms, last_seen_ts_ms
             FROM skill_candidates
             WHERE id = ?1",
            [candidate_id],
            |row| {
                Ok(SkillCandidate {
                    id: row.get(0)?,
                    command_name: row.get(1)?,
                    description: row.get(2)?,
                    prompt: row.get(3)?,
                    tool_sequence: serde_json::from_str(&row.get::<_, String>(4)?)
                        .unwrap_or_default(),
                    occurrence_count: row.get::<_, i64>(5)?.max(0) as u32,
                    first_seen_ts_ms: row.get::<_, i64>(6)?.max(0) as u128,
                    last_seen_ts_ms: row.get::<_, i64>(7)?.max(0) as u128,
                })
            },
        )
        .optional()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("skill candidate not found: {candidate_id}"))?;

    let dir = workspace_root.join(".harness").join("commands");
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let path = dir.join(format!("{}.toml", candidate.command_name));
    if path.exists() {
        return Err(format!("command already exists: {}", path.display()));
    }
    let prompt = candidate.prompt.replace('"', "\\\"");
    let contents = format!(
        "name = \"{name}\"\ndescription = \"{description}\"\nkind = \"prompt-template\"\nusage = \"/{name} [task]\"\nprompt = \"{prompt} {{args}}\"\n",
        name = candidate.command_name,
        description = candidate.description.replace('"', "\\\""),
        prompt = prompt
    );
    fs::write(&path, contents).map_err(|err| err.to_string())?;
    Ok(path)
}

fn open_memory_db(workspace_root: &Path) -> Result<Connection, String> {
    let harness_dir = workspace_root.join(".harness");
    fs::create_dir_all(&harness_dir).map_err(|err| err.to_string())?;
    let connection = Connection::open(memory_db_path(workspace_root)).map_err(|err| err.to_string())?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS trajectories (
               id INTEGER PRIMARY KEY,
               session_path TEXT NOT NULL UNIQUE,
               session_id TEXT,
               created_ts_ms INTEGER NOT NULL,
               updated_ts_ms INTEGER NOT NULL,
               title TEXT NOT NULL,
               current_goal TEXT NOT NULL,
               recent_work_summary TEXT NOT NULL,
               active_model TEXT,
               previous_model TEXT,
               latest_attempt TEXT,
               latest_failure TEXT,
               last_verification TEXT,
               next_step TEXT NOT NULL,
               active_files_json TEXT NOT NULL,
               open_tasks_json TEXT NOT NULL,
               recent_errors_json TEXT NOT NULL,
               verification_hints_json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS trajectory_steps (
               id INTEGER PRIMARY KEY,
               trajectory_id INTEGER NOT NULL,
               ts_ms INTEGER NOT NULL,
               kind TEXT NOT NULL,
               summary TEXT NOT NULL,
               file_path TEXT,
               FOREIGN KEY (trajectory_id) REFERENCES trajectories(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS skill_candidates (
               id INTEGER PRIMARY KEY,
               fingerprint TEXT NOT NULL UNIQUE,
               command_name TEXT NOT NULL,
               description TEXT NOT NULL,
               prompt TEXT NOT NULL,
               tool_sequence_json TEXT NOT NULL,
               occurrence_count INTEGER NOT NULL DEFAULT 1,
               first_seen_ts_ms INTEGER NOT NULL,
               last_seen_ts_ms INTEGER NOT NULL,
               source_trajectory_id INTEGER,
               FOREIGN KEY (source_trajectory_id) REFERENCES trajectories(id) ON DELETE SET NULL
             );
             CREATE TABLE IF NOT EXISTS skill_candidate_occurrences (
               fingerprint TEXT NOT NULL,
               session_path TEXT NOT NULL,
               PRIMARY KEY (fingerprint, session_path)
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS trajectory_fts USING fts5(
               title, current_goal, recent_work_summary, latest_attempt, latest_failure, next_step
             );",
        )
        .map_err(|err| err.to_string())?;
    Ok(connection)
}

fn upsert_trajectory(connection: &Connection, derived: &DerivedTrajectory) -> Result<i64, String> {
    connection
        .execute(
            "INSERT INTO trajectories (
               session_path, session_id, created_ts_ms, updated_ts_ms, title, current_goal,
               recent_work_summary, active_model, previous_model, latest_attempt, latest_failure,
               last_verification, next_step, active_files_json, open_tasks_json, recent_errors_json,
               verification_hints_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(session_path) DO UPDATE SET
               session_id=excluded.session_id,
               updated_ts_ms=excluded.updated_ts_ms,
               title=excluded.title,
               current_goal=excluded.current_goal,
               recent_work_summary=excluded.recent_work_summary,
               active_model=excluded.active_model,
               previous_model=excluded.previous_model,
               latest_attempt=excluded.latest_attempt,
               latest_failure=excluded.latest_failure,
               last_verification=excluded.last_verification,
               next_step=excluded.next_step,
               active_files_json=excluded.active_files_json,
               open_tasks_json=excluded.open_tasks_json,
               recent_errors_json=excluded.recent_errors_json,
               verification_hints_json=excluded.verification_hints_json",
            params![
                derived.record.session_path,
                derived.record.session_id,
                to_sql_i64(derived.record.created_ts_ms),
                to_sql_i64(derived.record.updated_ts_ms),
                derived.record.title,
                derived.record.current_goal,
                derived.record.recent_work_summary,
                derived.record.active_model,
                derived.record.previous_model,
                derived.record.latest_attempt,
                derived.record.latest_failure,
                derived.record.last_verification,
                derived.record.next_step,
                serde_json::to_string(&derived.record.active_files).map_err(|err| err.to_string())?,
                serde_json::to_string(&derived.record.open_tasks).map_err(|err| err.to_string())?,
                serde_json::to_string(&derived.record.recent_errors).map_err(|err| err.to_string())?,
                serde_json::to_string(&derived.record.verification_hints).map_err(|err| err.to_string())?,
            ],
        )
        .map_err(|err| err.to_string())?;

    let trajectory_id = connection
        .query_row(
            "SELECT id FROM trajectories WHERE session_path = ?1",
            [derived.record.session_path.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|err| err.to_string())?;

    connection
        .execute(
            "DELETE FROM trajectory_steps WHERE trajectory_id = ?1",
            [trajectory_id],
        )
        .map_err(|err| err.to_string())?;
    for step in &derived.steps {
        connection
            .execute(
                "INSERT INTO trajectory_steps (trajectory_id, ts_ms, kind, summary, file_path)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    trajectory_id,
                    to_sql_i64(step.ts_ms),
                    step.kind,
                    step.summary,
                    step.file_path
                ],
            )
            .map_err(|err| err.to_string())?;
    }

    connection
        .execute("DELETE FROM trajectory_fts WHERE rowid = ?1", [trajectory_id])
        .map_err(|err| err.to_string())?;
    connection
        .execute(
            "INSERT INTO trajectory_fts (rowid, title, current_goal, recent_work_summary, latest_attempt, latest_failure, next_step)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                trajectory_id,
                derived.record.title,
                derived.record.current_goal,
                derived.record.recent_work_summary,
                derived.record.latest_attempt,
                derived.record.latest_failure,
                derived.record.next_step
            ],
        )
        .map_err(|err| err.to_string())?;
    Ok(trajectory_id)
}

fn upsert_skill_candidate(
    connection: &Connection,
    session_path: &Path,
    tool_sequence: &[String],
    trajectory_id: i64,
) -> Result<(), String> {
    if tool_sequence.len() < 3 {
        return Ok(());
    }
    let fingerprint = tool_sequence.join(">");
    connection
        .execute(
            "INSERT OR IGNORE INTO skill_candidate_occurrences (fingerprint, session_path)
             VALUES (?1, ?2)",
            params![fingerprint, session_path.display().to_string()],
        )
        .map_err(|err| err.to_string())?;
    if connection.changes() == 0 {
        return Ok(());
    }

    let now_ms = now_ms();
    let command_name = suggest_command_name(tool_sequence);
    let description = format!(
        "Repeated workflow: {}",
        tool_sequence.join(" -> ")
    );
    let prompt = format!(
        "Follow this proven workflow for the current task: {}. Explain findings briefly, keep context continuity, and verify before claiming completion.",
        tool_sequence.join(" -> ")
    );
    connection
        .execute(
            "INSERT INTO skill_candidates (
               fingerprint, command_name, description, prompt, tool_sequence_json,
               occurrence_count, first_seen_ts_ms, last_seen_ts_ms, source_trajectory_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6, ?7)
             ON CONFLICT(fingerprint) DO UPDATE SET
               command_name=excluded.command_name,
               description=excluded.description,
               prompt=excluded.prompt,
               tool_sequence_json=excluded.tool_sequence_json,
               occurrence_count=skill_candidates.occurrence_count + 1,
               last_seen_ts_ms=excluded.last_seen_ts_ms,
               source_trajectory_id=excluded.source_trajectory_id",
            params![
                fingerprint,
                command_name,
                description,
                prompt,
                serde_json::to_string(tool_sequence).map_err(|err| err.to_string())?,
                to_sql_i64(now_ms),
                trajectory_id
            ],
        )
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn row_to_trajectory(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrajectoryRecord> {
    Ok(TrajectoryRecord {
        id: row.get(0)?,
        session_path: row.get(1)?,
        session_id: row.get(2)?,
        created_ts_ms: row.get::<_, i64>(3)?.max(0) as u128,
        updated_ts_ms: row.get::<_, i64>(4)?.max(0) as u128,
        title: row.get(5)?,
        current_goal: row.get(6)?,
        recent_work_summary: row.get(7)?,
        active_model: row.get(8)?,
        previous_model: row.get(9)?,
        latest_attempt: row.get(10)?,
        latest_failure: row.get(11)?,
        last_verification: row.get(12)?,
        next_step: row.get(13)?,
        active_files: serde_json::from_str(&row.get::<_, String>(14)?).unwrap_or_default(),
        open_tasks: serde_json::from_str(&row.get::<_, String>(15)?).unwrap_or_default(),
        recent_errors: serde_json::from_str(&row.get::<_, String>(16)?).unwrap_or_default(),
        verification_hints: serde_json::from_str(&row.get::<_, String>(17)?).unwrap_or_default(),
    })
}

fn derive_trajectory(
    workspace_root: &Path,
    session_path: &Path,
    events: &[SessionEvent],
) -> DerivedTrajectory {
    let session_id = SessionStore::session_id_from_path(session_path);
    let title = latest_goal(events)
        .or_else(|| session_id.clone().map(|id| format!("Session {id}")))
        .unwrap_or_else(|| "Active trajectory".to_string());
    let open_tasks = collect_goals(events, 4);
    let recent_errors = collect_errors(events, 4);
    let latest_attempt = latest_attempt(events);
    let latest_failure = latest_failure(events);
    let active_files = collect_active_files(events, 6);
    let models = latest_models(events);
    let last_verification = latest_verification(events);
    let recent_work_summary = build_recent_summary(events);
    let next_step = open_tasks
        .first()
        .map(|goal| format!("Continue with: {goal}"))
        .or_else(|| latest_failure.as_ref().map(|failure| format!("Recover from: {failure}")))
        .unwrap_or_else(|| "Continue the current task with the latest context.".to_string());
    let verification_hints = project_verification_hints(workspace_root);
    let steps = build_trajectory_steps(events);
    let tool_sequence = collect_tool_sequence(events);
    let title = truncate_text(&title, 80);
    let current_goal = open_tasks
        .first()
        .cloned()
        .or_else(|| latest_goal(events))
        .unwrap_or_else(|| "Continue the current task".to_string());
    let ts = newest_event_ts(events);

    DerivedTrajectory {
        record: TrajectoryRecord {
            id: 0,
            session_path: session_path.display().to_string(),
            session_id,
            created_ts_ms: ts,
            updated_ts_ms: ts,
            title,
            current_goal,
            recent_work_summary,
            active_model: models.0,
            previous_model: models.1,
            latest_attempt,
            latest_failure,
            last_verification,
            next_step,
            active_files,
            open_tasks,
            recent_errors,
            verification_hints,
        },
        steps,
        tool_sequence,
    }
}

fn latest_goal(events: &[SessionEvent]) -> Option<String> {
    events.iter().rev().find_map(|event| match event.kind.as_str() {
        "user_input" | "prompt_start" => event
            .payload
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty() && !text.starts_with('/') && !is_low_signal_user_text(text))
            .map(ToOwned::to_owned),
        _ => None,
    })
}

fn collect_goals(events: &[SessionEvent], limit: usize) -> Vec<String> {
    let mut goals = Vec::new();
    for event in events.iter().rev() {
        let Some(text) = (match event.kind.as_str() {
            "user_input" | "prompt_start" => event.payload.get("text").and_then(Value::as_str),
            _ => None,
        }) else {
            continue;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.starts_with('/') || is_low_signal_user_text(trimmed) {
            continue;
        }
        push_unique(&mut goals, trimmed.to_string(), limit);
        if goals.len() >= limit {
            break;
        }
    }
    goals
}

fn collect_errors(events: &[SessionEvent], limit: usize) -> Vec<String> {
    let mut errors = Vec::new();
    for event in events.iter().rev() {
        let maybe_error = match event.kind.as_str() {
            "tool_error" | "prompt_error" | "mcp_error" | "agent_tool_error" => event
                .payload
                .get("error")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| {
                    event.payload.get("errors").and_then(Value::as_array).and_then(|items| {
                        items.iter().find_map(|item| item.as_str().map(ToOwned::to_owned))
                    })
                }),
            "model_probe_failed" => event
                .payload
                .get("error")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            _ => None,
        };
        if let Some(error) = maybe_error {
            push_unique(&mut errors, truncate_text(error.trim(), 160), limit);
            if errors.len() >= limit {
                break;
            }
        }
    }
    errors
}

fn latest_attempt(events: &[SessionEvent]) -> Option<String> {
    for event in events.iter().rev() {
        match event.kind.as_str() {
            "agent_tool" => {
                let name = event.payload.get("name").and_then(Value::as_str).unwrap_or("tool");
                let summary = event.payload.get("summary").and_then(Value::as_str).unwrap_or("-");
                return Some(truncate_text(format!("{name}: {summary}").trim(), 160));
            }
            "tool_result" => {
                let name = event.payload.get("command").and_then(Value::as_str).unwrap_or("tool");
                let summary = event.payload.get("summary").and_then(Value::as_str).unwrap_or("-");
                return Some(truncate_text(format!("{name}: {summary}").trim(), 160));
            }
            "prompt_start" | "user_input" => {
                if let Some(text) = event.payload.get("text").and_then(Value::as_str) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with('/') {
                        return Some(truncate_text(trimmed, 160));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn latest_failure(events: &[SessionEvent]) -> Option<String> {
    collect_errors(events, 1).into_iter().next()
}

fn latest_models(events: &[SessionEvent]) -> (Option<String>, Option<String>) {
    for event in events.iter().rev() {
        match event.kind.as_str() {
            "model_handoff" => {
                let current = event
                    .payload
                    .get("to_model")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let previous = event
                    .payload
                    .get("from_model")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                return (current, previous);
            }
            "model_change" => {
                let current = event
                    .payload
                    .get("model")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let previous = event
                    .payload
                    .get("from")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                return (current, previous);
            }
            _ => {}
        }
    }
    (None, None)
}

fn latest_verification(events: &[SessionEvent]) -> Option<String> {
    for event in events.iter().rev() {
        let command = match event.kind.as_str() {
            "agent_tool" => event
                .payload
                .get("arguments")
                .and_then(Value::as_object)
                .and_then(|args| args.get("command"))
                .and_then(Value::as_str),
            "tool_result" => event.payload.get("command").and_then(Value::as_str),
            _ => None,
        }?;
        if is_verification_command(command) {
            return Some(truncate_text(command, 120));
        }
    }
    None
}

fn build_recent_summary(events: &[SessionEvent]) -> String {
    let mut lines = Vec::new();
    if let Some(attempt) = latest_attempt(events) {
        lines.push(format!("Latest attempt: {attempt}"));
    }
    if let Some(failure) = latest_failure(events) {
        lines.push(format!("Latest failure: {failure}"));
    }
    if let Some(verification) = latest_verification(events) {
        lines.push(format!("Last verification: {verification}"));
    }
    if let Some(result) = events.iter().rev().find_map(|event| match event.kind.as_str() {
        "agent_result" | "prompt_result" => event.payload.get("text").and_then(Value::as_str),
        _ => None,
    }) {
        lines.push(format!("Last result: {}", truncate_text(result.trim(), 220)));
    }
    if lines.is_empty() {
        "No recent work summary captured.".to_string()
    } else {
        truncate_text(&lines.join("\n"), 700)
    }
}

fn build_trajectory_steps(events: &[SessionEvent]) -> Vec<TrajectoryStep> {
    let mut steps = Vec::new();
    for event in events.iter().rev() {
        let summary = match event.kind.as_str() {
            "user_input" | "prompt_start" => event.payload.get("text").and_then(Value::as_str),
            "agent_tool" => event.payload.get("summary").and_then(Value::as_str),
            "tool_result" => event.payload.get("summary").and_then(Value::as_str),
            "tool_error" | "agent_tool_error" | "prompt_error" => {
                event.payload.get("error").and_then(Value::as_str)
            }
            "agent_result" | "prompt_result" => event.payload.get("text").and_then(Value::as_str),
            _ => None,
        };
        let Some(summary) = summary else {
            continue;
        };
        let file_path = first_path_in_value(&event.payload);
        steps.push(TrajectoryStep {
            ts_ms: event.ts_ms,
            kind: event.kind.clone(),
            summary: truncate_text(summary.trim(), 180),
            file_path,
        });
        if steps.len() >= 8 {
            break;
        }
    }
    steps.reverse();
    steps
}

fn collect_active_files(events: &[SessionEvent], limit: usize) -> Vec<String> {
    let mut files = Vec::new();
    for event in events.iter().rev() {
        for path in collect_paths_from_value(&event.payload) {
            push_unique(&mut files, path, limit);
            if files.len() >= limit {
                return files;
            }
        }
    }
    files
}

fn collect_tool_sequence(events: &[SessionEvent]) -> Vec<String> {
    let mut sequence = Vec::new();
    for event in events {
        let name = match event.kind.as_str() {
            "agent_tool" => event.payload.get("name").and_then(Value::as_str),
            "tool_result" | "tool_error" => event.payload.get("command").and_then(Value::as_str),
            _ => None,
        };
        let Some(name) = name else {
            continue;
        };
        let value = name.trim();
        if value.is_empty() {
            continue;
        }
        if sequence.last().is_some_and(|existing| existing == value) {
            continue;
        }
        sequence.push(value.to_string());
        if sequence.len() >= 6 {
            break;
        }
    }
    sequence
}

fn collect_paths_from_value(value: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    collect_paths_recursive(value, &mut paths);
    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        if seen.insert(path.clone()) {
            deduped.push(path);
        }
    }
    deduped
}

fn collect_paths_recursive(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(key.as_str(), "path" | "file" | "scope") {
                    if let Some(path) = value.as_str() {
                        let trimmed = path.trim();
                        if !trimmed.is_empty() {
                            output.push(trimmed.to_string());
                        }
                    }
                } else if matches!(key.as_str(), "paths" | "files") {
                    if let Some(items) = value.as_array() {
                        for item in items {
                            if let Some(path) = item.as_str() {
                                let trimmed = path.trim();
                                if !trimmed.is_empty() {
                                    output.push(trimmed.to_string());
                                }
                            }
                        }
                    }
                }
                collect_paths_recursive(value, output);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_paths_recursive(item, output);
            }
        }
        _ => {}
    }
}

fn first_path_in_value(value: &Value) -> Option<String> {
    collect_paths_from_value(value).into_iter().next()
}

fn render_active_trajectory_recall(record: &TrajectoryRecord) -> String {
    let mut lines = vec![
        "[active_trajectory]".to_string(),
        format!("Goal: {}", record.current_goal),
        format!("Next: {}", record.next_step),
    ];
    if let Some(attempt) = &record.latest_attempt {
        lines.push(format!("Latest attempt: {attempt}"));
    }
    if let Some(failure) = &record.latest_failure {
        lines.push(format!("Latest failure: {failure}"));
    }
    if !record.active_files.is_empty() {
        lines.push(format!(
            "Files: {}",
            record.active_files.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if !record.recent_errors.is_empty() {
        lines.push(format!("Open errors: {}", record.recent_errors.join(" | ")));
    }
    if let Some(verification) = &record.last_verification {
        lines.push(format!("Last verification: {verification}"));
    }
    lines.join("\n")
}

fn is_verification_command(command: &str) -> bool {
    let command = command.trim().to_ascii_lowercase();
    [
        "cargo test",
        "cargo check",
        "cargo build",
        "npm test",
        "npm run build",
        "pnpm test",
        "pnpm build",
        "yarn test",
        "yarn build",
        "pytest",
        "go test",
        "bundle exec rspec",
        "bin/rails test",
    ]
    .iter()
    .any(|candidate| command.contains(candidate))
}

fn project_verification_hints(workspace_root: &Path) -> Vec<String> {
    let mut hints = Vec::new();
    if workspace_root.join("Cargo.toml").is_file() {
        hints.push("cargo test --workspace".to_string());
        hints.push("cargo check --workspace".to_string());
    }
    if workspace_root.join("package.json").is_file() {
        if workspace_root.join("pnpm-lock.yaml").is_file() {
            hints.push("pnpm test".to_string());
            hints.push("pnpm build".to_string());
        } else if workspace_root.join("yarn.lock").is_file() {
            hints.push("yarn test".to_string());
            hints.push("yarn build".to_string());
        } else {
            hints.push("npm test".to_string());
            hints.push("npm run build".to_string());
        }
    }
    if workspace_root.join("pyproject.toml").is_file()
        || workspace_root.join("pytest.ini").is_file()
        || workspace_root.join("requirements.txt").is_file()
    {
        hints.push("pytest".to_string());
    }
    if workspace_root.join("go.mod").is_file() {
        hints.push("go test ./...".to_string());
    }
    if workspace_root.join("Gemfile").is_file() {
        hints.push("bundle exec rspec".to_string());
    }
    hints
}

fn suggest_command_name(tool_sequence: &[String]) -> String {
    let mut parts = tool_sequence
        .iter()
        .take(3)
        .map(|value| value.to_ascii_lowercase().replace('_', "-"))
        .map(|value| {
            value
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
                .collect::<String>()
        })
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        parts.push("workflow".to_string());
    }
    let mut name = format!("auto-{}", parts.join("-"));
    if name.len() > 32 {
        name.truncate(32);
        name = name.trim_end_matches('-').to_string();
    }
    name
}

fn newest_event_ts(events: &[SessionEvent]) -> u128 {
    events.last().map(|event| event.ts_ms).unwrap_or_else(now_ms)
}

fn to_sql_i64(value: u128) -> i64 {
    value.min(i64::MAX as u128) as i64
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
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

fn is_low_signal_user_text(text: &str) -> bool {
    let normalized = text.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return true;
    }
    if normalized.chars().count() <= 2 {
        return true;
    }
    let low_signal = [
        "hi", "hello", "hey", "안녕", "안녕하세요", "ㅎㅇ", "ㅂㅇ", "test", "ping", "pong",
    ];
    if low_signal.iter().any(|item| normalized == *item) {
        return true;
    }
    normalized.contains("모델명")
        || normalized.contains("네 모델")
        || normalized.contains("what model")
        || normalized.contains("your model")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::{
        active_trajectory, build_trajectory_recall_text, list_skill_candidates, memory_db_path,
        promote_skill_candidate, record_session_trajectory, search_trajectories,
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

    fn write_session(path: &Path, events: &[crate::session::SessionEvent]) {
        let contents = events
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn records_and_searches_trajectory() {
        let workspace = temp_workspace("trajectory-record");
        let session_path = workspace.join(".harness/sessions/session-test.jsonl");
        write_session(
            &session_path,
            &[
                crate::session::SessionEvent {
                    ts_ms: 1,
                    kind: "user_input".to_string(),
                    payload: json!({ "text": "wire trajectory memory" }),
                },
                crate::session::SessionEvent {
                    ts_ms: 2,
                    kind: "agent_tool".to_string(),
                    payload: json!({ "name": "read", "summary": "read README", "arguments": { "path": "README.md" }}),
                },
                crate::session::SessionEvent {
                    ts_ms: 3,
                    kind: "tool_error".to_string(),
                    payload: json!({ "error": "missing summary" }),
                },
            ],
        );

        let record = record_session_trajectory(&workspace, &session_path).unwrap();
        assert!(memory_db_path(&workspace).is_file());
        assert_eq!(record.current_goal, "wire trajectory memory");
        let active = active_trajectory(&workspace).unwrap().unwrap();
        assert_eq!(active.current_goal, "wire trajectory memory");
        let search = search_trajectories(&workspace, "trajectory", 5).unwrap();
        assert_eq!(search.len(), 1);

        cleanup(&workspace);
    }

    #[test]
    fn builds_trajectory_recall_and_skill_candidates() {
        let workspace = temp_workspace("trajectory-skill");
        for index in 0..2 {
            let session_path = workspace.join(format!(".harness/sessions/session-{index}.jsonl"));
            write_session(
                &session_path,
                &[
                    crate::session::SessionEvent {
                        ts_ms: 1,
                        kind: "user_input".to_string(),
                        payload: json!({ "text": "inspect provider flow" }),
                    },
                    crate::session::SessionEvent {
                        ts_ms: 2,
                        kind: "agent_tool".to_string(),
                        payload: json!({ "name": "grep", "summary": "grep provider", "arguments": { "path": "src" }}),
                    },
                    crate::session::SessionEvent {
                        ts_ms: 3,
                        kind: "agent_tool".to_string(),
                        payload: json!({ "name": "read", "summary": "read provider", "arguments": { "path": "provider.rs" }}),
                    },
                    crate::session::SessionEvent {
                        ts_ms: 4,
                        kind: "agent_tool".to_string(),
                        payload: json!({ "name": "exec", "summary": "exit status 0", "arguments": { "command": "cargo test --workspace" }}),
                    },
                ],
            );
            record_session_trajectory(&workspace, &session_path).unwrap();
        }

        let recall = build_trajectory_recall_text(&workspace, 3).unwrap();
        assert!(recall.contains("[active_trajectory]"));
        let candidates = list_skill_candidates(&workspace, 10).unwrap();
        assert_eq!(candidates.len(), 1);
        let promoted = promote_skill_candidate(&workspace, candidates[0].id).unwrap();
        assert!(promoted.is_file());

        cleanup(&workspace);
    }
}
