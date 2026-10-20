//! The whole `/api/*` surface, in process.
//!
//! The desktop build has no HTTP server, so this module keeps the exact routes,
//! query handling, error codes and response bodies the web backend exposed and
//! exposes them through one Tauri command (`api_request`). The frontend's
//! provider layer is therefore unchanged: it still asks for `/api/search`
//! etc., and this dispatcher answers.

use std::collections::HashMap;

use serde_json::{json, Value};
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

use crate::audio::{AudioEngine, PlayerStatus};
use crate::error::{ApiError, ApiResult};
use crate::providers::lyrics::LyricsLookup;
use crate::providers::{charts, lyrics, recommend, youtube};

/* --------------------------------- helpers -------------------------------- */

/// Percent-decode a single path segment or query value.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn split_path(raw: &str) -> (Vec<String>, HashMap<String, String>) {
    let (path, query) = match raw.split_once('?') {
        Some((path, query)) => (path, query),
        None => (raw, ""),
    };
    let segments = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(percent_decode)
        .collect();
    let mut params = HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        params.insert(percent_decode(key), percent_decode(value));
    }
    (segments, params)
}

/// Remove control characters from untrusted input (server parity).
fn strip_control(input: &str) -> String {
    input
        .chars()
        .filter(|c| {
            let code = *c as u32;
            code >= 32 && code != 127
        })
        .collect()
}

fn sanitize_query(value: Option<&str>, max_length: usize) -> ApiResult<String> {
    let value =
        value.ok_or_else(|| ApiError::invalid_input(Some("query must be a string".into())))?;
    let cleaned = strip_control(value).trim().to_string();
    if cleaned.is_empty() {
        return Err(ApiError::invalid_input(Some("query is required".into())));
    }
    if cleaned.chars().count() > max_length {
        return Err(ApiError::invalid_input(Some("query is too long".into())));
    }
    Ok(cleaned)
}

/// Optional variant: absent/blank becomes `None` instead of an error.
fn sanitize_optional(value: Option<&str>, max_length: usize) -> ApiResult<Option<String>> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => sanitize_query(Some(v), max_length).map(Some),
        None => Ok(None),
    }
}

fn parse_limit(value: Option<&str>, fallback: usize, max: usize) -> ApiResult<usize> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(fallback),
        Some(raw) => {
            let parsed: f64 = raw.parse().map_err(|_| {
                ApiError::invalid_input(Some("limit must be a positive number".into()))
            })?;
            if !parsed.is_finite() || parsed < 1.0 {
                return Err(ApiError::invalid_input(Some(
                    "limit must be a positive number".into(),
                )));
            }
            Ok((parsed.floor() as usize).min(max))
        }
    }
}

fn to_value<T: serde::Serialize>(value: T) -> ApiResult<Value> {
    serde_json::to_value(value).map_err(|err| ApiError::internal(Some(err.to_string())))
}

/* ------------------------------- dispatching ------------------------------ */

/// Handle one `/api/*` request.
pub async fn dispatch(method: &str, raw_path: &str, body: Option<Value>) -> ApiResult<Value> {
    let (segments, params) = split_path(raw_path);
    let mut parts: Vec<&str> = segments.iter().map(|s| s.as_str()).collect();

    if parts.first() == Some(&"api") {
        parts.remove(0);
    }
    let method = method.to_uppercase();
    let method = method.as_str();
    let body = body.unwrap_or(Value::Null);

    match (method, parts.as_slice()) {
        ("GET", ["health"]) => Ok(json!({
            "ok": true,
            "name": "ytrium-desktop",
            "version": env!("CARGO_PKG_VERSION"),
            "providers": ["youtube", "charts"],
            "time": iso_now(),
        })),
        ("GET", ["search"]) => search(&params).await,
        ("GET", ["resolve"]) | ("POST", ["resolve"]) => resolve(&params, &body).await,
        ("GET", ["tracks", id]) => track(id).await,
        ("GET", ["playlists", id, "tracks"]) => playlist_tracks(id, &params).await,
        ("GET", ["artist"]) => artist(&params).await,
        ("GET", ["artist", "playlists"]) => artist_playlists(&params).await,
        ("GET", ["album"]) => album(&params).await,
        ("GET", ["charts"]) => to_value(charts::get_charts().await?),
        ("GET", ["trending"]) => trending(&params).await,
        ("GET", ["recommendations"]) => recommendations(&params).await,
        ("GET", ["lyrics"]) => lyrics_route(&params).await,
        ("GET", ["stream", id, "meta"]) => stream_meta(id, &params).await,
        _ => Err(ApiError::not_found("Endpoint not found.")),
    }
}

