use std::fmt;

use serde::{Deserialize, Serialize};

use crate::config::{normalize_pack_name, PackConfig};
use crate::{Config, State};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackCatalogError {
    InvalidName,
    AlreadyExists(String),
    NotFound(String),
    InvalidConfig(crate::ConfigError),
}

impl fmt::Display for PackCatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => write!(f, "pack name is empty after normalization"),
            Self::AlreadyExists(name) => write!(f, "pack already exists: {name}"),
            Self::NotFound(name) => write!(f, "pack not found: {name}"),
            Self::InvalidConfig(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PackCatalogError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackSummary {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovePackResult {
    pub removed: String,
    pub detached_from_pool: bool,
    pub cleared_current_pack: bool,
}

impl Config {
    pub fn pack_summaries(&self) -> Vec<PackSummary> {
        self.packs
            .iter()
            .map(|(name, pack)| PackSummary {
                name: name.clone(),
                kind: pack.kind().into(),
            })
            .collect()
    }

    pub fn add_pack(
        &mut self,
        raw_name: &str,
        pack: PackConfig,
    ) -> Result<String, PackCatalogError> {
        let name = normalized_name(raw_name)?;
        if self.packs.contains_key(&name) {
            return Err(PackCatalogError::AlreadyExists(name));
        }
        self.packs.insert(name.clone(), pack);
        if let Err(error) = self.validate() {
            self.packs.remove(&name);
            return Err(PackCatalogError::InvalidConfig(error));
        }
        Ok(name)
    }

    pub fn update_pack(
        &mut self,
        raw_name: &str,
        pack: PackConfig,
    ) -> Result<String, PackCatalogError> {
        let name = normalized_name(raw_name)?;
        let previous = self
            .packs
            .insert(name.clone(), pack)
            .ok_or_else(|| PackCatalogError::NotFound(name.clone()))?;
        if let Err(error) = self.validate() {
            self.packs.insert(name.clone(), previous);
            return Err(PackCatalogError::InvalidConfig(error));
        }
        Ok(name)
    }

    pub fn remove_pack(
        &mut self,
        state: &mut State,
        raw_name: &str,
    ) -> Result<RemovePackResult, PackCatalogError> {
        let name = normalized_name(raw_name)?;
        self.packs
            .remove(&name)
            .ok_or_else(|| PackCatalogError::NotFound(name.clone()))?;
        let before = self.pool.sources.len();
        self.pool.sources.retain(|source| source.name != name);
        let cleared_current_pack = state.current_pack.as_deref() == Some(&name);
        if cleared_current_pack {
            state.current_pack = None;
        }
        Ok(RemovePackResult {
            removed: name,
            detached_from_pool: before != self.pool.sources.len(),
            cleared_current_pack,
        })
    }
}

impl PackConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local",
            Self::Wallhaven(_) => "wallhaven",
            Self::Reddit(_) => "reddit",
            Self::Unsplash(_) => "unsplash",
            Self::GenericJson(_) => "generic_json",
            Self::StaticUrl(_) => "static_url",
        }
    }
}

fn normalized_name(raw_name: &str) -> Result<String, PackCatalogError> {
    let name = normalize_pack_name(raw_name);
    if name.is_empty() {
        Err(PackCatalogError::InvalidName)
    } else {
        Ok(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PoolSource, StaticUrlPack};

    #[test]
    fn add_update_and_remove_preserve_catalog_invariants() {
        let mut config = Config::default();
        let name = config
            .add_pack(
                " My Local ",
                PackConfig::Local {
                    paths: vec!["~/Pictures".into()],
                },
            )
            .unwrap();
        assert_eq!(name, "my-local");
        assert!(matches!(
            config.add_pack(
                "my-local",
                PackConfig::Local {
                    paths: vec!["/other".into()]
                }
            ),
            Err(PackCatalogError::AlreadyExists(_))
        ));
        config
            .update_pack(
                "my-local",
                PackConfig::Local {
                    paths: vec!["/updated".into()],
                },
            )
            .unwrap();
        config.pool.sources.push(PoolSource {
            name: name.clone(),
            weight: Some(1.0),
            max_candidates: None,
        });
        let mut state = State {
            current_pack: Some(name.clone()),
            ..State::default()
        };
        let removed = config.remove_pack(&mut state, &name).unwrap();
        assert!(removed.detached_from_pool);
        assert!(removed.cleared_current_pack);
        assert!(state.current_pack.is_none());
    }

    #[test]
    fn invalid_pack_does_not_mutate_catalog() {
        let mut config = Config::default();
        let result = config.add_pack("broken", PackConfig::StaticUrl(StaticUrlPack::default()));
        assert!(matches!(result, Err(PackCatalogError::InvalidConfig(_))));
        assert!(config.packs.is_empty());
    }
}
