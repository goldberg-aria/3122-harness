use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub ts_ms: u128,
    pub kind: String,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn create(workspace_root: &Path) -> Result<Self, String> {
        Self::create_in(&workspace_root.join(".harness").join("sessions"))
    }

    pub fn create_in(dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        let path = dir.join(format!("session-{}.jsonl", now_ms()));
        File::create(&path).map_err(|err| err.to_string())?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, kind: &str, payload: Value) -> Result<(), String> {
        let event = SessionEvent {
            ts_ms: now_ms(),
            kind: kind.to_string(),
            payload,
        };
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|err| err.to_string())?;
        let line = serde_json::to_string(&event).map_err(|err| err.to_string())?;
        writeln!(file, "{line}").map_err(|err| err.to_string())
    }

    pub fn latest(workspace_root: &Path) -> Result<Option<PathBuf>, String> {
        Self::latest_in(&workspace_root.join(".harness").join("sessions"))
    }

    pub fn latest_in(dir: &Path) -> Result<Option<PathBuf>, String> {
        Ok(Self::list_in(dir)?.into_iter().next())
    }

    pub fn list(workspace_root: &Path) -> Result<Vec<PathBuf>, String> {
        Self::list_in(&workspace_root.join(".harness").join("sessions"))
    }

    pub fn list_in(dir: &Path) -> Result<Vec<PathBuf>, String> {
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(dir).map_err(|err| err.to_string())? {
            let entry = entry.map_err(|err| err.to_string())?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .map_err(|err| err.to_string())?;
            entries.push((path, modified));
        }

        entries.sort_by(|left, right| right.1.cmp(&left.1));
        Ok(entries.into_iter().map(|(path, _)| path).collect())
    }

    pub fn read_events(path: &Path) -> Result<Vec<SessionEvent>, String> {
        let contents = fs::read_to_string(path).map_err(|err| err.to_string())?;
        contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<SessionEvent>(line).map_err(|err| err.to_string()))
            .collect()
    }

    pub fn session_id_from_path(path: &Path) -> Option<String> {
        let stem = path.file_stem()?.to_str()?;
        stem.strip_prefix("session-").map(ToOwned::to_owned)
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