fn iso_now() -> String {
    // Seconds since the epoch formatted as an ISO-8601 UTC timestamp; keeps the
    // health payload shaped like the web build without pulling in a date crate.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

async fn search(params: &HashMap<String, String>) -> ApiResult<Value> {
    let query = sanitize_query(params.get("q").map(String::as_str), 200)?;
    let limit = parse_limit(
        params.get("limit").map(String::as_str),
        crate::config::search_limit(),
        50,
    )?;
    // Playlist search runs alongside the video search. A failure here must
    // never break song results, so it degrades to an empty list.
    let (results, playlists) = tokio::join!(
        youtube::search(&query, limit),
        youtube::search_playlists(&query, youtube::PLAYLIST_SEARCH_LIMIT)
    );
    let results = results?;
    let playlists = playlists.unwrap_or_default();
    let mut value = to_value(results)?;
    if let Some(map) = value.as_object_mut() {
        map.insert("playlists".to_string(), to_value(playlists)?);
        map.insert("query".to_string(), Value::String(query));
        map.insert("source".to_string(), Value::String("youtube".to_string()));
    }
    Ok(value)
}

/// `GET /api/playlists/:id/tracks`
///
/// Expands a public YouTube playlist into normalized tracks so a saved playlist
/// plays through the normal stream resolver.
async fn playlist_tracks(raw_id: &str, params: &HashMap<String, String>) -> ApiResult<Value> {
    let id = raw_id.trim();
    let limit = parse_limit(params.get("limit").map(String::as_str), 200, 500)?;
    match youtube::get_playlist_tracks(id, limit).await {
        Err(_) => Err(ApiError::not_found("That playlist could not be loaded.")),
        Ok(tracks) if tracks.is_empty() => Err(ApiError::not_found(
            "That playlist is empty or unavailable.",
        )),
        Ok(tracks) => Ok(json!({ "tracks": crate::providers::dedupe_tracks(tracks) })),
    }
}

/// `GET /api/artist/playlists?name=<artist>&limit=`
///
/// Playlists published by (or matching) an artist, used by the artist page's
/// "Playlists" rail. Degrades to an empty list, as on the web build.
async fn artist_playlists(params: &HashMap<String, String>) -> ApiResult<Value> {
    let name = sanitize_query(params.get("name").map(String::as_str), 120)?;
    let limit = parse_limit(params.get("limit").map(String::as_str), 12, 24)?;
    let playlists = youtube::search_playlists(&name, limit)
        .await
        .unwrap_or_default();
    Ok(json!({ "playlists": playlists }))
}

async fn resolve(params: &HashMap<String, String>, body: &Value) -> ApiResult<Value> {
    let raw_id = body.get("id").and_then(Value::as_str).map(str::to_string);
    let body_q = body.get("q").and_then(Value::as_str).map(str::to_string);
    let title = body
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string);
    let artist = body
        .get("artist")
        .and_then(Value::as_str)
        .map(str::to_string);

    if let Some(id) = raw_id.as_deref().map(str::trim).filter(|i| !i.is_empty()) {
        if let Some(track) = youtube::get_track(id).await? {
            return Ok(json!({ "track": track }));
        }
    }

    let query = body_q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_string)
        .or_else(|| {
            params
                .get("q")
                .map(|q| q.trim().to_string())
                .filter(|q| !q.is_empty())
        })
        .or_else(|| {
            title.as_ref().map(|title| {
                let mut parts = vec![title.clone()];
                if let Some(artist) = artist.as_ref().filter(|a| !a.is_empty()) {
                    parts.push(artist.clone());
                }
                parts.join(" ")
            })
        });

    let Some(query) = query else {
        return Err(ApiError::invalid_input(Some(
            "provide id, q, or title".to_string(),
        )));
    };

    let query = sanitize_query(Some(&query), 240)?;
    match youtube::first_playable(&query).await? {
        Some(track) => Ok(json!({ "track": track })),
        None => Err(ApiError::not_found(
            "No playable match was found for that query.",
        )),
    }
}

async fn track(raw_id: &str) -> ApiResult<Value> {
    let id = raw_id.trim();
    let valid = (4..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | ',' | '-'));
    if !valid {
        return Err(ApiError::invalid_input(Some(
            "invalid track id".to_string(),
        )));
    }
    match youtube::get_track(id).await? {
        Some(track) => Ok(json!({ "track": track })),
        None => Err(ApiError::not_found("Track metadata could not be found.")),
    }
}

