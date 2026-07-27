use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{JsonStore, StoreError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub timestamp: u64,
    pub pack: String,
    pub output: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HistoryState {
    #[serde(default)]
    pub entries: Vec<HistoryEntry>,
}

pub fn history_path() -> Result<PathBuf, StoreError> {
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or(StoreError::HomeUnavailable)?;
    Ok(base.join("kitowall/history.json"))
}

pub fn load_history() -> Result<HistoryState, StoreError> {
    let path = history_path()?;
    if !path.exists() {
        return Ok(HistoryState::default());
    }
    JsonStore::new(path).load_or_create()
}

pub fn append_history(entries: &[HistoryEntry]) -> Result<usize, StoreError> {
    if entries.is_empty() {
        return Ok(0);
    }
    let store = JsonStore::new(history_path()?);
    let mut history: HistoryState = store.load_or_create()?;
    history.entries.extend_from_slice(entries);
    store.save(&history)?;
    Ok(entries.len())
}

pub fn list_history(limit: Option<usize>) -> Result<Vec<HistoryEntry>, StoreError> {
    let history = load_history()?;
    let limit = limit.unwrap_or(history.entries.len());
    Ok(history.entries.into_iter().rev().take(limit).collect())
}

pub fn clear_history() -> Result<usize, StoreError> {
    let store = JsonStore::new(history_path()?);
    let mut history: HistoryState = store.load_or_create()?;
    let removed = history.entries.len();
    if removed > 0 {
        history.entries.clear();
        store.save(&history)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn history_round_trips_legacy_schema_and_lists_newest_first() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kitowall-history-{id}"));
        let store = JsonStore::new(root.join("history.json"));
        let state = HistoryState {
            entries: vec![
                HistoryEntry {
                    timestamp: 1,
                    pack: "a".into(),
                    output: "DP-1".into(),
                    path: "/a.png".into(),
                },
                HistoryEntry {
                    timestamp: 2,
                    pack: "b".into(),
                    output: "DP-1".into(),
                    path: "/b.png".into(),
                },
            ],
        };
        store.save(&state).unwrap();
        let listed = store.load_or_create::<HistoryState>().unwrap();
        assert_eq!(listed.entries.len(), 2);
        assert_eq!(listed.entries[1].path, "/b.png");
        let _ = fs::remove_dir_all(root);
    }
}
