use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};

use reqwest::blocking::Client;
use reqwest::{header, Method, StatusCode};
use rusqlite::{params, Connection};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::config::LoadedConfig;
use crate::trajectory::memory_db_path;

const DEFAULT_LOCAL_SCOPE_ID: &str = "local-workspace";
const APP_ID: &str = "3122";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AmcpScope {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AmcpOrigin {
    pub agent_id: String,
    pub app_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_timestamp_string")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AmcpRetention {
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AmcpSourceRef {
    pub kind: String,
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AmcpMemoryItem {
    pub id: String,
    pub content: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub scope: AmcpScope,
    pub origin: AmcpOrigin,
    pub visibility: String,
    pub retention: AmcpRetention,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default)]
    pub source_refs: Vec<AmcpSourceRef>,
    pub energy: f32,
    #[serde(
        default = "default_empty_string",
        deserialize_with = "deserialize_timestamp_string_or_empty"
    )]
    pub created_at: String,
    #[serde(
        default = "default_empty_string",
        deserialize_with = "deserialize_timestamp_string_or_empty"
    )]
    pub updated_at: String,
}

impl AmcpMemoryItem {
    fn normalize_timestamps(mut self) -> Self {
        let fallback = self
            .origin
            .timestamp
            .clone()
            .unwrap_or_else(default_timestamp_string);
        if self.created_at.is_empty() {
            self.created_at = if self.updated_at.is_empty() {
                fallback.clone()
            } else {
                self.updated_at.clone()
            };
        }
        if self.updated_at.is_empty() {
            self.updated_at = if self.created_at.is_empty() {
                fallback
            } else {
                self.created_at.clone()
            };
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AmcpSessionRef {
    pub key: String,
    pub item_count: usize,
    #[serde(deserialize_with = "deserialize_timestamp_string")]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RecallContextBudget {
    pub max_tokens: usize,
    pub priority: String,
    #[serde(default)]
    pub active_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RecallRequest {
    #[serde(default)]
    pub query: Option<String>,
    pub limit: usize,
    pub token_budget: usize,
    #[serde(default)]
    pub context_budget: Option<RecallContextBudget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AmcpBackendCapabilities {
    #[serde(default)]
    pub supports_budget_recall: bool,
    #[serde(default)]
    pub supports_compaction_checkpoints: bool,
    #[serde(default)]
    pub supports_provenance: bool,
    #[serde(default = "default_capability_max_item_size")]
    pub max_item_size: usize,
    #[serde(default)]
    pub retention_policies: Vec<String>,
}

impl AmcpBackendCapabilities {
    pub fn local_full() -> Self {
        Self {
            supports_budget_recall: true,
            supports_compaction_checkpoints: true,
            supports_provenance: true,
            max_item_size: 65_536,
            retention_policies: vec![
                "persistent".to_string(),
                "session-derived".to_string(),
                "ephemeral".to_string(),
            ],
        }
    }

    pub fn minimal() -> Self {
        Self {
            supports_budget_recall: false,
            supports_compaction_checkpoints: false,
            supports_provenance: false,
            max_item_size: 65_536,
            retention_policies: vec!["persistent".to_string()],
        }
    }
}

fn default_capability_max_item_size() -> usize {
    65_536
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryBackendKind {
    LocalAmcp,
    NexusCloud,
    ThirdPartyAmcp,
}

impl MemoryBackendKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" | "local-amcp" => Some(Self::LocalAmcp),
            "nexus" | "nexus-cloud" => Some(Self::NexusCloud),
            "third-party" | "third-party-amcp" => Some(Self::ThirdPartyAmcp),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalAmcp => "local-amcp",
            Self::NexusCloud => "nexus-cloud",
            Self::ThirdPartyAmcp => "third-party-amcp",
        }
    }
}

impl std::fmt::Display for MemoryBackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub trait AmcpMemoryBackend {
    fn kind(&self) -> MemoryBackendKind;
    fn capabilities(&self) -> Result<AmcpBackendCapabilities, String>;
    fn remember(&self, item: &AmcpMemoryItem) -> Result<bool, String>;
    fn recall(&self, request: &RecallRequest) -> Result<Vec<AmcpMemoryItem>, String>;
    fn sessions(&self) -> Result<Vec<AmcpSessionRef>, String>;
    fn session(&self, session_key: &str) -> Result<Vec<AmcpMemoryItem>, String>;
    fn export_items(&self) -> Result<Vec<AmcpMemoryItem>, String>;
    fn import_items(&self, items: &[AmcpMemoryItem]) -> Result<usize, String>;
    fn delete(&self, ids: &[String]) -> Result<usize, String>;
}

pub fn selected_backend_kind(config: &LoadedConfig) -> MemoryBackendKind {
    MemoryBackendKind::parse(config.memory_backend()).unwrap_or(MemoryBackendKind::LocalAmcp)
}

pub fn resolve_selected_memory_backend(
    workspace_root: &Path,
    config: &LoadedConfig,
) -> Result<Box<dyn AmcpMemoryBackend>, String> {
    resolve_memory_backend_kind(workspace_root, config, selected_backend_kind(config))
}

pub fn resolve_memory_backend_kind(
    workspace_root: &Path,
    config: &LoadedConfig,
    kind: MemoryBackendKind,
) -> Result<Box<dyn AmcpMemoryBackend>, String> {
    match kind {
        MemoryBackendKind::LocalAmcp => Ok(Box::new(LocalAmcpBackend::new(workspace_root))),
        MemoryBackendKind::NexusCloud => {
            Ok(Box::new(NexusCloudBackend::new(workspace_root, config)))
        }
        MemoryBackendKind::ThirdPartyAmcp => {
            Ok(Box::new(ThirdPartyAmcpBackend::new(workspace_root, config)))
        }
    }
}

pub fn export_items_jsonl(items: &[AmcpMemoryItem]) -> Result<String, String> {
    items
        .iter()
        .map(|item| serde_json::to_string(item).map_err(|err| err.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map(|lines| {
            if lines.is_empty() {
                String::new()
            } else {
                format!("{}\n", lines.join("\n"))
            }
        })
}

pub fn parse_items_jsonl(input: &str) -> Result<Vec<AmcpMemoryItem>, String> {
    let mut items = Vec::new();
    for line in input.lines().filter(|line| !line.trim().is_empty()) {
        items.push(
            serde_json::from_str::<AmcpMemoryItem>(line)
                .map_err(|err| err.to_string())?
                .normalize_timestamps(),
        );
    }
    Ok(items)
}

pub fn export_backend_jsonl(
    workspace_root: &Path,
    config: &LoadedConfig,
    kind: MemoryBackendKind,
) -> Result<String, String> {
    let backend = resolve_memory_backend_kind(workspace_root, config, kind)?;
    export_items_jsonl(&backend.export_items()?)
}

pub fn import_backend_jsonl(
    workspace_root: &Path,
    config: &LoadedConfig,
    kind: MemoryBackendKind,
    input: &str,
) -> Result<usize, String> {
    let backend = resolve_memory_backend_kind(workspace_root, config, kind)?;
    backend.import_items(&parse_items_jsonl(input)?)
}

pub fn migrate_backend_items(
    workspace_root: &Path,
    config: &LoadedConfig,
    from: MemoryBackendKind,
    to: MemoryBackendKind,
) -> Result<usize, String> {
    let source = resolve_memory_backend_kind(workspace_root, config, from)?;
    let target = resolve_memory_backend_kind(workspace_root, config, to)?;
    target.import_items(&source.export_items()?)
}

fn deserialize_timestamp_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    normalize_timestamp_value(value)
        .ok_or_else(|| serde::de::Error::custom("invalid timestamp value"))
}

fn deserialize_timestamp_string_or_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(String::new());
    }
    normalize_timestamp_value(value)
        .ok_or_else(|| serde::de::Error::custom("invalid timestamp value"))
}

fn default_empty_string() -> String {
    String::new()
}

fn default_timestamp_string() -> String {
    "1970-01-01T00:00:00Z".to_string()
}

fn deserialize_optional_timestamp_string<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(None);
    }
    normalize_timestamp_value(value)
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom("invalid timestamp value"))
}

