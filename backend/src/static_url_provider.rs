use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::StaticUrlPack;
use crate::media_preview::{MediaAvailability, MediaKind, MediaPreview, MediaSource};
use crate::transport::HttpTransport;
use crate::{CacheEntry, CacheManager, JsonStore, StoreError};

const MAX_IMAGE_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCandidate {
    pub id: String,
    pub source: String,
    pub url: String,
    #[serde(
        rename = "previewUrl",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub preview_url: Option<String>,
    #[serde(rename = "remoteId", default, skip_serializing_if = "Option::is_none")]
    pub remote_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(
        rename = "fileExtHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub file_ext_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
    #[serde(rename = "pageUrl", default, skip_serializing_if = "Option::is_none")]
    pub page_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(rename = "authorUrl", default, skip_serializing_if = "Option::is_none")]
    pub author_url: Option<String>,
    #[serde(rename = "ttlSec", default, skip_serializing_if = "Option::is_none")]
    pub ttl_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_preview: Option<MediaPreview>,
}

impl RemoteCandidate {
    pub fn new(id: String, source: impl Into<String>, url: String) -> Self {
        Self {
            id,
            source: source.into(),
            url,
            preview_url: None,
            remote_id: None,
            width: None,
            height: None,
            mime: None,
            file_ext_hint: None,
            tags: None,
            colors: None,
            rating: None,
            score: None,
            page_url: None,
            author: None,
            author_url: None,
            ttl_sec: None,
            media_preview: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StaticUrlIndex {
    #[serde(rename = "updatedAt")]
    pub updated_at: u64,
    #[serde(default)]
    pub candidates: Vec<RemoteCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaticUrlStatus {
    pub ok: bool,
    pub last_refresh: Option<u64>,
    pub candidates: usize,
    pub cache_items: usize,
    pub cache_bytes: u64,
}

#[derive(Debug)]
pub enum StaticUrlError {
    MissingUrl,
    InvalidCandidate,
    HttpStatus(u16),
    ResponseTooLarge(usize),
    InvalidContentType(String),
    Transport(crate::TransportError),
    Store(StoreError),
    Io(io::Error),
}

impl fmt::Display for StaticUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingUrl => write!(f, "static_url pack requires url or urls"),
            Self::InvalidCandidate => {
                write!(f, "candidate does not belong to this static_url pack")
            }
            Self::HttpStatus(status) => write!(f, "HTTP request failed with status {status}"),
            Self::ResponseTooLarge(bytes) => {
                write!(f, "response exceeds 100 MiB limit: {bytes} bytes")
            }
            Self::InvalidContentType(value) => {
                write!(f, "response is not an image: {value}")
            }
            Self::Transport(error) => error.fmt(f),
            Self::Store(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for StaticUrlError {}

impl From<StoreError> for StaticUrlError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

impl From<io::Error> for StaticUrlError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone)]
pub struct StaticUrlProvider {
    pack_name: String,
    config: StaticUrlPack,
    cache: CacheManager,
    index_path: PathBuf,
}

impl StaticUrlProvider {
    pub fn new(pack_name: impl Into<String>, config: StaticUrlPack, cache: CacheManager) -> Self {
        let pack_name = pack_name.into();
        let index_path = cache
            .cache_dir()
            .join("indexes")
            .join(format!("{pack_name}.json"));
        Self {
            pack_name,
            config,
            cache,
            index_path,
        }
    }

    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    pub fn refresh_index(&self, now_ms: u64) -> Result<StaticUrlIndex, StaticUrlError> {
        let urls = self.urls();
        if urls.is_empty() {
            return Err(StaticUrlError::MissingUrl);
        }
        let count = if self.config.different_images.unwrap_or(false) {
            self.config.count.unwrap_or(urls.len() as u32) as usize
        } else {
            1
        };
        let candidates = (0..count)
            .map(|index| {
                let url = urls[index % urls.len()].clone();
                let mut candidate = RemoteCandidate::new(
                    sha256_hex(&format!("{}:{url}:{index}", self.pack_name)),
                    "static_url",
                    url,
                );
                candidate.page_url = self
                    .config
                    .post_url
                    .clone()
                    .or_else(|| self.config.domain.clone());
                candidate.author = self.config.author_name.clone();
                candidate.author_url = self.config.author_url.clone();
                candidate.ttl_sec = self.config.ttl_sec;
                candidate
            })
            .collect();
        let index = StaticUrlIndex {
            updated_at: now_ms,
            candidates,
        };
        JsonStore::new(&self.index_path).save(&index)?;
        Ok(index)
    }

    pub fn load_index(&self) -> Result<StaticUrlIndex, StaticUrlError> {
        if !self.index_path.exists() {
            return Ok(StaticUrlIndex::default());
        }
        serde_json::from_slice(&fs::read(&self.index_path)?)
            .map_err(StoreError::Json)
            .map_err(StaticUrlError::Store)
    }

    pub fn list_candidates(&self) -> Result<Vec<RemoteCandidate>, StaticUrlError> {
        let mut candidates = self.load_index()?.candidates;
        for candidate in &mut candidates {
            if candidate.media_preview.is_none() {
                candidate.media_preview = Some(remote_preview(candidate));
            }
        }
        Ok(candidates)
    }

    pub fn status(&self) -> Result<StaticUrlStatus, StaticUrlError> {
        let provider_index = self.load_index()?;
        let cache_index = self.cache.load_index()?;
        let pack_root = self.cache.download_dir().join(&self.pack_name);
        let entries = cache_index
            .entries
            .iter()
            .filter(|entry| Path::new(&entry.local_path).starts_with(&pack_root))
            .collect::<Vec<_>>();
        Ok(StaticUrlStatus {
            ok: true,
            last_refresh: (provider_index.updated_at != 0).then_some(provider_index.updated_at),
            candidates: provider_index.candidates.len(),
            cache_items: entries.len(),
            cache_bytes: entries.iter().map(|entry| entry.size_bytes).sum(),
        })
    }

    pub fn hydrate<T: HttpTransport>(
        &self,
        candidate: &RemoteCandidate,
        transport: &T,
        now_ms: u64,
    ) -> Result<PathBuf, StaticUrlError> {
        if !self.owns(candidate) {
            return Err(StaticUrlError::InvalidCandidate);
        }
        let destination = self.local_path_for(candidate);
        if destination.is_file() {
            return Ok(destination);
        }

        let response = transport
            .get(&candidate.url)
            .map_err(StaticUrlError::Transport)?;
        if !(200..300).contains(&response.status) {
            return Err(StaticUrlError::HttpStatus(response.status));
        }
        if response.body.len() > MAX_IMAGE_BYTES {
            return Err(StaticUrlError::ResponseTooLarge(response.body.len()));
        }
        if response
            .content_type
            .as_deref()
            .is_some_and(|value| !value.to_ascii_lowercase().starts_with("image/"))
        {
            return Err(StaticUrlError::InvalidContentType(
                response.content_type.unwrap_or_default(),
            ));
        }
        let parent = destination
            .parent()
            .expect("download path always has a parent");
        fs::create_dir_all(parent)?;
        let temporary = destination.with_extension(format!("part-{}", std::process::id()));
        fs::write(&temporary, &response.body)?;
        if let Err(error) = fs::rename(&temporary, &destination) {
            let _ = fs::remove_file(&temporary);
            return Err(StaticUrlError::Io(error));
        }

        let mut cache_index = self.cache.load_index()?;
        self.cache.upsert(
            &mut cache_index,
            CacheEntry {
                key: candidate.id.clone(),
                local_path: destination.to_string_lossy().into_owned(),
                size_bytes: response.body.len() as u64,
                added_at: now_ms,
                ttl_sec: candidate.ttl_sec.unwrap_or(0),
            },
        );
        if let Err(error) = self.cache.save_index(&cache_index) {
            let _ = fs::remove_file(&destination);
            return Err(StaticUrlError::Store(error));
        }
        Ok(destination)
    }

    pub fn local_path_for(&self, candidate: &RemoteCandidate) -> PathBuf {
        let extension = safe_extension(&candidate.url);
        self.cache
            .download_dir()
            .join(&self.pack_name)
            .join(format!("{}.{}", sha256_hex(&candidate.id), extension))
    }

    fn urls(&self) -> Vec<String> {
        let urls = self
            .config
            .urls
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|url| url.trim().to_owned())
            .filter(|url| !url.is_empty())
            .collect::<Vec<_>>();
        if !urls.is_empty() {
            return urls;
        }
        self.config
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(|url| vec![url.to_owned()])
            .unwrap_or_default()
    }

    fn owns(&self, candidate: &RemoteCandidate) -> bool {
        if candidate.source != "static_url" || !self.urls().contains(&candidate.url) {
            return false;
        }
        self.load_index().ok().is_some_and(|index| {
            index
                .candidates
                .iter()
                .any(|item| item.id == candidate.id && item.url == candidate.url)
        })
    }
}

fn remote_preview(candidate: &RemoteCandidate) -> MediaPreview {
    MediaPreview {
        schema_version: 1,
        kind: MediaKind::Image,
        availability: MediaAvailability::Remote,
        source: MediaSource {
            remote_url: Some(
                candidate
                    .preview_url
                    .clone()
                    .unwrap_or_else(|| candidate.url.clone()),
            ),
            local_path: None,
        },
        thumbnail: None,
        width: None,
        height: None,
        duration_ms: None,
        mime_type: None,
        size_bytes: None,
    }
}

fn safe_extension(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpeg") => "jpeg",
        Some("png") => "png",
        Some("webp") => "webp",
        Some("bmp") => "bmp",
        Some("gif") => "gif",
        _ => "jpg",
    }
}

fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{HttpResponse, TransportError};
    use crate::Config;
    use std::cell::RefCell;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct FakeTransport {
        calls: RefCell<Vec<String>>,
        response: HttpResponse,
    }

