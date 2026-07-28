use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config::{DedupeStrategy, PackConfig};
use crate::{
    CacheManager, Config, ConfiguredProvider, HttpTransport, LocalProvider, RemoteCandidate,
    StaticUrlProvider,
};

enum HydrationProvider {
    StaticUrl(StaticUrlProvider),
    Configured(ConfiguredProvider),
}

struct HydrationTarget {
    provider: usize,
    candidate: RemoteCandidate,
}

struct PoolCandidate {
    path: String,
    remote_url: Option<String>,
    hydration: Option<HydrationTarget>,
}

pub struct ResolvedPool {
    pub name: String,
    pub paths: Vec<String>,
    providers: Vec<HydrationProvider>,
    hydration: BTreeMap<String, HydrationTarget>,
}

impl ResolvedPool {
    pub fn resolve<T: HttpTransport>(
        config: &Config,
        requested_pack: Option<&str>,
        current_pack: Option<&str>,
        home: &Path,
        transport: &T,
        now_ms: u64,
    ) -> Result<Self, String> {
        let name = choose_pack(config, requested_pack, current_pack)?;
        if name == "pool" {
            Self::resolve_combined(config, home, transport, now_ms)
        } else {
            Self::resolve_single(config, &name, home, transport, now_ms)
        }
    }

    pub fn hydrate<T: HttpTransport>(
        &self,
        path: &str,
        transport: &T,
        now_ms: u64,
    ) -> Result<PathBuf, String> {
        let Some(target) = self.hydration.get(path) else {
            return Ok(PathBuf::from(path));
        };
        match self
            .providers
            .get(target.provider)
            .ok_or_else(|| "invalid hydration provider index".to_owned())?
        {
            HydrationProvider::StaticUrl(provider) => provider
                .hydrate(&target.candidate, transport, now_ms)
                .map_err(|error| error.to_string()),
            HydrationProvider::Configured(provider) => provider
                .hydrate(&target.candidate, transport, now_ms)
                .map_err(|error| error.to_string()),
        }
    }

    pub fn path_for_id(&self, id: &str) -> Option<&str> {
        self.paths
            .iter()
            .find(|path| crate::catalog::wallpaper_id(&self.name, path) == id)
            .map(String::as_str)
    }

    fn resolve_single<T: HttpTransport>(
        config: &Config,
        name: &str,
        home: &Path,
        transport: &T,
        now_ms: u64,
    ) -> Result<Self, String> {
        let pack = config
            .packs
            .get(name)
            .ok_or_else(|| format!("pack not found: {name}"))?;
        let (candidates, providers) = resolve_pack(config, name, pack, home, transport, now_ms)?;
        Ok(from_candidates(name, candidates, providers))
    }

    fn resolve_combined<T: HttpTransport>(
        config: &Config,
        home: &Path,
        transport: &T,
        now_ms: u64,
    ) -> Result<Self, String> {
        if !config.pool.enabled || config.pool.sources.is_empty() {
            return Err("pool is not enabled or has no sources".into());
        }
        let mut combined = Vec::new();
        let mut providers = Vec::new();
        let mut seen = BTreeSet::new();
        for source in &config.pool.sources {
            let Some(pack) = config.packs.get(&source.name) else {
                continue;
            };
            let (mut candidates, source_providers) =
                resolve_pack(config, &source.name, pack, home, transport, now_ms)?;
            let provider_offset = providers.len();
            for candidate in &mut candidates {
                if let Some(target) = &mut candidate.hydration {
                    target.provider += provider_offset;
                }
            }
            providers.extend(source_providers);
            let limit = source.max_candidates.unwrap_or(candidates.len());
            let weight = source.weight.unwrap_or(1.0).floor().max(1.0) as usize;
            for candidate in candidates.into_iter().take(limit) {
                let key = match config.pool.dedupe {
                    DedupeStrategy::Path => candidate.path.clone(),
                    DedupeStrategy::Url => candidate
                        .remote_url
                        .clone()
                        .unwrap_or_else(|| candidate.path.clone()),
                    DedupeStrategy::Hash => sha256(&candidate.path),
                };
                if seen.insert(key) {
                    for _ in 0..weight {
                        combined.push(PoolCandidate {
                            path: candidate.path.clone(),
                            remote_url: candidate.remote_url.clone(),
                            hydration: None,
                        });
                    }
                    if let Some(target) = candidate.hydration {
                        combined
                            .last_mut()
                            .expect("weight is at least one")
                            .hydration = Some(target);
                    }
                }
            }
        }
        Ok(from_candidates("pool", combined, providers))
    }
}

