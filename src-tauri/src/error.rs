//! Structured API errors.
//!
//! Every command returns `Result<T, ApiError>`. The serialized shape mirrors
//! the web build's `{"error":{"code","message","detail"}}` payloads so the
//! frontend's `ApiError` class keeps working unchanged, and the codes/statuses
//! are the same stable set the server used.

use std::fmt;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ApiError {
    pub code: String,
    pub status: u16,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ApiError {
    pub fn new(code: &str, status: u16, message: &str, detail: Option<String>) -> Self {
        Self {
            code: code.to_string(),
            status,
            message: message.to_string(),
            detail,
        }
    }

    pub fn invalid_input(detail: impl Into<Option<String>>) -> Self {
        Self::new(
            "INVALID_INPUT",
            400,
            "That request could not be understood.",
            detail.into(),
        )
    }

    pub fn not_found(message: &str) -> Self {
        Self::new("NOT_FOUND", 404, message, None)
    }

    pub fn upstream_unavailable(detail: impl Into<Option<String>>) -> Self {
        Self::new(
            "UPSTREAM_UNAVAILABLE",
            502,
            "The music service is unavailable right now. Please try again shortly.",
            detail.into(),
        )
    }

    pub fn video_unavailable(detail: impl Into<Option<String>>) -> Self {
        Self::new(
            "VIDEO_UNAVAILABLE",
            422,
            "This track is unavailable. It may have been removed or blocked in your region.",
            detail.into(),
        )
    }

    pub fn extraction_failed(detail: impl Into<Option<String>>) -> Self {
        Self::new(
            "EXTRACTION_FAILED",
            502,
            "Unable to prepare this track for playback. Try again.",
            detail.into(),
        )
    }

    pub fn timeout(detail: impl Into<Option<String>>) -> Self {
        Self::new(
            "TIMEOUT",
            504,
            "The request timed out. Please try again.",
            detail.into(),
        )
    }

    pub fn rate_limited() -> Self {
        Self::new(
            "RATE_LIMITED",
            429,
            "Too many requests. Please slow down.",
            None,
        )
    }

    pub fn internal(detail: impl Into<Option<String>>) -> Self {
        Self::new(
            "INTERNAL",
            500,
            "Something went wrong on our side.",
            detail.into(),
        )
    }

    pub fn ytdlp_missing(detail: impl Into<Option<String>>) -> Self {
        Self::new(
            "YTDLP_MISSING",
            503,
            "The media service (yt-dlp) is not installed. Install it and restart Ytrium.",
            detail.into(),
        )
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.code, self.status, self.message)
    }
}

impl std::error::Error for ApiError {}

pub type ApiResult<T> = Result<T, ApiError>;