fn normalize_timestamp_value(value: Value) -> Option<String> {
    match value {
        Value::String(text) => normalize_timestamp_string(&text),
        Value::Number(number) => number
            .as_u64()
            .map(|millis| iso_timestamp_from_millis(millis as u128)),
        _ => None,
    }
}

fn normalize_timestamp_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(millis) = trimmed.parse::<u128>() {
        return Some(iso_timestamp_from_millis(millis));
    }

    OffsetDateTime::parse(trimmed, &Rfc3339)
        .ok()
        .and_then(|timestamp| timestamp.format(&Rfc3339).ok())
}

pub fn iso_timestamp_from_millis(millis: u128) -> String {
    let nanos = (millis.min(i128::MAX as u128) as i128) * 1_000_000;
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .ok()
        .and_then(|timestamp| timestamp.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

pub fn current_timestamp_iso() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    iso_timestamp_from_millis(millis)
}

pub fn timestamp_millis(value: &str) -> Option<u128> {
    normalize_timestamp_string(value).and_then(|normalized| {
        OffsetDateTime::parse(&normalized, &Rfc3339)
            .ok()
            .map(|timestamp| timestamp.unix_timestamp_nanos().max(0) as u128 / 1_000_000)
    })
}

pub fn default_local_scope() -> AmcpScope {
    AmcpScope {
        kind: "user".to_string(),
        id: DEFAULT_LOCAL_SCOPE_ID.to_string(),
    }
}

pub fn default_local_origin(session_id: Option<&str>, timestamp: u128) -> AmcpOrigin {
    AmcpOrigin {
        agent_id: env::var("HARNESS_AGENT_ID").unwrap_or_else(|_| "3122-harness".to_string()),
        app_id: APP_ID.to_string(),
        session_id: session_id.map(ToOwned::to_owned),
        timestamp: Some(iso_timestamp_from_millis(timestamp)),
    }
}

pub fn default_private_retention() -> AmcpRetention {
    AmcpRetention {
        mode: "persistent".to_string(),
    }
}

pub fn session_derived_retention() -> AmcpRetention {
    AmcpRetention {
        mode: "session-derived".to_string(),
    }
}

pub fn default_semantic_metadata(
    title: &str,
    legacy_kind: &str,
    session_path: Option<&str>,
) -> Value {
    json!({
        "title": title,
        "legacy_kind": legacy_kind,
        "confidence": 1.0,
        "inference_basis": "session_promotion",
        "evidence_span": session_path,
        "decay_rate": 0.0,
        "domain": "coding-harness",
        "valence": "neutral",
        "time_scope": "session",
        "relations": []
    })
}

pub fn source_ref_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

struct LocalAmcpBackend {
    workspace_root: PathBuf,
}

impl LocalAmcpBackend {
    fn new(workspace_root: &Path) -> Self {
        Self {
            workspace_root: workspace_root.to_path_buf(),
        }
    }

    fn open(&self) -> Result<Connection, String> {
        let harness_dir = self.workspace_root.join(".harness");
        std::fs::create_dir_all(&harness_dir).map_err(|err| err.to_string())?;
        let connection = Connection::open(memory_db_path(&self.workspace_root))
            .map_err(|err| err.to_string())?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS amcp_memory_items (
                   id TEXT PRIMARY KEY,
                   content TEXT NOT NULL,
                   type TEXT NOT NULL,
                   scope_json TEXT NOT NULL,
                   origin_json TEXT NOT NULL,
                   visibility TEXT NOT NULL,
                   retention_json TEXT NOT NULL,
                   tags_json TEXT NOT NULL,
                   metadata_json TEXT NOT NULL,
                   source_refs_json TEXT NOT NULL,
                   energy REAL NOT NULL,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL
                 );
                 CREATE VIRTUAL TABLE IF NOT EXISTS amcp_memory_fts USING fts5(
                   id UNINDEXED,
                   content,
                   type,
                   tags,
                   title
                 );",
            )
            .map_err(|err| err.to_string())?;
        Ok(connection)
    }

    fn sync_fts(&self, connection: &Connection, item: &AmcpMemoryItem) -> Result<(), String> {
        connection
            .execute("DELETE FROM amcp_memory_fts WHERE id = ?1", [&item.id])
            .map_err(|err| err.to_string())?;
        connection
            .execute(
                "INSERT INTO amcp_memory_fts (id, content, type, tags, title)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    item.id,
                    item.content,
                    item.item_type,
                    item.tags.join(" "),
                    metadata_title(item)
                ],
            )
            .map_err(|err| err.to_string())?;
        Ok(())
    }
}

