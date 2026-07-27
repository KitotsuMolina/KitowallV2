use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{JsonStore, StoreError};

const MAX_LOG_ENTRIES: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp_unix_ms: u64,
    pub level: LogLevel,
    pub source: String,
    pub action: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct LogState {
    #[serde(default)]
    entries: Vec<LogEntry>,
}

#[derive(Debug, Clone)]
pub struct LogStore {
    path: PathBuf,
}

impl LogStore {
    pub fn from_environment() -> Result<Self, StoreError> {
        let base = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .ok_or(StoreError::HomeUnavailable)?;
        Ok(Self {
            path: base.join("kitowall/logs.json"),
        })
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn append(&self, entry: LogEntry) -> Result<(), StoreError> {
        let store = JsonStore::new(&self.path);
        let mut state: LogState = store.load_or_create()?;
        state.entries.push(entry);
        if state.entries.len() > MAX_LOG_ENTRIES {
            state.entries.drain(..state.entries.len() - MAX_LOG_ENTRIES);
        }
        store.save(&state)
    }

    pub fn list(
        &self,
        limit: usize,
        level: Option<LogLevel>,
        source: Option<&str>,
        pack: Option<&str>,
    ) -> Result<Vec<LogEntry>, StoreError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let state: LogState = JsonStore::new(&self.path).load_or_create()?;
        Ok(state
            .entries
            .into_iter()
            .rev()
            .filter(|entry| level.is_none_or(|level| entry.level == level))
            .filter(|entry| source.is_none_or(|source| entry.source == source))
            .filter(|entry| pack.is_none_or(|pack| entry.pack.as_deref() == Some(pack)))
            .take(limit)
            .collect())
    }

    pub fn clear(&self) -> Result<usize, StoreError> {
        if !self.path.exists() {
            return Ok(0);
        }
        let store = JsonStore::new(&self.path);
        let mut state: LogState = store.load_or_create()?;
        let removed = state.entries.len();
        if removed > 0 {
            state.entries.clear();
            store.save(&state)?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn logs_filter_newest_first_and_clear() {
        let root = std::env::temp_dir().join(format!(
            "kitowall-logs-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = LogStore::new(root.join("logs.json"));
        for (timestamp, level, pack) in [
            (1, LogLevel::Info, "one"),
            (2, LogLevel::Warning, "two"),
            (3, LogLevel::Info, "one"),
        ] {
            store
                .append(LogEntry {
                    timestamp_unix_ms: timestamp,
                    level,
                    source: "wallpaper".into(),
                    action: "apply".into(),
                    message: "safe message".into(),
                    pack: Some(pack.into()),
                    output: None,
                    path: None,
                })
                .unwrap();
        }
        let entries = store
            .list(10, Some(LogLevel::Info), Some("wallpaper"), Some("one"))
            .unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.timestamp_unix_ms)
                .collect::<Vec<_>>(),
            [3, 1]
        );
        assert_eq!(store.clear().unwrap(), 3);
        assert!(store.list(10, None, None, None).unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
