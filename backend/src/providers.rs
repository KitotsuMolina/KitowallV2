use std::collections::BTreeSet;
use std::env;

use serde::Serialize;
use serde_json::Value;

use crate::config::{
    GenericJsonPack, PackConfig, ProviderCredentials, RedditPack, StringOrList, UnsplashPack,
    WallhavenPack,
};
use crate::remote_store::{query_url, sha256_hex};
use crate::{
    CacheManager, HttpTransport, ProviderIndex, ProviderStatus, RemoteCandidate, RemoteError,
    RemoteStore, StoreError,
};

pub enum ConfiguredProvider {
    Wallhaven {
        config: WallhavenPack,
        store: RemoteStore,
    },
    Reddit {
        config: RedditPack,
        store: RemoteStore,
    },
    Unsplash {
        config: UnsplashPack,
        store: RemoteStore,
    },
    GenericJson {
        config: GenericJsonPack,
        store: RemoteStore,
    },
}

impl ConfiguredProvider {
    pub fn from_pack(
        name: &str,
        pack: &PackConfig,
        credentials: Option<&ProviderCredentials>,
        cache: CacheManager,
    ) -> Option<Self> {
        let provider = match pack {
            PackConfig::Wallhaven(config) => Self::Wallhaven {
                config: wallhaven_with_credentials(config, credentials),
                store: RemoteStore::new(name, "wallhaven", cache),
            },
            PackConfig::Reddit(config) => Self::Reddit {
                config: config.clone(),
                store: RemoteStore::new(name, "reddit", cache),
            },
            PackConfig::Unsplash(config) => Self::Unsplash {
                config: unsplash_with_credentials(config, credentials),
                store: RemoteStore::new(name, "unsplash", cache),
            },
            PackConfig::GenericJson(config) => Self::GenericJson {
                config: config.clone(),
                store: RemoteStore::new(name, "generic_json", cache),
            },
            _ => return None,
        };
        Some(provider)
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Wallhaven { .. } => "wallhaven",
            Self::Reddit { .. } => "reddit",
            Self::Unsplash { .. } => "unsplash",
            Self::GenericJson { .. } => "generic_json",
        }
    }

    pub fn refresh<T: HttpTransport>(
        &self,
        transport: &T,
        now_ms: u64,
    ) -> Result<usize, ProviderError> {
        match self {
            Self::Wallhaven { config, store } => {
                refresh_wallhaven(config, store, transport, now_ms)
            }
            Self::Reddit { config, store } => refresh_reddit(config, store, transport, now_ms),
            Self::Unsplash { config, store } => refresh_unsplash(config, store, transport, now_ms),
            Self::GenericJson { config, store } => {
                refresh_generic_json(config, store, transport, now_ms)
            }
        }
    }

    pub fn list(&self) -> Result<Vec<RemoteCandidate>, ProviderError> {
        match self {
            Self::Wallhaven { config, store } => Ok(store.list(Some(&wallhaven_hash(config)?))?),
            Self::Reddit { store, .. }
            | Self::Unsplash { store, .. }
            | Self::GenericJson { store, .. } => Ok(store.list(None)?),
        }
    }

    pub fn status(&self) -> Result<ProviderStatus, ProviderError> {
        let store = match self {
            Self::Wallhaven { store, .. }
            | Self::Reddit { store, .. }
            | Self::Unsplash { store, .. }
            | Self::GenericJson { store, .. } => store,
        };
        Ok(store.status(None)?)
    }

    pub fn hydrate<T: HttpTransport>(
        &self,
        candidate: &RemoteCandidate,
        transport: &T,
        now_ms: u64,
    ) -> Result<std::path::PathBuf, ProviderError> {
        let store = match self {
            Self::Wallhaven { store, .. }
            | Self::Reddit { store, .. }
            | Self::Unsplash { store, .. }
            | Self::GenericJson { store, .. } => store,
        };
        Ok(store.hydrate(candidate, transport, now_ms)?)
    }

    pub fn local_path_for(&self, candidate: &RemoteCandidate) -> std::path::PathBuf {
        let store = match self {
            Self::Wallhaven { store, .. }
            | Self::Reddit { store, .. }
            | Self::Unsplash { store, .. }
            | Self::GenericJson { store, .. } => store,
        };
        store.local_path_for(candidate)
    }
}

