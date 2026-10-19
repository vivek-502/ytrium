//! yt-dlp process driver.
//!
//! Ported from the web build's `services/ytdlp.ts`: an explicit argument array
//! (no shell, so no injection), a hard timeout that kills the process, and the
//! same stable classification of failures into user-facing errors.

use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::de::DeserializeOwned;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::config;
use crate::error::{ApiError, ApiResult};

/// YouTube video ids are 11 chars of `[A-Za-z0-9_-]`. Anything else is rejected.
pub static VIDEO_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{11}$").expect("valid video id regex"));

pub fn is_valid_video_id(id: &str) -> bool {
    VIDEO_ID_RE.is_match(id)
}

/// Cap on captured stdout (32 MB, as on the server).
const MAX_STDOUT: usize = 32 * 1024 * 1024;
/// Cap on captured stderr; it is only used for classification and logs.
const MAX_STDERR: usize = 64 * 1024;

/// Run yt-dlp with an explicit argument array and a hard timeout.
pub async fn run_ytdlp(args: &[&str], timeout: Duration) -> ApiResult<String> {
    let mut cmd = Command::new(config::ytdlp_path());
    // A broken IPv6 route makes yt-dlp hang on the first connection attempt
    // until the timeout fires. --force-ipv4 keeps it on the working stack.
    if config::force_ipv4() {
        cmd.arg("--force-ipv4");
    }
    for extra in config::extra_ytdlp_args() {
        cmd.arg(extra);
    }
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ApiError::ytdlp_missing(Some(format!(
                "could not run `{}`",
                config::ytdlp_path()
            )))
        } else {
            ApiError::extraction_failed(Some(format!("could not start yt-dlp: {err}")))
        }
    })?;

    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");

    // Drain both pipes concurrently: a full pipe would otherwise deadlock the
    // child before it exits.
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::with_capacity(64 * 1024);
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match stdout.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() < MAX_STDOUT {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::with_capacity(8 * 1024);
        let mut chunk = [0u8; 8 * 1024];
        loop {
            match stderr.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() < MAX_STDERR {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
        buf
    });

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(err)) => return Err(ApiError::extraction_failed(Some(err.to_string()))),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(ApiError::timeout(Some(format!(
                "yt-dlp timed out after {}ms",
                timeout.as_millis()
            ))));
        }
    };

    let out = stdout_task.await.unwrap_or_default();
    let err = stderr_task.await.unwrap_or_default();
    let stdout = String::from_utf8_lossy(&out).into_owned();
    let stderr = String::from_utf8_lossy(&err).into_owned();

    if status.success() {
        if out.len() >= MAX_STDOUT {
            return Err(ApiError::upstream_unavailable(Some(
                "yt-dlp output exceeded limit".to_string(),
            )));
        }
        Ok(stdout)
    } else {
        Err(classify_failure(&stderr, status.code()))
    }
}

/// Map yt-dlp failures to stable, user-friendly errors.
pub fn classify_failure(stderr: &str, code: Option<i32>) -> ApiError {
    let s = stderr.to_lowercase();
    let slice = || Some(stderr.chars().take(400).collect::<String>());

    if s.contains("video unavailable") || s.contains("account associated with this video has been terminated") {
        return ApiError::video_unavailable(slice());
    }
    if s.contains("private video") || s.contains("sign in to confirm") || s.contains("age") {
        return ApiError::video_unavailable(Some("private or age-restricted content".into()));
    }
    if s.contains("not available in your country") || s.contains("blocked in your country") {
        return ApiError::video_unavailable(Some("not available in your region".into()));
    }
    if s.contains("unsupported url") {
        return ApiError::invalid_input(Some("unsupported url".into()));
    }
    if s.contains("no video formats found") || s.contains("requested format is not available") {
        return ApiError::extraction_failed(Some("no playable formats".into()));
    }
    if s.contains("getaddrinfo failed")
        || s.contains("network is unreachable")
        || s.contains("unable to download")
    {
        return ApiError::upstream_unavailable(Some("network failure".into()));
    }
    if s.contains("http error 429") || s.contains("rate") {
        return ApiError::rate_limited();
    }
    ApiError::extraction_failed(Some(format!(
        "yt-dlp exit {}",
        code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
    )))
}

/// Parse JSON lines produced by `--dump-json` (one object per line), skipping
/// malformed lines rather than failing the whole result set.
pub fn parse_json_lines<T: DeserializeOwned>(stdout: &str) -> Vec<T> {
    let mut out = Vec::new();
    for raw in stdout.split('\n') {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<T>(line) {
            out.push(value);
        }
    }
    out
}

/// Parse a single JSON document, tolerating trailing noise around it.
pub fn parse_json_doc<T: DeserializeOwned>(stdout: &str) -> Option<T> {
    let trimmed = stdout.trim();
    if let Ok(value) = serde_json::from_str::<T>(trimmed) {
        return Some(value);
    }
    let first = trimmed.find('{')?;
    let last = trimmed.rfind('}')?;
    if last <= first {
        return None;
    }
    serde_json::from_str::<T>(&trimmed[first..=last]).ok()
}
