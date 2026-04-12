use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::session::SessionEvent;
use crate::SessionStore;
use crate::{
    active_trajectory, assess_verification, count_matching_failure_sessions, current_timestamp_iso,
    default_local_origin, default_local_scope, load_config, load_promotion_candidate,
    load_skill_candidate, metadata_legacy_kind, metadata_title, normalize_failure_text,
    pending_promotion_candidate_count, record_session_trajectory, resolve_selected_memory_backend,
    session_derived_retention, session_key_for_item, source_ref_uri, store_promotion_candidate,
    timestamp_millis, update_promotion_candidate_status, AmcpMemoryItem, AmcpRetention,
    AmcpSourceRef, AutoPromotePolicy, RecallRequest, VerificationEvent,
};

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
    #[serde(default)]
    pub id: Option<String>,
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
    pub pending_candidates: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelHandoffSnapshot {
    #[serde(default)]
    pub from_model: Option<String>,
    pub to_model: String,
    pub current_goal: String,
    pub recent_work_summary: String,
    #[serde(default)]
    pub active_files: Vec<String>,
    #[serde(default)]
    pub latest_attempt: Option<String>,
    #[serde(default)]
    pub latest_failure: Option<String>,
    #[serde(default)]
    pub last_verification: Option<String>,
    #[serde(default)]
    pub open_tasks: Vec<String>,
    #[serde(default)]
    pub recent_errors: Vec<String>,
    pub suggested_next_step: String,
    #[serde(default)]
    pub session_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredModelHandoff {
    pub ts_ms: u128,
    pub snapshot: ModelHandoffSnapshot,
}

#[derive(Debug, Clone)]
struct ItemProvenance {
    origin: String,
    trigger: String,
    source_session: Option<String>,
    source_turns: Vec<usize>,
    derived_from: Vec<String>,
}

#[derive(Debug, Clone)]
struct BuildAmcpItemOptions {
    id_seed: String,
    legacy_kind: String,
    item_type: String,
    retention: AmcpRetention,
    provenance: ItemProvenance,
    metadata_extra: Option<Value>,
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
    let config = load_config(workspace_root).unwrap_or_default();
    let backend = resolve_selected_memory_backend(workspace_root, &config)?;
    let item = build_amcp_item_with_options(
        title,
        body,
        tags,
        session_path,
        &[],
        session_path.and_then(SessionStore::session_id_from_path),
        BuildAmcpItemOptions {
            id_seed: format!(
                "manual-memory|{}|{}|{}|{}",
                kind.as_str(),
                title,
                body,
                session_path
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".to_string())
            ),
            legacy_kind: kind.as_str().to_string(),
            item_type: portable_item_type(kind).to_string(),
            retention: crate::default_private_retention(),
            provenance: ItemProvenance {
                origin: "manual-memory".to_string(),
                trigger: "append-memory-record".to_string(),
                source_session: session_path.and_then(SessionStore::session_id_from_path),
                source_turns: Vec::new(),
                derived_from: Vec::new(),
            },
            metadata_extra: None,
        },
    );
    let _ = backend.remember(&item)?;
    let record = memory_record_from_item(&item);
    if config.dual_write_legacy_jsonl() {
        append_legacy_memory_record(workspace_root, &record)?;
        return Ok(memory_dir(workspace_root).join(kind.file_name()));
    }
    Ok(crate::memory_db_path(workspace_root))
}

pub fn list_memory_records(workspace_root: &Path) -> Result<Vec<MemoryRecord>, String> {
    let config = load_config(workspace_root).unwrap_or_default();
    let backend = resolve_selected_memory_backend(workspace_root, &config)?;
    let mut records = backend
        .export_items()?
        .into_iter()
        .map(|item| memory_record_from_item(&item))
        .collect::<Vec<_>>();
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

    let config = load_config(workspace_root).unwrap_or_default();
    let backend = resolve_selected_memory_backend(workspace_root, &config)?;
    Ok(backend
        .recall(&RecallRequest {
            query: Some(query),
            limit: 24,
            token_budget: 4000,
            context_budget: None,
        })?
        .into_iter()
        .map(|item| memory_record_from_item(&item))
        .collect())
}

pub fn save_session_summary(
    workspace_root: &Path,
    session_path: &Path,
) -> Result<(PathBuf, MemoryRecord), String> {
    let events = SessionStore::read_events(session_path)?;
    let _ = record_session_trajectory(workspace_root, session_path);
    let digest = summarize_session_events(&events);
    let trajectory = active_trajectory(workspace_root).ok().flatten();
    let session_id = SessionStore::session_id_from_path(session_path);
    let item = build_amcp_item_with_options(
        &digest.title,
        &digest.summary,
        &digest.tools,
        Some(session_path),
        &trajectory
            .as_ref()
            .map(|record| record.active_files.clone())
            .unwrap_or_default(),
        session_id.clone(),
        BuildAmcpItemOptions {
            id_seed: format!(
                "session-summary|{}|{}",
                session_id.as_deref().unwrap_or("-"),
                digest.title
            ),
            legacy_kind: MemoryKind::Summary.as_str().to_string(),
            item_type: portable_item_type(MemoryKind::Summary).to_string(),
            retention: crate::default_private_retention(),
            provenance: ItemProvenance {
                origin: "session-promotion".to_string(),
                trigger: "memory-save".to_string(),
                source_session: session_id,
                source_turns: all_event_indexes(&events),
                derived_from: Vec::new(),
            },
            metadata_extra: None,
        },
    );
    let record = memory_record_from_item(&item);
    let config = load_config(workspace_root).unwrap_or_default();
    let backend = resolve_selected_memory_backend(workspace_root, &config)?;
    let _ = backend.remember(&item)?;
    let path = if config.dual_write_legacy_jsonl() {
        append_legacy_memory_if_missing(workspace_root, &record)?;
        memory_dir(workspace_root).join(MemoryKind::Summary.file_name())
    } else {
        crate::memory_db_path(workspace_root)
    };
    Ok((path, record))
}

pub fn save_session_memory_bundle(
    workspace_root: &Path,
    session_path: &Path,
) -> Result<SavedMemoryBundle, String> {
    let events = SessionStore::read_events(session_path)?;
    let trajectory = record_session_trajectory(workspace_root, session_path).ok();
    let digest = summarize_session_events(&events);
    let config = load_config(workspace_root).unwrap_or_default();
    let backend = resolve_selected_memory_backend(workspace_root, &config)?;
    let mut saved_records = Vec::new();
    let source_turns = all_event_indexes(&events);

    let file_paths = trajectory
        .as_ref()
        .map(|record| record.active_files.clone())
        .unwrap_or_default();
    let session_id = SessionStore::session_id_from_path(session_path);

    let summary_item = build_amcp_item_with_options(
        &digest.title,
        &digest.summary,
        &digest.tools,
        Some(session_path),
        &file_paths,
        session_id.clone(),
        BuildAmcpItemOptions {
            id_seed: format!(
                "session-summary|{}|{}",
                session_id.as_deref().unwrap_or("-"),
                digest.title
            ),
            legacy_kind: MemoryKind::Summary.as_str().to_string(),
            item_type: portable_item_type(MemoryKind::Summary).to_string(),
            retention: crate::default_private_retention(),
            provenance: ItemProvenance {
                origin: "session-promotion".to_string(),
                trigger: "memory-save".to_string(),
                source_session: session_id.clone(),
                source_turns: source_turns.clone(),
                derived_from: Vec::new(),
            },
            metadata_extra: None,
        },
    );
    if backend.remember(&summary_item)? {
        let record = memory_record_from_item(&summary_item);
        saved_records.push(record);
    }
    if config.dual_write_legacy_jsonl() {
        append_legacy_memory_if_missing(workspace_root, &memory_record_from_item(&summary_item))?;
    }

    for goal in &digest.goals {
        let item = build_amcp_item_with_options(
            goal,
            goal,
            &digest.tools,
            Some(session_path),
            &file_paths,
            session_id.clone(),
            BuildAmcpItemOptions {
                id_seed: format!(
                    "session-task|{}|{}",
                    session_id.as_deref().unwrap_or("-"),
                    goal
                ),
                legacy_kind: MemoryKind::Task.as_str().to_string(),
                item_type: portable_item_type(MemoryKind::Task).to_string(),
                retention: crate::default_private_retention(),
                provenance: ItemProvenance {
                    origin: "session-promotion".to_string(),
                    trigger: "memory-save".to_string(),
                    source_session: session_id.clone(),
                    source_turns: source_turns.clone(),
                    derived_from: Vec::new(),
                },
                metadata_extra: None,
            },
        );
        if backend.remember(&item)? {
            saved_records.push(memory_record_from_item(&item));
        }
        if config.dual_write_legacy_jsonl() {
            append_legacy_memory_if_missing(workspace_root, &memory_record_from_item(&item))?;
        }
    }

    for error in &digest.errors {
        let item = build_amcp_item_with_options(
            error,
            error,
            &digest.tools,
            Some(session_path),
            &file_paths,
            session_id.clone(),
            BuildAmcpItemOptions {
                id_seed: format!(
                    "session-error|{}|{}",
                    session_id.as_deref().unwrap_or("-"),
                    error
                ),
                legacy_kind: MemoryKind::Error.as_str().to_string(),
                item_type: portable_item_type(MemoryKind::Error).to_string(),
                retention: crate::default_private_retention(),
                provenance: ItemProvenance {
                    origin: "session-promotion".to_string(),
                    trigger: "memory-save".to_string(),
                    source_session: session_id.clone(),
                    source_turns: source_turns.clone(),
                    derived_from: Vec::new(),
                },
                metadata_extra: None,
            },
        );
        if backend.remember(&item)? {
            saved_records.push(memory_record_from_item(&item));
        }
        if config.dual_write_legacy_jsonl() {
            append_legacy_memory_if_missing(workspace_root, &memory_record_from_item(&item))?;
        }
    }

    let mut auto_saved = run_auto_promotion_pipeline(
        workspace_root,
        session_path,
        &events,
        trajectory.as_ref(),
        &digest,
        &config,
        &*backend,
    )?;
    saved_records.append(&mut auto_saved);

    Ok(SavedMemoryBundle {
        saved_records,
        pending_candidates: pending_promotion_candidate_count(workspace_root).unwrap_or(0),
    })
}

fn build_amcp_item_with_options(
    title: &str,
    body: &str,
    tags: &[String],
    session_path: Option<&Path>,
    file_paths: &[String],
    session_id: Option<String>,
    options: BuildAmcpItemOptions,
) -> AmcpMemoryItem {
    let timestamp = now_ms();
    let timestamp_iso = current_timestamp_iso();
    let session_text = session_path.map(|path| path.display().to_string());
    let mut source_refs = Vec::new();
    if let Some(path) = session_path {
        source_refs.push(AmcpSourceRef {
            kind: "session".to_string(),
            uri: source_ref_uri(path),
        });
    }
    for path in file_paths {
        if path.trim().is_empty() {
            continue;
        }
        source_refs.push(AmcpSourceRef {
            kind: "file".to_string(),
            uri: format!("file://{path}"),
        });
    }
    let mut metadata = crate::portable_memory::default_semantic_metadata(
        title,
        &options.legacy_kind,
        session_text.as_deref(),
    );
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "inference_basis".to_string(),
            Value::String(options.provenance.trigger.clone()),
        );
        object.insert(
            "provenance".to_string(),
            json!({
                "origin": options.provenance.origin,
                "trigger": options.provenance.trigger,
                "source_session": options.provenance.source_session,
                "source_turns": options.provenance.source_turns,
                "derived_from": options.provenance.derived_from,
            }),
        );
        if let Some(extra) = options.metadata_extra {
            merge_metadata_map(object, extra);
        }
    }
    AmcpMemoryItem {
        id: format!("amcp-{}", stable_hex_hash(&options.id_seed)),
        content: body.to_string(),
        item_type: options.item_type,
        scope: default_local_scope(),
        origin: default_local_origin(session_id.as_deref(), timestamp),
        visibility: "private".to_string(),
        retention: options.retention,
        tags: tags.to_vec(),
        metadata,
        source_refs,
        energy: 1.0,
        created_at: timestamp_iso.clone(),
        updated_at: timestamp_iso,
    }
}

