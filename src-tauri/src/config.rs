//! Runtime configuration.
//!
//! Mirrors the web build's `config.ts` defaults, including the environment
//! overrides that matter on a desktop (`YTDLP_PATH`, timeouts, providers).
//! Everything is read once at startup.

use std::sync::OnceLock;
use std::time::Duration;

/* --------------------------------- yt-dlp --------------------------------- */

pub fn ytdlp_path() -> &'static str {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| env::var("YTDLP_PATH").unwrap_or_else(|_| "yt-dlp".to_string()))
        .as_str()
}

pub fn ytdlp_timeout() -> Duration {
    static V: OnceLock<Duration> = OnceLock::new();
    *V.get_or_init(|| env_millis("YTDLP_TIMEOUT_MS", 30_000))
}

pub fn subs_timeout() -> Duration {
    static V: OnceLock<Duration> = OnceLock::new();
    *V.get_or_init(|| env_millis("LYRICS_SUBS_TIMEOUT_MS", 12_000))
}

pub fn search_limit() -> usize {
    static V: OnceLock<usize> = OnceLock::new();
    *V.get_or_init(|| {
        env::var("YTDLP_SEARCH_LIMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(20)
            .min(50)
    })
}

/// Home networks often have a broken IPv6 route: yt-dlp tries IPv6 first and
/// hangs until it times out, so force IPv4 unless told otherwise.
pub fn force_ipv4() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| env_bool("YTDLP_FORCE_IPV4", true))
}

pub fn extra_ytdlp_args() -> &'static [String] {
    static V: OnceLock<Vec<String>> = OnceLock::new();
    V.get_or_init(|| {
        env::var("YTDLP_EXTRA_ARGS")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    })
    .as_slice()
}

/* ---------------------------------- cache --------------------------------- */

pub const SEARCH_TTL: Duration = Duration::from_secs(10 * 60);
pub const METADATA_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub const CHART_TTL: Duration = Duration::from_secs(30 * 60);
pub const RECOMMENDATION_TTL: Duration = Duration::from_secs(30 * 60);
pub const LYRICS_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub const LYRICS_MAX_ENTRIES: usize = 400;
pub const CACHE_MAX_ENTRIES: usize = 500;
pub const STREAM_TTL: Duration = Duration::from_secs(30 * 60);

/* -------------------------------- providers ------------------------------- */

pub fn charts_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| env_bool("ENABLE_CHARTS_PROVIDER", true))
}

pub fn charts_base() -> &'static str {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        env::var("CHARTS_API_BASE")
            .unwrap_or_else(|_| "https://itunes.apple.com".to_string())
            .trim_end_matches('/')
            .to_string()
    })
    .as_str()
}

pub fn lyrics_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| env_bool("ENABLE_LYRICS_PROVIDER", true))
}

pub fn lyrics_base() -> &'static str {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        env::var("LYRICS_API_BASE")
            .unwrap_or_else(|_| "https://lrclib.net".to_string())
            .trim_end_matches('/')
            .to_string()
    })
    .as_str()
}

pub fn lyrics_timeout() -> Duration {
    static V: OnceLock<Duration> = OnceLock::new();
    *V.get_or_init(|| env_millis("LYRICS_TIMEOUT_MS", 10_000))
}

/* --------------------------------- helpers -------------------------------- */

use std::env;

fn env_millis(key: &str, fallback: u64) -> Duration {
    Duration::from_millis(
        env::var(key)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(fallback),
    )
}

fn env_bool(key: &str, fallback: bool) -> bool {
    match env::var(key) {
        Ok(v) if !v.is_empty() => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => fallback,
    }
}
