//! Chart/toplist metadata provider.
//!
//! Uses Apple's public iTunes RSS + lookup APIs (metadata only, no media).
//! Chart tracks are `itunes:` records that carry a `resolveQuery`; playback is
//! resolved lazily through the YouTube provider right before a track plays.
//! Falls back to yt-dlp search derived charts when iTunes is unavailable.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::Deserialize;

use super::{album_id_for, artist_id_for, clean_title, split_title};
use super::{Album, Artist, ChartsResult, Track};
use crate::cache::TtlCache;
use crate::config;
use crate::error::{ApiError, ApiResult};
use crate::http::{self, FetchError};
use crate::ytdlp::{parse_json_lines, run_ytdlp};

static CHARTS_CACHE: OnceLock<TtlCache<ChartsResult>> = OnceLock::new();

fn charts_cache() -> &'static TtlCache<ChartsResult> {
    CHARTS_CACHE.get_or_init(|| TtlCache::new(config::CHART_TTL, 10))
}

const FALLBACK_QUERIES: [&str; 3] = [
    "trending songs this week",
    "top 50 popular songs",
    "new music hits",
];

/// The subset of a flat yt-dlp entry the fallback charts need.
#[derive(Debug, Clone, Default, Deserialize)]
struct FallbackEntry {
    id: Option<String>,
    title: Option<String>,
    duration: Option<f64>,
    uploader: Option<String>,
    view_count: Option<f64>,
}

/* --------------------------------- helpers -------------------------------- */

fn fetch_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, FetchError> {
    http::get_json(http::json_agent(), url)
}

/// Ask mzstatic for a large artwork size instead of the 170px thumbnail.
fn upgrade_artwork(url: Option<&str>) -> Option<String> {
    let url = url?;
    if url.is_empty() {
        return None;
    }
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"/\d+x\d+bb\.(png|jpg)").expect("artwork regex"));
    Some(re.replace(url, "/600x600bb.$1").into_owned())
}