async fn artist(params: &HashMap<String, String>) -> ApiResult<Value> {
    let name = sanitize_query(params.get("name").map(String::as_str), 120)?;
    let limit = parse_limit(params.get("limit").map(String::as_str), 20, 40)?;

    let related_query = format!("{name} similar artists");
    let (main, related) = tokio::join!(
        youtube::search(&name, limit),
        youtube::search(&related_query, 15)
    );
    let main = main?;
    let related = related.ok();

    let tracks = crate::providers::dedupe_tracks(main.tracks.clone());
    let mut popular = tracks.clone();
    popular.sort_by_key(|a| std::cmp::Reverse(a.view_count.unwrap_or(0)));

    let mut albums: Vec<crate::providers::Album> = Vec::new();
    for t in &tracks {
        if let (Some(album), Some(album_id)) = (t.album.as_ref(), t.album_id.as_ref()) {
            if albums.iter().any(|a| &a.id == album_id) {
                continue;
            }
            albums.push(crate::providers::Album {
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

    let self_id = crate::providers::artist_id_for(&name);
    let mut related_artists: Vec<crate::providers::Artist> = Vec::new();
    for source in [Some(&main), related.as_ref()] {
        let Some(source) = source else { continue };
        for a in &source.artists {
            if a.id == self_id || related_artists.iter().any(|r| r.id == a.id) {
                continue;
            }
            related_artists.push(a.clone());
        }
    }

    let artist = crate::providers::Artist {
        id: self_id,
        name,
        image: popular
            .first()
            .and_then(|t| t.thumbnail.clone())
            .or_else(|| main.artists.first().and_then(|a| a.image.clone())),
        description: None,
        source: "youtube".to_string(),
    };

    albums.truncate(12);
    related_artists.truncate(8);

    Ok(json!({
        "artist": artist,
        "tracks": popular,
        "albums": albums,
        "related": related_artists,
    }))
}

async fn album(params: &HashMap<String, String>) -> ApiResult<Value> {
    let artist = sanitize_query(params.get("artist").map(String::as_str), 120)?;
    let title = sanitize_query(params.get("title").map(String::as_str), 160)?;

    let results = youtube::search(&format!("{artist} {title}"), 30).await?;
    let wanted = crate::providers::album_id_for(&artist, &title);

    let mut exact: Vec<crate::providers::Track> = Vec::new();
    let mut loose: Vec<crate::providers::Track> = Vec::new();
    let lowered = title.to_lowercase();
    for t in results.tracks {
        if t.album_id.as_deref() == Some(wanted.as_str()) {
            exact.push(t);
        } else if t
            .album
            .as_deref()
            .map(|a| a.to_lowercase().contains(&lowered))
            .unwrap_or(false)
            || t.title.to_lowercase().contains(&lowered)
        {
            loose.push(t);
        }
    }

    let mut tracks = exact.clone();
    tracks.extend(loose);

    let album = crate::providers::Album {
        id: wanted,
        title,
        artist: artist.clone(),
        artist_id: Some(crate::providers::artist_id_for(&artist)),
        image: tracks.first().and_then(|t| t.thumbnail.clone()),
        release_date: tracks
            .iter()
            .find(|t| t.release_date.is_some())
            .and_then(|t| t.release_date.clone()),
        track_count: Some(tracks.len()),
        source: "youtube".to_string(),
    };

    Ok(json!({
        "album": album,
        "tracks": tracks,
        "exactMatch": !exact.is_empty(),
    }))
}

async fn trending(params: &HashMap<String, String>) -> ApiResult<Value> {
    let limit = parse_limit(params.get("limit").map(String::as_str), 20, 50)?;
    let data = charts::get_charts().await?;
    let tracks: Vec<_> = data.trending.iter().take(limit).cloned().collect();
    Ok(json!({ "tracks": tracks, "source": data.charts_source }))
}

async fn recommendations(params: &HashMap<String, String>) -> ApiResult<Value> {
    let raw = params.get("seeds").map(String::as_str).unwrap_or("");
    let mut seeds: Vec<String> = Vec::new();
    for seed in raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(6)
    {
        seeds.push(sanitize_query(Some(seed), 80)?);
    }
    let limit = parse_limit(params.get("limit").map(String::as_str), 20, 40)?;
    let tracks = recommend::get_recommendations(&seeds, limit).await?;
    Ok(json!({ "tracks": tracks, "seeds": seeds }))
}

async fn lyrics_route(params: &HashMap<String, String>) -> ApiResult<Value> {
    let title = sanitize_query(params.get("title").map(String::as_str), 300)?;
    let artist = sanitize_query(params.get("artist").map(String::as_str), 200)?;
    let album = sanitize_optional(params.get("album").map(String::as_str), 200)?;

    let duration = params
        .get("duration")
        .filter(|v| !v.is_empty() && v.len() <= 5 && v.chars().all(|c| c.is_ascii_digit()))
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0 && *v <= 86_400);

    let video_id = params
        .get("id")
        .map(String::as_str)
        .filter(|id| crate::ytdlp::is_valid_video_id(id))
        .map(str::to_string);

    let lyrics = lyrics::get_lyrics_for(LyricsLookup {
        title,
        artist,
        album,
        duration,
        video_id,
    })
    .await;

    Ok(json!({ "lyrics": lyrics }))
}

async fn stream_meta(id: &str, params: &HashMap<String, String>) -> ApiResult<Value> {
    let video_id = id.strip_prefix("yt:").unwrap_or(id);
    if !crate::ytdlp::is_valid_video_id(video_id) {
        return Err(ApiError::invalid_input(Some(
            "invalid stream id".to_string(),
        )));
    }
    let quality = youtube::normalize_quality(params.get("q").map(String::as_str));
    // Warm the provider cache exactly like the web build's prefetch did.
    let source = youtube::resolve_media_url(video_id, quality).await?;
    Ok(json!({
        "id": video_id,
        "expiresAt": source.expires_at,
        "mode": "native",
        "quality": quality,
        "path": format!("/api/stream/{video_id}"),
    }))
}

/* ---------------------------- tauri commands ------------------------------ */

/// Single entry point for the frontend's `/api/*` calls.
#[tauri::command]
pub async fn api_request(
    method: String,
    path: String,
    body: Option<Value>,
) -> Result<Value, ApiError> {
    dispatch(&method, &path, body).await
}

/// Native save dialog + write, used by the settings export.
#[tauri::command]
pub async fn export_library(
    app: AppHandle,
    contents: String,
    suggested_name: String,
) -> Result<bool, ApiError> {
    let chosen = app
        .dialog()
        .file()
        .set_title("Export Ytrium data")
        .set_file_name(&suggested_name)
        .add_filter("JSON", &["json"])
        .blocking_save_file();

    let Some(path) = chosen else {
        return Ok(false);
    };
    let path = path
        .into_path()
        .map_err(|err| ApiError::internal(Some(err.to_string())))?;
    std::fs::write(&path, contents)
        .map_err(|err| ApiError::internal(Some(format!("could not write the export: {err}"))))?;
    Ok(true)
}

/* --------------------------------- audio ---------------------------------- */

/// Start playing a video id (the media itself is fetched by the Rust engine).
#[tauri::command]
pub async fn audio_load(
    video_id: String,
    quality: Option<String>,
    start_at: Option<f64>,
    autoplay: Option<bool>,
) -> Result<(), ApiError> {
    let quality = quality.unwrap_or_else(|| "auto".to_string());
    AudioEngine::global()
        .load(
            video_id,
            quality,
            start_at.unwrap_or(0.0).max(0.0),
            autoplay.unwrap_or(true),
        )
        .await;
    Ok(())
}

#[tauri::command]
pub fn audio_play() {
    AudioEngine::global().play();
}

#[tauri::command]
pub fn audio_pause() {
    AudioEngine::global().pause();
}

#[tauri::command]
pub fn audio_seek(seconds: f64) {
    AudioEngine::global().seek(seconds.max(0.0));
}

#[tauri::command]
pub fn audio_set_volume(value: f32) {
    AudioEngine::global().set_volume(value);
}

#[tauri::command]
pub fn audio_set_muted(muted: bool) {
    AudioEngine::global().set_muted(muted);
}

#[tauri::command]
pub fn audio_fade(value: f32, ms: u64) {
    AudioEngine::global().fade(value, ms);
}

#[tauri::command]
pub fn audio_set_normalize(enabled: bool) {
    AudioEngine::global().set_normalize(enabled);
}

#[tauri::command]
pub fn audio_status() -> PlayerStatus {
    AudioEngine::global().status()
}
