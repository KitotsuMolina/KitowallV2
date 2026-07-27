use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::CacheConfig;
use crate::{JsonStore, StoreError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub key: String,
    #[serde(rename = "localPath")]
    pub local_path: String,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    #[serde(rename = "addedAt")]
    pub added_at: u64,
    #[serde(rename = "ttlSec")]
    pub ttl_sec: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CacheIndex {
    #[serde(default)]
    pub entries: Vec<CacheEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheStatus {
    pub index_path: String,
    pub download_dir: String,
    pub entries: usize,
    pub indexed_bytes: u64,
    pub max_bytes: u64,
    pub expired_entries: usize,
    pub favorite_entries: usize,
    pub missing_files: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneReason {
    Expired,
    SizeLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedRemoval {
    pub key: String,
    pub local_path: String,
    pub size_bytes: u64,
    pub reason: PruneReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePrunePlan {
    pub dry_run: bool,
    pub pack: Option<String>,
    pub removals: Vec<PlannedRemoval>,
    pub removed_bytes: u64,
    pub remaining_entries: usize,
    pub remaining_bytes: u64,
    pub protected_favorites: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePruneResult {
    pub dry_run: bool,
    pub pack: Option<String>,
    pub removed_entries: usize,
    pub removed_files: usize,
    pub removed_bytes: u64,
    pub remaining_entries: usize,
    pub remaining_bytes: u64,
    #[serde(default)]
    pub cleanup_failures: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CacheManager {
    index_path: PathBuf,
    download_dir: PathBuf,
    max_bytes: u64,
    default_ttl_sec: u64,
}

impl CacheManager {
    pub fn from_config(config: &CacheConfig) -> Result<Self, StoreError> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(StoreError::HomeUnavailable)?;
        Ok(Self::new(config, &home))
    }

    pub fn new(config: &CacheConfig, home: impl AsRef<Path>) -> Self {
        let cache_dir = expand_tilde(&config.dir, home.as_ref());
        Self {
            index_path: cache_dir.join("index.json"),
            download_dir: expand_tilde(&config.download_dir, home.as_ref()),
            max_bytes: config.max_mb.saturating_mul(1024 * 1024),
            default_ttl_sec: config.default_ttl_sec,
        }
    }

    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    pub fn cache_dir(&self) -> &Path {
        self.index_path
            .parent()
            .expect("cache index always has a parent directory")
    }

    pub fn download_dir(&self) -> &Path {
        &self.download_dir
    }

    pub fn load_index(&self) -> Result<CacheIndex, StoreError> {
        if !self.index_path.exists() {
            return Ok(CacheIndex::default());
        }
        Ok(serde_json::from_slice(&fs::read(&self.index_path)?)?)
    }

    pub fn save_index(&self, index: &CacheIndex) -> Result<(), StoreError> {
        JsonStore::new(&self.index_path).save(index)
    }

    pub fn upsert(&self, index: &mut CacheIndex, mut entry: CacheEntry) {
        if entry.ttl_sec == 0 {
            entry.ttl_sec = self.default_ttl_sec;
        }
        if let Some(existing) = index.entries.iter_mut().find(|item| item.key == entry.key) {
            *existing = entry;
        } else {
            index.entries.push(entry);
        }
    }

    pub fn status(
        &self,
        index: &CacheIndex,
        favorites: &BTreeSet<String>,
        now_ms: u64,
    ) -> CacheStatus {
        CacheStatus {
            index_path: self.index_path.to_string_lossy().into_owned(),
            download_dir: self.download_dir.to_string_lossy().into_owned(),
            entries: index.entries.len(),
            indexed_bytes: total_bytes(&index.entries),
            max_bytes: self.max_bytes,
            expired_entries: index
                .entries
                .iter()
                .filter(|entry| is_expired(entry, now_ms))
                .count(),
            favorite_entries: index
                .entries
                .iter()
                .filter(|entry| favorites.contains(&entry.local_path))
                .count(),
            missing_files: index
                .entries
                .iter()
                .filter(|entry| !Path::new(&entry.local_path).is_file())
                .count(),
        }
    }

    pub fn plan_prune(
        &self,
        index: &CacheIndex,
        favorites: &BTreeSet<String>,
        now_ms: u64,
        pack: Option<&str>,
    ) -> CachePrunePlan {
        let mut removals = BTreeMap::<String, PlannedRemoval>::new();
        let mut protected_favorites = BTreeSet::new();

        for entry in &index.entries {
            if !matches_pack(&self.download_dir, entry, pack) {
                continue;
            }
            if favorites.contains(&entry.local_path) {
                if is_expired(entry, now_ms) {
                    protected_favorites.insert(entry.key.clone());
                }
                continue;
            }
            if is_expired(entry, now_ms) {
                removals.insert(
                    entry.key.clone(),
                    planned_removal(entry, PruneReason::Expired),
                );
            }
        }

        // The global size limit does not apply to a single-pack TTL plan.
        if pack.is_none() {
            let mut remaining = index
                .entries
                .iter()
                .filter(|entry| !removals.contains_key(&entry.key))
                .collect::<Vec<_>>();
            remaining.sort_by_key(|entry| (entry.added_at, &entry.key));
            let mut remaining_bytes = remaining.iter().map(|entry| entry.size_bytes).sum::<u64>();
            for entry in remaining {
                if remaining_bytes <= self.max_bytes {
                    break;
                }
                if favorites.contains(&entry.local_path) {
                    protected_favorites.insert(entry.key.clone());
                    continue;
                }
                remaining_bytes = remaining_bytes.saturating_sub(entry.size_bytes);
                removals.insert(
                    entry.key.clone(),
                    planned_removal(entry, PruneReason::SizeLimit),
                );
            }
        }

        let removals = removals.into_values().collect::<Vec<_>>();
        let removed_bytes = removals.iter().map(|entry| entry.size_bytes).sum();
        CachePrunePlan {
            dry_run: true,
            pack: pack.map(str::to_owned),
            remaining_entries: index.entries.len().saturating_sub(removals.len()),
            remaining_bytes: total_bytes(&index.entries).saturating_sub(removed_bytes),
            removals,
            removed_bytes,
            protected_favorites: protected_favorites.len(),
        }
    }

    pub fn validate_managed_file(&self, path: &Path) -> Result<PathBuf, io::Error> {
        let root = fs::canonicalize(&self.download_dir)?;
        let file = fs::canonicalize(path)?;
        if !file.starts_with(&root) || !file.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cache file is outside the canonical download directory",
            ));
        }
        Ok(file)
    }

    pub fn apply_prune(
        &self,
        index: &CacheIndex,
        plan: &CachePrunePlan,
    ) -> Result<CachePruneResult, StoreError> {
        let removal_keys = plan
            .removals
            .iter()
            .map(|removal| removal.key.as_str())
            .collect::<BTreeSet<_>>();
        let mut moved = BTreeMap::<PathBuf, PathBuf>::new();
        for removal in &plan.removals {
            let path = Path::new(&removal.local_path);
            if !path.exists() {
                continue;
            }
            let canonical = self.validate_managed_file(path)?;
            if moved.contains_key(&canonical) {
                continue;
            }
            let backup = prune_backup_path(&canonical);
            if backup.exists() {
                restore_moved(&moved);
                return Err(StoreError::InvalidData(format!(
                    "cache prune backup already exists: {}",
                    backup.display()
                )));
            }
            if let Err(error) = fs::rename(&canonical, &backup) {
                restore_moved(&moved);
                return Err(StoreError::Io(error));
            }
            moved.insert(canonical, backup);
        }

        let next = CacheIndex {
            entries: index
                .entries
                .iter()
                .filter(|entry| !removal_keys.contains(entry.key.as_str()))
                .cloned()
                .collect(),
        };
        if let Err(error) = self.save_index(&next) {
            restore_moved(&moved);
            return Err(error);
        }
        let mut cleanup_failures = Vec::new();
        for backup in moved.values() {
            if let Err(error) = fs::remove_file(backup) {
                cleanup_failures.push(format!("{}: {error}", backup.display()));
            }
        }
        Ok(CachePruneResult {
            dry_run: false,
            pack: plan.pack.clone(),
            removed_entries: plan.removals.len(),
            removed_files: moved.len(),
            removed_bytes: plan.removed_bytes,
            remaining_entries: next.entries.len(),
            remaining_bytes: total_bytes(&next.entries),
            cleanup_failures,
        })
    }
}

fn prune_backup_path(path: &Path) -> PathBuf {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("file");
    path.with_extension(format!("{extension}.kitowall-prune-{}", std::process::id()))
}

fn restore_moved(moved: &BTreeMap<PathBuf, PathBuf>) {
    for (original, backup) in moved.iter().rev() {
        let _ = fs::rename(backup, original);
    }
}

fn expand_tilde(input: &str, home: &Path) -> PathBuf {
    if input == "~" {
        return home.to_path_buf();
    }
    input
        .strip_prefix("~/")
        .map(|rest| home.join(rest))
        .unwrap_or_else(|| PathBuf::from(input))
}

fn is_expired(entry: &CacheEntry, now_ms: u64) -> bool {
    now_ms.saturating_sub(entry.added_at) > entry.ttl_sec.saturating_mul(1000)
}

fn total_bytes(entries: &[CacheEntry]) -> u64 {
    entries
        .iter()
        .fold(0, |total, entry| total.saturating_add(entry.size_bytes))
}

fn planned_removal(entry: &CacheEntry, reason: PruneReason) -> PlannedRemoval {
    PlannedRemoval {
        key: entry.key.clone(),
        local_path: entry.local_path.clone(),
        size_bytes: entry.size_bytes,
        reason,
    }
}

fn matches_pack(download_dir: &Path, entry: &CacheEntry, pack: Option<&str>) -> bool {
    let Some(pack) = pack else {
        return true;
    };
    Path::new(&entry.local_path).starts_with(download_dir.join(pack))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn entry(key: &str, path: &str, bytes: u64, added_at: u64, ttl_sec: u64) -> CacheEntry {
        CacheEntry {
            key: key.into(),
            local_path: path.into(),
            size_bytes: bytes,
            added_at,
            ttl_sec,
        }
    }

    #[test]
    fn legacy_index_round_trips() {
        let json = serde_json::json!({"entries": [{
            "key": "one", "localPath": "/walls/one.jpg", "sizeBytes": 12,
            "addedAt": 1000, "ttlSec": 60
        }]});
        let index: CacheIndex = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(index).unwrap(), json);
    }

    #[test]
    fn plan_respects_ttl_size_and_favorites() {
        let mut config = Config::default().cache;
        config.download_dir = "/walls".into();
        config.max_mb = 1;
        let manager = CacheManager::new(&config, "/home/test");
        let index = CacheIndex {
            entries: vec![
                entry("expired", "/walls/p/a.jpg", 400_000, 0, 1),
                entry("favorite", "/walls/p/b.jpg", 900_000, 0, 1),
                entry("old", "/walls/p/c.jpg", 800_000, 9_000, 100),
            ],
        };
        let favorites = BTreeSet::from(["/walls/p/b.jpg".into()]);
        let plan = manager.plan_prune(&index, &favorites, 10_000, None);
        assert_eq!(plan.removals.len(), 2);
        assert_eq!(plan.protected_favorites, 1);
        assert!(plan.removals.iter().any(|item| item.key == "expired"));
        assert!(plan.removals.iter().any(|item| item.key == "old"));
    }

    #[test]
    fn pack_plan_matches_path_component_not_substring() {
        let mut config = Config::default().cache;
        config.download_dir = "/walls".into();
        let manager = CacheManager::new(&config, "/home/test");
        let index = CacheIndex {
            entries: vec![
                entry("target", "/walls/cat/a.jpg", 1, 0, 1),
                entry("other", "/walls/cat-extra/b.jpg", 1, 0, 1),
            ],
        };
        let plan = manager.plan_prune(&index, &BTreeSet::new(), 2_000, Some("cat"));
        assert_eq!(plan.removals.len(), 1);
        assert_eq!(plan.removals[0].key, "target");
    }

    #[test]
    fn managed_file_validation_rejects_symlink_escape() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!("kitowall-cache-{id}"));
        let downloads = root.join("downloads");
        fs::create_dir_all(&downloads).unwrap();
        let outside = root.join("outside.jpg");
        fs::write(&outside, b"image").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, downloads.join("escape.jpg")).unwrap();
            let mut config = Config::default().cache;
            config.download_dir = downloads.to_string_lossy().into_owned();
            let manager = CacheManager::new(&config, &root);
            assert!(manager
                .validate_managed_file(&downloads.join("escape.jpg"))
                .is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prune_moves_validated_files_updates_index_and_preserves_favorites() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!("kitowall-cache-prune-{id}"));
        let downloads = root.join("downloads");
        fs::create_dir_all(downloads.join("pack")).unwrap();
        let removed = downloads.join("pack/removed.jpg");
        let favorite = downloads.join("pack/favorite.jpg");
        fs::write(&removed, b"remove").unwrap();
        fs::write(&favorite, b"favorite").unwrap();
        let mut config = Config::default().cache;
        config.dir = root.join("cache").to_string_lossy().into_owned();
        config.download_dir = downloads.to_string_lossy().into_owned();
        let manager = CacheManager::new(&config, &root);
        let index = CacheIndex {
            entries: vec![
                entry("remove", &removed.to_string_lossy(), 6, 0, 1),
                entry("favorite", &favorite.to_string_lossy(), 8, 0, 1),
            ],
        };
        manager.save_index(&index).unwrap();
        let favorites = BTreeSet::from([favorite.to_string_lossy().into_owned()]);
        let plan = manager.plan_prune(&index, &favorites, 2_000, None);
        let result = manager.apply_prune(&index, &plan).unwrap();
        assert_eq!(result.removed_entries, 1);
        assert_eq!(result.removed_files, 1);
        assert!(!removed.exists());
        assert!(favorite.exists());
        assert_eq!(manager.load_index().unwrap().entries.len(), 1);
        assert!(result.cleanup_failures.is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
