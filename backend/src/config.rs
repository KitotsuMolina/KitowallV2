use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub mode: Mode,
    #[serde(default)]
    pub wallpaper_backend: WallpaperBackend,
    pub rotation_interval_seconds: u64,
    pub transition: TransitionConfig,
    pub selection: SelectionConfig,
    pub cache: CacheConfig,
    #[serde(default)]
    pub pool: PoolConfig,
    #[serde(
        rename = "providerCredentials",
        default,
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub provider_credentials: BTreeMap<String, ProviderCredentials>,
    #[serde(default)]
    pub packs: BTreeMap<String, PackConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProviderCredentials {
    #[serde(rename = "apiKey", default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(rename = "apiKeyEnv", default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Manual,
    Rotate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WallpaperBackend {
    #[default]
    Auto,
    Swww,
    Awww,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitionConfig {
    #[serde(rename = "type")]
    pub kind: String,
    pub fps: u32,
    pub duration: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionConfig {
    #[serde(rename = "perOutputCooldown")]
    pub per_output_cooldown: usize,
    #[serde(rename = "globalCooldown")]
    pub global_cooldown: usize,
    #[serde(rename = "avoidSameTickDuplicates")]
    pub avoid_same_tick_duplicates: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConfig {
    pub dir: String,
    #[serde(rename = "downloadDir")]
    pub download_dir: String,
    #[serde(rename = "maxMB")]
    pub max_mb: u64,
    #[serde(rename = "defaultTtlSec")]
    pub default_ttl_sec: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolConfig {
    pub enabled: bool,
    #[serde(default)]
    pub sources: Vec<PoolSource>,
    #[serde(default)]
    pub dedupe: DedupeStrategy,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sources: Vec::new(),
            dedupe: DedupeStrategy::Path,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolSource {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    #[serde(
        rename = "maxCandidates",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub max_candidates: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DedupeStrategy {
    #[default]
    Path,
    Hash,
    Url,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PackConfig {
    #[serde(rename = "local")]
    Local { paths: Vec<String> },
    #[serde(rename = "wallhaven")]
    Wallhaven(WallhavenPack),
    #[serde(rename = "reddit")]
    Reddit(RedditPack),
    #[serde(rename = "unsplash")]
    Unsplash(UnsplashPack),
    #[serde(rename = "generic_json")]
    GenericJson(GenericJsonPack),
    #[serde(rename = "static_url")]
    StaticUrl(StaticUrlPack),
}

macro_rules! optional_pack_struct {
    ($name:ident { $($field:ident : $ty:ty => $json_name:literal),* $(,)? }) => {
        #[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
        pub struct $name {
            $(
                #[serde(rename = $json_name, default, skip_serializing_if = "Option::is_none")]
                pub $field: Option<$ty>,
            )*
        }
    };
}

optional_pack_struct! {
    WallhavenPack {
        api_key: String => "apiKey",
        api_key_env: String => "apiKeyEnv",
        keyword: String => "keyword",
        subthemes: Vec<String> => "subthemes",
        categories: String => "categories",
        purity: String => "purity",
        allow_sfw: bool => "allowSfw",
        allow_sketchy: bool => "allowSketchy",
        allow_nsfw: bool => "allowNsfw",
        category_general: bool => "categoryGeneral",
        category_anime: bool => "categoryAnime",
        category_people: bool => "categoryPeople",
        ratios: Vec<String> => "ratios",
        colors: String => "colors",
        atleast: String => "atleast",
        sorting: String => "sorting",
        ai_art: bool => "aiArt",
        ttl_sec: u64 => "ttlSec"
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringOrList {
    String(String),
    List(Vec<String>),
}

optional_pack_struct! {
    RedditPack {
        subreddits: StringOrList => "subreddits",
        subthemes: Vec<String> => "subthemes",
        allow_sfw: bool => "allowSfw",
        min_width: u32 => "minWidth",
        min_height: u32 => "minHeight",
        ratio_w: f64 => "ratioW",
        ratio_h: f64 => "ratioH",
        sort: String => "sort",
        time: String => "time",
        ttl_sec: u64 => "ttlSec"
    }
}

optional_pack_struct! {
    UnsplashPack {
        api_key: String => "apiKey",
        api_key_env: String => "apiKeyEnv",
        query: String => "query",
        subthemes: Vec<String> => "subthemes",
        topics: String => "topics",
        collections: String => "collections",
        username: String => "username",
        orientation: String => "orientation",
        content_filter: String => "contentFilter",
        image_width: u32 => "imageWidth",
        image_height: u32 => "imageHeight",
        image_fit: String => "imageFit",
        image_quality: u32 => "imageQuality",
        ttl_sec: u64 => "ttlSec"
    }
}

optional_pack_struct! {
    GenericJsonPack {
        endpoint: String => "endpoint",
        image_path: String => "imagePath",
        image_prefix: String => "imagePrefix",
        candidate_limit: usize => "candidateLimit",
        post_path: String => "postPath",
        post_prefix: String => "postPrefix",
        author_name_path: String => "authorNamePath",
        author_url_path: String => "authorUrlPath",
        author_url_prefix: String => "authorUrlPrefix",
        domain: String => "domain",
        ttl_sec: u64 => "ttlSec"
    }
}

optional_pack_struct! {
    StaticUrlPack {
        url: String => "url",
        urls: Vec<String> => "urls",
        author_name: String => "authorName",
        author_url: String => "authorUrl",
        domain: String => "domain",
        post_url: String => "postUrl",
        different_images: bool => "differentImages",
        count: u32 => "count",
        ttl_sec: u64 => "ttlSec"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub errors: Vec<String>,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid config: {}", self.errors.join("; "))
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn provider_credentials(&self, provider: &str) -> Option<&ProviderCredentials> {
        self.provider_credentials.get(provider)
    }

    pub fn promote_legacy_provider_credentials(&mut self, provider: &str) -> bool {
        if self
            .provider_credentials(provider)
            .is_some_and(ProviderCredentials::is_configured)
        {
            return false;
        }

        let mut direct = None;
        let mut variable = None;
        for pack in self.packs.values().filter(|pack| pack.kind() == provider) {
            let (pack_direct, pack_variable) = pack.credentials();
            if !merge_unique(&mut direct, pack_direct)
                || !merge_unique(&mut variable, pack_variable)
            {
                return false;
            }
        }
        let credentials = ProviderCredentials {
            api_key: direct,
            api_key_env: variable,
        };
        if !credentials.is_configured() {
            return false;
        }
        self.provider_credentials
            .insert(provider.to_owned(), credentials);
        self.clear_legacy_provider_credentials(provider);
        true
    }

    pub fn clear_legacy_provider_credentials(&mut self, provider: &str) {
        for pack in self
            .packs
            .values_mut()
            .filter(|pack| pack.kind() == provider)
        {
            pack.clear_credentials();
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut errors = Vec::new();
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            errors.push(format!("schemaVersion must be {CONFIG_SCHEMA_VERSION}"));
        }
        if self.rotation_interval_seconds == 0 {
            errors.push("rotation_interval_seconds must be > 0".into());
        }
        if !is_supported_transition(&self.transition.kind) {
            errors.push(format!(
                "transition.type must be one of: {}",
                SUPPORTED_TRANSITIONS.join(", ")
            ));
        }
        if self.transition.fps == 0 || self.transition.fps > 240 {
            errors.push("transition.fps must be between 1 and 240".into());
        }
        if !self.transition.duration.is_finite()
            || self.transition.duration < 0.0
            || self.transition.duration > 60.0
        {
            errors.push("transition.duration must be between 0 and 60".into());
        }
        if self.cache.dir.trim().is_empty() || self.cache.download_dir.trim().is_empty() {
            errors.push("cache.dir and cache.downloadDir are required".into());
        }
        if self.cache.max_mb == 0 {
            errors.push("cache.maxMB must be > 0".into());
        }
        if self.cache.default_ttl_sec == 0 {
            errors.push("cache.defaultTtlSec must be > 0".into());
        }
        for (name, pack) in &self.packs {
            validate_pack(name, pack, &mut errors);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError { errors })
        }
    }
}

pub const SUPPORTED_TRANSITIONS: &[&str] = &[
    "simple", "fade", "left", "right", "top", "bottom", "wipe", "wave", "grow", "center", "outer",
    "any", "random",
];

pub fn is_supported_transition(value: &str) -> bool {
    SUPPORTED_TRANSITIONS.contains(&value.trim().to_ascii_lowercase().as_str())
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            mode: Mode::Manual,
            wallpaper_backend: WallpaperBackend::Auto,
            rotation_interval_seconds: 1800,
            transition: TransitionConfig {
                kind: "center".into(),
                fps: 60,
                duration: 0.7,
                angle: None,
                pos: None,
            },
            selection: SelectionConfig {
                per_output_cooldown: 10,
                global_cooldown: 20,
                avoid_same_tick_duplicates: true,
            },
            cache: CacheConfig {
                dir: "~/.cache/kitowall".into(),
                download_dir: "~/Pictures/Wallpapers".into(),
                max_mb: 2048,
                default_ttl_sec: 604_800,
            },
            pool: PoolConfig::default(),
            provider_credentials: BTreeMap::new(),
            packs: BTreeMap::new(),
        }
    }
}

impl ProviderCredentials {
    pub fn is_configured(&self) -> bool {
        non_empty(self.api_key.as_deref()) || non_empty(self.api_key_env.as_deref())
    }
}

impl PackConfig {
    pub fn credentials(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::Wallhaven(pack) => (pack.api_key.as_deref(), pack.api_key_env.as_deref()),
            Self::Unsplash(pack) => (pack.api_key.as_deref(), pack.api_key_env.as_deref()),
            _ => (None, None),
        }
    }

    pub fn clear_credentials(&mut self) {
        match self {
            Self::Wallhaven(pack) => {
                pack.api_key = None;
                pack.api_key_env = None;
            }
            Self::Unsplash(pack) => {
                pack.api_key = None;
                pack.api_key_env = None;
            }
            _ => {}
        }
    }
}

fn non_empty(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}

fn merge_unique(target: &mut Option<String>, candidate: Option<&str>) -> bool {
    let Some(candidate) = candidate.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    match target {
        Some(value) => value == candidate,
        None => {
            *target = Some(candidate.to_owned());
            true
        }
    }
}

fn validate_pack(name: &str, pack: &PackConfig, errors: &mut Vec<String>) {
    let required = |value: Option<&str>| value.is_some_and(|v| !v.trim().is_empty());
    let prefix = || format!("pack \"{name}\"");
    match pack {
        PackConfig::Local { paths } if paths.is_empty() => {
            errors.push(format!(
                "{}: local.paths must be a non-empty array",
                prefix()
            ));
        }
        PackConfig::Wallhaven(pack) => {
            if !required(pack.keyword.as_deref()) {
                errors.push(format!("{}: wallhaven.keyword is required", prefix()));
            }
            for (field, value) in [("categories", &pack.categories), ("purity", &pack.purity)] {
                if value.as_deref().is_some_and(|v| {
                    v.len() != 3 || !v.bytes().all(|byte| matches!(byte, b'0' | b'1'))
                }) {
                    errors.push(format!(
                        "{}: wallhaven.{field} must be a 3-bit mask",
                        prefix()
                    ));
                }
            }
            validate_positive(name, "wallhaven.ttlSec", pack.ttl_sec, errors);
        }
        PackConfig::Reddit(pack) => {
            let present = match &pack.subreddits {
                Some(StringOrList::String(value)) => !value.trim().is_empty(),
                Some(StringOrList::List(values)) => !values.is_empty(),
                None => false,
            };
            if !present {
                errors.push(format!("{}: reddit.subreddits is required", prefix()));
            }
            validate_positive(name, "reddit.ttlSec", pack.ttl_sec, errors);
        }
        PackConfig::Unsplash(pack) => {
            if !required(pack.query.as_deref()) {
                errors.push(format!("{}: unsplash.query is required", prefix()));
            }
            validate_positive(name, "unsplash.ttlSec", pack.ttl_sec, errors);
        }
        PackConfig::GenericJson(pack) => {
            if !required(pack.endpoint.as_deref()) {
                errors.push(format!("{}: generic_json.endpoint is required", prefix()));
            }
            if !required(pack.image_path.as_deref()) {
                errors.push(format!("{}: generic_json.imagePath is required", prefix()));
            }
            validate_positive(name, "generic_json.ttlSec", pack.ttl_sec, errors);
        }
        PackConfig::StaticUrl(pack) => {
            let has_url = required(pack.url.as_deref());
            let has_urls = pack.urls.as_ref().is_some_and(|urls| !urls.is_empty());
            if !has_url && !has_urls {
                errors.push(format!("{}: static_url requires url or urls", prefix()));
            }
            validate_positive(name, "static_url.count", pack.count, errors);
            validate_positive(name, "static_url.ttlSec", pack.ttl_sec, errors);
        }
        _ => {}
    }
}

fn validate_positive<T>(name: &str, field: &str, value: Option<T>, errors: &mut Vec<String>)
where
    T: Default + PartialEq,
{
    if value.is_some_and(|value| value == T::default()) {
        errors.push(format!("pack \"{name}\": {field} must be > 0"));
    }
}

pub fn normalize_pack_name(input: &str) -> String {
    let mut normalized = String::new();
    let mut previous_dash = false;
    for character in input.trim().to_ascii_lowercase().chars() {
        let character = if character.is_ascii_whitespace() || character == '_' {
            '-'
        } else {
            character
        };
        if character.is_ascii_alphanumeric() {
            normalized.push(character);
            previous_dash = false;
        } else if character == '-' && !previous_dash && !normalized.is_empty() {
            normalized.push('-');
            previous_dash = true;
        }
    }
    normalized.trim_end_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn zero_duration_is_a_valid_immediate_transition() {
        let mut config = Config::default();
        config.transition.duration = 0.0;
        config.validate().unwrap();
    }

    #[test]
    fn legacy_field_names_round_trip() {
        let json = serde_json::json!({
            "schemaVersion": 1,
            "mode": "manual",
            "wallpaper_backend": "auto",
            "rotation_interval_seconds": 1800,
            "transition": {"type": "center", "fps": 60, "duration": 0.7},
            "selection": {"perOutputCooldown": 10, "globalCooldown": 20, "avoidSameTickDuplicates": true},
            "cache": {"dir": "~/.cache/kitowall", "downloadDir": "~/Pictures/Wallpapers", "maxMB": 2048, "defaultTtlSec": 604800},
            "pool": {"enabled": false, "sources": [], "dedupe": "path"},
            "packs": {"local-one": {"type": "local", "paths": ["/wallpapers"]}}
        });
        let config: Config = serde_json::from_value(json.clone()).unwrap();
        config.validate().unwrap();
        assert_eq!(serde_json::to_value(config).unwrap(), json);
    }

    #[test]
    fn promotes_one_legacy_key_to_shared_provider_credentials() {
        let mut config = Config::default();
        for name in ["anime", "landscapes"] {
            config.packs.insert(
                name.into(),
                PackConfig::Wallhaven(WallhavenPack {
                    api_key: Some("shared-secret".into()),
                    keyword: Some(name.into()),
                    ..WallhavenPack::default()
                }),
            );
        }

        assert!(config.promote_legacy_provider_credentials("wallhaven"));
        assert_eq!(
            config
                .provider_credentials("wallhaven")
                .unwrap()
                .api_key
                .as_deref(),
            Some("shared-secret")
        );
        assert!(config
            .packs
            .values()
            .all(|pack| pack.credentials() == (None, None)));
    }

    #[test]
    fn conflicting_legacy_keys_are_not_migrated_implicitly() {
        let mut config = Config::default();
        for (name, key) in [("one", "first"), ("two", "second")] {
            config.packs.insert(
                name.into(),
                PackConfig::Wallhaven(WallhavenPack {
                    api_key: Some(key.into()),
                    keyword: Some(name.into()),
                    ..WallhavenPack::default()
                }),
            );
        }

        assert!(!config.promote_legacy_provider_credentials("wallhaven"));
        assert!(config.provider_credentials("wallhaven").is_none());
        assert!(config
            .packs
            .values()
            .all(|pack| pack.credentials().0.is_some()));
    }

    #[test]
    fn invalid_provider_is_reported() {
        let mut config = Config::default();
        config.packs.insert(
            "remote".into(),
            PackConfig::StaticUrl(StaticUrlPack::default()),
        );
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("url or urls"));
    }

    #[test]
    fn pack_names_match_typescript_normalization() {
        assert_eq!(normalize_pack_name("  My_pack !! name "), "my-pack-name");
    }
}
