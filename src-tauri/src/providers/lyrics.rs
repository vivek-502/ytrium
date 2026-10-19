//! Lyrics provider: LRCLIB exact -> scored LRCLIB search -> YouTube captions.
//!
//! Ported from the web build's `providers/lyrics.ts`: same endpoints, same
//! scoring/threshold, same LRC + WebVTT parsing, same caption fallback (which
//! keeps only the caption *text* in a temporary directory that is removed
//! again before returning).

use std::path::PathBuf;
use std::sync::{LazyLock, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::normalize_key;
use crate::cache::TtlCache;
use crate::config;
use crate::http;
use crate::ytdlp::{is_valid_video_id, run_ytdlp};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncedLine {
    /// Seconds from the start of the track.
    pub time: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricsResult {
    /// Provider that supplied the text: lrclib | youtube-captions.
    pub source: String,
    pub instrumental: bool,
    pub plain_text: Option<String>,
    pub synced: Option<Vec<SyncedLine>>,
    pub track_name: Option<String>,
    pub artist_name: Option<String>,
    pub album_name: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LyricsLookup {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    /// Seconds; used to pick the right match when a provider returns several.
    pub duration: Option<u32>,
    /// YouTube video id (used for the caption fallback).
    pub video_id: Option<String>,
}

/* --------------------------------- caching -------------------------------- */

static LYRICS_CACHE: OnceLock<TtlCache<Option<LyricsResult>>> = OnceLock::new();

fn lyrics_cache() -> &'static TtlCache<Option<LyricsResult>> {
    LYRICS_CACHE.get_or_init(|| TtlCache::new(config::LYRICS_TTL, config::LYRICS_MAX_ENTRIES))
}

/* ---------------------------------- regex --------------------------------- */

/// Caption lines that carry no lyric text (YouTube auto cues).
static NOISE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\(?\[?[♪♫]*\s*(music|applause|laughter|instrumental|chorus|verse|intro|outro|silence|background|inaudible|speaking)\s*[)\]♪♫!~-]*$",
    )
    .expect("noise line regex")
});
/// Lines consisting only of music-note glyphs (instrumental cues).
static MUSIC_ONLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[(\[♪♫\]]+[\s!~-]*$").expect("music only regex"));
static LRC_STAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[(\d{1,2}):(\d{2})(?:[.:](\d{1,3}))?\]").expect("lrc stamp regex")
});
static VTT_CUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d{1,2}):(\d{2}):(\d{2})\.(\d{1,3})\s*-->").expect("vtt cue regex")
});
static VTT_TAG_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^<[^>]+>").expect("vtt tag regex"));
static VTT_TAG_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<[^>]+>$").expect("vtt tag regex"));
static NOTE_LEAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[♪♫\s]+").expect("note regex"));
static NOTE_TRAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[♪♫\s]+$").expect("note regex"));

