//! End-to-end checks for the parts a unit test cannot fake: yt-dlp metadata,
//! the chart/lyrics providers, the `/api/*` dispatcher and the decode pipeline.
//!
//! These hit the network (and shell out to `yt-dlp`), so they are run on
//! demand: `cargo test -- --test-threads=1 --nocapture`.

use std::time::Duration;

use serde_json::json;

use crate::api;

#[tokio::test]
async fn search_returns_playable_tracks() {
    let out = api::dispatch(
        "GET",
        "/api/search?q=daft%20punk%20instant%20crush&limit=5",
        None,
    )
    .await
    .expect("search failed");
    let tracks = out["tracks"].as_array().expect("tracks array");
    assert!(!tracks.is_empty(), "no search results: {out}");
    println!("[search] {} tracks", tracks.len());
    for t in tracks.iter().take(3) {
        println!(
            "[search]  {} | {} | {:.0}s | id={}",
            t["title"].as_str().unwrap_or("?"),
            t["artist"].as_str().unwrap_or("?"),
            t["duration"].as_f64().unwrap_or(0.0),
            t["id"].as_str().unwrap_or("?")
        );
    }
    assert!(
        tracks.iter().any(|t| t["sourceId"].is_string()),
        "no track had a resolvable source id"
    );
    assert!(out["artists"].is_array() && out["albums"].is_array());
}

#[tokio::test]
async fn search_requires_a_query() {
    let err = api::dispatch("GET", "/api/search", None).await.unwrap_err();
    assert_eq!(err.code, "INVALID_INPUT");
    assert_eq!(err.detail.as_deref(), Some("query must be a string"));

    let err = api::dispatch("GET", "/api/search?q=%20%20", None)
        .await
        .unwrap_err();
    assert_eq!(err.detail.as_deref(), Some("query is required"));

    let err = api::dispatch("GET", "/api/search?limit=0&q=x", None)
        .await
        .unwrap_err();
    assert_eq!(
        err.detail.as_deref(),
        Some("limit must be a positive number")
    );
}

#[tokio::test]
async fn unknown_routes_are_not_found() {
    let err = api::dispatch("GET", "/api/nope", None).await.unwrap_err();
    assert_eq!(err.code, "NOT_FOUND");
    assert_eq!(err.status, 404);
}

#[tokio::test]
async fn track_metadata_and_id_validation() {
    let out = api::dispatch("GET", "/api/tracks/yt%3AdQw4w9WgXcQ", None)
        .await
        .expect("track lookup failed");
    println!("[track] {}", out["track"]["title"]);
    assert_eq!(out["track"]["sourceId"], json!("dQw4w9WgXcQ"));

    let err = api::dispatch("GET", "/api/tracks/!!", None)
        .await
        .unwrap_err();
    assert_eq!(err.detail.as_deref(), Some("invalid track id"));
}

#[tokio::test]
async fn resolve_by_id_and_by_query() {
    let by_id = api::dispatch(
        "POST",
        "/api/resolve",
        Some(json!({ "id": "yt:dQw4w9WgXcQ" })),
    )
    .await
    .expect("resolve by id failed");
    assert_eq!(by_id["track"]["sourceId"], json!("dQw4w9WgXcQ"));

    let by_query = api::dispatch(
        "POST",
        "/api/resolve",
        Some(json!({ "title": "Never Gonna Give You Up", "artist": "Rick Astley" })),
    )
    .await
    .expect("resolve by query failed");
    println!("[resolve] {}", by_query["track"]["title"]);
    assert!(by_query["track"]["sourceId"].is_string());

    let err = api::dispatch("POST", "/api/resolve", Some(json!({})))
        .await
        .unwrap_err();
    assert_eq!(err.detail.as_deref(), Some("provide id, q, or title"));
}