fn choose_pack(
    config: &Config,
    requested: Option<&str>,
    current: Option<&str>,
) -> Result<String, String> {
    if let Some(requested) = requested {
        let name = crate::config::normalize_pack_name(requested);
        if name == "pool" || config.packs.contains_key(&name) {
            return Ok(name);
        }
        return Err(format!("pack not found: {name}"));
    }
    if config.pool.enabled && !config.pool.sources.is_empty() {
        return Ok("pool".into());
    }
    if let Some(current) = current.filter(|name| config.packs.contains_key(*name)) {
        return config
            .packs
            .keys()
            .find(|name| name.as_str() > current)
            .or_else(|| config.packs.keys().next())
            .cloned()
            .ok_or_else(|| "no packs configured".into());
    }
    config
        .packs
        .keys()
        .next()
        .cloned()
        .ok_or_else(|| "no packs configured".into())
}

fn resolve_pack<T: HttpTransport>(
    config: &Config,
    name: &str,
    pack: &PackConfig,
    home: &Path,
    transport: &T,
    now_ms: u64,
) -> Result<(Vec<PoolCandidate>, Vec<HydrationProvider>), String> {
    match pack {
        PackConfig::Local { paths } => {
            let candidates = LocalProvider::new(home)
                .discover(paths)
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|candidate| PoolCandidate {
                    path: candidate.path,
                    remote_url: None,
                    hydration: None,
                })
                .collect();
            Ok((candidates, Vec::new()))
        }
        PackConfig::StaticUrl(pack) => {
            let cache =
                CacheManager::from_config(&config.cache).map_err(|error| error.to_string())?;
            let provider = StaticUrlProvider::new(name, pack.clone(), cache);
            let mut candidates = provider
                .list_candidates()
                .map_err(|error| error.to_string())?;
            if candidates.is_empty() {
                provider
                    .refresh_index(now_ms)
                    .map_err(|error| error.to_string())?;
                candidates = provider
                    .list_candidates()
                    .map_err(|error| error.to_string())?;
            }
            let rows = remote_candidates(&provider, candidates, 0);
            Ok((rows, vec![HydrationProvider::StaticUrl(provider)]))
        }
        _ => {
            let cache =
                CacheManager::from_config(&config.cache).map_err(|error| error.to_string())?;
            let provider = ConfiguredProvider::from_pack(
                name,
                pack,
                config.provider_credentials(pack.kind()),
                cache,
            )
            .ok_or_else(|| format!("provider unavailable for pack: {name}"))?;
            let mut candidates = provider.list().map_err(|error| error.to_string())?;
            if candidates.is_empty() {
                provider
                    .refresh(transport, now_ms)
                    .map_err(|error| error.to_string())?;
                candidates = provider.list().map_err(|error| error.to_string())?;
            }
            let rows = candidates
                .into_iter()
                .map(|candidate| PoolCandidate {
                    path: provider
                        .local_path_for(&candidate)
                        .to_string_lossy()
                        .into_owned(),
                    remote_url: Some(candidate.url.clone()),
                    hydration: Some(HydrationTarget {
                        provider: 0,
                        candidate,
                    }),
                })
                .collect();
            Ok((rows, vec![HydrationProvider::Configured(provider)]))
        }
    }
}

