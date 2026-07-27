pub mod cache;
pub mod catalog;
pub mod config;
pub mod controller;
pub mod favorites;
pub mod history;
pub mod jobs;
pub mod library;
pub mod local_provider;
pub mod logs;
pub mod media_preview;
pub mod packs;
pub mod providers;
pub mod remote_store;
pub mod selector;
pub mod state;
pub mod static_url_provider;
pub mod store;
pub mod transport;

pub use cache::{
    CacheEntry, CacheIndex, CacheManager, CachePrunePlan, CachePruneResult, CacheStatus,
};
pub use catalog::{
    list_wallpapers, WallpaperCatalogFacets, WallpaperCatalogItem, WallpaperCatalogPage,
};
pub use config::{Config, ConfigError, PackConfig, ProviderCredentials, CONFIG_SCHEMA_VERSION};
pub use controller::{
    apply_next, apply_wallpaper, apply_wallpaper_batch, ApplyNextOptions, ApplyNextResult,
    ApplyOutcome, ApplyWallpaperBatchOptions, ApplyWallpaperBatchResult, ApplyWallpaperOptions,
    ApplyWallpaperResult, ApplyWallpaperTarget, WallpaperApply, WallpaperGateway,
};
pub use history::{HistoryEntry, HistoryState};
pub use jobs::{JobKind, JobRecord, JobStatus, JobStore};
pub use library::ResolvedPool;
pub use local_provider::{LocalProvider, WallpaperCandidate};
pub use logs::{LogEntry, LogLevel, LogStore};
pub use packs::{PackCatalogError, PackSummary, RemovePackResult};
pub use providers::{ConfiguredProvider, ProviderError};
pub use remote_store::{ProviderIndex, ProviderStatus, RemoteError, RemoteStore};
pub use selector::{pick_images_for_outputs, OutputPick};
pub use state::{State, STATE_SCHEMA_VERSION};
pub use static_url_provider::{RemoteCandidate, StaticUrlIndex, StaticUrlProvider};
pub use store::{config_path, inspect_config, inspect_state, state_path, JsonStore, StoreError};
pub use transport::{
    HttpResponse, HttpTransport, TransportError, UreqTransport, UreqTransportConfig,
};