/// iTunes feeds sometimes emit a single object where a list is expected.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            OneOrMany::One(one) => vec![one],
            OneOrMany::Many(many) => many,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Label {
    label: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Collection {
    #[serde(rename = "im:name")]
    name: Option<Label>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ReleaseDate {
    attributes: Option<Label>,
    label: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct IdAttr {
    attributes: Option<ImId>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ImId {
    #[serde(rename = "im:id")]
    im_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Link {
    attributes: Option<Href>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Href {
    href: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Category {
    attributes: Option<Label>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RssEntry {
    #[serde(rename = "im:name")]
    name: Option<Label>,
    #[serde(rename = "im:image")]
    image: Option<OneOrMany<Label>>,
    #[serde(rename = "im:collection")]
    collection: Option<Collection>,
    #[serde(rename = "im:artist")]
    artist: Option<Label>,
    #[serde(rename = "im:releaseDate")]
    release_date: Option<ReleaseDate>,
    id: Option<IdAttr>,
    link: Option<OneOrMany<Link>>,
    title: Option<Label>,
    category: Option<Category>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FeedBody {
    entry: Option<OneOrMany<RssEntry>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RssFeed {
    feed: Option<FeedBody>,
}

fn entries_of(feed: RssFeed) -> Vec<RssEntry> {
    feed.feed
        .and_then(|f| f.entry)
        .map(OneOrMany::into_vec)
        .unwrap_or_default()
}

fn last_image(entry: &RssEntry) -> Option<String> {
    let images = entry.image.as_ref()?;
    let list = match images {
        OneOrMany::One(one) => vec![one.clone()],
        OneOrMany::Many(many) => many.clone(),
    };
    list.last().and_then(|img| upgrade_artwork(img.label.as_deref()))
}

/// The alternate (web) link for an entry. The top-songs feed repeats `link` as
/// a list (with a preview enclosure second), the albums feed sends one object.
fn first_link(entry: &RssEntry) -> Option<&Link> {
    let link = entry.link.as_ref()?;
    let candidates: Vec<&Link> = match link {
        OneOrMany::One(one) => vec![one],
        OneOrMany::Many(many) => many.iter().collect(),
    };
    candidates
        .iter()
        .find(|l| l.attributes.as_ref().is_some_and(|a| a.href.is_some()))
        .copied()
        .or_else(|| candidates.first().copied())
}

fn release_label(entry: &RssEntry) -> Option<String> {
    let rd = entry.release_date.as_ref()?;
    rd.attributes
        .as_ref()
        .and_then(|a| a.label.clone())
        .or_else(|| rd.label.clone())
        .filter(|v| !v.is_empty())
}

fn entry_im_id(entry: &RssEntry) -> Option<i64> {
    entry
        .id
        .as_ref()
        .and_then(|i| i.attributes.as_ref())
        .and_then(|a| a.im_id.as_ref())
        .and_then(|v| v.parse::<i64>().ok())
}

/* ------------------------------ iTunes lookup ----------------------------- */

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LookupTrack {
    wrapper_type: Option<String>,
    track_id: Option<i64>,
    track_name: Option<String>,
    artist_name: Option<String>,
    collection_name: Option<String>,
    track_time_millis: Option<f64>,
    artwork_url100: Option<String>,
    release_date: Option<String>,
    primary_genre_name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct LookupResponse {
    results: Option<Vec<LookupTrack>>,
}

fn charts_base() -> String {
    config::charts_base().trim_end_matches('/').to_string()
}

fn lookup_tracks(ids: &[i64]) -> HashMap<i64, LookupTrack> {
    let mut map = HashMap::new();
    if ids.is_empty() {
        return map;
    }
    let base = charts_base();
    for chunk in ids.chunks(25) {
        let joined = chunk
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let url = format!("{base}/lookup?id={joined}&entity=song&limit=100");
        match fetch_json::<LookupResponse>(&url) {
            Ok(res) => {
                for r in res.results.unwrap_or_default() {
                    if let Some(id) = r.track_id {
                        map.insert(id, r);
                    }
                }
            }
            Err(err) => {
                eprintln!("[charts] itunes lookup enrichment failed: {}", err.message());
            }
        }
    }
    map
}

fn song_from_rss(entry: &RssEntry, detail: Option<&LookupTrack>) -> Option<Track> {
    let track_id = entry_im_id(entry)?;
    let title = detail
        .and_then(|d| d.track_name.clone())
        .or_else(|| entry.name.as_ref().and_then(|n| n.label.clone()))
        .or_else(|| entry.title.as_ref().and_then(|t| t.label.clone()))?;
    if title.is_empty() {
        return None;
    }

    let artist = detail
        .and_then(|d| d.artist_name.clone())
        .or_else(|| entry.artist.as_ref().and_then(|a| a.label.clone()))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let album = detail
        .and_then(|d| d.collection_name.clone())
        .or_else(|| {
            entry
                .collection
                .as_ref()
                .and_then(|c| c.name.as_ref())
                .and_then(|n| n.label.clone())
        });
    let genre = detail
        .and_then(|d| d.primary_genre_name.clone())
        .or_else(|| {
            entry
                .category
                .as_ref()
                .and_then(|c| c.attributes.as_ref())
                .and_then(|a| a.label.clone())
        });
    let release_date = detail
        .and_then(|d| d.release_date.as_deref().map(|r| r.chars().take(10).collect::<String>()))
        .or_else(|| release_label(entry));
    let artwork = detail
        .and_then(|d| upgrade_artwork(d.artwork_url100.as_deref()))
        .or_else(|| last_image(entry));
    let resolve_query = Some(format!("{title} {artist}"));

    Some(Track {
        id: format!("itunes:{track_id}"),
        source: "itunes".to_string(),
        source_id: None,
        title: split_title(&clean_title(&title), &artist),
        artist_id: Some(artist_id_for(&artist)),
        album_id: album.as_deref().map(|a| album_id_for(&artist, a)),
        album,
        artist,
        duration: detail
            .and_then(|d| d.track_time_millis)
            .map(|ms| (ms / 1000.0).round() as u32)
            .unwrap_or(0),
        thumbnail: artwork,
        view_count: None,
        genre,
        release_date,
        url: first_link(entry)
            .and_then(|l| l.attributes.as_ref())
            .and_then(|a| a.href.clone()),
        resolve_query,
    })
}

fn album_from_rss(entry: &RssEntry) -> Option<Album> {
    entry_im_id(entry)?;
    let title = entry.name.as_ref().and_then(|n| n.label.clone())?;
    let artist = entry
        .artist
        .as_ref()
        .and_then(|a| a.label.clone())
        .unwrap_or_else(|| "Unknown Artist".to_string());
    Some(Album {
        id: album_id_for(&artist, &title),
        title,
        artist_id: Some(artist_id_for(&artist)),
        artist,
        image: last_image(entry),
        release_date: release_label(entry),
        track_count: None,
        source: "itunes".to_string(),
    })
}

/// Build a track record from a bare iTunes lookup result (no RSS entry).
fn track_from_lookup(detail: &LookupTrack) -> Option<Track> {
    let track_id = detail.track_id?;
    let title = detail.track_name.clone()?;
    let artist = detail
        .artist_name
        .clone()
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let album = detail.collection_name.clone();
    let resolve_query = Some(format!("{title} {artist}"));
    Some(Track {
        id: format!("itunes:{track_id}"),
        source: "itunes".to_string(),
        source_id: None,
        title: split_title(&clean_title(&title), &artist),
        artist_id: Some(artist_id_for(&artist)),
        album_id: album.as_deref().map(|a| album_id_for(&artist, a)),
        album,
        artist,
        duration: detail
            .track_time_millis
            .map(|ms| (ms / 1000.0).round() as u32)
            .unwrap_or(0),
        thumbnail: upgrade_artwork(detail.artwork_url100.as_deref()),
        view_count: None,
        genre: detail.primary_genre_name.clone(),
        release_date: detail
            .release_date
            .as_deref()
            .map(|r| r.chars().take(10).collect()),
        url: None,
        resolve_query,
    })
}

/// Songs belonging to the given collection ids (`entity=song` returns the
/// collection itself plus its tracks - non-track rows are skipped).
fn lookup_album_songs(album_ids: &[i64]) -> Vec<Track> {
    let mut out = Vec::new();
    if album_ids.is_empty() {
        return out;
    }
    let base = charts_base();
    for chunk in album_ids.chunks(10) {
        let joined = chunk
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let url = format!("{base}/lookup?id={joined}&entity=song&limit=200");
        match fetch_json::<LookupResponse>(&url) {
            Ok(res) => {
                for r in res.results.unwrap_or_default() {
                    if let Some(wrapper) = r.wrapper_type.as_deref() {
                        if wrapper != "track" {
                            continue;
                        }
                    }
                    if let Some(track) = track_from_lookup(&r) {
                        out.push(track);
                    }
                }
            }
            Err(err) => {
                eprintln!("[charts] itunes album song lookup failed: {}", err.message());
            }
        }
    }
    out
}

fn derive_artists(songs: &[Track], albums: &[Album]) -> Vec<Artist> {
    fn push(
        order: &mut Vec<String>,
        map: &mut HashMap<String, Artist>,
        id: String,
        name: String,
        image: Option<String>,
    ) {
        if !map.contains_key(&id) {
            order.push(id.clone());
            map.insert(
                id.clone(),
                Artist {
                    id,
                    name,
                    image,
                    description: None,
                    source: "itunes".to_string(),
                },
            );
        }
    }

    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, Artist> = HashMap::new();
    for s in songs {
        if let Some(id) = s.artist_id.clone() {
            push(&mut order, &mut map, id, s.artist.clone(), s.thumbnail.clone());
        }
    }
    for a in albums {
        if let Some(id) = a.artist_id.clone() {
            push(&mut order, &mut map, id, a.artist.clone(), a.image.clone());
        }
    }
    order.into_iter().filter_map(|id| map.remove(&id)).collect()
}

/* --------------------------------- charts --------------------------------- */

async fn itunes_charts() -> ApiResult<ChartsResult> {
    let base = charts_base();
    let songs_feed = fetch_json::<RssFeed>(&format!("{base}/rss/topsongs/limit=30/json"))
        .map_err(|e| ApiError::upstream_unavailable(Some(format!("charts http {}", e.message()))))?;
    let albums_feed = fetch_json::<RssFeed>(&format!("{base}/rss/topalbums/limit=20/json"))
        .map_err(|e| ApiError::upstream_unavailable(Some(format!("charts http {}", e.message()))))?;

    let song_entries = entries_of(songs_feed);
    let album_entries = entries_of(albums_feed);
    if song_entries.is_empty() {
        return Err(ApiError::upstream_unavailable(Some(
            "empty top songs feed".into(),
        )));
    }

    let ids: Vec<i64> = song_entries.iter().filter_map(entry_im_id).collect();
    let details = lookup_tracks(&ids);

    let mut trending: Vec<Track> = Vec::new();
    for entry in &song_entries {
        if let Some(id) = entry_im_id(entry) {
            if let Some(track) = song_from_rss(entry, details.get(&id)) {
                trending.push(track);
            }
        }
    }

    let albums: Vec<Album> = album_entries.iter().filter_map(album_from_rss).collect();

    /* "New releases": real tracks from the newest chart albums. The feed is
     * ordered by popularity, so re-rank albums by release date and resolve
     * their songs. Falls back to the newest chart songs when the song lookup is
     * unavailable, so the field is always a track list. */
    let mut dated: Vec<(i64, String)> = album_entries
        .iter()
        .filter_map(|e| Some((entry_im_id(e)?, release_label(e).unwrap_or_default())))
        .collect();
    dated.sort_by(|a, b| b.1.cmp(&a.1));
    let newest_album_ids: Vec<i64> = dated.iter().take(8).map(|(id, _)| *id).collect();

    let mut seen_song_ids: HashSet<String> = HashSet::new();
    let new_release_songs: Vec<Track> = lookup_album_songs(&newest_album_ids)
        .into_iter()
        .filter(|t| seen_song_ids.insert(t.id.clone()))
        .collect();

    let new_releases: Vec<Track> = if !new_release_songs.is_empty() {
        new_release_songs.into_iter().take(12).collect()
    } else {
        let mut with_dates: Vec<Track> = trending
            .iter()
            .filter(|t| t.release_date.is_some())
            .cloned()
            .collect();
        with_dates.sort_by(|a, b| {
            b.release_date
                .clone()
                .unwrap_or_default()
                .cmp(&a.release_date.clone().unwrap_or_default())
        });
        with_dates.into_iter().take(12).collect()
    };

    let popular_artists: Vec<Artist> = derive_artists(&trending, &albums)
        .into_iter()
        .take(12)
        .collect();

    if trending.is_empty() {
        return Err(ApiError::upstream_unavailable(Some(
            "chart normalization produced nothing".into(),
        )));
    }

    Ok(ChartsResult {
        popular: trending.clone(),
        trending,
        new_releases,
        popular_artists,
        popular_albums: albums.into_iter().take(12).collect(),
        charts_source: "itunes".to_string(),
    })
}

/// Fallback: derive trending material from real YouTube search results.
async fn ytdlp_charts() -> ApiResult<ChartsResult> {
    let mut collected: Vec<Track> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for query in FALLBACK_QUERIES {
        let search_term = format!("ytsearch10:{query}");
        let stdout = match run_ytdlp(
            &[
                "--flat-playlist",
                "--dump-json",
                "--no-warnings",
                "--skip-download",
                &search_term,
            ],
            config::ytdlp_timeout(),
        )
        .await
        {
            Ok(out) => out,
            Err(err) => {
                eprintln!("[charts] trending fallback query failed ({query}): {err}");
                continue;
            }
        };
        for entry in parse_json_lines::<FallbackEntry>(&stdout) {
            let Some(id) = entry.id.clone() else { continue };
            if id.len() != 11 || !seen.insert(id.clone()) {
                continue;
            }
            let artist = super::strip_topic(
                entry
                    .uploader
                    .as_deref()
                    .filter(|u| !u.is_empty())
                    .unwrap_or("Unknown Artist"),
            );
            let raw_title = entry.title.clone().unwrap_or_else(|| "Untitled".to_string());
            collected.push(Track {
                id: format!("yt:{id}"),
                source: "youtube".to_string(),
                source_id: Some(id.clone()),
                title: split_title(&clean_title(&raw_title), &artist),
                artist_id: Some(artist_id_for(&artist)),
                artist,
                album: None,
                album_id: None,
                duration: entry.duration.filter(|d| *d > 0.0).map(|d| d.round() as u32).unwrap_or(0),
                thumbnail: Some(super::thumbnail_for(&id)),
                view_count: entry.view_count.map(|v| v.round() as i64),
                genre: None,
                release_date: None,
                url: Some(format!("https://www.youtube.com/watch?v={id}")),
                resolve_query: None,
            });
        }
    }

    if collected.is_empty() {
        return Err(ApiError::upstream_unavailable(Some(
            "no chart data available".into(),
        )));
    }

    Ok(ChartsResult {
        trending: collected.iter().take(24).cloned().collect(),
        popular: collected.iter().take(24).cloned().collect(),
        new_releases: collected.iter().take(12).cloned().collect(),
        popular_artists: Vec::new(),
        popular_albums: Vec::new(),
        charts_source: "youtube-search".to_string(),
    })
}

/// Aggregated chart data with TTL caching and graceful provider fallback.
pub async fn get_charts() -> ApiResult<ChartsResult> {
    if let Some(hit) = charts_cache().get("charts") {
        return Ok(hit);
    }
    let result = if config::charts_enabled() {
        match itunes_charts().await {
            Ok(charts) => charts,
            Err(err) => {
                eprintln!(
                    "[charts] provider failed, falling back to yt-dlp search: {} {}",
                    err.code,
                    err.detail.as_deref().unwrap_or(&err.message)
                );
                ytdlp_charts().await?
            }
        }
    } else {
        ytdlp_charts().await?
    };
    charts_cache().set("charts", result.clone(), None);
    Ok(result)
}