impl AmcpMemoryBackend for LocalAmcpBackend {
    fn kind(&self) -> MemoryBackendKind {
        MemoryBackendKind::LocalAmcp
    }

    fn capabilities(&self) -> Result<AmcpBackendCapabilities, String> {
        Ok(AmcpBackendCapabilities::local_full())
    }

    fn remember(&self, item: &AmcpMemoryItem) -> Result<bool, String> {
        let connection = self.open()?;
        let existed = connection
            .query_row(
                "SELECT 1 FROM amcp_memory_items WHERE id = ?1 LIMIT 1",
                [&item.id],
                |_| Ok(()),
            )
            .map(|_| true)
            .or_else(|err| {
                if matches!(err, rusqlite::Error::QueryReturnedNoRows) {
                    Ok(false)
                } else {
                    Err(err)
                }
            })
            .map_err(|err: rusqlite::Error| err.to_string())?;
        connection
            .execute(
                "INSERT INTO amcp_memory_items (
                   id, content, type, scope_json, origin_json, visibility, retention_json,
                   tags_json, metadata_json, source_refs_json, energy, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(id) DO UPDATE SET
                   content=excluded.content,
                   type=excluded.type,
                   scope_json=excluded.scope_json,
                   origin_json=excluded.origin_json,
                   visibility=excluded.visibility,
                   retention_json=excluded.retention_json,
                   tags_json=excluded.tags_json,
                   metadata_json=excluded.metadata_json,
                   source_refs_json=excluded.source_refs_json,
                   energy=excluded.energy,
                   updated_at=excluded.updated_at",
                params![
                    item.id,
                    item.content,
                    item.item_type,
                    serde_json::to_string(&item.scope).map_err(|err| err.to_string())?,
                    serde_json::to_string(&item.origin).map_err(|err| err.to_string())?,
                    item.visibility,
                    serde_json::to_string(&item.retention).map_err(|err| err.to_string())?,
                    serde_json::to_string(&item.tags).map_err(|err| err.to_string())?,
                    serde_json::to_string(&item.metadata).map_err(|err| err.to_string())?,
                    serde_json::to_string(&item.source_refs).map_err(|err| err.to_string())?,
                    item.energy,
                    timestamp_millis(&item.created_at).unwrap_or(0) as i64,
                    timestamp_millis(&item.updated_at).unwrap_or(0) as i64,
                ],
            )
            .map_err(|err| err.to_string())?;
        self.sync_fts(&connection, item)?;
        Ok(!existed)
    }

    fn recall(&self, request: &RecallRequest) -> Result<Vec<AmcpMemoryItem>, String> {
        let connection = self.open()?;
        let limit = request.limit.max(1) as i64;
        let trimmed = request.query.as_deref().unwrap_or_default().trim();
        let mut items = Vec::new();
        if trimmed.is_empty() {
            let mut statement = connection
                .prepare(
                    "SELECT id, content, type, scope_json, origin_json, visibility, retention_json,
                            tags_json, metadata_json, source_refs_json, energy, created_at, updated_at
                     FROM amcp_memory_items
                     ORDER BY updated_at DESC, energy DESC
                     LIMIT ?1",
                )
                .map_err(|err| err.to_string())?;
            let rows = statement
                .query_map([limit], row_to_item)
                .map_err(|err| err.to_string())?;
            for row in rows {
                items.push(row.map_err(|err| err.to_string())?);
            }
            return Ok(reorder_items_for_budget(items, request));
        }

        let mut statement = connection
            .prepare(
                "SELECT i.id, i.content, i.type, i.scope_json, i.origin_json, i.visibility,
                        i.retention_json, i.tags_json, i.metadata_json, i.source_refs_json,
                        i.energy, i.created_at, i.updated_at
                 FROM amcp_memory_fts f
                 JOIN amcp_memory_items i ON i.id = f.id
                 WHERE amcp_memory_fts MATCH ?1
                 ORDER BY i.energy DESC, i.updated_at DESC
                 LIMIT ?2",
            )
            .map_err(|err| err.to_string())?;
        let rows = statement
            .query_map(params![trimmed, limit], row_to_item)
            .map_err(|err| err.to_string())?;
        for row in rows {
            items.push(row.map_err(|err| err.to_string())?);
        }
        Ok(reorder_items_for_budget(items, request))
    }

