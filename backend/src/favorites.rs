use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{JsonStore, StoreError};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FavoritesState {
    #[serde(default)]
    pub favorites: Vec<String>,
}

pub fn favorites_path() -> Result<PathBuf, StoreError> {
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or(StoreError::HomeUnavailable)?;
    Ok(base.join("kitowall/favorites.json"))
}

pub fn load_favorites() -> Result<BTreeSet<String>, StoreError> {
    let path = favorites_path()?;
    if !path.exists() {
        return Ok(BTreeSet::new());
    }
    let state: FavoritesState = JsonStore::new(path).load_or_create()?;
    Ok(state.favorites.into_iter().collect())
}

pub fn list_favorites() -> Result<Vec<String>, StoreError> {
    let path = favorites_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(JsonStore::new(path)
        .load_or_create::<FavoritesState>()?
        .favorites)
}

pub fn add_favorite(path: &str) -> Result<bool, StoreError> {
    let path = normalized_path(path)?;
    let store = JsonStore::new(favorites_path()?);
    let mut state: FavoritesState = store.load_or_create()?;
    if state.favorites.iter().any(|favorite| favorite == path) {
        return Ok(false);
    }
    state.favorites.push(path.into());
    store.save(&state)?;
    Ok(true)
}

pub fn remove_favorite(path: &str) -> Result<bool, StoreError> {
    let path = normalized_path(path)?;
    let store = JsonStore::new(favorites_path()?);
    let mut state: FavoritesState = store.load_or_create()?;
    let previous = state.favorites.len();
    state.favorites.retain(|favorite| favorite != path);
    let removed = state.favorites.len() != previous;
    if removed {
        store.save(&state)?;
    }
    Ok(removed)
}

fn normalized_path(path: &str) -> Result<&str, StoreError> {
    let path = path.trim();
    if path.is_empty() {
        Err(StoreError::InvalidData(
            "favorite path cannot be empty".into(),
        ))
    } else {
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn favorites_are_ordered_idempotent_and_legacy_compatible() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kitowall-favorites-{id}"));
        let store = JsonStore::new(root.join("favorites.json"));
        store
            .save(&FavoritesState {
                favorites: vec!["/a.png".into()],
            })
            .unwrap();
        let mut state: FavoritesState = store.load_or_create().unwrap();
        assert_eq!(state.favorites, ["/a.png"]);
        if !state.favorites.contains(&"/b.png".to_owned()) {
            state.favorites.push("/b.png".into());
        }
        store.save(&state).unwrap();
        assert_eq!(
            store.load_or_create::<FavoritesState>().unwrap().favorites,
            ["/a.png", "/b.png"]
        );
        let _ = fs::remove_dir_all(root);
    }
}
