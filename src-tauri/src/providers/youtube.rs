//! YouTube provider: search, metadata and media-URL resolution through yt-dlp.
//!
//! Ported 1:1 from the web build's `providers/youtube.ts`, including the exact
//! yt-dlp argument lists, the quality selectors and the URL-expiry handling.
//! Resolved media URLs live in memory only, keyed by the `expire=` value
//! YouTube embeds, and are re-resolved on demand.

use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::Deserialize;

use super::{
    album_id_for, artist_id_for, build_search_results, clean_title, split_title, strip_topic,
    thumbnail_for, RemotePlaylist, SearchResults, StreamSource, Track,
};
use crate::cache::TtlCache;
use crate::config;
use crate::error::{ApiError, ApiResult};
use crate::ytdlp::{is_valid_video_id, parse_json_doc, parse_json_lines, run_ytdlp};

/* --------------------------------- caches --------------------------------- */

static SEARCH_CACHE: OnceLock<TtlCache<SearchResults>> = OnceLock::new();
static METADATA_CACHE: OnceLock<TtlCache<Option<Track>>> = OnceLock::new();
static STREAM_CACHE: OnceLock<TtlCache<StreamSource>> = OnceLock::new();
static PLAYLIST_CACHE: OnceLock<TtlCache<Vec<RemotePlaylist>>> = OnceLock::new();
static PLAYLIST_TRACKS_CACHE: OnceLock<TtlCache<Vec<Track>>> = OnceLock::new();

fn search_cache() -> &'static TtlCache<SearchResults> {
    SEARCH_CACHE.get_or_init(|| TtlCache::new(config::SEARCH_TTL, config::CACHE_MAX_ENTRIES))
}
fn metadata_cache() -> &'static TtlCache<Option<Track>> {
    METADATA_CACHE.get_or_init(|| TtlCache::new(config::METADATA_TTL, config::CACHE_MAX_ENTRIES))
}
fn stream_cache() -> &'static TtlCache<StreamSource> {
    STREAM_CACHE.get_or_init(|| TtlCache::new(config::STREAM_TTL, config::CACHE_MAX_ENTRIES))
}
fn playlist_cache() -> &'static TtlCache<Vec<RemotePlaylist>> {
    PLAYLIST_CACHE.get_or_init(|| TtlCache::new(config::SEARCH_TTL, config::CACHE_MAX_ENTRIES))
}
fn playlist_tracks_cache() -> &'static TtlCache<Vec<Track>> {
    PLAYLIST_TRACKS_CACHE
        .get_or_init(|| TtlCache::new(config::METADATA_TTL, config::CACHE_MAX_ENTRIES))
}

/* ------------------------------- yt-dlp json ------------------------------ */

/// One yt-dlp JSON object. `--dump-json` (flat) and `--dump-single-json`
/// (full) both land here; only the fields either mode provides are read.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Entry {
    id: Option<String>,
    title: Option<String>,
    duration: Option<f64>,
    uploader: Option<String>,
    channel: Option<String>,
    artist: Option<String>,
    uploader_id: Option<String>,
    view_count: Option<f64>,
    thumbnail: Option<String>,
    thumbnails: Option<Vec<ThumbEntry>>,
    live_status: Option<String>,
    /// Flat playlist entries carry the number of videos in the playlist.
    playlist_count: Option<f64>,
    /* full metadata only */
    album: Option<String>,
    genre: Option<String>,
    release_date: Option<String>,
    upload_date: Option<String>,
    webpage_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ThumbEntry {
    url: Option<String>,
    preference: Option<i64>,
}

fn pick_artist(e: &Entry) -> String {
    for candidate in [&e.artist, &e.uploader, &e.channel, &e.uploader_id]
        .into_iter()
        .flatten()
    {
        if !candidate.trim().is_empty() {
            return strip_topic(candidate);
        }
    }
    "Unknown Artist".to_string()
}

fn pick_thumb(e: &Entry, id: &str) -> String {
    if let Some(thumbs) = e.thumbnails.as_ref() {
        if !thumbs.is_empty() {
            let mut sorted: Vec<&ThumbEntry> = thumbs.iter().collect();
            sorted.sort_by_key(|t| std::cmp::Reverse(t.preference.unwrap_or(0)));
            if let Some(url) = sorted.first().and_then(|t| t.url.as_ref()) {
                if !url.is_empty() {
                    return url.clone();
                }
            }
        }
    }
    if let Some(thumb) = e.thumbnail.as_ref() {
        if !thumb.is_empty() {
            return thumb.clone();
        }
    }
    thumbnail_for(id)
}

fn duration_of(e: &Entry) -> u32 {
    match e.duration {
        Some(d) if d > 0.0 => d.round() as u32,
        _ => 0,
    }
}

