//! Context-window discovery, caching, and provenance.

use kitsu_providers::compiled::CompiledProvider;
use kitsu_providers::{ProviderError, ProviderRouteCurrent};

use crate::presentation::terminal_inline;
#[derive(Clone)]
pub(crate) struct ContextWindowRefreshConfig {
    pub(crate) cache_path: std::path::PathBuf,
    pub(crate) base_url: String,
    pub(crate) models_path: Option<String>,
}

impl ContextWindowRefreshConfig {
    pub(crate) fn cache_key(&self, model: &str) -> String {
        context_window_cache_key(&self.base_url, self.models_path.as_deref(), model)
    }

    pub(crate) fn enabled(&self) -> bool {
        self.models_path.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContextWindowSource {
    Cli,
    Profile,
    DiskCache,
    LiveDiscovery,
    ServedWindow,
    ConservativeDefault,
}

impl ContextWindowSource {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Cli => "CLI --context-window",
            Self::Profile => "gateway profile context_window",
            Self::DiskCache => "disk cache",
            Self::LiveDiscovery => "live model discovery",
            Self::ServedWindow => "runtime-served window (clamped)",
            Self::ConservativeDefault => "conservative default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContextWindowCacheState {
    NotConsulted,
    Miss,
    Unreadable,
    WriteFailed,
    Fresh { age_secs: u64, ttl_secs: u64 },
    Stale { age_secs: u64, ttl_secs: u64 },
}

impl ContextWindowCacheState {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::NotConsulted => "not consulted".to_string(),
            Self::Miss => "cold (no entry)".to_string(),
            Self::Unreadable => "cold (cache unreadable or invalid)".to_string(),
            Self::WriteFailed => "cache write failed".to_string(),
            Self::Fresh { age_secs, ttl_secs } => format!(
                "fresh (age {}; TTL {})",
                compact_duration(*age_secs),
                compact_duration(*ttl_secs)
            ),
            Self::Stale { age_secs, ttl_secs } => format!(
                "stale (age {}; TTL {})",
                compact_duration(*age_secs),
                compact_duration(*ttl_secs)
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContextWindowDiscoveryState {
    NotNeeded,
    Disabled,
    Pending,
    ReadyForNextRun { window: u64 },
    CompleteButNotCached { window: u64 },
    FinishedWithoutMetadata,
    LiveComplete,
}

impl ContextWindowDiscoveryState {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::NotNeeded => "not needed".to_string(),
            Self::Disabled => "disabled (models_path is not configured)".to_string(),
            Self::Pending => {
                "in progress (current run keeps the default; result applies next run)".to_string()
            }
            Self::ReadyForNextRun { window } => format!(
                "complete ({window} tokens cached for the next run; current run keeps the default)"
            ),
            Self::CompleteButNotCached { window } => format!(
                "complete ({window} tokens discovered, but the private cache write failed; current run keeps the default)"
            ),
            Self::FinishedWithoutMetadata => {
                "finished without usable metadata (or the advisory request failed)".to_string()
            }
            Self::LiveComplete => "complete for the current model".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextWindowProvenance {
    pub(crate) source: ContextWindowSource,
    pub(crate) cache: ContextWindowCacheState,
    pub(crate) discovery: ContextWindowDiscoveryState,
}

impl ContextWindowProvenance {
    pub(crate) fn pinned(source: ContextWindowSource) -> Self {
        Self {
            source,
            cache: ContextWindowCacheState::NotConsulted,
            discovery: ContextWindowDiscoveryState::NotNeeded,
        }
    }

    pub(crate) fn cached(age_secs: u64) -> Self {
        Self {
            source: ContextWindowSource::DiskCache,
            cache: ContextWindowCacheState::Fresh {
                age_secs,
                ttl_secs: CONTEXT_WINDOW_CACHE_TTL_SECS,
            },
            discovery: ContextWindowDiscoveryState::NotNeeded,
        }
    }

    pub(crate) fn defaulted(
        cache: ContextWindowCacheState,
        discovery: ContextWindowDiscoveryState,
    ) -> Self {
        Self {
            source: ContextWindowSource::ConservativeDefault,
            cache,
            discovery,
        }
    }

    pub(crate) fn live_discovery(window: Option<u64>) -> Self {
        match window {
            Some(_) => Self {
                source: ContextWindowSource::LiveDiscovery,
                cache: ContextWindowCacheState::NotConsulted,
                discovery: ContextWindowDiscoveryState::LiveComplete,
            },
            None => Self::defaulted(
                ContextWindowCacheState::NotConsulted,
                ContextWindowDiscoveryState::FinishedWithoutMetadata,
            ),
        }
    }

    pub(crate) fn refreshed(cache_written: bool) -> Self {
        Self {
            source: ContextWindowSource::LiveDiscovery,
            cache: if cache_written {
                ContextWindowCacheState::Fresh {
                    age_secs: 0,
                    ttl_secs: CONTEXT_WINDOW_CACHE_TTL_SECS,
                }
            } else {
                ContextWindowCacheState::WriteFailed
            },
            discovery: ContextWindowDiscoveryState::LiveComplete,
        }
    }
}

pub(crate) fn compact_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 24 * 60 * 60 {
        let hours = seconds / (60 * 60);
        let minutes = (seconds % (60 * 60)) / 60;
        if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        }
    } else {
        let days = seconds / (24 * 60 * 60);
        let hours = (seconds % (24 * 60 * 60)) / (60 * 60);
        if hours == 0 {
            format!("{days}d")
        } else {
            format!("{days}d {hours}h")
        }
    }
}

/// How long a discovered context window is trusted before re-discovering. A
/// model's window essentially never changes, so this just spares every later
/// invocation of the same gateway+model a `/v1/models` round-trip purely to
/// re-learn a number we already know.
pub(crate) const CONTEXT_WINDOW_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// On-disk cache for discovered context windows, keyed by an opaque local hash
/// of the endpoint and model so URL credentials never appear in the file.
/// Without this, `build_setup` paid a blocking `/v1/models` round-trip on
/// *every single* `ask`/`chat` invocation whenever `context_window` wasn't
/// pre-configured — doubling perceived startup latency on a slow gateway, even
/// for one-off `ask` calls where the result is immediately thrown away.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct ContextWindowCache {
    #[serde(default)]
    pub(crate) entries: std::collections::BTreeMap<String, (u64, u64)>,
}

pub(crate) fn context_window_cache_path(state_dir: &std::path::Path) -> std::path::PathBuf {
    state_dir.join("context_window_cache.json")
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn context_window_cache_key(
    base_url: &str,
    models_path: Option<&str>,
    model: &str,
) -> String {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let digest = [base_url, models_path.unwrap_or(""), model]
        .into_iter()
        .flat_map(|part| part.as_bytes().iter().copied().chain(std::iter::once(0)))
        .fold(OFFSET, |hash, byte| {
            (hash ^ u128::from(byte)).wrapping_mul(PRIME)
        });
    format!("v2-fnv128-{digest:032x}")
}

/// Status output should identify the endpoint without echoing credentials that
/// a user may have embedded in URL user-info or a query string.
pub(crate) fn redact_endpoint(endpoint: &str) -> String {
    let mut display = endpoint.to_string();
    if let Some(query) = display.find('?') {
        display.truncate(query);
        display.push_str("?<redacted>");
    }
    if let Some(authority_start) = display.find("://").map(|index| index + 3) {
        let authority_end = display[authority_start..]
            .find('/')
            .map(|index| authority_start + index)
            .unwrap_or(display.len());
        if let Some(at) = display[authority_start..authority_end].rfind('@') {
            let at = authority_start + at;
            display.replace_range(authority_start..at, "<redacted>");
        }
    }
    terminal_inline(&display)
}

pub(crate) struct CachedContextWindow {
    pub(crate) window: Option<u64>,
    pub(crate) state: ContextWindowCacheState,
}

pub(crate) fn inspect_cached_context_window_at(
    path: &std::path::Path,
    key: &str,
    now: u64,
) -> CachedContextWindow {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CachedContextWindow {
                window: None,
                state: ContextWindowCacheState::Miss,
            };
        }
        Err(_) => {
            return CachedContextWindow {
                window: None,
                state: ContextWindowCacheState::Unreadable,
            };
        }
    };
    let cache: ContextWindowCache = match serde_json::from_str(&text) {
        Ok(cache) => cache,
        Err(_) => {
            return CachedContextWindow {
                window: None,
                state: ContextWindowCacheState::Unreadable,
            };
        }
    };
    let Some((window, fetched_at)) = cache.entries.get(key) else {
        return CachedContextWindow {
            window: None,
            state: ContextWindowCacheState::Miss,
        };
    };
    let age_secs = now.saturating_sub(*fetched_at);
    if age_secs < CONTEXT_WINDOW_CACHE_TTL_SECS {
        CachedContextWindow {
            window: Some(*window),
            state: ContextWindowCacheState::Fresh {
                age_secs,
                ttl_secs: CONTEXT_WINDOW_CACHE_TTL_SECS,
            },
        }
    } else {
        CachedContextWindow {
            window: None,
            state: ContextWindowCacheState::Stale {
                age_secs,
                ttl_secs: CONTEXT_WINDOW_CACHE_TTL_SECS,
            },
        }
    }
}

pub(crate) fn inspect_cached_context_window(
    path: &std::path::Path,
    key: &str,
) -> CachedContextWindow {
    inspect_cached_context_window_at(path, key, unix_now())
}

pub(crate) fn read_cached_context_window(path: &std::path::Path, key: &str) -> Option<u64> {
    inspect_cached_context_window(path, key).window
}

pub(crate) fn write_cached_context_window(
    path: &std::path::Path,
    key: &str,
    window: u64,
) -> std::io::Result<()> {
    use std::io::Write;
    let mut cache: ContextWindowCache = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    // Discard prior unstable/incomplete cache identities on the next write.
    cache.entries.retain(|key, _| key.starts_with("v2-fnv128-"));
    cache.entries.insert(key.to_string(), (window, unix_now()));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec(&cache).map_err(std::io::Error::other)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&json)?;
    file.flush()
}

