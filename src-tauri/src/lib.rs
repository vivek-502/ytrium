//! Ytrium desktop shell.
//!
//! One process, one window, no network listener: the React UI is served from
//! the bundled assets and reaches the Rust side over Tauri's IPC (`invoke`).
//! Everything the web build did in an Express server lives behind the two
//! command groups below - `api_request` (the `/api/*` surface) and the
//! `audio_*` commands (the media element, re-implemented in Rust).

mod api;
mod audio;
mod cache;
mod config;
#[cfg(test)]
mod devcheck;
#[cfg(test)]
mod devcheck_audio;
mod error;
mod http;
mod providers;
mod ytdlp;

use tauri::window::Color;
use tauri::{WebviewUrl, WebviewWindowBuilder};

/// Window geometry and the dark first-paint colour (matches the UI's `--bg`,
/// so there is no white flash before React mounts).
const WINDOW_WIDTH: f64 = 1180.0;
const WINDOW_HEIGHT: f64 = 760.0;
const WINDOW_MIN_WIDTH: f64 = 900.0;
const WINDOW_MIN_HEIGHT: f64 = 560.0;
const WINDOW_BACKGROUND: Color = Color(11, 15, 13, 255);

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Remembers size/position across restarts.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .invoke_handler(tauri::generate_handler![
            api::api_request,
            api::export_library,
            api::audio_load,
            api::audio_play,
            api::audio_pause,
            api::audio_seek,
            api::audio_set_volume,
            api::audio_set_muted,
            api::audio_fade,
            api::audio_set_normalize,
            api::audio_status,
        ])
        .setup(|app| {
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("Ytrium")
                .inner_size(WINDOW_WIDTH, WINDOW_HEIGHT)
                .min_inner_size(WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT)
                .center()
                .background_color(WINDOW_BACKGROUND)
                .resizable(true)
                .build()?;
            let _ = window.set_focus();
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Ytrium");
}