fn merge_metadata_map(target: &mut Map<String, Value>, extra: Value) {
    let Some(extra_map) = extra.as_object() else {
        return;
    };
    for (key, value) in extra_map {
        target.insert(key.clone(), value.clone());
    }
}

fn all_event_indexes(events: &[SessionEvent]) -> Vec<usize> {
    (0..events.len()).collect()
}

fn session_verification_events(events: &[SessionEvent]) -> Vec<VerificationEvent> {
    let mut verification_events = Vec::new();
    for event in events {
        match event.kind.as_str() {
            "agent_tool" => {
                if let Some(arguments) = event.payload.get("arguments").and_then(Value::as_object) {
                    verification_events.push(VerificationEvent {
                        name: event
                            .payload
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string(),
                        arguments: Value::Object(arguments.clone()),
                    });
                }
            }
            "tool_result" | "tool_error" => {
                let command = event
                    .payload
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                verification_events.push(VerificationEvent {
                    name: "exec".to_string(),
                    arguments: json!({ "command": command }),
                });
            }
            _ => {}
        }
    }
    verification_events
}

fn default_recall_request(limit: usize) -> RecallRequest {
    RecallRequest {
        query: None,
        limit: limit.max(1),
        token_budget: 4000,
        context_budget: None,
    }
}