    fn sessions(&self) -> Result<Vec<AmcpSessionRef>, String> {
        let mut grouped = HashMap::<String, (usize, u128)>::new();
        for item in self.export_items()? {
            let key = session_key_for_item(&item).unwrap_or_else(|| "-".to_string());
            let entry = grouped.entry(key).or_insert((0, 0));
            entry.0 += 1;
            entry.1 = entry
                .1
                .max(timestamp_millis(&item.updated_at).unwrap_or_default());
        }
        let mut sessions = grouped
            .into_iter()
            .map(|(key, (item_count, updated_at))| AmcpSessionRef {
                key,
                item_count,
                updated_at: iso_timestamp_from_millis(updated_at),
            })
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            timestamp_millis(&right.updated_at)
                .unwrap_or_default()
                .cmp(&timestamp_millis(&left.updated_at).unwrap_or_default())
        });
        Ok(sessions)
    }

    fn session(&self, session_key: &str) -> Result<Vec<AmcpMemoryItem>, String> {
        Ok(self
            .export_items()?
            .into_iter()
            .filter(|item| session_key_for_item(item).as_deref() == Some(session_key))
            .collect())
    }

    fn export_items(&self) -> Result<Vec<AmcpMemoryItem>, String> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT id, content, type, scope_json, origin_json, visibility, retention_json,
                        tags_json, metadata_json, source_refs_json, energy, created_at, updated_at
                 FROM amcp_memory_items
                 ORDER BY updated_at DESC, energy DESC",
            )
            .map_err(|err| err.to_string())?;
        let rows = statement
            .query_map([], row_to_item)
            .map_err(|err| err.to_string())?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(|err| err.to_string())?);
        }
        Ok(items)
    }

    fn import_items(&self, items: &[AmcpMemoryItem]) -> Result<usize, String> {
        let mut imported = 0;
        for item in items {
            if self.remember(item)? {
                imported += 1;
            }
        }
        Ok(imported)
    }

    fn delete(&self, ids: &[String]) -> Result<usize, String> {
        let connection = self.open()?;
        let mut deleted = 0;
        for id in ids {
            deleted += connection
                .execute("DELETE FROM amcp_memory_items WHERE id = ?1", [id])
                .map_err(|err| err.to_string())?;
            connection
                .execute("DELETE FROM amcp_memory_fts WHERE id = ?1", [id])
                .map_err(|err| err.to_string())?;
        }
        Ok(deleted)
    }
}

struct NexusCloudBackend {
    _workspace_root: PathBuf,
    endpoint: Option<String>,
    api_key_env: Option<String>,
    namespace: Option<String>,
    client: Client,
    capabilities_cache: RefCell<Option<AmcpBackendCapabilities>>,
}

impl NexusCloudBackend {
    fn new(workspace_root: &Path, config: &LoadedConfig) -> Self {
        Self {
            _workspace_root: workspace_root.to_path_buf(),
            endpoint: config.data.memory.nexus_cloud.endpoint.clone(),
            api_key_env: config.data.memory.nexus_cloud.api_key_env.clone(),
            namespace: config.data.memory.nexus_cloud.namespace.clone(),
            client: Client::new(),
            capabilities_cache: RefCell::new(None),
        }
    }

    fn api_base(&self) -> Result<String, String> {
        let endpoint = self
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "nexus-cloud endpoint is not configured".to_string())?;
        let trimmed = endpoint.trim_end_matches('/');
        if trimmed.ends_with("/v1/amcp") {
            Ok(trimmed.to_string())
        } else {
            Ok(format!("{trimmed}/v1/amcp"))
        }
    }

    fn api_key(&self) -> Result<String, String> {
        let env_name = self.api_key_env.as_deref().unwrap_or("NEXUS_API_KEY");
        env::var(env_name)
            .map_err(|_| format!("missing nexus-cloud api key in env var `{env_name}`"))
    }

    fn request(
        &self,
        method: Method,
        path: &str,
    ) -> Result<reqwest::blocking::RequestBuilder, String> {
        let url = format!("{}{}", self.api_base()?, path);
        let mut request = self
            .client
            .request(method, url)
            .bearer_auth(self.api_key()?)
            .header(header::ACCEPT, "application/json")
            .header("X-AMCP-Agent-Name", "3122-harness")
            .header("X-Nexus-Agent-Name", "3122-harness");
        if let Some(namespace) = self
            .namespace
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            request = request.header("X-Workspace-Id", namespace);
        }
        Ok(request)
    }

    fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<T, String> {
        let mut request = self.request(method, path)?;
        if let Some(body) = payload {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .json(&body);
        }
        let response = request.send().map_err(|err| err.to_string())?;
        self.parse_json_response(response)
    }

    fn parse_json_response<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::blocking::Response,
    ) -> Result<T, String> {
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            let trimmed = body.trim();
            return Err(format!(
                "nexus-cloud request failed: status={} body={}",
                status.as_u16(),
                if trimmed.is_empty() { "-" } else { trimmed }
            ));
        }
        response.json::<T>().map_err(|err| err.to_string())
    }

    fn discover_capabilities(&self) -> AmcpBackendCapabilities {
        if let Some(cached) = self.capabilities_cache.borrow().clone() {
            return cached;
        }

        let discovered = self
            .request(Method::GET, "/capabilities")
            .and_then(|request| request.send().map_err(|err| err.to_string()))
            .ok()
            .and_then(|response| match response.status() {
                status if status.is_success() => response.json::<AmcpBackendCapabilities>().ok(),
                StatusCode::NOT_FOUND | StatusCode::NOT_IMPLEMENTED => None,
                _ => None,
            })
            .unwrap_or_else(AmcpBackendCapabilities::minimal);
        self.capabilities_cache.replace(Some(discovered.clone()));
        discovered
    }

    fn export_response_items(&self) -> Result<Vec<AmcpMemoryItem>, String> {
        let response: ExportResponse =
            self.send_json(Method::POST, "/export", Some(json!({ "format": "json" })))?;
        Ok(response
            .items
            .into_iter()
            .map(AmcpMemoryItem::normalize_timestamps)
            .collect())
    }

    fn delete_one(&self, id: &str) -> Result<bool, String> {
        let response = self
            .request(Method::DELETE, &format!("/memories/{id}"))?
            .send()
            .map_err(|err| err.to_string())?;
        match response.status() {
            status if status.is_success() => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            status => {
                let body = response.text().unwrap_or_default();
                Err(format!(
                    "nexus-cloud delete failed: status={} body={}",
                    status.as_u16(),
                    body.trim()
                ))
            }
        }
    }
}