fn from_flat(e: &Entry) -> Option<Track> {
    let id = e.id.as_deref()?;
    if !is_valid_video_id(id) {
        return None; // Filters playlists / non-video entries.
    }
    if let Some(live) = e.live_status.as_deref() {
        if live != "not_live" {
            return None;
        }
    }
    let artist = pick_artist(e);
    let raw_title = clean_title(e.title.as_deref().unwrap_or("Unknown title"));
    let title = split_title(&raw_title, &artist);
    Some(Track {
        id: format!("yt:{id}"),
        source: "youtube".to_string(),
        source_id: Some(id.to_string()),
        title,
        artist_id: Some(artist_id_for(&artist)),
        artist,
        album: None,
        album_id: None,
        duration: duration_of(e),
        thumbnail: Some(pick_thumb(e, id)),
        view_count: e.view_count.map(|v| v.round() as i64),
        genre: None,
        release_date: None,
        url: Some(watch_url(id)),
        resolve_query: None,
    })
}

fn from_full(e: &Entry, fallback_id: &str) -> Option<Track> {
    let id = match e.id.as_deref() {
        Some(candidate) if is_valid_video_id(candidate) => candidate.to_string(),
        _ => fallback_id.to_string(),
    };
    if !is_valid_video_id(&id) {
        return None;
    }
    let artist = pick_artist(e);
    let raw_title = clean_title(e.title.as_deref().unwrap_or("Unknown title"));
    let title = split_title(&raw_title, &artist);
    let album = e
        .album
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string);
    let release_date =
        super::normalize_date(e.release_date.as_deref().or(e.upload_date.as_deref()));
    let genre = e
        .genre
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(str::to_string);
    Some(Track {
        id: format!("yt:{id}"),
        source: "youtube".to_string(),
        source_id: Some(id.clone()),
        title,
        artist_id: Some(artist_id_for(&artist)),
        album_id: album.as_deref().map(|a| album_id_for(&artist, a)),
        album,
        artist,
        duration: duration_of(e),
        thumbnail: Some(pick_thumb(e, &id)),
        view_count: e.view_count.map(|v| v.round() as i64),
        genre,
        release_date,
        url: Some(e.webpage_url.clone().unwrap_or_else(|| watch_url(&id))),
        resolve_query: None,
    })
}

fn watch_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

/* ---------------------------------- search -------------------------------- */

pub async fn search(query: &str, limit: usize) -> ApiResult<SearchResults> {
    let limit = limit.clamp(1, 50);
    let key = format!("search::{limit}::{}", query.to_lowercase());
    if let Some(hit) = search_cache().get(&key) {
        return Ok(hit);
    }

    let search_term = format!("ytsearch{limit}:{query}");
    let stdout = run_ytdlp(
        &[
            "--flat-playlist",
            "--dump-json",
            "--no-warnings",
            "--skip-download",
            &search_term,
        ],
        config::ytdlp_timeout(),
    )
    .await?;

    let entries: Vec<Entry> = parse_json_lines(&stdout);
    let tracks: Vec<Track> = entries.iter().filter_map(from_flat).take(limit).collect();
    let results = build_search_results(tracks);
    search_cache().set(&key, results.clone(), None);
    Ok(results)
}

/* ------------------------------- playlists -------------------------------- */

/// `sp=EgIQAw%3D%3D` is YouTube's "filter: playlist" search parameter, so every
/// entry the results page returns is a playlist rather than a video.
const PLAYLIST_FILTER: &str = "EgIQAw%3D%3D";

/// How many playlist results to attach to each search response.
pub const PLAYLIST_SEARCH_LIMIT: usize = 12;

/// Playlist ids are `PL`, `UU`, `OLAK5uy_` ... followed by 10+ url-safe chars.
static PLAYLIST_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{12,64}$").expect("valid playlist id regex"));

pub fn is_valid_playlist_id(id: &str) -> bool {
    PLAYLIST_ID_RE.is_match(id)
}

/// Channels that are almost certainly the artist/label rather than a fan.
static OFFICIAL_CHANNEL_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)\s*-\s*topic$", // YouTube's auto-generated artist channels
        r"(?i)vevo$",
        r"(?i)\bofficial\b",
        r"(?i)\brecords?\b",
        r"(?i)\bmusic\b",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("official channel regex"))
    .collect()
});

/// Best-effort "is this an official playlist" test.
///
/// YouTube exposes no official flag, so we infer it: the channel must look like
/// an artist/label channel (Topic, VEVO, "Official", a label) or match the
/// artist we searched for. This is deliberately conservative - a false negative
/// only means the playlist is hidden when "official only" is on.
fn looks_official(owner: &str, query: &str) -> bool {
    let channel = owner.trim();
    if channel.is_empty() {
        return false;
    }
    if OFFICIAL_CHANNEL_PATTERNS
        .iter()
        .any(|re| re.is_match(channel))
    {
        return true;
    }
    // A channel that starts with the artist we searched for ("Daft Punk").
    let q = query.trim().to_lowercase();
    if q.chars().count() >= 3 {
        let c = channel.to_lowercase();
        if c.starts_with(&q) || q.starts_with(&c) {
            return true;
        }
    }
    false
}