fn portable_item_type(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Summary | MemoryKind::Note => "context",
        MemoryKind::Decision => "decision",
        MemoryKind::Task => "task",
        MemoryKind::Error => "error",
    }
}

fn memory_record_from_item(item: &AmcpMemoryItem) -> MemoryRecord {
    MemoryRecord {
        id: Some(item.id.clone()),
        ts_ms: timestamp_millis(&item.updated_at).unwrap_or_default(),
        kind: metadata_legacy_kind(item),
        title: metadata_title(item),
        body: item.content.clone(),
        tags: item.tags.clone(),
        session_path: session_key_for_item(item).map(|value| value.replacen("file://", "", 1)),
    }
}

fn legacy_list_memory_records(workspace_root: &Path) -> Result<Vec<MemoryRecord>, String> {
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

fn append_legacy_memory_record(
    workspace_root: &Path,
    record: &MemoryRecord,
) -> Result<PathBuf, String> {
    let dir = memory_dir(workspace_root);
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let path = dir.join(legacy_file_name_for_kind(&record.kind));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| err.to_string())?;
    let line = serde_json::to_string(record).map_err(|err| err.to_string())?;
    writeln!(file, "{line}").map_err(|err| err.to_string())?;
    Ok(path)
}

fn append_legacy_memory_if_missing(
    workspace_root: &Path,
    record: &MemoryRecord,
) -> Result<(), String> {
    let existing = legacy_list_memory_records(workspace_root)?;
    let session_path = record.session_path.as_deref().unwrap_or("-");
    if existing.iter().any(|candidate| {
        candidate.kind == record.kind
            && candidate.title == record.title
            && candidate.session_path.as_deref().unwrap_or("-") == session_path
    }) {
        return Ok(());
    }
    let _ = append_legacy_memory_record(workspace_root, record)?;
    Ok(())
}

fn legacy_file_name_for_kind(kind: &str) -> &'static str {
    match kind {
        "summary" => "summaries.jsonl",
        "decision" => "decisions.jsonl",
        "task" => "tasks.jsonl",
        "error" => "errors.jsonl",
        "note" | "context" | "artifact" => "notes.jsonl",
        _ => "notes.jsonl",
    }
}

fn stable_hex_hash(input: &str) -> String {
    let mut hash = 1469598103934665603u64;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("{hash:016x}")
}