impl AmcpMemoryBackend for NexusCloudBackend {
    fn kind(&self) -> MemoryBackendKind {
        MemoryBackendKind::NexusCloud
    }

    fn capabilities(&self) -> Result<AmcpBackendCapabilities, String> {
        Ok(self.discover_capabilities())
    }

    fn remember(&self, item: &AmcpMemoryItem) -> Result<bool, String> {
        let _: RememberResponse =
            self.send_json(Method::POST, "/remember", Some(json!({ "items": [item] })))?;
        Ok(true)
    }

    fn recall(&self, request: &RecallRequest) -> Result<Vec<AmcpMemoryItem>, String> {
        let trimmed = request.query.as_deref().unwrap_or_default().trim();
        if trimmed.is_empty() {
            return self
                .export_response_items()
                .map(|items| items.into_iter().take(request.limit.max(1)).collect());
        }

        let capabilities = self.discover_capabilities();
        let mut payload = json!({
            "query": trimmed,
            "limit": request.limit.max(1),
            "token_budget": request.token_budget.max(1),
            "depth": "shallow"
        });
        if capabilities.supports_budget_recall {
            if let Some(context_budget) = &request.context_budget {
                payload["context_budget"] =
                    serde_json::to_value(context_budget).map_err(|err| err.to_string())?;
            }
        }
        let response: RecallResponse = self.send_json(Method::POST, "/recall", Some(payload))?;
        Ok(response
            .items
            .into_iter()
            .map(AmcpMemoryItem::normalize_timestamps)
            .collect())
    }

    fn sessions(&self) -> Result<Vec<AmcpSessionRef>, String> {
        let response: SessionListResponse =
            self.send_json(Method::GET, "/sessions?limit=100", None)?;
        Ok(response
            .items
            .into_iter()
            .map(|item| AmcpSessionRef {
                key: item.id,
                item_count: item.atom_count,
                updated_at: item
                    .last_activity
                    .or(item.timestamp)
                    .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
            })
            .collect())
    }

    fn session(&self, session_key: &str) -> Result<Vec<AmcpMemoryItem>, String> {
        let response: SessionDetailResponse =
            self.send_json(Method::GET, &format!("/sessions/{session_key}"), None)?;
        let fallback_agent_id = response
            .item
            .as_ref()
            .and_then(|item| item.origin.as_ref())
            .and_then(|origin| origin.agent_id.as_deref())
            .map(ToOwned::to_owned);
        let fallback_timestamp = response
            .item
            .as_ref()
            .and_then(|item| item.last_activity.as_deref())
            .map(ToOwned::to_owned);
        Ok(response
            .chain
            .into_iter()
            .map(|entry| {
                entry.into_amcp(
                    session_key,
                    self.namespace.as_deref(),
                    fallback_agent_id.as_deref(),
                    fallback_timestamp.as_deref(),
                )
            })
            .collect())
    }

    fn export_items(&self) -> Result<Vec<AmcpMemoryItem>, String> {
        self.export_response_items()
    }

    fn import_items(&self, items: &[AmcpMemoryItem]) -> Result<usize, String> {
        let response: ImportResponse = self.send_json(
            Method::POST,
            "/import",
            Some(json!({
                "items": items,
                "preserve_timestamps": true
            })),
        )?;
        Ok(response.imported_count)
    }

    fn delete(&self, ids: &[String]) -> Result<usize, String> {
        let mut deleted = 0usize;
        for id in ids {
            if self.delete_one(id)? {
                deleted += 1;
            }
        }
        Ok(deleted)
    }
}

#[derive(Debug, Deserialize)]
struct RememberResponse {
    #[allow(dead_code)]
    items: Vec<RememberedItem>,
    #[allow(dead_code)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RememberedItem {
    #[allow(dead_code)]
    id: String,
}

#[derive(Debug, Deserialize)]
struct RecallResponse {
    #[serde(default)]
    items: Vec<AmcpMemoryItem>,
}

#[derive(Debug, Deserialize)]
struct SessionListResponse {
    #[serde(default)]
    items: Vec<NexusSessionSummary>,
}

#[derive(Debug, Deserialize)]
struct SessionDetailResponse {
    item: Option<NexusSessionDetailSummary>,
    #[serde(default)]
    chain: Vec<SessionDetailChainItem>,
}

#[derive(Debug, Deserialize)]
struct ExportResponse {
    #[serde(default)]
    items: Vec<AmcpMemoryItem>,
}

#[derive(Debug, Deserialize)]
struct ImportResponse {
    #[serde(default)]
    imported_count: usize,
}