fn pick_playlist_thumb(e: &Entry) -> Option<String> {
    if let Some(thumbs) = e.thumbnails.as_ref() {
        if !thumbs.is_empty() {
            let mut sorted: Vec<&ThumbEntry> = thumbs.iter().collect();
            sorted.sort_by_key(|t| std::cmp::Reverse(t.preference.unwrap_or(0)));
            if let Some(url) = sorted.first().and_then(|t| t.url.as_ref()) {
                if !url.is_empty() {
                    return Some(url.clone());
                }
            }
        }
    }
    match e.thumbnail.as_deref() {
        Some(thumb) if !thumb.is_empty() => Some(thumb.to_string()),
        _ => None,
    }
}

fn from_playlist_entry(e: &Entry, query: &str) -> Option<RemotePlaylist> {
    let id = e.id.as_deref()?;
    if !is_valid_playlist_id(id) {
        return None;
    }
    let title = e.title.as_deref()?.trim();
    if title.is_empty() {
        return None;
    }
    let owner = e
        .channel
        .as_deref()
        .or(e.uploader.as_deref())
        .unwrap_or("")
        .trim();
    Some(RemotePlaylist {
        id: id.to_string(),
        title: title.to_string(),
        owner: if owner.is_empty() {
            "YouTube".to_string()
        } else {
            owner.to_string()
        },
        image: pick_playlist_thumb(e),
        track_count: e.playlist_count.map(|c| c as usize),
        official: looks_official(owner, query),
        source: "youtube".to_string(),
    })
}

/// The single-document shape `--dump-single-json` prints for a search page.
#[derive(Debug, Clone, Default, Deserialize)]
struct PlaylistDoc {
    entries: Option<Vec<Entry>>,
}

/// Percent-encode like `encodeURIComponent` (unreserved bytes pass through).
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Search YouTube for playlists matching `query`.
pub async fn search_playlists(query: &str, limit: usize) -> ApiResult<Vec<RemotePlaylist>> {
    let limit = limit.clamp(1, 50);
    let key = format!("plsearch::{limit}::{}", query.to_lowercase());
    if let Some(hit) = playlist_cache().get(&key) {
        return Ok(hit);
    }

    let url = format!(
        "https://www.youtube.com/results?search_query={}&sp={PLAYLIST_FILTER}",
        urlencode(query),
    );
    let end = (limit * 2).max(20).to_string();
    let stdout = run_ytdlp(
        &[
            "--flat-playlist",
            "--dump-single-json",
            "--no-warnings",
            "--skip-download",
            "--playlist-end",
            &end,
            &url,
        ],
        config::ytdlp_timeout(),
    )
    .await?;

    let doc = parse_json_doc::<PlaylistDoc>(&stdout);
    let entries = doc.and_then(|d| d.entries).unwrap_or_default();
    let playlists: Vec<RemotePlaylist> = entries
        .iter()
        .filter_map(|e| from_playlist_entry(e, query))
        .take(limit)
        .collect();
    playlist_cache().set(&key, playlists.clone(), None);
    Ok(playlists)
}

/// Fetch the videos inside a public playlist.
///
/// Flat extraction gives us video ids, titles, durations and thumbnails without
/// downloading anything; the entries are normalized exactly like search results
/// so they play through the normal stream resolver.
pub async fn get_playlist_tracks(playlist_id: &str, limit: usize) -> ApiResult<Vec<Track>> {
    if !is_valid_playlist_id(playlist_id) {
        return Err(ApiError::invalid_input(Some(
            "invalid playlist id".to_string(),
        )));
    }
    let key = format!("{playlist_id}::{limit}");
    if let Some(hit) = playlist_tracks_cache().get(&key) {
        return Ok(hit);
    }

    let end = limit.to_string();
    let url = format!("https://www.youtube.com/playlist?list={playlist_id}");
    let stdout = run_ytdlp(
        &[
            "--flat-playlist",
            "--dump-json",
            "--no-warnings",
            "--skip-download",
            "--playlist-end",
            &end,
            &url,
        ],
        config::ytdlp_timeout(),
    )
    .await?;

    let entries: Vec<Entry> = parse_json_lines(&stdout);
    let tracks: Vec<Track> = entries.iter().filter_map(from_flat).collect();
    playlist_tracks_cache().set(&key, tracks.clone(), None);
    Ok(tracks)
}

/* --------------------------------- metadata ------------------------------- */