fn wallhaven_with_credentials(
    config: &WallhavenPack,
    credentials: Option<&ProviderCredentials>,
) -> WallhavenPack {
    let mut config = config.clone();
    apply_credentials(&mut config.api_key, &mut config.api_key_env, credentials);
    config
}

fn unsplash_with_credentials(
    config: &UnsplashPack,
    credentials: Option<&ProviderCredentials>,
) -> UnsplashPack {
    let mut config = config.clone();
    apply_credentials(&mut config.api_key, &mut config.api_key_env, credentials);
    config
}

fn apply_credentials(
    direct: &mut Option<String>,
    variable: &mut Option<String>,
    credentials: Option<&ProviderCredentials>,
) {
    if direct
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || variable
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        return;
    }
    if let Some(credentials) = credentials {
        *direct = credentials.api_key.clone();
        *variable = credentials.api_key_env.clone();
    }
}

fn refresh_wallhaven<T: HttpTransport>(
    config: &WallhavenPack,
    store: &RemoteStore,
    transport: &T,
    now_ms: u64,
) -> Result<usize, ProviderError> {
    let key = resolve_api_key(config.api_key.as_deref(), config.api_key_env.as_deref())
        .ok_or(ProviderError::MissingApiKey("wallhaven"))?;
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    for query in build_queries(config.keyword.as_deref(), config.subthemes.as_deref()) {
        let mut pairs = Vec::new();
        push_pair(&mut pairs, "q", non_empty_option(Some(&query)));
        push_pair(&mut pairs, "categories", effective_categories(config));
        push_pair(&mut pairs, "purity", effective_purity(config));
        push_pair(&mut pairs, "sorting", config.sorting.clone());
        push_pair(&mut pairs, "atleast", config.atleast.clone());
        push_pair(&mut pairs, "colors", config.colors.clone());
        if let Some(ratios) = &config.ratios {
            if !ratios.is_empty() {
                pairs.push(("ratios", ratios.join(",")));
            }
        }
        if let Some(ai_art) = config.ai_art {
            pairs.push(("ai_art_filter", if ai_art { "0" } else { "1" }.into()));
        }
        let url = query_url("https://wallhaven.cc/api/v1/search", &pairs);
        let response = transport.get_with_headers(&url, &[("X-API-Key", &key)])?;
        ensure_success(&response)?;
        let json: Value = serde_json::from_slice(&response.body)?;
        for item in json
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(image_url) = item.get("path").and_then(Value::as_str) else {
                continue;
            };
            if !seen.insert(image_url.to_owned()) {
                continue;
            }
            let mut candidate = RemoteCandidate::new(
                sha256_hex(&format!("{}:{image_url}", store.pack_name())),
                "wallhaven",
                image_url.into(),
            );
            candidate.remote_id = string_value(item.get("id"));
            candidate.page_url = string_value(item.get("url"));
            candidate.rating = item
                .get("purity")
                .and_then(Value::as_str)
                .and_then(map_purity)
                .map(str::to_owned);
            candidate.colors = item
                .get("colors")
                .and_then(Value::as_array)
                .map(|colors| {
                    colors
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .filter(|colors| !colors.is_empty());
            if let Some((width, height)) = item
                .get("resolution")
                .and_then(Value::as_str)
                .and_then(parse_resolution)
            {
                candidate.width = Some(width);
                candidate.height = Some(height);
            }
            candidate.ttl_sec = config.ttl_sec;
            candidates.push(candidate);
        }
    }
    let config_hash = wallhaven_hash(config)?;
    let previous = store.load()?;
    let mut merged = if previous.config_hash.as_deref() == Some(config_hash.as_str()) {
        previous.candidates
    } else {
        Vec::new()
    };
    let mut known_urls = merged
        .iter()
        .map(|candidate| candidate.url.clone())
        .collect::<BTreeSet<_>>();
    let mut count = 0;
    for candidate in candidates {
        if known_urls.insert(candidate.url.clone()) {
            merged.push(candidate);
            count += 1;
        }
    }
    store.save(&ProviderIndex {
        updated_at: now_ms,
        config_hash: Some(config_hash),
        candidates: merged,
    })?;
    Ok(count)
}

fn refresh_reddit<T: HttpTransport>(
    config: &RedditPack,
    store: &RemoteStore,
    transport: &T,
    now_ms: u64,
) -> Result<usize, ProviderError> {
    let subreddits = normalize_subreddits(config.subreddits.as_ref());
    if subreddits.is_empty() {
        return Err(ProviderError::MissingField("reddit.subreddits"));
    }
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    for query in build_queries(Some(store.pack_name()), config.subthemes.as_deref()) {
        let path = format!("/r/{}/search.json", subreddits.join("+"));
        let mut pairs = vec![("q", query), ("restrict_sr", "1".into())];
        push_pair(&mut pairs, "sort", config.sort.clone());
        push_pair(&mut pairs, "t", config.time.clone());
        let mut payload = None;
        let mut last_transport_error = None;
        for host in ["https://www.reddit.com", "https://old.reddit.com"] {
            let url = query_url(&format!("{host}{path}"), &pairs);
            let response = match transport.get_with_headers(
                &url,
                &[
                    ("Accept", "application/json"),
                    ("Accept-Language", "en-US,en;q=0.8"),
                ],
            ) {
                Ok(response) => response,
                Err(error) => {
                    last_transport_error = Some(error);
                    continue;
                }
            };
            if response.status == 403 || response.status == 429 {
                continue;
            }
            ensure_success(&response)?;
            payload = Some(serde_json::from_slice::<Value>(&response.body)?);
            break;
        }
        let json = payload.ok_or_else(|| {
            last_transport_error
                .map(ProviderError::Transport)
                .unwrap_or(ProviderError::HttpStatus(403))
        })?;
        for child in json
            .pointer("/data/children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let data = child.get("data").unwrap_or(child);
            if data.get("post_hint").and_then(Value::as_str) != Some("image") {
                continue;
            }
            let over_18 = data
                .get("over_18")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if config.allow_sfw.unwrap_or(false) && over_18 {
                continue;
            }
            let Some(image) = data.pointer("/preview/images/0/source") else {
                continue;
            };
            let width = image.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
            let height = image.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
            if width < config.min_width.unwrap_or(0) || height < config.min_height.unwrap_or(0) {
                continue;
            }
            if !passes_ratio(width, height, config.ratio_w, config.ratio_h) {
                continue;
            }
            let Some(raw_url) = image.get("url").and_then(Value::as_str) else {
                continue;
            };
            let image_url = raw_url.replace("&amp;", "&");
            if !seen.insert(image_url.clone()) {
                continue;
            }
            let mut candidate = RemoteCandidate::new(
                sha256_hex(&format!("{}:{image_url}", store.pack_name())),
                "reddit",
                image_url.clone(),
            );
            candidate.preview_url = Some(image_url);
            candidate.page_url = data
                .get("permalink")
                .and_then(Value::as_str)
                .map(|path| format!("https://www.reddit.com{path}"));
            candidate.rating = Some(if over_18 { "nsfw" } else { "safe" }.into());
            candidate.score = data.get("ups").and_then(Value::as_i64);
            candidate.width = Some(width);
            candidate.height = Some(height);
            candidate.ttl_sec = config.ttl_sec;
            candidates.push(candidate);
        }
    }
    let count = candidates.len();
    store.save(&ProviderIndex {
        updated_at: now_ms,
        config_hash: None,
        candidates,
    })?;
    Ok(count)
}

fn refresh_unsplash<T: HttpTransport>(
    config: &UnsplashPack,
    store: &RemoteStore,
    transport: &T,
    now_ms: u64,
) -> Result<usize, ProviderError> {
    let key = resolve_api_key(config.api_key.as_deref(), config.api_key_env.as_deref())
        .ok_or(ProviderError::MissingApiKey("unsplash"))?;
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    for query in build_queries(config.query.as_deref(), config.subthemes.as_deref()) {
        let mut pairs = vec![("count", "1".into()), ("client_id", key.clone())];
        push_pair(&mut pairs, "query", non_empty_option(Some(&query)));
        push_pair(&mut pairs, "collections", config.collections.clone());
        push_pair(&mut pairs, "topics", config.topics.clone());
        push_pair(&mut pairs, "username", config.username.clone());
        push_pair(&mut pairs, "orientation", config.orientation.clone());
        push_pair(&mut pairs, "content_filter", config.content_filter.clone());
        let response =
            transport.get(&query_url("https://api.unsplash.com/photos/random", &pairs))?;
        ensure_success(&response)?;
        let payload: Value = serde_json::from_slice(&response.body)?;
        let items = payload.as_array().cloned().unwrap_or_else(|| vec![payload]);
        for item in items {
            let Some(raw) = item.pointer("/urls/raw").and_then(Value::as_str) else {
                continue;
            };
            let image_url = unsplash_image_url(raw, config);
            if !seen.insert(image_url.clone()) {
                continue;
            }
            let mut candidate = RemoteCandidate::new(
                sha256_hex(&format!("{}:{image_url}", store.pack_name())),
                "unsplash",
                image_url.clone(),
            );
            candidate.preview_url = Some(image_url);
            candidate.page_url = item
                .pointer("/links/html")
                .and_then(Value::as_str)
                .map(str::to_owned);
            candidate.author = item
                .pointer("/user/name")
                .and_then(Value::as_str)
                .map(str::to_owned);
            candidate.author_url = item
                .pointer("/user/links/html")
                .and_then(Value::as_str)
                .map(str::to_owned);
            candidate.ttl_sec = config.ttl_sec;
            candidates.push(candidate);
        }
    }
    let count = candidates.len();
    store.save(&ProviderIndex {
        updated_at: now_ms,
        config_hash: None,
        candidates,
    })?;
    Ok(count)
}

fn refresh_generic_json<T: HttpTransport>(
    config: &GenericJsonPack,
    store: &RemoteStore,
    transport: &T,
    now_ms: u64,
) -> Result<usize, ProviderError> {
    let endpoint = required(config.endpoint.as_deref(), "generic_json.endpoint")?;
    let image_path = required(config.image_path.as_deref(), "generic_json.imagePath")?;
    let response = transport.get(endpoint)?;
    ensure_success(&response)?;
    let json: Value = serde_json::from_slice(&response.body)?;
    let max = config.candidate_limit.unwrap_or(50);
    let mut candidates = Vec::new();
    let iterations = if image_path.contains("@random") {
        max
    } else {
        1
    };
    let mut seen = BTreeSet::new();
    for seed in 0..iterations {
        let Some((target, resolved)) = json_target(&json, image_path, seed) else {
            continue;
        };
        let values = target.as_array().cloned().unwrap_or_else(|| vec![target]);
        for value in values {
            if candidates.len() >= max {
                break;
            }
            let Some(raw) = scalar_string(&value) else {
                continue;
            };
            let image_url = format!("{}{}", config.image_prefix.as_deref().unwrap_or(""), raw);
            if !seen.insert(image_url.clone()) {
                continue;
            }
            let mut candidate = RemoteCandidate::new(
                sha256_hex(&format!("{}:{image_url}", store.pack_name())),
                "generic_json",
                image_url,
            );
            candidate.page_url = related_value(&json, config.post_path.as_deref(), &resolved)
                .map(|value| format!("{}{}", config.post_prefix.as_deref().unwrap_or(""), value));
            candidate.author = related_value(&json, config.author_name_path.as_deref(), &resolved);
            candidate.author_url =
                related_value(&json, config.author_url_path.as_deref(), &resolved).map(|value| {
                    format!(
                        "{}{}",
                        config.author_url_prefix.as_deref().unwrap_or(""),
                        value
                    )
                });
            candidate.ttl_sec = config.ttl_sec;
            candidates.push(candidate);
        }
    }
    let count = candidates.len();
    store.save(&ProviderIndex {
        updated_at: now_ms,
        config_hash: None,
        candidates,
    })?;
    Ok(count)
}

fn ensure_success(response: &crate::HttpResponse) -> Result<(), ProviderError> {
    if (200..300).contains(&response.status) {
        Ok(())
    } else {
        Err(ProviderError::HttpStatus(response.status))
    }
}

fn resolve_api_key(direct: Option<&str>, variable: Option<&str>) -> Option<String> {
    direct
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            variable
                .and_then(|name| env::var(name).ok())
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
}

fn effective_categories(config: &WallhavenPack) -> Option<String> {
    config.categories.clone().or_else(|| {
        (config.category_general.is_some()
            || config.category_anime.is_some()
            || config.category_people.is_some())
        .then(|| {
            format!(
                "{}{}{}",
                bit(config.category_general),
                bit(config.category_anime),
                bit(config.category_people)
            )
        })
    })
}
fn effective_purity(config: &WallhavenPack) -> Option<String> {
    config.purity.clone().or_else(|| {
        (config.allow_sfw.is_some()
            || config.allow_sketchy.is_some()
            || config.allow_nsfw.is_some())
        .then(|| {
            format!(
                "{}{}{}",
                bit(config.allow_sfw),
                bit(config.allow_sketchy),
                bit(config.allow_nsfw)
            )
        })
    })
}
fn bit(value: Option<bool>) -> u8 {
    value.unwrap_or(false) as u8
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WallhavenHash<'a> {
    keyword: &'a str,
    subthemes: &'a [String],
    categories: &'a str,
    purity: &'a str,
    ratios: &'a [String],
    colors: &'a str,
    atleast: &'a str,
    sorting: &'a str,
    ai_art: Option<bool>,
}
fn wallhaven_hash(config: &WallhavenPack) -> Result<String, ProviderError> {
    let categories = effective_categories(config).unwrap_or_default();
    let purity = effective_purity(config).unwrap_or_default();
    Ok(sha256_hex(&serde_json::to_string(&WallhavenHash {
        keyword: config.keyword.as_deref().unwrap_or(""),
        subthemes: config.subthemes.as_deref().unwrap_or(&[]),
        categories: &categories,
        purity: &purity,
        ratios: config.ratios.as_deref().unwrap_or(&[]),
        colors: config.colors.as_deref().unwrap_or(""),
        atleast: config.atleast.as_deref().unwrap_or(""),
        sorting: config.sorting.as_deref().unwrap_or(""),
        ai_art: config.ai_art,
    })?))
}

fn build_queries(base: Option<&str>, subthemes: Option<&[String]>) -> Vec<String> {
    let base = base.unwrap_or("").trim();
    let mut values = Vec::new();
    if !base.is_empty() {
        values.push(base.to_owned());
    }
    for subtheme in subthemes.unwrap_or(&[]) {
        let subtheme = subtheme.trim();
        if !subtheme.is_empty() {
            values.push(if base.is_empty() {
                subtheme.into()
            } else {
                format!("{base} {subtheme}")
            });
        }
    }
    if values.is_empty() {
        values.push(String::new());
    }
    values
}

fn push_pair(pairs: &mut Vec<(&'static str, String)>, key: &'static str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        pairs.push((key, value));
    }
}
fn non_empty_option(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}
fn required<'a>(value: Option<&'a str>, name: &'static str) -> Result<&'a str, ProviderError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(ProviderError::MissingField(name))
}
fn string_value(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| {
        value
            .as_str()
            .map(str::to_owned)
            .or_else(|| value.as_i64().map(|v| v.to_string()))
    })
}
fn map_purity(value: &str) -> Option<&'static str> {
    if value.as_bytes().first() == Some(&b'1')
        && value.as_bytes().get(1) == Some(&b'0')
        && value.as_bytes().get(2) == Some(&b'0')
    {
        Some("safe")
    } else if value.as_bytes().get(1) == Some(&b'1') {
        Some("sketchy")
    } else if value.as_bytes().get(2) == Some(&b'1') {
        Some("nsfw")
    } else {
        None
    }
}
fn parse_resolution(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}
fn normalize_subreddits(value: Option<&StringOrList>) -> Vec<String> {
    match value {
        Some(StringOrList::String(value)) => value
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect(),
        Some(StringOrList::List(values)) => values
            .iter()
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect(),
        None => Vec::new(),
    }
}
fn passes_ratio(width: u32, height: u32, ratio_w: Option<f64>, ratio_h: Option<f64>) -> bool {
    let (Some(ratio_w), Some(ratio_h)) = (ratio_w, ratio_h) else {
        return true;
    };
    ratio_w <= 0.0 || ratio_h <= 0.0 || width as f64 / ratio_w * ratio_h >= height as f64
}
fn unsplash_image_url(raw: &str, config: &UnsplashPack) -> String {
    let mut pairs = Vec::new();
    if let Some(value) = config.image_width {
        pairs.push(("w", value.to_string()));
    }
    if let Some(value) = config.image_height {
        pairs.push(("h", value.to_string()));
    }
    push_pair(&mut pairs, "fit", config.image_fit.clone());
    if let Some(value) = config.image_quality {
        pairs.push(("q", value.to_string()));
    }
    query_url(raw, &pairs)
}

