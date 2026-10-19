//! HTTP clients used by the metadata providers and the media reader.
//!
//! Two agents: one for small JSON calls (short timeouts, size-capped bodies)
//! and one for media bytes, which are read incrementally over a long-lived
//! response body. Both treat 4xx/5xx as ordinary responses so callers can
//! inspect the status (`http_status_as_error(false)`).

use std::sync::OnceLock;
use std::time::Duration;

use serde::de::DeserializeOwned;
use ureq::Agent;

const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Ytrium/1.0";

/// Bodies larger than this are refused for JSON endpoints.
const JSON_BODY_LIMIT: u64 = 8 * 1024 * 1024;

/// Small, fast requests: charts, lyrics, lookups.
pub fn json_agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = Agent::config_builder()
            .user_agent(UA)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(15)))
            .timeout_recv_body(Some(Duration::from_secs(30)))
            .build();
        Agent::new_with_config(config)
    })
}

/// Media requests: no overall deadline (a stream can take minutes), but a
/// stalled socket is cut loose instead of hanging forever.
pub fn media_agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = Agent::config_builder()
            .user_agent(UA)
            .http_status_as_error(false)
            .max_redirects(6)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(900)))
            .build();
        Agent::new_with_config(config)
    })
}

/// Lyrics lookups get the web build's tighter deadline (`LYRICS_TIMEOUT_MS`):
/// a slow lyrics provider must never hold a track's UI back.
pub fn lyrics_agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let timeout = crate::config::lyrics_timeout();
        let config = Agent::config_builder()
            .user_agent(UA)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_recv_response(Some(timeout))
            .timeout_recv_body(Some(timeout))
            .build();
        Agent::new_with_config(config)
    })
}

/// A failed metadata request. Providers mostly turn these into fallbacks.
#[derive(Debug)]
pub enum FetchError {
    /// The server answered with a non-2xx status.
    Status(u16),
    /// Socket/DNS/TLS/timeout failure.
    Transport(String),
    /// 2xx but the payload was not what we expected.
    Decode(String),
}

impl FetchError {
    pub fn message(&self) -> String {
        match self {
            FetchError::Status(s) => format!("upstream responded with HTTP {s}"),
            FetchError::Transport(m) => m.clone(),
            FetchError::Decode(m) => m.clone(),
        }
    }
}

impl From<ureq::Error> for FetchError {
    fn from(err: ureq::Error) -> Self {
        match err {
            ureq::Error::StatusCode(code) => FetchError::Status(code),
            other => FetchError::Transport(other.to_string()),
        }
    }
}

/// GET a JSON document. Non-2xx responses surface as `FetchError::Status`.
pub fn get_json<T: DeserializeOwned>(agent: &Agent, url: &str) -> Result<T, FetchError> {
    let res = agent.get(url).header("accept", "application/json").call()?;
    let status = res.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(FetchError::Status(status));
    }
    let text = res
        .into_body()
        .with_config()
        .limit(JSON_BODY_LIMIT)
        .read_to_string()
        .map_err(|e| FetchError::Transport(e.to_string()))?;
    serde_json::from_str::<T>(&text).map_err(|e| FetchError::Decode(e.to_string()))
}

/// GET a text document (used for subtitle/caption tracks).
pub fn get_text(agent: &Agent, url: &str, limit: u64) -> Result<String, FetchError> {
    let res = agent.get(url).call()?;
    let status = res.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(FetchError::Status(status));
    }
    res.into_body()
        .with_config()
        .limit(limit)
        .read_to_string()
        .map_err(|e| FetchError::Transport(e.to_string()))
}