pub async fn get_track(id: &str) -> ApiResult<Option<Track>> {
    let video_id = id.strip_prefix("yt:").unwrap_or(id).to_string();
    if !is_valid_video_id(&video_id) {
        return Ok(None);
    }
    if let Some(hit) = metadata_cache().get(&video_id) {
        return Ok(hit);
    }

    let stdout = run_ytdlp(
        &[
            "--dump-single-json",
            "--no-playlist",
            "--no-warnings",
            "--skip-download",
            &watch_url(&video_id),
        ],
        config::ytdlp_timeout(),
    )
    .await?;

    let track = parse_json_doc::<Entry>(&stdout).and_then(|doc| from_full(&doc, &video_id));
    metadata_cache().set(&video_id, track.clone(), None);
    Ok(track)
}

/// Resolve a query to the first genuinely playable track.
pub async fn first_playable(query: &str) -> ApiResult<Option<Track>> {
    let results = search(query, 8).await?;
    if let Some(playable) = results.tracks.iter().find(|t| t.source_id.is_some()) {
        return Ok(Some(playable.clone()));
    }
    let candidate = results.tracks.first().cloned();
    if let Some(resolve_query) = candidate.as_ref().and_then(|c| c.resolve_query.clone()) {
        let retry = search(&resolve_query, 8).await?;
        if let Some(resolved) = retry.tracks.iter().find(|t| t.source_id.is_some()) {
            return Ok(Some(resolved.clone()));
        }
    }
    Ok(candidate)
}

/* ------------------------------ stream quality ---------------------------- */

/// Client-facing quality levels (`GET /api/stream/:id?q=...`).
pub const QUALITIES: [&str; 3] = ["auto", "low", "high"];

/// yt-dlp selectors: `low` caps bitrate so slow links start faster.
fn quality_format(quality: &str) -> &'static str {
    match quality {
        "low" => {
            "bestaudio[abr<=96][ext=m4a]/bestaudio[abr<=96]/bestaudio[ext=m4a]/bestaudio/best/best"
        }
        _ => "bestaudio[ext=m4a]/bestaudio/best/best",
    }
}

/// Untrusted query value -> a known quality (anything else falls back to auto).
pub fn normalize_quality(raw: Option<&str>) -> &'static str {
    match raw.map(|v| v.trim().to_lowercase()) {
        Some(v) if v == "low" => "low",
        Some(v) if v == "high" => "high",
        _ => "auto",
    }
}

fn stream_key(video_id: &str, quality: &str) -> String {
    if quality == "auto" {
        video_id.to_string()
    } else {
        format!("{video_id}::{quality}")
    }
}

/// Resolve a streamable media URL for a video id.
pub async fn resolve_media_url(video_id: &str, quality: &str) -> ApiResult<StreamSource> {
    if !is_valid_video_id(video_id) {
        return Err(ApiError::invalid_input(Some("invalid video id".into())));
    }
    let q = normalize_quality(Some(quality));
    let cache_key = stream_key(video_id, q);
    if let Some(hit) = stream_cache().get_min_ttl(&cache_key, Duration::from_secs(60)) {
        return Ok(hit);
    }

    let stdout = run_ytdlp(
        &[
            "-f",
            quality_format(q),
            "-g",
            "--no-playlist",
            "--no-warnings",
            &watch_url(video_id),
        ],
        config::ytdlp_timeout(),
    )
    .await?;

    let url = stdout
        .split('\n')
        .map(str::trim)
        .find(|line| line.starts_with("http"))
        .map(str::to_string);
    let Some(url) = url else {
        return Err(ApiError::extraction_failed(Some(
            "yt-dlp returned no media url".into(),
        )));
    };

    let expires_at = compute_expiry(&url);
    let now_ms = now_ms();
    let remaining = Duration::from_millis((expires_at - now_ms).max(0) as u64);
    let ttl = remaining
        .max(Duration::from_secs(30))
        .min(config::STREAM_TTL);
    let source = StreamSource {
        url,
        expires_at,
        content_type: None,
    };
    stream_cache().set(&cache_key, source.clone(), Some(ttl));
    Ok(source)
}

/// YouTube appends `expire=<unix seconds>` to media URLs; fall back to the TTL.
fn compute_expiry(url: &str) -> i64 {
    if let Ok(parsed) = url::Url::parse(url) {
        for (key, value) in parsed.query_pairs() {
            if key == "expire" && !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(seconds) = value.parse::<i64>() {
                    return seconds * 1000;
                }
            }
        }
    }
    now_ms() + config::STREAM_TTL.as_millis() as i64
}

/// Drop every quality variant of this id (used when the CDN answers 403/410).
pub fn invalidate_stream(video_id: &str) {
    for q in QUALITIES {
        stream_cache().delete(&stream_key(video_id, q));
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