fn json_target(input: &Value, path: &str, seed: usize) -> Option<(Value, String)> {
    let mut current = input;
    let mut resolved = String::new();
    for (position, raw) in path.split('.').enumerate() {
        if position > 0 {
            resolved.push('.');
        }
        if raw == "$" {
            resolved.push('$');
            continue;
        }
        let (key, index) = raw
            .split_once('[')
            .map(|(key, rest)| (key, Some(rest.trim_end_matches(']'))))
            .unwrap_or((raw, None));
        if !key.is_empty() {
            current = current.get(key)?;
            resolved.push_str(key);
        }
        if let Some(index) = index {
            let array = current.as_array()?;
            if array.is_empty() {
                return None;
            }
            let chosen = if index == "@random" {
                seed % array.len()
            } else {
                index.parse().ok()?
            };
            current = array.get(chosen)?;
            resolved.push_str(&format!("[{chosen}]"));
        }
    }
    Some((current.clone(), resolved))
}
fn related_value(json: &Value, path: Option<&str>, resolved: &str) -> Option<String> {
    let path = path?.trim();
    if path.is_empty() {
        return None;
    }
    let concrete = replace_random(path, resolved);
    json_target(json, &concrete, 0).and_then(|(value, _)| scalar_string(&value))
}
fn replace_random(path: &str, resolved: &str) -> String {
    let indexes = resolved
        .split('[')
        .skip(1)
        .filter_map(|part| part.split(']').next())
        .collect::<Vec<_>>();
    let mut result = path.to_owned();
    for index in indexes {
        result = result.replacen("@random", index, 1);
    }
    result
}
fn scalar_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|v| v.to_string()))
        .or_else(|| value.as_u64().map(|v| v.to_string()))
}

