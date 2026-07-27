use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::media_preview::{MediaAvailability, MediaKind, MediaPreview};
use crate::{
    CacheManager, Config, ConfiguredProvider, LocalProvider, PackConfig, State, StaticUrlProvider,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallpaperCatalogItem {
    pub id: String,
    pub pack: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    pub preview: MediaPreview,
    pub hydrated: bool,
    pub favorite: bool,
    pub favorite_key: String,
    #[serde(default)]
    pub active_outputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallpaperCatalogFacets {
    pub images: usize,
    pub videos: usize,
    pub favorites: usize,
    pub hydrated: usize,
    pub by_provider: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallpaperCatalogPage {
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub facets: WallpaperCatalogFacets,
    pub items: Vec<WallpaperCatalogItem>,
}

pub fn list_wallpapers(
    config: &Config,
    requested_pack: Option<&str>,
    home: &Path,
    favorites: &BTreeSet<String>,
    state: &State,
    offset: usize,
    limit: usize,
) -> Result<WallpaperCatalogPage, String> {
    if limit == 0 || limit > 200 {
        return Err("wallpaper list limit must be between 1 and 200".into());
    }
    let names = match requested_pack {
        Some(name) => {
            let name = crate::config::normalize_pack_name(name);
            if !config.packs.contains_key(&name) {
                return Err(format!("pack not found: {name}"));
            }
            vec![name]
        }
        None => config.packs.keys().cloned().collect(),
    };
    let mut items = Vec::new();
    for name in names {
        let pack = config
            .packs
            .get(&name)
            .expect("pack name came from the config");
        items.extend(list_pack(config, &name, pack, home, favorites, state)?);
    }
    items.sort_by(|left, right| {
        left.pack
            .cmp(&right.pack)
            .then_with(|| left.id.cmp(&right.id))
    });
    let total = items.len();
    let mut facets = WallpaperCatalogFacets {
        images: 0,
        videos: 0,
        favorites: 0,
        hydrated: 0,
        by_provider: BTreeMap::new(),
    };
    for item in &items {
        match item.preview.kind {
            MediaKind::Image => facets.images += 1,
            MediaKind::Video => facets.videos += 1,
        }
        facets.favorites += usize::from(item.favorite);
        facets.hydrated += usize::from(item.hydrated);
        *facets.by_provider.entry(item.provider.clone()).or_default() += 1;
    }
    let items = items.into_iter().skip(offset).take(limit).collect();
    Ok(WallpaperCatalogPage {
        offset,
        limit,
        total,
        facets,
        items,
    })
}

pub(crate) fn wallpaper_id(pack: &str, path: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{pack}\0{path}").as_bytes()))
}

fn list_pack(
    config: &Config,
    name: &str,
    pack: &PackConfig,
    home: &Path,
    favorites: &BTreeSet<String>,
    state: &State,
) -> Result<Vec<WallpaperCatalogItem>, String> {
    match pack {
        PackConfig::Local { paths } => LocalProvider::new(home)
            .discover(paths)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|candidate| {
                let path = candidate.path;
                Ok(local_item(
                    name,
                    &candidate.source,
                    path,
                    candidate.media_preview,
                    candidate.mime,
                    favorites,
                    state,
                ))
            })
            .collect(),
        PackConfig::StaticUrl(pack) => {
            let cache =
                CacheManager::from_config(&config.cache).map_err(|error| error.to_string())?;
            let provider = StaticUrlProvider::new(name, pack.clone(), cache);
            provider
                .list_candidates()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|candidate| {
                    let local_path = provider
                        .local_path_for(&candidate)
                        .to_string_lossy()
                        .into_owned();
                    remote_item(name, "static_url", local_path, candidate, favorites, state)
                })
                .collect()
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
            provider
                .list()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|candidate| {
                    let local_path = provider
                        .local_path_for(&candidate)
                        .to_string_lossy()
                        .into_owned();
                    remote_item(
                        name,
                        provider.kind(),
                        local_path,
                        candidate,
                        favorites,
                        state,
                    )
                })
                .collect()
        }
    }
}

fn local_item(
    pack: &str,
    provider: &str,
    path: String,
    preview: MediaPreview,
    mime: Option<String>,
    favorites: &BTreeSet<String>,
    state: &State,
) -> WallpaperCatalogItem {
    WallpaperCatalogItem {
        id: wallpaper_id(pack, &path),
        pack: pack.into(),
        provider: provider.into(),
        local_path: Some(path.clone()),
        remote_url: None,
        preview,
        hydrated: true,
        favorite: favorites.contains(&path),
        favorite_key: path.clone(),
        active_outputs: active_outputs(state, &path),
        width: None,
        height: None,
        mime,
        tags: None,
        colors: None,
        rating: None,
        score: None,
        page_url: None,
        author: None,
        author_url: None,
    }
}

