use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::media_preview::{MediaAvailability, MediaKind, MediaPreview, MediaSource};
use crate::static_url_provider::RemoteCandidate;
use crate::{CacheEntry, CacheManager, HttpTransport, JsonStore, StoreError, TransportError};

const MAX_IMAGE_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProviderIndex {
    #[serde(rename = "updatedAt")]
    pub updated_at: u64,
    #[serde(
        rename = "configHash",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub config_hash: Option<String>,
    #[serde(default)]
    pub candidates: Vec<RemoteCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<u64>,
    pub candidates: usize,
    pub cache_items: usize,
    pub cache_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteStore {
    pack_name: String,
    source: String,
    cache: CacheManager,
    index_path: PathBuf,
}

impl RemoteStore {
    pub fn new(
        pack_name: impl Into<String>,
        source: impl Into<String>,
        cache: CacheManager,
    ) -> Self {
        let pack_name = pack_name.into();
        let index_path = cache
            .cache_dir()
            .join("indexes")
            .join(format!("{pack_name}.json"));
        Self {
            pack_name,
            source: source.into(),
            cache,
            index_path,
        }
    }

    pub fn load(&self) -> Result<ProviderIndex, StoreError> {
        if !self.index_path.exists() {
            return Ok(ProviderIndex::default());
        }
        Ok(serde_json::from_slice(&fs::read(&self.index_path)?)?)
    }

    pub fn pack_name(&self) -> &str {
        &self.pack_name
    }

    pub fn save(&self, index: &ProviderIndex) -> Result<(), StoreError> {
        JsonStore::new(&self.index_path).save(index)
    }

    pub fn list(&self, expected_hash: Option<&str>) -> Result<Vec<RemoteCandidate>, StoreError> {
        let index = self.load()?;
        if expected_hash.is_some() && index.config_hash.as_deref() != expected_hash {
            return Ok(Vec::new());
        }
        Ok(index
            .candidates
            .into_iter()
            .map(with_remote_preview)
            .collect())
    }

    pub fn status(&self, last_error: Option<String>) -> Result<ProviderStatus, StoreError> {
        let index = self.load()?;
        let cache_index = self.cache.load_index()?;
        let pack_root = self.cache.download_dir().join(&self.pack_name);
        let entries = cache_index
            .entries
            .iter()
            .filter(|entry| Path::new(&entry.local_path).starts_with(&pack_root))
            .collect::<Vec<_>>();
        Ok(ProviderStatus {
            ok: last_error.is_none(),
            last_refresh: (index.updated_at != 0).then_some(index.updated_at),
            candidates: index.candidates.len(),
            cache_items: entries.len(),
            cache_bytes: entries.iter().map(|entry| entry.size_bytes).sum(),
            last_error,
        })
    }

    pub fn hydrate<T: HttpTransport>(
        &self,
        candidate: &RemoteCandidate,
        transport: &T,
        now_ms: u64,
    ) -> Result<PathBuf, RemoteError> {
        let belongs = candidate.source == self.source
            && self
                .load()?
                .candidates
                .iter()
                .any(|item| item.id == candidate.id && item.url == candidate.url);
        if !belongs {
            return Err(RemoteError::InvalidCandidate);
        }
        let destination = self.local_path_for(candidate);
        if destination.is_file() {
            return Ok(destination);
        }
        let response = transport.get(&candidate.url)?;
        if !(200..300).contains(&response.status) {
            return Err(RemoteError::HttpStatus(response.status));
        }
        if response.body.len() > MAX_IMAGE_BYTES {
            return Err(RemoteError::ResponseTooLarge(response.body.len()));
        }
        if response
            .content_type
            .as_deref()
            .is_some_and(|value| !value.to_ascii_lowercase().starts_with("image/"))
        {
            return Err(RemoteError::InvalidContentType(
                response.content_type.unwrap_or_default(),
            ));
        }
        let parent = destination.parent().expect("download path has a parent");
        fs::create_dir_all(parent)?;
        let temporary = destination.with_extension(format!("part-{}", std::process::id()));
        fs::write(&temporary, &response.body)?;
        if let Err(error) = fs::rename(&temporary, &destination) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
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
            return Err(error.into());
        }
        Ok(destination)
    }

    pub fn local_path_for(&self, candidate: &RemoteCandidate) -> PathBuf {
        self.cache
            .download_dir()
            .join(&self.pack_name)
            .join(format!(
                "{}.{}",
                sha256_hex(&candidate.id),
                safe_extension(&candidate.url)
            ))
    }
}

fn with_remote_preview(mut candidate: RemoteCandidate) -> RemoteCandidate {
    if candidate.media_preview.is_none() {
        candidate.media_preview = Some(MediaPreview {
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
            width: candidate.width,
            height: candidate.height,
            duration_ms: None,
            mime_type: candidate.mime.clone(),
            size_bytes: None,
        });
    }
    candidate
}

#[derive(Debug)]
pub enum RemoteError {
    InvalidCandidate,
    HttpStatus(u16),
    ResponseTooLarge(usize),
    InvalidContentType(String),
    Transport(TransportError),
    Store(StoreError),
    Io(std::io::Error),
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCandidate => write!(f, "candidate does not belong to this provider index"),
            Self::HttpStatus(value) => write!(f, "HTTP request failed with status {value}"),
            Self::ResponseTooLarge(value) => {
                write!(f, "response exceeds image limit: {value} bytes")
            }
            Self::InvalidContentType(value) => write!(f, "response is not an image: {value}"),
            Self::Transport(error) => error.fmt(f),
            Self::Store(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RemoteError {}
impl From<TransportError> for RemoteError {
    fn from(value: TransportError) -> Self {
        Self::Transport(value)
    }
}
impl From<StoreError> for RemoteError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}
impl From<std::io::Error> for RemoteError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

pub fn query_url(base: &str, pairs: &[(&str, String)]) -> String {
    let separator = if base.contains('?') { '&' } else { '?' };
    let query = pairs
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        base.to_owned()
    } else {
        format!("{base}{separator}{query}")
    }
}

pub fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn safe_extension(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, HttpResponse};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct ImageTransport;
    impl HttpTransport for ImageTransport {
        fn get(&self, _url: &str) -> Result<crate::HttpResponse, TransportError> {
            Ok(HttpResponse {
                status: 200,
                body: b"image-data".to_vec(),
                content_type: Some("image/jpeg".into()),
            })
        }
    }

    #[test]
    fn shared_hydration_writes_atomically_and_updates_cache() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kitowall-remote-store-{id}"));
        let mut config = Config::default().cache;
        config.dir = root.join("cache").to_string_lossy().into_owned();
        config.download_dir = root.join("downloads").to_string_lossy().into_owned();
        let cache = CacheManager::new(&config, &root);
        let inspect_cache = cache.clone();
        let store = RemoteStore::new("demo", "wallhaven", cache);
        let mut candidate = RemoteCandidate::new(
            "candidate".into(),
            "wallhaven",
            "https://images.test/wall.jpg".into(),
        );
        candidate.ttl_sec = Some(60);
        store
            .save(&ProviderIndex {
                updated_at: 1,
                config_hash: None,
                candidates: vec![candidate.clone()],
            })
            .unwrap();
        let path = store.hydrate(&candidate, &ImageTransport, 2).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"image-data");
        assert_eq!(inspect_cache.load_index().unwrap().entries.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