pub(crate) enum ContextWindowRefreshOutcome {
    Cached(u64),
    DiscoveredButNotCached(u64),
    NoMetadata,
}

/// Perform one advisory context-window discovery and cache only a numeric
/// result. Provider diagnostics are intentionally not accepted here, so an
/// endpoint URL or response body can never enter the private cache.
pub(crate) async fn refresh_cached_context_window(
    provider: &ProviderRouteCurrent<CompiledProvider>,
    model: &str,
    path: &std::path::Path,
    key: &str,
) -> Result<ContextWindowRefreshOutcome, ProviderError> {
    let Some(window) = provider.advisory_model_context_length(model).await? else {
        return Ok(ContextWindowRefreshOutcome::NoMetadata);
    };
    if write_cached_context_window(path, key, window).is_ok() {
        Ok(ContextWindowRefreshOutcome::Cached(window))
    } else {
        Ok(ContextWindowRefreshOutcome::DiscoveredButNotCached(window))
    }
}

pub(crate) async fn refresh_cached_context_window_explicit(
    provider: &ProviderRouteCurrent<CompiledProvider>,
    model: &str,
    path: &std::path::Path,
    key: &str,
) -> Result<ContextWindowRefreshOutcome, ProviderError> {
    let Some(window) = provider.model_context_length(model).await? else {
        return Ok(ContextWindowRefreshOutcome::NoMetadata);
    };
    if write_cached_context_window(path, key, window).is_ok() {
        Ok(ContextWindowRefreshOutcome::Cached(window))
    } else {
        Ok(ContextWindowRefreshOutcome::DiscoveredButNotCached(window))
    }
}