    impl HttpTransport for FakeTransport {
        fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
            self.calls.borrow_mut().push(url.into());
            Ok(self.response.clone())
        }
    }

    fn fixture() -> (PathBuf, StaticUrlProvider) {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kitowall-static-url-{id}"));
        let mut cache_config = Config::default().cache;
        cache_config.dir = root.join("cache").to_string_lossy().into_owned();
        cache_config.download_dir = root.join("downloads").to_string_lossy().into_owned();
        let config = StaticUrlPack {
            urls: Some(vec![
                "https://example.test/a.png".into(),
                "https://example.test/b.jpg".into(),
            ]),
            different_images: Some(true),
            count: Some(3),
            ttl_sec: Some(60),
            ..StaticUrlPack::default()
        };
        let provider =
            StaticUrlProvider::new("demo", config, CacheManager::new(&cache_config, &root));
        (root, provider)
    }

    #[test]
    fn refresh_generates_legacy_compatible_candidates_and_previews() {
        let (root, provider) = fixture();
        let index = provider.refresh_index(1234).unwrap();
        assert_eq!(index.candidates.len(), 3);
        assert_eq!(index.candidates[0].url, "https://example.test/a.png");
        assert_eq!(index.candidates[2].url, "https://example.test/a.png");
        assert!(index.candidates[0].media_preview.is_none());
        let listed = provider.list_candidates().unwrap();
        assert_eq!(
            listed[0].media_preview.as_ref().unwrap().availability,
            MediaAvailability::Remote
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hydration_is_atomic_and_updates_cache() {
        let (root, provider) = fixture();
        let candidate = provider.refresh_index(1234).unwrap().candidates.remove(0);
        let transport = FakeTransport {
            calls: RefCell::new(Vec::new()),
            response: HttpResponse {
                status: 200,
                body: b"fake-png".to_vec(),
                content_type: Some("image/png".into()),
            },
        };
        let path = provider.hydrate(&candidate, &transport, 2000).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"fake-png");
        assert_eq!(provider.cache.load_index().unwrap().entries.len(), 1);
        assert!(!path
            .with_extension(format!("part-{}", std::process::id()))
            .exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hydration_rejects_candidates_not_present_in_its_index() {
        let (root, provider) = fixture();
        provider.refresh_index(1234).unwrap();
        let forged = RemoteCandidate::new(
            "forged".into(),
            "static_url",
            "https://example.test/a.png".into(),
        );
        let transport = FakeTransport {
            calls: RefCell::new(Vec::new()),
            response: HttpResponse {
                status: 200,
                body: vec![],
                content_type: None,
            },
        };
        assert!(matches!(
            provider.hydrate(&forged, &transport, 2000),
            Err(StaticUrlError::InvalidCandidate)
        ));
        assert!(transport.calls.borrow().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hydration_rejects_declared_non_image_content() {
        let (root, provider) = fixture();
        let candidate = provider.refresh_index(1234).unwrap().candidates.remove(0);
        let transport = FakeTransport {
            calls: RefCell::new(Vec::new()),
            response: HttpResponse {
                status: 200,
                body: b"<html>not an image</html>".to_vec(),
                content_type: Some("text/html".into()),
            },
        };
        assert!(matches!(
            provider.hydrate(&candidate, &transport, 2000),
            Err(StaticUrlError::InvalidContentType(_))
        ));
        assert!(!provider.local_path_for(&candidate).exists());
        assert!(provider.cache.load_index().unwrap().entries.is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