#[derive(Debug)]
pub enum ProviderError {
    MissingApiKey(&'static str),
    MissingField(&'static str),
    HttpStatus(u16),
    Transport(crate::TransportError),
    Store(StoreError),
    Remote(RemoteError),
    Json(serde_json::Error),
}
impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingApiKey(name) => {
                write!(f, "missing API key for {name}; configure apiKeyEnv")
            }
            Self::MissingField(name) => write!(f, "missing required field: {name}"),
            Self::HttpStatus(value) => write!(f, "HTTP request failed with status {value}"),
            Self::Transport(error) => error.fmt(f),
            Self::Store(error) => error.fmt(f),
            Self::Remote(error) => error.fmt(f),
            Self::Json(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ProviderError {}
impl From<crate::TransportError> for ProviderError {
    fn from(value: crate::TransportError) -> Self {
        Self::Transport(value)
    }
}
impl From<StoreError> for ProviderError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}
impl From<RemoteError> for ProviderError {
    fn from(value: RemoteError) -> Self {
        Self::Remote(value)
    }
}
impl From<serde_json::Error> for ProviderError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, HttpResponse, TransportError};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    type RecordedCall = (String, Vec<(String, String)>);

    struct FakeTransport {
        responses: RefCell<VecDeque<HttpResponse>>,
        calls: RefCell<Vec<RecordedCall>>,
    }

