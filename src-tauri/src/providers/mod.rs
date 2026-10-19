//! Provider layer: normalized types plus the shared title/artist helpers.
//!
//! Ported from the web build's `providers/types.ts` + the shared helpers in
//! `providers/youtube.ts`. Field names are camelCase on the wire because the
//! frontend's `normalizeTrack` reads them exactly as the server sent them.

pub mod charts;
pub mod lyrics;
pub mod recommend;
pub mod youtube;

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

/* ---------------------------------- types --------------------------------- */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// Stable identifier, e.g. `yt:dQw4w9WgXcQ` or `itunes:123456`.
    pub id: String,
    pub source: String,
    /// Native provider id (YouTube video id) when playback can be resolved.
    pub source_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    pub duration: u32,
    pub thumbnail: Option<String>,
    pub view_count: Option<i64>,
    pub genre: Option<String>,
    pub release_date: Option<String>,
    pub url: Option<String>,
    /// Query used to lazily resolve playback for metadata-only sources.
    pub resolve_query: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub image: Option<String>,
    pub description: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub image: Option<String>,
    pub release_date: Option<String>,
    pub track_count: Option<usize>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartsResult {
    pub trending: Vec<Track>,
    pub popular: Vec<Track>,
    pub new_releases: Vec<Track>,
    pub popular_artists: Vec<Artist>,
    pub popular_albums: Vec<Album>,
    pub charts_source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSource {
    pub url: String,
    /// Epoch ms after which the URL should be considered expired.
    pub expires_at: i64,
    pub content_type: Option<String>,
}

/* --------------------------------- helpers -------------------------------- */

/// `\s*-\s*Topic$` — official artist channels are not artist names.
static OFFICIAL_ARTIST_CHANNEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*-\s*Topic$").expect("topic regex"));

/// Display-only title noise that YouTube uploaders add.
static NOISE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)\((?:official\s+)?(?:music\s+)?video\)",
        r"(?i)\[(?:official\s+)?(?:music\s+)?video\]",
        r"(?i)\((?:official\s+)?audio\)",
        r"(?i)\[(?:official\s+)?audio\]",
        r"(?i)\(lyrics?\)",
        r"(?i)\(lyric\s+video\)",
        r"(?i)\((?:lyric\s+)?video\)",
        r"(?i)\(visuali[sz]er\)",
        r"(?i)\(official\s+visuali[sz]er\)",
        r"(?i)\(hd\)",
        r"(?i)\(4k\)",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("noise regex"))
    .collect()
});

static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s{2,}").expect("space regex"));
static DANGLING_DASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+[-–]\s*$").expect("dash regex"));
static NON_ALNUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-z0-9]+").expect("alnum regex"));
static DATE_8: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{8}$").expect("date regex"));
static DATE_ISO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}").expect("iso date regex"));
static DATE_YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}$").expect("year regex"));

pub fn artist_id_for(name: &str) -> String {
    name.trim().to_lowercase()
}

pub fn album_id_for(artist: &str, album: &str) -> String {
    format!("{}::{}", artist_id_for(artist), album.trim().to_lowercase())
}

pub fn thumbnail_for(video_id: &str) -> String {
    format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")
}

/// Light, conservative title cleanup for display purposes only.
pub fn clean_title(raw: &str) -> String {
    let mut t = raw.trim().to_string();
    for re in NOISE_PATTERNS.iter() {
        t = re.replace_all(&t, "").into_owned();
    }
    t = MULTI_SPACE.replace_all(&t, " ").into_owned();
    t = DANGLING_DASH.replace_all(&t, "").into_owned();
    let t = t.trim().to_string();
    if t.is_empty() {
        raw.trim().to_string()
    } else {
        t
    }
}

/// Strip a leading "Artist - " from the title when it duplicates the artist.
pub fn split_title(title: &str, artist: &str) -> String {
    if artist.is_empty() {
        return title.to_string();
    }
    let prefix = format!("{artist} - ");
    if title.to_lowercase().starts_with(&prefix.to_lowercase()) {
        // Char-based slicing: `to_lowercase` can change byte lengths.
        let rest: String = title.chars().skip(prefix.chars().count()).collect();
        let rest = rest.trim().to_string();
        if !rest.is_empty() {
            return rest;
        }
    }
    title.to_string()
}

/// Fold a string down to lowercase alphanumerics (lyrics matching).
pub fn normalize_key(s: &str) -> String {
    NON_ALNUM.replace_all(&s.to_lowercase(), "").into_owned()
}

/// `20240131` / `2024-01-31T…` / `2024` -> `YYYY-MM-DD`.
pub fn normalize_date(value: Option<&str>) -> Option<String> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    if DATE_8.is_match(raw) {
        return Some(format!("{}-{}-{}", &raw[0..4], &raw[4..6], &raw[6..8]));
    }
    if DATE_ISO.is_match(raw) {
        return Some(raw[0..10].to_string());
    }
    if DATE_YEAR.is_match(raw) {
        return Some(format!("{raw}-01-01"));
    }
    None
}

pub fn strip_topic(name: &str) -> String {
    OFFICIAL_ARTIST_CHANNEL.replace(name, "").trim().to_string()
}

/// Deduplicate tracks by id, preserving order.
pub fn dedupe_tracks(tracks: Vec<Track>) -> Vec<Track> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(tracks.len());
    for t in tracks {
        if seen.insert(t.id.clone()) {
            out.push(t);
        }
    }
    out
}

/// Build the `artists`/`albums` side-lists the search endpoint returns.
pub fn build_search_results(tracks: Vec<Track>) -> SearchResults {
    let mut artists: Vec<Artist> = Vec::new();
    let mut albums: Vec<Album> = Vec::new();
    let mut artist_ids = std::collections::HashSet::new();
    let mut album_ids = std::collections::HashSet::new();

    for t in &tracks {
        if let Some(artist_id) = t.artist_id.as_ref() {
            if artist_ids.insert(artist_id.clone()) {
                artists.push(Artist {
                    id: artist_id.clone(),
                    name: t.artist.clone(),
                    image: t.thumbnail.clone(),
                    description: None,
                    source: t.source.clone(),
                });
            }
        }
        if let (Some(album), Some(album_id)) = (t.album.as_ref(), t.album_id.as_ref()) {
            if album_ids.insert(album_id.clone()) {
                albums.push(Album {
                    id: album_id.clone(),
                    title: album.clone(),
                    artist: t.artist.clone(),
                    artist_id: t.artist_id.clone(),
                    image: t.thumbnail.clone(),
                    release_date: t.release_date.clone(),
                    track_count: None,
                    source: t.source.clone(),
                });
            }
        }
    }

    SearchResults {
        tracks,
        artists,
        albums,
    }
}