fn remote_candidates(
    provider: &StaticUrlProvider,
    candidates: Vec<RemoteCandidate>,
    provider_index: usize,
) -> Vec<PoolCandidate> {
    candidates
        .into_iter()
        .map(|candidate| PoolCandidate {
            path: provider
                .local_path_for(&candidate)
                .to_string_lossy()
                .into_owned(),
            remote_url: Some(candidate.url.clone()),
            hydration: Some(HydrationTarget {
                provider: provider_index,
                candidate,
            }),
        })
        .collect()
}

fn from_candidates(
    name: &str,
    candidates: Vec<PoolCandidate>,
    providers: Vec<HydrationProvider>,
) -> ResolvedPool {
    let mut hydration = BTreeMap::new();
    let mut paths = Vec::new();
    for candidate in candidates {
        if let Some(target) = candidate.hydration {
            hydration.entry(candidate.path.clone()).or_insert(target);
        }
        paths.push(candidate.path);
    }
    ResolvedPool {
        name: name.into(),
        paths,
        providers,
        hydration,
    }
}

fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PoolSource, StaticUrlPack};
    use crate::{HttpResponse, TransportError};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct ImageTransport;

    impl HttpTransport for ImageTransport {
        fn get(&self, _url: &str) -> Result<HttpResponse, TransportError> {
            Ok(HttpResponse {
                status: 200,
                content_type: Some("image/png".into()),
                body: b"image".to_vec(),
            })
        }
    }

    fn root(name: &str) -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("kitowall-library-{name}-{id}"))
    }

    #[test]
    fn automatic_rotation_advances_between_configured_packs() {
        let mut config = Config::default();
        config.packs.insert(
            "edge-runners".into(),
            PackConfig::Local {
                paths: vec!["/edge".into()],
            },
        );
        config.packs.insert(
            "sao".into(),
            PackConfig::Local {
                paths: vec!["/sao".into()],
            },
        );

        assert_eq!(
            choose_pack(&config, None, Some("sao")).unwrap(),
            "edge-runners"
        );
        assert_eq!(
            choose_pack(&config, None, Some("edge-runners")).unwrap(),
            "sao"
        );
    }

    #[test]
    fn remote_pack_refreshes_empty_index_and_hydrates_selected_path() {
        let root = root("remote");
        fs::create_dir_all(&root).unwrap();
        let mut config = Config::default();
        config.cache.dir = root.join("cache").to_string_lossy().into_owned();
        config.cache.download_dir = root.join("downloads").to_string_lossy().into_owned();
        config.packs.insert(
            "remote".into(),
            PackConfig::StaticUrl(StaticUrlPack {
                url: Some("https://example.test/wall.png".into()),
                ..StaticUrlPack::default()
            }),
        );
        let pool = ResolvedPool::resolve(&config, Some("remote"), None, &root, &ImageTransport, 10)
            .unwrap();
        assert_eq!(pool.paths.len(), 1);
        let hydrated = pool.hydrate(&pool.paths[0], &ImageTransport, 11).unwrap();
        assert!(hydrated.is_file());
        assert_eq!(hydrated, PathBuf::from(&pool.paths[0]));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn combined_pool_applies_weights_without_losing_hydration() {
        let root = root("pool");
        fs::create_dir_all(&root).unwrap();
        let mut config = Config::default();
        config.cache.dir = root.join("cache").to_string_lossy().into_owned();
        config.cache.download_dir = root.join("downloads").to_string_lossy().into_owned();
        config.packs.insert(
            "remote".into(),
            PackConfig::StaticUrl(StaticUrlPack {
                url: Some("https://example.test/wall.png".into()),
                ..StaticUrlPack::default()
            }),
        );
        config.pool.enabled = true;
        config.pool.sources.push(PoolSource {
            name: "remote".into(),
            weight: Some(3.0),
            max_candidates: None,
        });
        let pool = ResolvedPool::resolve(&config, None, None, &root, &ImageTransport, 10).unwrap();
        assert_eq!(pool.name, "pool");
        assert_eq!(pool.paths.len(), 3);
        pool.hydrate(&pool.paths[0], &ImageTransport, 11).unwrap();
        assert!(Path::new(&pool.paths[0]).is_file());
        let _ = fs::remove_dir_all(root);
    }
}