    impl FakeTransport {
        fn json(values: Vec<Value>) -> Self {
            Self {
                responses: RefCell::new(
                    values
                        .into_iter()
                        .map(|value| HttpResponse {
                            status: 200,
                            body: serde_json::to_vec(&value).unwrap(),
                            content_type: Some("application/json".into()),
                        })
                        .collect(),
                ),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl HttpTransport for FakeTransport {
        fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
            self.get_with_headers(url, &[])
        }

        fn get_with_headers(
            &self,
            url: &str,
            headers: &[(&str, &str)],
        ) -> Result<HttpResponse, TransportError> {
            self.calls.borrow_mut().push((
                url.into(),
                headers
                    .iter()
                    .map(|(name, value)| ((*name).into(), (*value).into()))
                    .collect(),
            ));
            self.responses
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| TransportError::new("no fake response"))
        }
    }

    fn cache(name: &str) -> (PathBuf, CacheManager) {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kitowall-{name}-{id}"));
        let mut config = Config::default().cache;
        config.dir = root.join("cache").to_string_lossy().into_owned();
        config.download_dir = root.join("downloads").to_string_lossy().into_owned();
        let manager = CacheManager::new(&config, &root);
        (root, manager)
    }

    #[test]
    fn wallhaven_applies_filters_and_api_header() {
        let (root, cache) = cache("wallhaven");
        let config = WallhavenPack {
            api_key: Some("secret".into()),
            keyword: Some("sao".into()),
            subthemes: Some(vec!["dark".into()]),
            category_general: Some(true),
            category_anime: Some(true),
            category_people: Some(false),
            allow_sfw: Some(true),
            allow_sketchy: Some(false),
            allow_nsfw: Some(false),
            ratios: Some(vec!["16x9".into()]),
            atleast: Some("1920x1080".into()),
            sorting: Some("random".into()),
            ai_art: Some(false),
            ..WallhavenPack::default()
        };
        let payload = serde_json::json!({"data": [{
            "id": "abc", "path": "https://w.wallhaven.cc/a.jpg",
            "url": "https://wallhaven.cc/w/abc", "purity": "100",
            "resolution": "1920x1080", "colors": ["#663399", "#000000"]
        }]});
        let transport = FakeTransport::json(vec![payload.clone(), payload]);
        let provider = ConfiguredProvider::Wallhaven {
            config,
            store: RemoteStore::new("sao", "wallhaven", cache),
        };
        assert_eq!(provider.refresh(&transport, 10).unwrap(), 1);
        let calls = transport.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].0.contains("categories=110"));
        assert!(calls[0].0.contains("purity=100"));
        assert!(calls[0].0.contains("ratios=16x9"));
        assert!(!calls[0].0.contains("secret"));
        assert!(calls[0].1.contains(&("X-API-Key".into(), "secret".into())));
        let candidates = provider.list().unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].width, Some(1920));
        assert_eq!(
            candidates[0].colors,
            Some(vec!["#663399".into(), "#000000".into()])
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn wallhaven_rejects_missing_api_key_before_network() {
        let (root, cache) = cache("wallhaven-no-key");
        let provider = ConfiguredProvider::Wallhaven {
            config: WallhavenPack {
                keyword: Some("landscape".into()),
                ..WallhavenPack::default()
            },
            store: RemoteStore::new("landscape", "wallhaven", cache),
        };
        let transport = FakeTransport::json(Vec::new());
        assert!(matches!(
            provider.refresh(&transport, 10),
            Err(ProviderError::MissingApiKey("wallhaven"))
        ));
        assert!(transport.calls.borrow().is_empty());
        if root.exists() {
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn wallhaven_refresh_extends_the_index_for_the_same_configuration() {
        let (root, cache) = cache("wallhaven-merge");
        let config = WallhavenPack {
            api_key: Some("secret".into()),
            keyword: Some("sao".into()),
            sorting: Some("random".into()),
            ..WallhavenPack::default()
        };
        let first = serde_json::json!({"data": [{
            "id": "one",
            "path": "https://w.wallhaven.cc/one.jpg",
            "url": "https://wallhaven.cc/w/one"
        }]});
        let second = serde_json::json!({"data": [{
            "id": "two",
            "path": "https://w.wallhaven.cc/two.jpg",
            "url": "https://wallhaven.cc/w/two"
        }]});
        let transport = FakeTransport::json(vec![first, second]);
        let provider = ConfiguredProvider::Wallhaven {
            config,
            store: RemoteStore::new("sao", "wallhaven", cache),
        };

        assert_eq!(provider.refresh(&transport, 10).unwrap(), 1);
        assert_eq!(provider.refresh(&transport, 20).unwrap(), 1);
        assert_eq!(provider.list().unwrap().len(), 2);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn wallhaven_pack_inherits_shared_provider_credentials() {
        let (root, cache) = cache("wallhaven-shared-key");
        let credentials = ProviderCredentials {
            api_key: Some("shared-secret".into()),
            api_key_env: None,
        };
        let provider = ConfiguredProvider::from_pack(
            "landscape",
            &PackConfig::Wallhaven(WallhavenPack {
                keyword: Some("landscape".into()),
                ..WallhavenPack::default()
            }),
            Some(&credentials),
            cache,
        )
        .unwrap();

        match provider {
            ConfiguredProvider::Wallhaven { config, .. } => {
                assert_eq!(config.api_key.as_deref(), Some("shared-secret"));
            }
            _ => panic!("expected wallhaven provider"),
        }
        if root.exists() {
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn reddit_filters_resolution_and_decodes_preview_url() {
        let (root, cache) = cache("reddit");
        let config = RedditPack {
            subreddits: Some(StringOrList::List(vec!["wallpapers".into()])),
            allow_sfw: Some(true),
            min_width: Some(1920),
            min_height: Some(1080),
            ratio_w: Some(16.0),
            ratio_h: Some(9.0),
            ..RedditPack::default()
        };
        let payload = serde_json::json!({"data": {"children": [{"data": {
            "post_hint": "image", "over_18": false, "permalink": "/r/w/one", "ups": 42,
            "preview": {"images": [{"source": {
                "width": 1920, "height": 1080, "url": "https://img.test/a.jpg?x=1&amp;y=2"
            }}]}
        }}]}});
        let transport = FakeTransport::json(vec![payload]);
        let provider = ConfiguredProvider::Reddit {
            config,
            store: RemoteStore::new("anime", "reddit", cache),
        };
        assert_eq!(provider.refresh(&transport, 10).unwrap(), 1);
        let candidates = provider.list().unwrap();
        assert_eq!(candidates[0].score, Some(42));
        assert!(candidates[0].url.contains("&y=2"));
        assert!(transport.calls.borrow()[0].0.contains("q=anime"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsplash_applies_search_and_image_parameters() {
        let (root, cache) = cache("unsplash");
        let config = UnsplashPack {
            api_key: Some("secret".into()),
            query: Some("cyberpunk".into()),
            orientation: Some("landscape".into()),
            content_filter: Some("high".into()),
            image_width: Some(1920),
            image_height: Some(1080),
            image_fit: Some("crop".into()),
            image_quality: Some(80),
            ..UnsplashPack::default()
        };
        let payload = serde_json::json!({
            "urls": {"raw": "https://images.unsplash.com/photo?ixid=one"},
            "links": {"html": "https://unsplash.com/photos/one"},
            "user": {"name": "Author", "links": {"html": "https://unsplash.com/@author"}}
        });
        let transport = FakeTransport::json(vec![payload]);
        let provider = ConfiguredProvider::Unsplash {
            config,
            store: RemoteStore::new("cyber", "unsplash", cache),
        };
        assert_eq!(provider.refresh(&transport, 10).unwrap(), 1);
        let request = &transport.calls.borrow()[0].0;
        assert!(request.contains("query=cyberpunk"));
        assert!(request.contains("orientation=landscape"));
        let candidate = provider.list().unwrap().remove(0);
        assert!(candidate.url.contains("w=1920"));
        assert!(candidate.url.contains("h=1080"));
        assert_eq!(candidate.author.as_deref(), Some("Author"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generic_json_correlates_random_item_metadata() {
        let (root, cache) = cache("generic");
        let config = GenericJsonPack {
            endpoint: Some("https://api.test/items".into()),
            image_path: Some("$.items[@random].image".into()),
            image_prefix: Some("https://cdn.test/".into()),
            candidate_limit: Some(2),
            post_path: Some("$.items[@random].post".into()),
            author_name_path: Some("$.items[@random].author".into()),
            ..GenericJsonPack::default()
        };
        let payload = serde_json::json!({"items": [
            {"image": "a.jpg", "post": "post-a", "author": "A"},
            {"image": "b.jpg", "post": "post-b", "author": "B"}
        ]});
        let transport = FakeTransport::json(vec![payload]);
        let provider = ConfiguredProvider::GenericJson {
            config,
            store: RemoteStore::new("json", "generic_json", cache),
        };
        assert_eq!(provider.refresh(&transport, 10).unwrap(), 2);
        let candidates = provider.list().unwrap();
        assert_eq!(candidates[0].author.as_deref(), Some("A"));
        assert_eq!(candidates[0].page_url.as_deref(), Some("post-a"));
        assert_eq!(candidates[1].author.as_deref(), Some("B"));
        fs::remove_dir_all(root).unwrap();
    }
}