pub fn build_resume_text(workspace_root: &Path) -> Result<String, String> {
    let latest_session = SessionStore::latest(workspace_root)?;
    if let Some(path) = latest_session.as_deref() {
        let _ = record_session_trajectory(workspace_root, path);
    }
    let active_trajectory = active_trajectory(workspace_root)?;
    let latest_digest = latest_session
        .as_deref()
        .map(SessionStore::read_events)
        .transpose()?
        .map(|events| summarize_session_events(&events));
    let latest_handoff = latest_session
        .as_deref()
        .map(latest_model_handoff)
        .transpose()?
        .flatten();
    let memories = list_memory_records(workspace_root).unwrap_or_default();
    let latest_summary = memories
        .iter()
        .find(|record| record.kind == MemoryKind::Summary.as_str());
    let mut out = String::new();
    out.push_str("Resume\n");
    out.push_str(&format!(
        "latest_session: {}\n",
        latest_session
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_string())
    ));
    if let Some(trajectory) = active_trajectory.as_ref() {
        out.push_str(&format!("trajectory: {}\n", trajectory.title));
    }
    if let Some(handoff) = latest_handoff {
        out.push_str(&format!("active_model: {}\n", handoff.snapshot.to_model));
        if let Some(previous) = handoff.snapshot.from_model.as_deref() {
            out.push_str(&format!("previous_model: {}\n", previous));
        }
        out.push_str(&format!(
            "current_goal: {}\n",
            handoff.snapshot.current_goal
        ));
        if !handoff.snapshot.active_files.is_empty() {
            out.push_str(&format!(
                "active_files: {}\n",
                handoff.snapshot.active_files.join(", ")
            ));
        }
        if let Some(attempt) = handoff.snapshot.latest_attempt.as_deref() {
            out.push_str(&format!("latest_attempt: {attempt}\n"));
        }
        if let Some(failure) = handoff.snapshot.latest_failure.as_deref() {
            out.push_str(&format!("latest_failure: {failure}\n"));
        }
        if let Some(verification) = handoff.snapshot.last_verification.as_deref() {
            out.push_str(&format!("last_verification: {verification}\n"));
        }
        out.push_str(&format!(
            "next_step: {}\n",
            handoff.snapshot.suggested_next_step
        ));
    } else if let Some(trajectory) = active_trajectory.as_ref() {
        if let Some(model) = trajectory.active_model.as_deref() {
            out.push_str(&format!("active_model: {model}\n"));
        }
        out.push_str(&format!("current_goal: {}\n", trajectory.current_goal));
        if !trajectory.active_files.is_empty() {
            out.push_str(&format!(
                "active_files: {}\n",
                trajectory.active_files.join(", ")
            ));
        }
        if let Some(attempt) = trajectory.latest_attempt.as_deref() {
            out.push_str(&format!("latest_attempt: {attempt}\n"));
        }
        if let Some(failure) = trajectory.latest_failure.as_deref() {
            out.push_str(&format!("latest_failure: {failure}\n"));
        }
        if let Some(verification) = trajectory.last_verification.as_deref() {
            out.push_str(&format!("last_verification: {verification}\n"));
        }
        out.push_str(&format!("next_step: {}\n", trajectory.next_step));
    }
    if let Some(digest) = latest_digest.as_ref() {
        out.push_str(&format!("latest_summary: {}\n", digest.title));
        out.push_str(&digest.summary);
        if !digest.summary.ends_with('\n') {
            out.push('\n');
        }
    } else if let Some(summary) = latest_summary {
        out.push_str(&format!("latest_summary: {}\n", summary.title));
        out.push_str(&summary.body);
        out.push('\n');
    } else {
        out.push_str("latest_summary: none\n");
    }
    let latest_tasks = latest_digest
        .as_ref()
        .map(|digest| digest.goals.iter().take(3).cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if !latest_tasks.is_empty() {
        out.push_str("recent_tasks:\n");
        for task in latest_tasks {
            out.push_str("- ");
            out.push_str(&task);
            out.push('\n');
        }
    }
    let latest_errors = latest_digest
        .as_ref()
        .map(|digest| digest.errors.iter().take(3).cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if !latest_errors.is_empty() {
        out.push_str("recent_errors:\n");
        for error in latest_errors {
            out.push_str("- ");
            out.push_str(&error);
            out.push('\n');
        }
    }
    Ok(out)
}

pub fn build_handoff_text(workspace_root: &Path) -> Result<String, String> {
    let latest_session = SessionStore::latest(workspace_root)?;
    if let Some(path) = latest_session.as_deref() {
        let _ = record_session_trajectory(workspace_root, path);
    }
    let active_trajectory = active_trajectory(workspace_root)?;
    let latest_digest = latest_session
        .as_deref()
        .map(SessionStore::read_events)
        .transpose()?
        .map(|events| summarize_session_events(&events));
    let summary_record = list_memory_records(workspace_root)
        .unwrap_or_default()
        .into_iter()
        .find(|record| record.kind == MemoryKind::Summary.as_str());
    let latest_handoff = latest_session
        .as_deref()
        .map(latest_model_handoff)
        .transpose()?
        .flatten();

    let mut out = String::new();
    out.push_str("Handoff\n\n");
    if let Some(path) = latest_session {
        out.push_str(&format!("Latest session: {}\n\n", path.display()));
    }
    if let Some(trajectory) = active_trajectory.as_ref() {
        out.push_str(&format!("Trajectory: {}\n", trajectory.title));
        if let Some(model) = trajectory.active_model.as_deref() {
            out.push_str(&format!("Active model: {model}\n"));
        }
        if !trajectory.active_files.is_empty() {
            out.push_str(&format!(
                "Active files: {}\n",
                trajectory.active_files.join(", ")
            ));
        }
        out.push('\n');
    }
    if let Some(handoff) = latest_handoff {
        let merged = merge_handoff_with_digest(&handoff.snapshot, latest_digest.as_ref());
        out.push_str(&render_model_handoff_text(&merged));
        out.push_str("\n\n");
    } else if let Some(summary) = summary_record {
        out.push_str("Session summary:\n");
        out.push_str(&summary.body);
        out.push_str("\n\n");
    } else {
        out.push_str("Session summary: none\n\n");
    }
    let recent_tasks = latest_digest
        .as_ref()
        .map(|digest| digest.goals.iter().take(3).cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if !recent_tasks.is_empty() {
        out.push_str("Recent tasks:\n");
        for task in recent_tasks {
            out.push_str("- ");
            out.push_str(&task);
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str("Continue from this context. Preserve the current direction, reuse saved memory, and avoid restarting discovery from scratch.\n");
    Ok(out)
}

pub fn build_memory_recall_text_with_request(
    workspace_root: &Path,
    request: RecallRequest,
) -> Result<String, String> {
    if let Some(path) = SessionStore::latest(workspace_root)? {
        let _ = record_session_trajectory(workspace_root, &path);
    }
    let config = load_config(workspace_root).unwrap_or_default();
    let backend = resolve_selected_memory_backend(workspace_root, &config)?;
    let items = backend.recall(&request)?;
    if items.is_empty() {
        return Ok("none".to_string());
    }

    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for item in items {
        let record = memory_record_from_item(&item);
        let key = format!("{}|{}|{}", record.kind, record.title, record.body);
        if seen.insert(key) {
            deduped.push(record);
        }
        if deduped.len() >= request.limit.max(1) {
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

pub fn build_memory_recall_text(workspace_root: &Path, limit: usize) -> Result<String, String> {
    build_memory_recall_text_with_request(workspace_root, default_recall_request(limit))
}

pub fn list_memory_candidates(
    workspace_root: &Path,
    limit: usize,
) -> Result<Vec<crate::PromotionCandidate>, String> {
    crate::list_promotion_candidates(workspace_root, limit)
}

pub fn promote_memory_candidate(
    workspace_root: &Path,
    candidate_id: i64,
) -> Result<Option<MemoryRecord>, String> {
    let Some(candidate) = load_promotion_candidate(workspace_root, candidate_id)? else {
        return Ok(None);
    };
    let config = load_config(workspace_root).unwrap_or_default();
    let backend = resolve_selected_memory_backend(workspace_root, &config)?;
    let inserted = backend.remember(&candidate.item)?;
    let _ = update_promotion_candidate_status(workspace_root, candidate_id, "promoted");
    Ok(inserted.then(|| memory_record_from_item(&candidate.item)))
}

pub fn dismiss_memory_candidate(workspace_root: &Path, candidate_id: i64) -> Result<bool, String> {
    update_promotion_candidate_status(workspace_root, candidate_id, "dismissed")
}

pub fn maybe_record_prompt_compaction_checkpoint(
    workspace_root: &Path,
    session_path: Option<&Path>,
    session_id: Option<&str>,
    budget_profile: &str,
    kept_summary: &str,
    dropped_signals: &[String],
    truncated_sections: &[String],
    turn_count: usize,
) -> Result<Option<MemoryRecord>, String> {
    if truncated_sections.is_empty() {
        return Ok(None);
    }
    let config = load_config(workspace_root).unwrap_or_default();
    let backend = resolve_selected_memory_backend(workspace_root, &config)?;
    let capabilities = backend
        .capabilities()
        .unwrap_or_else(|_| crate::AmcpBackendCapabilities::minimal());
    if !capabilities.supports_compaction_checkpoints {
        return Ok(None);
    }

    let item = build_amcp_item_with_options(
        "Prompt compaction checkpoint",
        &format!(
            "Prompt context compaction occurred.\nBudget profile: {budget_profile}\nTruncated sections: {}\nKept summary: {kept_summary}",
            truncated_sections.join(", ")
        ),
        &["compaction-checkpoint".to_string(), "prompt-compaction".to_string()],
        session_path,
        &[],
        session_id.map(ToOwned::to_owned),
        BuildAmcpItemOptions {
            id_seed: format!(
                "prompt-compaction|{}|{}|{}",
                session_id.unwrap_or("-"),
                turn_count,
                truncated_sections.join("|")
            ),
            legacy_kind: MemoryKind::Note.as_str().to_string(),
            item_type: "compaction-checkpoint".to_string(),
            retention: session_derived_retention(),
            provenance: ItemProvenance {
                origin: "compaction".to_string(),
                trigger: "prompt-compaction".to_string(),
                source_session: session_id.map(ToOwned::to_owned),
                source_turns: (0..turn_count).collect(),
                derived_from: Vec::new(),
            },
            metadata_extra: Some(json!({
                "budget_profile": budget_profile,
                "kept_summary": kept_summary,
                "dropped_signals": dropped_signals,
                "truncated_sections": truncated_sections,
                "turn_count": turn_count,
            })),
        },
    );
    let inserted = backend.remember(&item)?;
    if inserted {
        maybe_append_compaction_event(
            session_path,
            &item.id,
            budget_profile,
            dropped_signals,
            truncated_sections,
        )?;
    }
    Ok(inserted.then(|| memory_record_from_item(&item)))
}

pub fn maybe_track_skill_candidate_promotion(
    workspace_root: &Path,
    candidate_id: i64,
) -> Result<Option<MemoryRecord>, String> {
    let Some(candidate) = load_skill_candidate(workspace_root, candidate_id)? else {
        return Ok(None);
    };
    let config = load_config(workspace_root).unwrap_or_default();
    let policy = config.auto_promote_policy();
    if policy == AutoPromotePolicy::Off {
        return Ok(None);
    }
    let item = build_amcp_item_with_options(
        &candidate.command_name,
        &format!(
            "Promoted repeated workflow `{}` into a slash command. Sequence: {}",
            candidate.command_name,
            candidate.tool_sequence.join(" -> ")
        ),
        &[
            "trajectory-auto".to_string(),
            "skill-command-promoted".to_string(),
        ],
        None,
        &[],
        None,
        BuildAmcpItemOptions {
            id_seed: format!("skill-command-promoted|{}", candidate.id),
            legacy_kind: MemoryKind::Note.as_str().to_string(),
            item_type: "context".to_string(),
            retention: session_derived_retention(),
            provenance: ItemProvenance {
                origin: "trajectory-auto".to_string(),
                trigger: "skill-command-promoted".to_string(),
                source_session: None,
                source_turns: Vec::new(),
                derived_from: Vec::new(),
            },
            metadata_extra: Some(json!({
                "command_name": candidate.command_name,
                "tool_sequence": candidate.tool_sequence,
            })),
        },
    );
    let backend = resolve_selected_memory_backend(workspace_root, &config)?;
    submit_auto_promotion_item(
        workspace_root,
        policy,
        &*backend,
        "skill-command-promoted",
        "Workflow promoted to reusable command",
        None,
        &item,
    )
}

fn run_auto_promotion_pipeline(
    workspace_root: &Path,
    session_path: &Path,
    events: &[SessionEvent],
    trajectory: Option<&crate::TrajectoryRecord>,
    digest: &SessionDigest,
    config: &crate::LoadedConfig,
    backend: &dyn crate::AmcpMemoryBackend,
) -> Result<Vec<MemoryRecord>, String> {
    let policy = config.auto_promote_policy();
    if policy == AutoPromotePolicy::Off {
        return Ok(Vec::new());
    }

    let mut saved = Vec::new();
    let session_id = SessionStore::session_id_from_path(session_path);
    let file_paths = trajectory
        .map(|record| record.active_files.clone())
        .unwrap_or_default();
    let source_turns = all_event_indexes(events);

    if let Some(item) = build_verified_completion_item(
        workspace_root,
        session_path,
        events,
        digest,
        trajectory,
        &file_paths,
        session_id.clone(),
        source_turns.clone(),
    ) {
        if let Some(record) = submit_auto_promotion_item(
            workspace_root,
            policy,
            backend,
            "verification-passed",
            "Verified successful task completion",
            session_id.as_deref(),
            &item,
        )? {
            saved.push(record);
        }
    }

    if let Some(item) = build_repeated_failure_item(
        workspace_root,
        session_path,
        trajectory,
        &file_paths,
        session_id.clone(),
    )? {
        if let Some(record) = submit_auto_promotion_item(
            workspace_root,
            policy,
            backend,
            "repeated-failure-pattern",
            "Repeated failure pattern detected across sessions",
            session_id.as_deref(),
            &item,
        )? {
            saved.push(record);
        }
    }

    if let Some(item) = build_handoff_consumed_item(
        session_path,
        events,
        trajectory,
        &file_paths,
        session_id.clone(),
    ) {
        if let Some(record) = submit_auto_promotion_item(
            workspace_root,
            policy,
            backend,
            "handoff-consumed",
            "Model handoff context boost was consumed",
            session_id.as_deref(),
            &item,
        )? {
            saved.push(record);
        }
    }

    Ok(saved)
}

fn build_verified_completion_item(
    workspace_root: &Path,
    session_path: &Path,
    events: &[SessionEvent],
    digest: &SessionDigest,
    trajectory: Option<&crate::TrajectoryRecord>,
    file_paths: &[String],
    session_id: Option<String>,
    source_turns: Vec<usize>,
) -> Option<AmcpMemoryItem> {
    let verification = assess_verification(workspace_root, &session_verification_events(events));
    if !verification.requires_verification || !verification.has_verification_after_last_mutation {
        return None;
    }
    let outcome = digest.last_assistant_text.as_deref()?.trim();
    if outcome.is_empty() {
        return None;
    }
    let goal = trajectory
        .map(|record| record.current_goal.clone())
        .or_else(|| digest.goals.first().cloned())
        .unwrap_or_else(|| "Completed task".to_string());
    let body = format!(
        "Goal: {goal}\nOutcome: {outcome}\nFiles: {}",
        if file_paths.is_empty() {
            "-".to_string()
        } else {
            file_paths.join(", ")
        }
    );
    Some(build_amcp_item_with_options(
        &goal,
        &body,
        &[
            "trajectory-auto".to_string(),
            "verification-passed".to_string(),
        ],
        Some(session_path),
        file_paths,
        session_id.clone(),
        BuildAmcpItemOptions {
            id_seed: format!(
                "verification-passed|{}",
                session_id.as_deref().unwrap_or("-")
            ),
            legacy_kind: MemoryKind::Note.as_str().to_string(),
            item_type: "context".to_string(),
            retention: session_derived_retention(),
            provenance: ItemProvenance {
                origin: "trajectory-auto".to_string(),
                trigger: "verification-passed".to_string(),
                source_session: session_id,
                source_turns,
                derived_from: Vec::new(),
            },
            metadata_extra: Some(json!({
                "goal": goal,
                "outcome": outcome,
                "files_touched": file_paths,
            })),
        },
    ))
}

fn build_repeated_failure_item(
    workspace_root: &Path,
    session_path: &Path,
    trajectory: Option<&crate::TrajectoryRecord>,
    file_paths: &[String],
    session_id: Option<String>,
) -> Result<Option<AmcpMemoryItem>, String> {
    let Some(failure) = trajectory.and_then(|record| record.latest_failure.clone()) else {
        return Ok(None);
    };
    let normalized = normalize_failure_text(&failure);
    if normalized.is_empty() {
        return Ok(None);
    }
    if count_matching_failure_sessions(workspace_root, &normalized)? < 3 {
        return Ok(None);
    }
    let body = format!(
        "Repeated failure pattern: {failure}\nNormalized class: {normalized}\nFiles: {}",
        if file_paths.is_empty() {
            "-".to_string()
        } else {
            file_paths.join(", ")
        }
    );
    Ok(Some(build_amcp_item_with_options(
        &failure,
        &body,
        &[
            "trajectory-auto".to_string(),
            "repeated-failure-pattern".to_string(),
        ],
        Some(session_path),
        file_paths,
        session_id.clone(),
        BuildAmcpItemOptions {
            id_seed: format!("repeated-failure-pattern|{normalized}"),
            legacy_kind: MemoryKind::Error.as_str().to_string(),
            item_type: "error".to_string(),
            retention: session_derived_retention(),
            provenance: ItemProvenance {
                origin: "trajectory-auto".to_string(),
                trigger: "repeated-failure-pattern".to_string(),
                source_session: session_id,
                source_turns: Vec::new(),
                derived_from: Vec::new(),
            },
            metadata_extra: Some(json!({
                "normalized_failure": normalized,
                "files_touched": file_paths,
            })),
        },
    )))
}

fn build_handoff_consumed_item(
    session_path: &Path,
    events: &[SessionEvent],
    trajectory: Option<&crate::TrajectoryRecord>,
    file_paths: &[String],
    session_id: Option<String>,
) -> Option<AmcpMemoryItem> {
    let handoff_index = events
        .iter()
        .rposition(|event| event.kind == "model_handoff")?;
    let consumed_index =
        events
            .iter()
            .enumerate()
            .skip(handoff_index + 1)
            .find_map(|(index, event)| {
                matches!(event.kind.as_str(), "agent_result" | "prompt_result").then_some(index)
            })?;
    let goal = trajectory
        .map(|record| record.current_goal.clone())
        .unwrap_or_else(|| "Model handoff".to_string());
    let body = format!(
        "Model handoff context boost was consumed.\nGoal: {goal}\nFiles: {}",
        if file_paths.is_empty() {
            "-".to_string()
        } else {
            file_paths.join(", ")
        }
    );
    Some(build_amcp_item_with_options(
        &goal,
        &body,
        &[
            "trajectory-auto".to_string(),
            "handoff-consumed".to_string(),
        ],
        Some(session_path),
        file_paths,
        session_id.clone(),
        BuildAmcpItemOptions {
            id_seed: format!(
                "handoff-consumed|{}|{}",
                session_id.as_deref().unwrap_or("-"),
                handoff_index
            ),
            legacy_kind: MemoryKind::Note.as_str().to_string(),
            item_type: "context".to_string(),
            retention: session_derived_retention(),
            provenance: ItemProvenance {
                origin: "handoff-derived".to_string(),
                trigger: "handoff-consumed".to_string(),
                source_session: session_id,
                source_turns: vec![handoff_index, consumed_index],
                derived_from: Vec::new(),
            },
            metadata_extra: Some(json!({
                "handoff_event_index": handoff_index,
                "consumed_event_index": consumed_index,
                "files_touched": file_paths,
            })),
        },
    ))
}

fn submit_auto_promotion_item(
    workspace_root: &Path,
    policy: AutoPromotePolicy,
    backend: &dyn crate::AmcpMemoryBackend,
    trigger: &str,
    summary: &str,
    session_id: Option<&str>,
    item: &AmcpMemoryItem,
) -> Result<Option<MemoryRecord>, String> {
    match policy {
        AutoPromotePolicy::Off => Ok(None),
        AutoPromotePolicy::Suggest => {
            let _ = store_promotion_candidate(
                workspace_root,
                trigger,
                summary,
                session_id,
                item,
                "pending",
            )?;
            Ok(None)
        }
        AutoPromotePolicy::Auto => {
            let inserted = backend.remember(item)?;
            let _ = store_promotion_candidate(
                workspace_root,
                trigger,
                summary,
                session_id,
                item,
                "auto-saved",
            );
            Ok(inserted.then(|| memory_record_from_item(item)))
        }
    }
}

fn maybe_append_compaction_event(
    session_path: Option<&Path>,
    fingerprint: &str,
    budget_profile: &str,
    dropped_signals: &[String],
    truncated_sections: &[String],
) -> Result<(), String> {
    let Some(session_path) = session_path else {
        return Ok(());
    };
    let events = SessionStore::read_events(session_path).unwrap_or_default();
    if events.iter().any(|event| {
        event.kind == "prompt_compaction"
            && event.payload.get("fingerprint").and_then(Value::as_str) == Some(fingerprint)
    }) {
        return Ok(());
    }
    let mut file = OpenOptions::new()
        .append(true)
        .open(session_path)
        .map_err(|err| err.to_string())?;
    let line = serde_json::to_string(&SessionEvent {
        ts_ms: now_ms(),
        kind: "prompt_compaction".to_string(),
        payload: json!({
            "fingerprint": fingerprint,
            "budget_profile": budget_profile,
            "dropped_signals": dropped_signals,
            "truncated_sections": truncated_sections,
        }),
    })
    .map_err(|err| err.to_string())?;
    writeln!(file, "{line}").map_err(|err| err.to_string())
}

pub fn build_model_handoff_snapshot(
    workspace_root: &Path,
    session_path: &Path,
    from_model: Option<&str>,
    to_model: &str,
) -> Result<ModelHandoffSnapshot, String> {
    let events = SessionStore::read_events(session_path)?;
    let digest = summarize_session_events(&events);
    let records = list_memory_records(workspace_root).unwrap_or_default();
    let active_trajectory = active_trajectory(workspace_root)?;

    let latest_summary = records
        .iter()
        .find(|record| record.kind == MemoryKind::Summary.as_str());
    let current_goal = digest
        .goals
        .first()
        .cloned()
        .unwrap_or_else(|| "Continue the current task".to_string());

    let recent_work_summary = if digest.summary.trim() != "No significant events captured." {
        truncate_text(&digest.summary, 600)
    } else if let Some(summary) = latest_summary {
        truncate_text(&summary.body, 600)
    } else {
        "No recent work summary captured.".to_string()
    };

    let open_tasks = digest.goals.iter().take(3).cloned().collect::<Vec<_>>();
    let recent_errors = digest.errors.iter().take(2).cloned().collect::<Vec<_>>();
    let suggested_next_step = open_tasks
        .first()
        .map(|task| format!("Continue with: {task}"))
        .unwrap_or_else(|| {
            if current_goal == "Continue the current task" {
                "Read the latest context and continue.".to_string()
            } else {
                format!("Continue the current goal: {current_goal}")
            }
        });

    Ok(ModelHandoffSnapshot {
        from_model: from_model.map(ToOwned::to_owned),
        to_model: to_model.to_string(),
        current_goal: active_trajectory
            .as_ref()
            .map(|trajectory| trajectory.current_goal.clone())
            .unwrap_or(current_goal),
        recent_work_summary: active_trajectory
            .as_ref()
            .map(|trajectory| trajectory.recent_work_summary.clone())
            .unwrap_or(recent_work_summary),
        active_files: active_trajectory
            .as_ref()
            .map(|trajectory| trajectory.active_files.clone())
            .unwrap_or_default(),
        latest_attempt: active_trajectory
            .as_ref()
            .and_then(|trajectory| trajectory.latest_attempt.clone()),
        latest_failure: active_trajectory
            .as_ref()
            .and_then(|trajectory| trajectory.latest_failure.clone()),
        last_verification: active_trajectory
            .as_ref()
            .and_then(|trajectory| trajectory.last_verification.clone()),
        open_tasks: if let Some(trajectory) = active_trajectory.as_ref() {
            if trajectory.open_tasks.is_empty() {
                open_tasks
            } else {
                trajectory.open_tasks.clone()
            }
        } else {
            open_tasks
        },
        recent_errors: if let Some(trajectory) = active_trajectory.as_ref() {
            if trajectory.recent_errors.is_empty() {
                recent_errors
            } else {
                trajectory.recent_errors.clone()
            }
        } else {
            recent_errors
        },
        suggested_next_step: active_trajectory
            .as_ref()
            .map(|trajectory| trajectory.next_step.clone())
            .unwrap_or(suggested_next_step),
        session_path: Some(session_path.display().to_string()),
    })
}

pub fn latest_model_handoff(session_path: &Path) -> Result<Option<StoredModelHandoff>, String> {
    let events = SessionStore::read_events(session_path)?;
    for event in events.into_iter().rev() {
        if event.kind != "model_handoff" {
            continue;
        }
        let snapshot = serde_json::from_value::<ModelHandoffSnapshot>(event.payload)
            .map_err(|err| err.to_string())?;
        return Ok(Some(StoredModelHandoff {
            ts_ms: event.ts_ms,
            snapshot,
        }));
    }
    Ok(None)
}

pub fn pending_model_handoff(session_path: &Path) -> Result<Option<StoredModelHandoff>, String> {
    let events = SessionStore::read_events(session_path)?;
    let Some(index) = events
        .iter()
        .rposition(|event| event.kind == "model_handoff")
    else {
        return Ok(None);
    };

    let handoff_event = &events[index];
    if events.iter().skip(index + 1).any(|event| {
        matches!(
            event.kind.as_str(),
            "agent_result" | "prompt_result" | "model_probe_failed"
        )
    }) {
        return Ok(None);
    }

    let snapshot = serde_json::from_value::<ModelHandoffSnapshot>(handoff_event.payload.clone())
        .map_err(|err| err.to_string())?;
    Ok(Some(StoredModelHandoff {
        ts_ms: handoff_event.ts_ms,
        snapshot,
    }))
}

pub fn render_model_handoff_text(snapshot: &ModelHandoffSnapshot) -> String {
    let mut out = String::new();
    out.push_str("Current goal:\n");
    out.push_str(&snapshot.current_goal);
    out.push_str("\n\nDone recently:\n");
    out.push_str(&snapshot.recent_work_summary);
    out.push('\n');
    if !snapshot.active_files.is_empty() {
        out.push_str("\nActive files:\n");
        for path in &snapshot.active_files {
            out.push_str("- ");
            out.push_str(path);
            out.push('\n');
        }
    }
    if let Some(attempt) = snapshot.latest_attempt.as_deref() {
        out.push_str("\nLatest attempt:\n");
        out.push_str(attempt);
        out.push('\n');
    }
    if let Some(failure) = snapshot.latest_failure.as_deref() {
        out.push_str("\nLatest failure:\n");
        out.push_str(failure);
        out.push('\n');
    }
    if let Some(verification) = snapshot.last_verification.as_deref() {
        out.push_str("\nLast verification:\n");
        out.push_str(verification);
        out.push('\n');
    }
    if !snapshot.open_tasks.is_empty() {
        out.push_str("\nOpen tasks:\n");
        for task in &snapshot.open_tasks {
            out.push_str("- ");
            out.push_str(task);
            out.push('\n');
        }
    }
    if !snapshot.recent_errors.is_empty() {
        out.push_str("\nWarnings:\n");
        for error in &snapshot.recent_errors {
            out.push_str("- ");
            out.push_str(error);
            out.push('\n');
        }
    }
    out.push_str("\nNext step:\n");
    out.push_str(&snapshot.suggested_next_step);
    out
}

fn merge_handoff_with_digest(
    snapshot: &ModelHandoffSnapshot,
    digest: Option<&SessionDigest>,
) -> ModelHandoffSnapshot {
    let Some(digest) = digest else {
        return snapshot.clone();
    };

    let mut merged = snapshot.clone();
    if !digest.summary.trim().is_empty() {
        merged.recent_work_summary = truncate_text(&digest.summary, 600);
    }
    merged.open_tasks = digest.goals.iter().take(3).cloned().collect();
    merged.recent_errors = digest.errors.iter().take(2).cloned().collect();
    if let Some(task) = merged.open_tasks.first() {
        merged.suggested_next_step = format!("Continue with: {task}");
    }
    merged
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
                    if !trimmed.is_empty()
                        && !trimmed.starts_with('/')
                        && !is_low_signal_user_text(trimmed)
                    {
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

fn is_low_signal_user_text(text: &str) -> bool {
    let normalized = text.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return true;
    }
    if normalized.chars().count() <= 2 {
        return true;
    }
    let low_signal = [
        "hi",
        "hello",
        "hey",
        "안녕",
        "안녕하세요",
        "ㅎㅇ",
        "ㅂㅇ",
        "test",
        "ping",
        "pong",
    ];
    if low_signal.iter().any(|item| normalized == *item) {
        return true;
    }
    normalized.contains("모델명")
        || normalized.contains("네 모델")
        || normalized.contains("what model")
        || normalized.contains("your model")
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
        append_memory_record, build_handoff_text, build_memory_recall_text,
        build_model_handoff_snapshot, build_resume_text, latest_model_handoff,
        list_memory_candidates, list_memory_records, pending_model_handoff,
        promote_memory_candidate, save_session_memory_bundle, save_session_summary,
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
        let handoff_snapshot = build_model_handoff_snapshot(
            &workspace,
            &session_path,
            Some("anthropic/claude-sonnet-4-6"),
            "openai/gpt-4.1-mini",
        )
        .unwrap();
        fs::write(
            &session_path,
            [
                serde_json::to_string(&SessionEvent {
                    ts_ms: 1,
                    kind: "user_input".to_string(),
                    payload: json!({ "text": "continue provider integration" }),
                })
                .unwrap(),
                serde_json::to_string(&SessionEvent {
                    ts_ms: 2,
                    kind: "model_handoff".to_string(),
                    payload: serde_json::to_value(handoff_snapshot).unwrap(),
                })
                .unwrap(),
            ]
            .join("\n"),
        )
        .unwrap();
        let _ = save_session_summary(&workspace, &session_path).unwrap();

        let resume = build_resume_text(&workspace).unwrap();
        let handoff = build_handoff_text(&workspace).unwrap();

        assert!(resume.contains("latest_session:"));
        assert!(resume.contains("active_model: openai/gpt-4.1-mini"));
        assert!(handoff.contains("Handoff"));
        assert!(handoff.contains("Current goal:"));
        assert!(handoff.contains("Next step:"));

        cleanup(&workspace);
    }

    #[test]
    fn ignores_low_signal_greetings_and_model_questions_in_digest() {
        let digest = summarize_session_events(&[
            SessionEvent {
                ts_ms: 1,
                kind: "user_input".to_string(),
                payload: json!({ "text": "hi" }),
            },
            SessionEvent {
                ts_ms: 2,
                kind: "user_input".to_string(),
                payload: json!({ "text": "네 모델명이 뭐야?" }),
            },
            SessionEvent {
                ts_ms: 3,
                kind: "user_input".to_string(),
                payload: json!({ "text": "현재 프로젝트 구조를 설명해" }),
            },
        ]);

        assert_eq!(
            digest.goals,
            vec!["현재 프로젝트 구조를 설명해".to_string()]
        );
    }

    #[test]
    fn builds_model_handoff_snapshot_from_session_and_memory() {
        let workspace = temp_workspace("memory-model-handoff");
        let session_path = workspace.join(".harness/sessions/session-test.jsonl");
        fs::write(
            &session_path,
            [
                serde_json::to_string(&SessionEvent {
                    ts_ms: 1,
                    kind: "user_input".to_string(),
                    payload: json!({ "text": "finish provider switching UX" }),
                })
                .unwrap(),
                serde_json::to_string(&SessionEvent {
                    ts_ms: 2,
                    kind: "tool_error".to_string(),
                    payload: json!({ "error": "missing handoff snapshot" }),
                })
                .unwrap(),
            ]
            .join("\n"),
        )
        .unwrap();
        append_memory_record(
            &workspace,
            MemoryKind::Task,
            "finish provider switching UX",
            "finish provider switching UX",
            &[],
            Some(&session_path),
        )
        .unwrap();

        let snapshot = build_model_handoff_snapshot(
            &workspace,
            &session_path,
            Some("anthropic/claude-sonnet-4-6"),
            "openai/gpt-4.1-mini",
        )
        .unwrap();

        assert_eq!(
            snapshot.from_model.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(snapshot.to_model, "openai/gpt-4.1-mini");
        assert!(snapshot
            .current_goal
            .contains("finish provider switching UX"));
        assert!(!snapshot.open_tasks.is_empty());
        assert!(!snapshot.recent_errors.is_empty());

        cleanup(&workspace);
    }

    #[test]
    fn tracks_pending_model_handoff_until_completion() {
        let workspace = temp_workspace("memory-pending-handoff");
        let session_path = workspace.join(".harness/sessions/session-test.jsonl");
        fs::write(
            &session_path,
            serde_json::to_string(&SessionEvent {
                ts_ms: 1,
                kind: "user_input".to_string(),
                payload: json!({ "text": "finish handoff wiring" }),
            })
            .unwrap(),
        )
        .unwrap();
        let snapshot = build_model_handoff_snapshot(
            &workspace,
            &session_path,
            Some("anthropic/claude-sonnet-4-6"),
            "openai/gpt-4.1-mini",
        )
        .unwrap();
        fs::write(
            &session_path,
            serde_json::to_string(&SessionEvent {
                ts_ms: 1,
                kind: "model_handoff".to_string(),
                payload: serde_json::to_value(&snapshot).unwrap(),
            })
            .unwrap(),
        )
        .unwrap();

        let pending = pending_model_handoff(&session_path).unwrap().unwrap();
        assert_eq!(pending.snapshot.to_model, "openai/gpt-4.1-mini");
        let latest = latest_model_handoff(&session_path).unwrap().unwrap();
        assert_eq!(latest.snapshot.current_goal, snapshot.current_goal);

        fs::write(
            &session_path,
            [
                serde_json::to_string(&SessionEvent {
                    ts_ms: 1,
                    kind: "model_handoff".to_string(),
                    payload: serde_json::to_value(&snapshot).unwrap(),
                })
                .unwrap(),
                serde_json::to_string(&SessionEvent {
                    ts_ms: 2,
                    kind: "agent_result".to_string(),
                    payload: json!({ "text": "done" }),
                })
                .unwrap(),
            ]
            .join("\n"),
        )
        .unwrap();

        assert!(pending_model_handoff(&session_path).unwrap().is_none());

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
    fn suggest_auto_promotion_creates_pending_candidate_and_can_promote_it() {
        let workspace = temp_workspace("memory-auto-promote");
        let session_path = workspace.join(".harness/sessions/session-test.jsonl");
        fs::write(
            &session_path,
            [
                serde_json::to_string(&SessionEvent {
                    ts_ms: 1,
                    kind: "user_input".to_string(),
                    payload: json!({ "text": "finish the provider refactor" }),
                })
                .unwrap(),
                serde_json::to_string(&SessionEvent {
                    ts_ms: 2,
                    kind: "agent_tool".to_string(),
                    payload: json!({
                        "name": "edit",
                        "arguments": { "path": "src/lib.rs", "needle": "old", "replacement": "new" },
                        "summary": "updated provider wiring"
                    }),
                })
                .unwrap(),
                serde_json::to_string(&SessionEvent {
                    ts_ms: 3,
                    kind: "tool_result".to_string(),
                    payload: json!({ "command": "cargo test --workspace", "summary": "tests passed" }),
                })
                .unwrap(),
                serde_json::to_string(&SessionEvent {
                    ts_ms: 4,
                    kind: "agent_result".to_string(),
                    payload: json!({ "text": "provider refactor verified and complete" }),
                })
                .unwrap(),
            ]
            .join("\n"),
        )
        .unwrap();

        let bundle = save_session_memory_bundle(&workspace, &session_path).unwrap();
        assert!(bundle.pending_candidates >= 1);

        let candidates = list_memory_candidates(&workspace, 8).unwrap();
        let verification_candidate = candidates
            .iter()
            .find(|candidate| candidate.trigger == "verification-passed")
            .unwrap();
        let promoted = promote_memory_candidate(&workspace, verification_candidate.id).unwrap();
        assert!(promoted.is_some());
        assert!(list_memory_candidates(&workspace, 8).unwrap().is_empty());

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