#[derive(Debug, Deserialize)]
struct NexusSessionSummary {
    id: String,
    #[serde(default)]
    atom_count: usize,
    #[serde(default, deserialize_with = "deserialize_optional_timestamp_string")]
    last_activity: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_timestamp_string")]
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NexusSessionDetailSummary {
    #[serde(default, deserialize_with = "deserialize_optional_timestamp_string")]
    last_activity: Option<String>,
    #[serde(default)]
    origin: Option<NexusSessionOrigin>,
}

#[derive(Debug, Deserialize)]
struct NexusSessionOrigin {
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SessionDetailChainItem {
    Amcp(AmcpMemoryItem),
    Summary(NexusSessionChainItem),
}

impl SessionDetailChainItem {
    fn into_amcp(
        self,
        session_key: &str,
        namespace: Option<&str>,
        fallback_agent_id: Option<&str>,
        fallback_timestamp: Option<&str>,
    ) -> AmcpMemoryItem {
        match self {
            SessionDetailChainItem::Amcp(item) => item.normalize_timestamps(),
            SessionDetailChainItem::Summary(item) => {
                let metadata = json!({
                    "session_position": item.position,
                    "session_link_type": item.link_type,
                    "session_link_from": item.link_from,
                });
                AmcpMemoryItem {
                    id: item.id,
                    content: item.content,
                    item_type: item.item_type,
                    scope: AmcpScope {
                        kind: "user".to_string(),
                        id: namespace.unwrap_or(DEFAULT_LOCAL_SCOPE_ID).to_string(),
                    },
                    origin: AmcpOrigin {
                        agent_id: fallback_agent_id.unwrap_or("3122-harness").to_string(),
                        app_id: APP_ID.to_string(),
                        session_id: Some(session_key.to_string()),
                        timestamp: item
                            .timestamp
                            .clone()
                            .or_else(|| fallback_timestamp.map(ToOwned::to_owned)),
                    },
                    visibility: "private".to_string(),
                    retention: AmcpRetention {
                        mode: "persistent".to_string(),
                    },
                    tags: Vec::new(),
                    metadata,
                    source_refs: Vec::new(),
                    energy: 1.0,
                    created_at: item.timestamp.clone().unwrap_or_default(),
                    updated_at: item.timestamp.unwrap_or_default(),
                }
                .normalize_timestamps()
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct NexusSessionChainItem {
    id: String,
    #[serde(rename = "type")]
    item_type: String,
    content: String,
    #[serde(default)]
    position: Option<usize>,
    #[serde(default)]
    link_type: Option<String>,
    #[serde(default)]
    link_from: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_timestamp_string")]
    timestamp: Option<String>,
}

struct ThirdPartyAmcpBackend {
    _workspace_root: PathBuf,
    endpoint: Option<String>,
    api_key_env: Option<String>,
    namespace: Option<String>,
}

impl ThirdPartyAmcpBackend {
    fn new(workspace_root: &Path, config: &LoadedConfig) -> Self {
        Self {
            _workspace_root: workspace_root.to_path_buf(),
            endpoint: config.data.memory.third_party_amcp.endpoint.clone(),
            api_key_env: config.data.memory.third_party_amcp.api_key_env.clone(),
            namespace: config.data.memory.third_party_amcp.namespace.clone(),
        }
    }

    fn unsupported_message(&self) -> String {
        let endpoint = self.endpoint.as_deref().unwrap_or("-");
        let api_key_env = self.api_key_env.as_deref().unwrap_or("-");
        let namespace = self.namespace.as_deref().unwrap_or("-");
        format!(
            "third-party-amcp backend is a config stub only (endpoint={endpoint}, api_key_env={api_key_env}, namespace={namespace})"
        )
    }
}

impl AmcpMemoryBackend for ThirdPartyAmcpBackend {
    fn kind(&self) -> MemoryBackendKind {
        MemoryBackendKind::ThirdPartyAmcp
    }

    fn capabilities(&self) -> Result<AmcpBackendCapabilities, String> {
        Ok(AmcpBackendCapabilities::minimal())
    }

    fn remember(&self, _item: &AmcpMemoryItem) -> Result<bool, String> {
        Err(self.unsupported_message())
    }

    fn recall(&self, _request: &RecallRequest) -> Result<Vec<AmcpMemoryItem>, String> {
        Err(self.unsupported_message())
    }

    fn sessions(&self) -> Result<Vec<AmcpSessionRef>, String> {
        Err(self.unsupported_message())
    }

    fn session(&self, _session_key: &str) -> Result<Vec<AmcpMemoryItem>, String> {
        Err(self.unsupported_message())
    }

    fn export_items(&self) -> Result<Vec<AmcpMemoryItem>, String> {
        Err(self.unsupported_message())
    }

    fn import_items(&self, _items: &[AmcpMemoryItem]) -> Result<usize, String> {
        Err(self.unsupported_message())
    }

    fn delete(&self, _ids: &[String]) -> Result<usize, String> {
        Err(self.unsupported_message())
    }
}

fn reorder_items_for_budget(
    mut items: Vec<AmcpMemoryItem>,
    request: &RecallRequest,
) -> Vec<AmcpMemoryItem> {
    let active_files = request
        .context_budget
        .as_ref()
        .map(|budget| {
            budget
                .active_files
                .iter()
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    if active_files.is_empty() {
        return items;
    }
    items.sort_by(|left, right| {
        score_item_file_hits(right, &active_files)
            .cmp(&score_item_file_hits(left, &active_files))
            .then_with(|| {
                timestamp_millis(&right.updated_at)
                    .unwrap_or_default()
                    .cmp(&timestamp_millis(&left.updated_at).unwrap_or_default())
            })
    });
    items
}

fn score_item_file_hits(item: &AmcpMemoryItem, active_files: &HashSet<String>) -> usize {
    item.source_refs
        .iter()
        .filter_map(|source| {
            if source.kind != "file" {
                return None;
            }
            Some(source.uri.trim_start_matches("file://").to_string())
        })
        .filter(|path| active_files.contains(path))
        .count()
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<AmcpMemoryItem> {
    Ok(AmcpMemoryItem {
        id: row.get(0)?,
        content: row.get(1)?,
        item_type: row.get(2)?,
        scope: serde_json::from_str(&row.get::<_, String>(3)?)
            .unwrap_or_else(|_| default_local_scope()),
        origin: serde_json::from_str(&row.get::<_, String>(4)?)
            .unwrap_or_else(|_| default_local_origin(None, 0)),
        visibility: row.get(5)?,
        retention: serde_json::from_str(&row.get::<_, String>(6)?)
            .unwrap_or_else(|_| default_private_retention()),
        tags: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
        metadata: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or(Value::Null),
        source_refs: serde_json::from_str(&row.get::<_, String>(9)?).unwrap_or_default(),
        energy: row.get(10)?,
        created_at: iso_timestamp_from_millis(row.get::<_, i64>(11)?.max(0) as u128),
        updated_at: iso_timestamp_from_millis(row.get::<_, i64>(12)?.max(0) as u128),
    }
    .normalize_timestamps())
}

pub fn metadata_title(item: &AmcpMemoryItem) -> String {
    item.metadata
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| truncate_chars(&item.content, 80))
}

pub fn metadata_legacy_kind(item: &AmcpMemoryItem) -> String {
    item.metadata
        .get("legacy_kind")
        .and_then(Value::as_str)
        .unwrap_or(item.item_type.as_str())
        .to_string()
}

pub fn session_key_for_item(item: &AmcpMemoryItem) -> Option<String> {
    item.source_refs
        .iter()
        .find(|source| source.kind == "session")
        .map(|source| source.uri.clone())
        .or_else(|| item.origin.session_id.clone())
}

pub fn truncate_chars(input: &str, max_chars: usize) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let shortened = trimmed.chars().take(max_chars).collect::<String>();
    format!("{shortened}...")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::{
        default_local_origin, default_local_scope, default_private_retention, export_items_jsonl,
        iso_timestamp_from_millis, metadata_title, parse_items_jsonl, resolve_memory_backend_kind,
        session_key_for_item, source_ref_uri, timestamp_millis, AmcpMemoryBackend, AmcpMemoryItem,
        AmcpSourceRef, MemoryBackendKind, NexusCloudBackend, RecallContextBudget, RecallRequest,
    };
    use crate::{load_config, LoadedConfig};

    fn temp_workspace(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{unique}"));
        fs::create_dir_all(path.join(".harness")).unwrap();
        path
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    fn sample_item() -> AmcpMemoryItem {
        AmcpMemoryItem {
            id: "amcp-sample".to_string(),
            content: "Remember provider routing defaults".to_string(),
            item_type: "context".to_string(),
            scope: default_local_scope(),
            origin: default_local_origin(Some("session-1"), 10),
            visibility: "private".to_string(),
            retention: default_private_retention(),
            tags: vec!["provider".to_string()],
            metadata: json!({ "title": "Routing defaults", "legacy_kind": "summary" }),
            source_refs: vec![AmcpSourceRef {
                kind: "session".to_string(),
                uri: "file:///tmp/session-1.jsonl".to_string(),
            }],
            energy: 1.0,
            created_at: iso_timestamp_from_millis(10),
            updated_at: iso_timestamp_from_millis(20),
        }
    }

    #[test]
    fn local_backend_round_trips_amcp_items() {
        let workspace = temp_workspace("portable-memory-local");
        let config = load_config(&workspace).unwrap();
        let backend =
            resolve_memory_backend_kind(&workspace, &config, MemoryBackendKind::LocalAmcp).unwrap();
        let item = sample_item();

        assert!(backend.remember(&item).unwrap());
        assert!(!backend.remember(&item).unwrap());

        let recall = backend
            .recall(&RecallRequest {
                query: Some("routing".to_string()),
                limit: 5,
                token_budget: 4000,
                context_budget: None,
            })
            .unwrap();
        assert_eq!(recall.len(), 1);
        assert_eq!(metadata_title(&recall[0]), "Routing defaults");
        assert_eq!(
            session_key_for_item(&recall[0]).as_deref(),
            Some("file:///tmp/session-1.jsonl")
        );

        cleanup(&workspace);
    }

    #[test]
    fn export_and_import_jsonl_round_trip() {
        let item = sample_item();
        let exported = export_items_jsonl(&[item]).unwrap();
        let parsed = parse_items_jsonl(&exported).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "amcp-sample");
        assert_eq!(timestamp_millis(&parsed[0].updated_at), Some(20));
    }

    #[test]
    fn parse_items_jsonl_backfills_missing_timestamps_from_origin() {
        let input = r#"{"id":"amcp-missing-ts","content":"portable recall item","type":"task","scope":{"kind":"user","id":"workspace-a"},"origin":{"agent_id":"3122-harness","app_id":"3122","session_id":"session-1","timestamp":"2026-04-13T14:00:35.670Z"},"visibility":"private","retention":{"mode":"persistent"},"tags":[],"metadata":{},"source_refs":[],"energy":1.0}"#;
        let parsed = parse_items_jsonl(input).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].created_at, "2026-04-13T14:00:35.67Z");
        assert_eq!(parsed[0].updated_at, "2026-04-13T14:00:35.67Z");
    }

    #[test]
    fn source_ref_uri_uses_file_scheme() {
        assert_eq!(
            source_ref_uri(Path::new("/tmp/example.rs")),
            "file:///tmp/example.rs"
        );
    }

    #[test]
    fn nexus_cloud_backend_uses_nexus_amcp_contract() {
        let workspace = temp_workspace("portable-memory-nexus-cloud");
        let requests = Arc::new(Mutex::new(Vec::<(String, String, String, String)>::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let captured = Arc::clone(&requests);
        let item = sample_item();
        let item_json = serde_json::to_string(&item).unwrap();

        let server = thread::spawn(move || {
            for _ in 0..8 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let read = stream.read(&mut chunk).unwrap();
                    if read == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                    if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let header_end = buffer
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|index| index + 4)
                    .unwrap();
                let header_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
                let request_line = header_text.lines().next().unwrap_or_default();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("Content-Length: ")
                            .or_else(|| line.strip_prefix("content-length: "))
                    })
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = buffer[header_end..].to_vec();
                while body.len() < content_length {
                    let read = stream.read(&mut chunk).unwrap();
                    if read == 0 {
                        break;
                    }
                    body.extend_from_slice(&chunk[..read]);
                }
                captured.lock().unwrap().push((
                    method.clone(),
                    path.clone(),
                    header_text.clone(),
                    String::from_utf8_lossy(&body).to_string(),
                ));

                let response_body = match (method.as_str(), path.as_str()) {
                    ("POST", "/v1/amcp/remember") => {
                        "{\"items\":[{\"id\":\"amcp-sample\"}],\"session_id\":\"session-1\"}".to_string()
                    }
                    ("POST", "/v1/amcp/recall") => {
                        format!("{{\"items\":[{item_json}]}}")
                    }
                    ("GET", "/v1/amcp/capabilities") => {
                        "{\"supports_budget_recall\":true,\"supports_compaction_checkpoints\":true,\"supports_provenance\":true,\"max_item_size\":65536,\"retention_policies\":[\"persistent\",\"session-derived\"]}".to_string()
                    }
                    ("GET", "/v1/amcp/sessions?limit=100") => {
                        "{\"items\":[{\"id\":\"session-1\",\"atom_count\":1,\"last_activity\":\"1970-01-01T00:00:00.020Z\"}]}".to_string()
                    }
                    ("GET", "/v1/amcp/sessions/session-1") => {
                        "{\"item\":{\"id\":\"session-1\",\"atom_count\":1,\"last_activity\":\"1970-01-01T00:00:00.020Z\",\"origin\":{\"agent_id\":\"3122-harness\"}},\"chain\":[{\"id\":\"amcp-sample\",\"type\":\"context\",\"content\":\"Remember provider routing defaults\",\"position\":1,\"link_type\":\"sequential\",\"link_from\":null,\"timestamp\":\"1970-01-01T00:00:00.020Z\"}]}".to_string()
                    }
                    ("POST", "/v1/amcp/export") => {
                        format!("{{\"format\":\"json\",\"items\":[{item_json}]}}")
                    }
                    ("POST", "/v1/amcp/import") => {
                        "{\"imported_count\":1,\"skipped_count\":0,\"items\":[]}".to_string()
                    }
                    ("DELETE", "/v1/amcp/memories/amcp-sample") => {
                        "{\"id\":\"amcp-sample\",\"status\":\"deleted\"}".to_string()
                    }
                    _ => "{\"error\":\"unexpected\"}".to_string(),
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let mut config = LoadedConfig::default();
        config.data.memory.nexus_cloud.endpoint = Some(format!("http://{address}"));
        config.data.memory.nexus_cloud.api_key_env = Some("NEXUS_TEST_API_KEY".to_string());
        config.data.memory.nexus_cloud.namespace = Some("workspace-a".to_string());
        std::env::set_var("NEXUS_TEST_API_KEY", "nxs_test_key");

        let backend = NexusCloudBackend::new(&workspace, &config);
        assert!(backend.remember(&item).unwrap());
        assert_eq!(
            backend
                .recall(&RecallRequest {
                    query: Some("routing".to_string()),
                    limit: 5,
                    token_budget: 4000,
                    context_budget: Some(RecallContextBudget {
                        max_tokens: 4000,
                        priority: "balanced".to_string(),
                        active_files: vec!["src/main.rs".to_string()],
                    }),
                })
                .unwrap()
                .len(),
            1
        );
        assert_eq!(backend.sessions().unwrap()[0].key, "session-1");
        assert_eq!(backend.session("session-1").unwrap().len(), 1);
        assert_eq!(backend.export_items().unwrap().len(), 1);
        assert_eq!(backend.import_items(&[item.clone()]).unwrap(), 1);
        assert_eq!(backend.delete(&["amcp-sample".to_string()]).unwrap(), 1);

        server.join().unwrap();

        let captured = requests.lock().unwrap();
        let first_headers = captured[0].2.to_ascii_lowercase();
        assert_eq!(captured.len(), 8);
        assert_eq!(captured[0].0, "POST");
        assert_eq!(captured[0].1, "/v1/amcp/remember");
        assert!(first_headers.contains("authorization: bearer nxs_test_key"));
        assert!(first_headers.contains("x-amcp-agent-name: 3122-harness"));
        assert!(first_headers.contains("x-nexus-agent-name: 3122-harness"));
        assert!(first_headers.contains("x-workspace-id: workspace-a"));
        assert!(captured[0]
            .3
            .contains("\"created_at\":\"1970-01-01T00:00:00"));
        assert!(captured[0]
            .3
            .contains("\"updated_at\":\"1970-01-01T00:00:00"));
        assert_eq!(captured[1].1, "/v1/amcp/capabilities");
        assert_eq!(captured[2].1, "/v1/amcp/recall");
        assert!(captured[2].3.contains("\"query\":\"routing\""));
        assert!(captured[2].3.contains("\"context_budget\""));
        assert_eq!(captured[3].1, "/v1/amcp/sessions?limit=100");
        assert_eq!(captured[4].1, "/v1/amcp/sessions/session-1");
        assert_eq!(captured[5].1, "/v1/amcp/export");
        assert_eq!(captured[6].1, "/v1/amcp/import");
        assert!(captured[6].3.contains("\"preserve_timestamps\":true"));
        assert_eq!(captured[7].1, "/v1/amcp/memories/amcp-sample");

        cleanup(&workspace);
    }
}