/* --------------------------------- LRCLIB --------------------------------- */

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrclibEntry {
    track_name: Option<String>,
    artist_name: Option<String>,
    album_name: Option<String>,
    duration: Option<f64>,
    instrumental: Option<bool>,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

fn lrclib_base() -> String {
    config::lyrics_base().trim_end_matches('/').to_string()
}

fn lrclib_get(path: &str, params: &[(&str, &str)]) -> Result<String, http::FetchError> {
    let mut url = format!("{}{path}", lrclib_base());
    let mut sep = '?';
    for (k, v) in params {
        let encoded = urlencoding(v);
        url.push(sep);
        url.push_str(k);
        url.push('=');
        url.push_str(&encoded);
        sep = '&';
    }
    http::get_text(http::lyrics_agent(), &url, 4 * 1024 * 1024)
}

/// Percent-encode a query value (RFC 3986 unreserved characters are kept).
fn urlencoding(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Exact-title lookup. 404 means "no exact match", not an outage.
fn lrclib_exact(artist: &str, title: &str) -> Result<Option<LrclibEntry>, http::FetchError> {
    match lrclib_get(
        "/api/get",
        &[("artist_name", artist), ("track_name", title)],
    ) {
        Ok(text) => Ok(serde_json::from_str::<LrclibEntry>(&text).ok()),
        Err(http::FetchError::Status(404)) => Ok(None),
        Err(other) => Err(other),
    }
}

/// Fuzzy search; results are ranked locally against the known track.
fn lrclib_search(artist: &str, title: &str) -> Result<Vec<LrclibEntry>, http::FetchError> {
    let text = lrclib_get(
        "/api/search",
        &[("artist_name", artist), ("track_name", title)],
    )?;
    Ok(serde_json::from_str::<Vec<LrclibEntry>>(&text).unwrap_or_default())
}

fn pick_best<'a>(entries: &'a [LrclibEntry], lookup: &LyricsLookup) -> Option<&'a LrclibEntry> {
    let want_artist = normalize_key(&lookup.artist);
    let want_title = normalize_key(&lookup.title);
    let want_album = lookup
        .album
        .as_deref()
        .map(normalize_key)
        .unwrap_or_default();
    let want_duration = lookup.duration.unwrap_or(0) as f64;

    let mut best: Option<&LrclibEntry> = None;
    let mut best_score = 0i32;

    for entry in entries {
        if entry.plain_lyrics.is_none() && entry.synced_lyrics.is_none() {
            continue;
        }

        let mut score = 0i32;
        let a = normalize_key(entry.artist_name.as_deref().unwrap_or(""));
        let t = normalize_key(entry.track_name.as_deref().unwrap_or(""));
        if !want_artist.is_empty() && !a.is_empty() {
            if a == want_artist {
                score += 100;
            } else if a.contains(&want_artist) || want_artist.contains(&a) {
                score += 50;
            }
        }
        if !want_title.is_empty() && !t.is_empty() {
            if t == want_title {
                score += 60;
            } else if t.contains(&want_title) || want_title.contains(&t) {
                score += 30;
            }
        }
        if !want_album.is_empty()
            && normalize_key(entry.album_name.as_deref().unwrap_or("")) == want_album
        {
            score += 25;
        }
        if entry.synced_lyrics.is_some() {
            score += 8;
        }
        if want_duration > 0.0 {
            if let Some(d) = entry.duration {
                if d > 0.0 {
                    let diff = (d - want_duration).abs();
                    if diff <= 3.0 {
                        score += 20;
                    } else if diff <= 12.0 {
                        score += 10;
                    }
                }
            }
        }

        if score > best_score {
            best_score = score;
            best = Some(entry);
        }
    }

    // A plausible match needs at least a strong artist or title signal.
    if best_score >= 60 {
        best
    } else {
        None
    }
}

/* --------------------------------- parsing -------------------------------- */