#[tokio::test]
async fn charts_trending_and_recommendations() {
    let charts = api::dispatch("GET", "/api/charts", None)
        .await
        .expect("charts failed");
    println!(
        "[charts] source={} trending={} popular={} albums={}",
        charts["chartsSource"],
        charts["trending"].as_array().map(Vec::len).unwrap_or(0),
        charts["popular"].as_array().map(Vec::len).unwrap_or(0),
        charts["popularAlbums"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0)
    );
    assert!(!charts["trending"].as_array().unwrap().is_empty());
    // A decode regression in the iTunes feed should fail here, not silently
    // degrade to the yt-dlp fallback.
    assert_eq!(charts["chartsSource"], json!("itunes"));

    let trending = api::dispatch("GET", "/api/trending?limit=3", None)
        .await
        .expect("trending failed");
    assert_eq!(trending["tracks"].as_array().unwrap().len(), 3);

    let recs = api::dispatch(
        "GET",
        "/api/recommendations?seeds=daft%20punk&limit=5",
        None,
    )
    .await
    .expect("recommendations failed");
    println!(
        "[recommendations] {} tracks",
        recs["tracks"].as_array().unwrap().len()
    );
    assert!(!recs["tracks"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn lyrics_lookup_and_envelope() {
    let out = api::dispatch(
        "GET",
        "/api/lyrics?title=Never%20Gonna%20Give%20You%20Up&artist=Rick%20Astley&duration=213&id=dQw4w9WgXcQ",
        None,
    )
    .await
    .expect("lyrics failed");
    match &out["lyrics"] {
        serde_json::Value::Null => println!("[lyrics] no match (allowed)"),
        value => {
            println!(
                "[lyrics] source={} synced={} plain={}",
                value["source"],
                value["synced"].is_array(),
                value["plainText"].is_string()
            );
            assert!(value["plainText"].is_string() || value["synced"].is_array());
        }
    }

    let missing = api::dispatch("GET", "/api/lyrics?artist=x", None)
        .await
        .unwrap_err();
    assert_eq!(missing.detail.as_deref(), Some("query must be a string"));

    let missing = api::dispatch("GET", "/api/lyrics?title=x", None)
        .await
        .unwrap_err();
    assert_eq!(missing.detail.as_deref(), Some("query must be a string"));
}

#[tokio::test]
async fn stream_meta_warms_the_cache() {
    let out = api::dispatch("GET", "/api/stream/yt:dQw4w9WgXcQ/meta?q=low", None)
        .await
        .expect("stream meta failed");
    println!("[stream] mode={} quality={}", out["mode"], out["quality"]);
    assert_eq!(out["mode"], json!("native"));
    assert!(out["expiresAt"].as_i64().unwrap_or(0) > 0);

    let err = api::dispatch("GET", "/api/stream/yt:bad/meta", None)
        .await
        .unwrap_err();
    assert_eq!(err.detail.as_deref(), Some("invalid stream id"));
}

/// Downloads a real stream and decodes it: duration, non-silent samples, seek.
#[tokio::test]
async fn decoder_reads_and_seeks() {
    use rodio::Source;

    let video_id = "dQw4w9WgXcQ";
    let source = crate::providers::youtube::resolve_media_url(video_id, "low")
        .await
        .expect("could not resolve a media url");
    println!(
        "[decode] url host={} contentType={:?}",
        source.url.split('/').nth(2).unwrap_or("?"),
        source.content_type
    );

    let (mut decoder, control, duration) =
        crate::audio::open_stream(&source.url, video_id).expect("could not open the stream");
    let channels = decoder.channels().get();
    let rate = decoder.sample_rate().get();
    println!("[decode] duration={duration:.2}s channels={channels} rate={rate}");
    assert!(duration > 30.0, "unexpected duration: {duration}");

    let budget = rate as usize * channels as usize * 20; // ~20 s of audio
    let mut decoded = 0usize;
    let mut audible = 0usize;
    let mut peak = 0f32;
    for sample in decoder.by_ref() {
        if sample.abs() > 1e-5 {
            audible += 1;
            peak = peak.max(sample.abs());
        }
        decoded += 1;
        if decoded >= budget {
            break;
        }
    }
    println!("[decode] samples={decoded} audible={audible} peak={peak:.3}");
    assert!(decoded > 0, "decoder produced no samples");
    assert!(audible > 1_000, "decoded audio was silent");

    decoder
        .try_seek(Duration::from_secs(45))
        .expect("seek was rejected");
    let mut after = 0usize;
    let mut after_audible = 0usize;
    for _ in 0..budget {
        match decoder.next() {
            Some(sample) => {
                if sample.abs() > 1e-5 {
                    after_audible += 1;
                }
                after += 1;
            }
            None => break,
        }
    }
    println!("[decode] after seek: samples={after} audible={after_audible}");
    assert!(after_audible > 1_000, "no audio decoded after seeking");

    control.cancel();
}

/// Playlist search (v1.1.0): `/api/search` attaches playlists, an artist rail
/// resolves, and the first playlist expands into playable tracks.
#[tokio::test]
async fn playlist_search_and_expansion() {
    let out = api::dispatch("GET", "/api/search?q=daft%20punk&limit=5", None)
        .await
        .expect("search failed");
    let playlists = out["playlists"].as_array().expect("playlists array");
    println!("[search] {} playlists", playlists.len());
    assert!(!playlists.is_empty(), "no playlist results: {out}");

    let first = &playlists[0];
    assert!(first["id"].is_string(), "playlist id missing: {first}");
    assert!(first["title"].is_string(), "playlist title missing: {first}");
    assert!(first["owner"].is_string(), "playlist owner missing: {first}");
    assert!(first["official"].is_boolean(), "official flag missing: {first}");
    assert_eq!(first["source"], "youtube");

    // Native playlist ids are URL-safe, so they pass through unescaped.
    let id = first["id"].as_str().expect("playlist id str");
    let tracks = api::dispatch("GET", &format!("/api/playlists/{id}/tracks?limit=10"), None)
        .await
        .expect("playlist tracks failed");
    let count = tracks["tracks"].as_array().map(Vec::len).unwrap_or(0);
    println!("[playlist] {id} -> {count} tracks");
    assert!(count > 0, "playlist {id} expanded to nothing: {tracks}");

    let rail = api::dispatch("GET", "/api/artist/playlists?name=daft%20punk&limit=5", None)
        .await
        .expect("artist playlists failed");
    let rail_count = rail["playlists"].as_array().map(Vec::len).unwrap_or(0);
    println!("[artist playlists] {rail_count} results");
    assert!(rail_count > 0, "artist playlist rail empty: {rail}");
}

/// Offline error parity for the playlist routes (messages match the web build).
#[tokio::test]
async fn playlist_route_errors() {
    let err = api::dispatch("GET", "/api/playlists/not-a-playlist/tracks", None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "NOT_FOUND");
    assert_eq!(err.status, 404);
    assert_eq!(err.message, "That playlist could not be loaded.");

    let err = api::dispatch(
        "GET",
        "/api/playlists/PL01234567890ABCD/tracks?limit=0",
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "INVALID_INPUT");
    assert_eq!(err.status, 400);

    let err = api::dispatch("GET", "/api/artist/playlists", None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "INVALID_INPUT");
    assert_eq!(err.status, 400);

    let err = api::dispatch("GET", "/api/playlists/x", None).await.unwrap_err();
    assert_eq!(err.code, "NOT_FOUND");
}
