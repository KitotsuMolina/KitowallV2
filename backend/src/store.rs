use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{Config, State};

#[derive(Debug)]
pub enum StoreError {
    HomeUnavailable,
    Io(io::Error),
    Json(serde_json::Error),
    InvalidConfig(crate::ConfigError),
    InvalidData(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HomeUnavailable => write!(f, "HOME is not available"),
            Self::Io(error) => write!(f, "filesystem error: {error}"),
            Self::Json(error) => write!(f, "JSON error: {error}"),
            Self::InvalidConfig(error) => error.fmt(f),
            Self::InvalidData(error) => write!(f, "invalid data: {error}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub fn config_path() -> Result<PathBuf, StoreError> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or(StoreError::HomeUnavailable)?;
    Ok(base.join("kitowall/config.json"))
}

pub fn state_path() -> Result<PathBuf, StoreError> {
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or(StoreError::HomeUnavailable)?;
    Ok(base.join("kitowall/state.json"))
}

#[derive(Debug, Clone)]
pub struct JsonStore {
    path: PathBuf,
}

impl JsonStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_or_create<T>(&self) -> Result<T, StoreError>
    where
        T: Default + DeserializeOwned + Serialize,
    {
        if !self.path.exists() {
            let value = T::default();
            self.save(&value)?;
            return Ok(value);
        }
        let bytes = fs::read(&self.path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn save<T: Serialize>(&self, value: &T) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        let mut bytes = serde_json::to_vec_pretty(value)?;
        bytes.push(b'\n');
        fs::write(&temporary, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        }
        if let Err(error) = fs::rename(&temporary, &self.path) {
            let _ = fs::remove_file(&temporary);
            return Err(StoreError::Io(error));
        }
        Ok(())
    }
}

pub fn load_config() -> Result<Config, StoreError> {
    let config: Config = JsonStore::new(config_path()?).load_or_create()?;
    config.validate().map_err(StoreError::InvalidConfig)?;
    Ok(config)
}

pub fn inspect_config() -> Result<Option<Config>, StoreError> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let config: Config = serde_json::from_slice(&fs::read(path)?)?;
    config.validate().map_err(StoreError::InvalidConfig)?;
    Ok(Some(config))
}

pub fn save_config(config: &Config) -> Result<(), StoreError> {
    config.validate().map_err(StoreError::InvalidConfig)?;
    JsonStore::new(config_path()?).save(config)
}

pub fn load_state() -> Result<State, StoreError> {
    JsonStore::new(state_path()?).load_or_create()
}

pub fn inspect_state() -> Result<Option<State>, StoreError> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

pub fn save_state(state: &State) -> Result<(), StoreError> {
    JsonStore::new(state_path()?).save(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn creates_and_reloads_json_atomically() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!("kitowall-store-{id}"));
        let path = root.join("config.json");
        let store = JsonStore::new(&path);
        let first: Config = store.load_or_create().unwrap();
        let second: Config = store.load_or_create().unwrap();
        assert_eq!(first, second);
        assert!(!path
            .with_extension(format!("tmp-{}", std::process::id()))
            .exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