fn remote_item(
    pack: &str,
    provider: &str,
    local_path: String,
    candidate: crate::RemoteCandidate,
    favorites: &BTreeSet<String>,
    state: &State,
) -> Result<WallpaperCatalogItem, String> {
    let hydrated = Path::new(&local_path).is_file();
    let mut preview = candidate
        .media_preview
        .clone()
        .ok_or_else(|| format!("provider candidate has no preview: {}", candidate.id))?;
    if hydrated {
        preview.availability = MediaAvailability::RemoteAndLocal;
        preview.source.local_path = Some(local_path.clone());
    }
    Ok(WallpaperCatalogItem {
        id: wallpaper_id(pack, &local_path),
        pack: pack.into(),
        provider: provider.into(),
        local_path: hydrated.then_some(local_path.clone()),
        remote_url: Some(candidate.url),
        preview,
        hydrated,
        favorite: favorites.contains(&local_path),
        favorite_key: local_path.clone(),
        active_outputs: active_outputs(state, &local_path),
        width: candidate.width,
        height: candidate.height,
        mime: candidate.mime,
        tags: candidate.tags,
        colors: candidate.colors,
        rating: candidate.rating,
        score: candidate.score,
        page_url: candidate.page_url,
        author: candidate.author,
        author_url: candidate.author_url,
    })
}

fn active_outputs(state: &State, path: &str) -> Vec<String> {
    state
        .last_set
        .iter()
        .filter(|(_, current)| current.as_str() == path)
        .map(|(output, _)| output.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PackConfig;
    use crate::media_preview::MediaSource;
    use crate::RemoteCandidate;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn local_catalog_is_stable_paginated_and_enriched() {
        let root = std::env::temp_dir().join(format!(
            "kitowall-catalog-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let first = root.join("a.png");
        fs::write(&first, b"image").unwrap();
        fs::write(root.join("b.jpg"), b"image").unwrap();
        let mut config = Config::default();
        config.packs.insert(
            "local".into(),
            PackConfig::Local {
                paths: vec![root.to_string_lossy().into_owned()],
            },
        );
        let canonical = fs::canonicalize(first)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let favorites = [canonical.clone()].into_iter().collect();
        let mut state = State::default();
        state.last_set.insert("DP-1".into(), canonical);

        let page =
            list_wallpapers(&config, Some("local"), &root, &favorites, &state, 0, 2).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.facets.images, 2);
        assert_eq!(page.facets.videos, 0);
        assert_eq!(page.facets.favorites, 1);
        assert_eq!(page.facets.hydrated, 2);
        assert_eq!(page.facets.by_provider["local"], 2);
        let favorite = page.items.iter().find(|item| item.favorite).unwrap();
        assert_eq!(favorite.active_outputs, ["DP-1"]);
        assert_eq!(favorite.preview.availability, MediaAvailability::Local);
        let second_page =
            list_wallpapers(&config, Some("local"), &root, &favorites, &state, 1, 1).unwrap();
        assert_eq!(second_page.items.len(), 1);
        let again =
            list_wallpapers(&config, Some("local"), &root, &favorites, &state, 0, 2).unwrap();
        assert_eq!(page.items[0].id, again.items[0].id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remote_favorite_key_survives_hydration() {
        let root = std::env::temp_dir().join(format!(
            "kitowall-remote-favorite-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let local_path = root.join("wallpaper.jpg").to_string_lossy().into_owned();
        let mut candidate = RemoteCandidate::new(
            "remote".into(),
            "wallhaven",
            "https://w.wallhaven.cc/wallpaper.jpg".into(),
        );
        candidate.media_preview = Some(MediaPreview {
            schema_version: 1,
            kind: MediaKind::Image,
            availability: MediaAvailability::Remote,
            source: MediaSource {
                remote_url: Some(candidate.url.clone()),
                local_path: None,
            },
            thumbnail: None,
            width: Some(1920),
            height: Some(1080),
            duration_ms: None,
            mime_type: Some("image/jpeg".into()),
            size_bytes: None,
        });
        let favorites = BTreeSet::from([local_path.clone()]);

        let pending = remote_item(
            "sao",
            "wallhaven",
            local_path.clone(),
            candidate.clone(),
            &favorites,
            &State::default(),
        )
        .unwrap();
        assert!(pending.favorite);
        assert_eq!(pending.favorite_key, local_path);
        assert!(!pending.hydrated);
        assert!(pending.local_path.is_none());

        fs::write(&pending.favorite_key, b"image").unwrap();
        let hydrated = remote_item(
            "sao",
            "wallhaven",
            pending.favorite_key.clone(),
            candidate,
            &favorites,
            &State::default(),
        )
        .unwrap();
        assert!(hydrated.favorite);
        assert!(hydrated.hydrated);
        assert_eq!(
            hydrated.local_path.as_deref(),
            Some(hydrated.favorite_key.as_str())
        );
        let _ = fs::remove_dir_all(root);
    }
}
