use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaAvailability {
    Remote,
    Local,
    RemoteAndLocal,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MediaSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaPreview {
    pub schema_version: u32,
    pub kind: MediaKind,
    pub availability: MediaAvailability,
    #[serde(flatten)]
    pub source: MediaSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<MediaSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

impl MediaPreview {
    pub fn local_image(path: String, mime_type: Option<String>, size_bytes: Option<u64>) -> Self {
        Self {
            schema_version: 1,
            kind: MediaKind::Image,
            availability: MediaAvailability::Local,
            source: MediaSource {
                remote_url: None,
                local_path: Some(path),
            },
            thumbnail: None,
            width: None,
            height: None,
            duration_ms: None,
            mime_type,
            size_bytes,
        }
    }
}