/// Parse LRC-style synced lyrics: `[mm:ss.xx]text` (one or many stamps/line).
pub fn parse_lrc(lrc: &str) -> Vec<SyncedLine> {
    let mut out: Vec<SyncedLine> = Vec::new();
    for raw in lrc.split('\n') {
        let line = raw.trim();
        if !line.starts_with('[') {
            continue;
        }
        let mut times: Vec<f64> = Vec::new();
        for cap in LRC_STAMP.captures_iter(line) {
            let min: f64 = cap[1].parse().unwrap_or(0.0);
            let sec: f64 = cap[2].parse().unwrap_or(0.0);
            let frac = match cap.get(3) {
                Some(m) => {
                    let digits = format!("{:0<3}", m.as_str());
                    digits.parse::<f64>().unwrap_or(0.0) / 1000.0
                }
                None => 0.0,
            };
            times.push(min * 60.0 + sec + frac);
        }
        let text = LRC_STAMP.replace_all(line, "").trim().to_string();
        if times.is_empty() || text.is_empty() {
            continue;
        }
        for t in times {
            out.push(SyncedLine {
                time: (t * 1000.0).round() / 1000.0,
                text: text.clone(),
            });
        }
    }
    out.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Parse a WebVTT subtitle document into timed lyric lines.
pub fn parse_vtt(vtt: &str) -> Vec<SyncedLine> {
    let normalized = vtt.replace("\r\n", "\n").replace('\r', "\n");
    let mut out: Vec<SyncedLine> = Vec::new();
    for block in normalized.split("\n\n") {
        let lines: Vec<&str> = block
            .split('\n')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let Some(first) = lines.first() else { continue };
        let Some(cap) = VTT_CUE.captures(first) else {
            continue;
        };
        let h: f64 = cap[1].parse().unwrap_or(0.0);
        let m: f64 = cap[2].parse().unwrap_or(0.0);
        let s: f64 = cap[3].parse().unwrap_or(0.0);
        let frac = format!("{:0<3}", &cap[4]);
        let frac: f64 = frac.parse::<f64>().unwrap_or(0.0) / 1000.0;
        let time = h * 3600.0 + m * 60.0 + s + frac;

        let text = lines[1..]
            .iter()
            .map(|l| {
                let l = VTT_TAG_START.replace(l, "");
                VTT_TAG_END.replace(&l, "").trim().to_string()
            })
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let text = NOTE_LEAD.replace(&text, "");
        let text = NOTE_TRAIL.replace(&text, "").trim().to_string();
        if text.is_empty() || NOISE_LINE.is_match(&text) || MUSIC_ONLY.is_match(&text) {
            continue;
        }
        out.push(SyncedLine {
            time: (time * 1000.0).round() / 1000.0,
            text,
        });
    }
    out
}

/// Trim/space blanks; `None` when nothing usable remains.
fn clean_plain(text: Option<&str>) -> Option<String> {
    static BLANK_LINES: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\n{3,}").expect("blank lines regex"));
    let text = text?;
    if text.is_empty() {
        return None;
    }
    let cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
    let cleaned = BLANK_LINES.replace_all(&cleaned, "\n\n").trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn lrclib_to_result(entry: &LrclibEntry) -> Option<LyricsResult> {
    let instrumental = entry.instrumental == Some(true);
    let synced = entry
        .synced_lyrics
        .as_deref()
        .map(parse_lrc)
        .filter(|lines| !lines.is_empty());
    let plain_text = clean_plain(entry.plain_lyrics.as_deref()).or_else(|| {
        synced.as_ref().map(|lines| {
            lines
                .iter()
                .map(|l| l.text.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
    });
    if !instrumental && plain_text.is_none() {
        return None;
    }
    Some(LyricsResult {
        source: "lrclib".to_string(),
        instrumental,
        // Instrumentals have no text; keep the flag and drop empty bodies.
        plain_text: if instrumental { None } else { plain_text },
        synced,
        track_name: entry.track_name.clone(),
        artist_name: entry.artist_name.clone(),
        album_name: entry.album_name.clone(),
    })
}

/* ------------------------------ YouTube subs ------------------------------ */

/// Prefer the official lyrics caption track over regional/auto variants.
fn caption_rank(code: &str) -> i32 {
    let c = code.to_lowercase();
    if c.contains("lyric") {
        return 0;
    }
    if c == "en" {
        return 1;
    }
    if c.len() == 5 && c.starts_with("en-") && c[3..].chars().all(|ch| ch.is_ascii_lowercase()) {
        return 2; // en-US / en-GB ...
    }
    3 // auto captions (en-orig) and anything else
}

fn pick_best_caption(files: &[String], video_id: &str) -> Option<String> {
    let prefix = format!("{video_id}.");
    let mut best: Option<&String> = None;
    let mut best_rank = i32::MAX;
    for f in files {
        let code = if let Some(rest) = f.strip_prefix(&prefix) {
            rest.strip_suffix(".vtt").unwrap_or(rest)
        } else {
            f.as_str()
        };
        let rank = caption_rank(code);
        if rank < best_rank {
            best_rank = rank;
            best = Some(f);
        }
    }
    best.cloned()
}

/// Original-language caption tracks worth trying (never translated copies).
const CAPTION_LANGS: [&str; 5] = ["en", "en-US", "en-GB", "en-orig", "en-lyrics"];

fn temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut dir = std::env::temp_dir();
    dir.push(format!("{prefix}{}-{nanos}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn vtt_files(dir: &PathBuf) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.ends_with(".vtt"))
        .collect();
    files.sort();
    files
}

async fn youtube_caption_lyrics(video_id: &str) -> Option<LyricsResult> {
    let tmp = temp_dir("ytrium-lyrics-");
    let tmp_path = tmp.to_string_lossy().to_string();

    let result = async {
        let url = format!("https://www.youtube.com/watch?v={video_id}");
        let langs = CAPTION_LANGS.join(",");
        let run = run_ytdlp(
            &[
                "--no-playlist",
                "--skip-download",
                "--write-subs",
                "--write-auto-subs",
                "--sub-langs",
                &langs,
                "--sub-format",
                "vtt",
                "--paths",
                &tmp_path,
                "-o",
                "%(id)s.%(ext)s",
                &url,
            ],
            config::subs_timeout(),
        )
        .await;

        // yt-dlp aborts with a non-zero exit if a *single* caption track fails
        // (say, one 429 among several). Captions that already downloaded are
        // still useful, so keep going when we have at least one .vtt file.
        if let Err(err) = run {
            if vtt_files(&tmp).is_empty() {
                eprintln!("[lyrics] youtube caption download failed for {video_id}: {err}");
                return None;
            }
        }

        let files = vtt_files(&tmp);
        let chosen = pick_best_caption(&files, video_id)?;
        let mut path = tmp.clone();
        path.push(chosen);
        let vtt = std::fs::read_to_string(path).ok()?;
        let synced = parse_vtt(&vtt);
        if synced.is_empty() {
            return None;
        }
        Some(LyricsResult {
            source: "youtube-captions".to_string(),
            instrumental: false,
            plain_text: Some(
                synced
                    .iter()
                    .map(|l| l.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            synced: Some(synced),
            track_name: None,
            artist_name: None,
            album_name: None,
        })
    }
    .await;

    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/* ------------------------------- orchestrator ----------------------------- */

/// Lyrics for a track: LRCLIB exact -> scored LRCLIB search -> captions.
pub async fn get_lyrics_for(lookup: LyricsLookup) -> Option<LyricsResult> {
    let duration_key = lookup
        .duration
        .filter(|d| *d > 0)
        .map(|d| ((f64::from(d) / 10.0).round() * 10.0) as i64)
        .map(|v| v.to_string())
        .unwrap_or_default();
    let key = [
        normalize_key(&lookup.artist),
        normalize_key(&lookup.title),
        normalize_key(lookup.album.as_deref().unwrap_or("")),
        duration_key,
        lookup.video_id.clone().unwrap_or_default(),
    ]
    .join("|");

    if let Some(hit) = lyrics_cache().get(&key) {
        return hit;
    }

    let mut result: Option<LyricsResult> = None;

    if config::lyrics_enabled() {
        match lrclib_exact(&lookup.artist, &lookup.title) {
            Ok(Some(entry)) => {
                result = lrclib_to_result(&entry);
            }
            Ok(None) => {}
            Err(err) => eprintln!("[lyrics] lrclib exact lookup failed: {}", err.message()),
        }

        if result.is_none() {
            match lrclib_search(&lookup.artist, &lookup.title) {
                Ok(entries) => {
                    if let Some(best) = pick_best(&entries, &lookup) {
                        result = lrclib_to_result(best);
                    }
                }
                Err(err) => eprintln!("[lyrics] lrclib search failed: {}", err.message()),
            }
        }
    }

    if result.is_none() {
        if let Some(video_id) = lookup.video_id.as_deref() {
            if is_valid_video_id(video_id) {
                result = youtube_caption_lyrics(video_id).await;
            }
        }
    }

    lyrics_cache().set(&key, result.clone(), None);
    result
}
