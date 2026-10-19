//! Rust-native audio engine.
//!
//! The web build drove an `<audio>` element with byte ranges proxied through
//! the server. Here the work is split the same way conceptually, but the sink
//! and decoder live in Rust:
//!
//! * the UI sends commands (`Cmd`) over a channel - never blocking on audio;
//! * one audio thread owns the rodio `Player` and applies volume/fades;
//! * a per-track loader thread downloads + opens the stream and appends it;
//! * a separate seek thread performs seeks, so a stalled sink can never wedge
//!   the command loop;
//! * the UI polls `audio_status` and turns state transitions into the same
//!   `onTime`/`onPlaying`/`onEnded`/... callbacks the `<audio>` element gave it.
//!
//! Every state change is sequenced (`load_seq`, `ended_seq`, `error_seq`) so
//! the poller can tell a real transition from a repeated snapshot.

pub mod normalizer;
pub mod reader;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};
use serde::Serialize;

use self::normalizer::Normalized;
use self::reader::{RangeReader, ReaderControl};
use crate::error::ApiResult;
use crate::providers::youtube;

/// Command-loop and status granularity. Fades are applied in these steps.
const TICK_MS: u64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Idle,
    Loading,
    Playing,
    Paused,
    Ended,
    Error,
}

impl Transport {
    fn as_str(self) -> &'static str {
        match self {
            Transport::Idle => "idle",
            Transport::Loading => "loading",
            Transport::Playing => "playing",
            Transport::Paused => "paused",
            Transport::Ended => "ended",
            Transport::Error => "error",
        }
    }
}

/// Snapshot handed to the frontend on every poll.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerStatus {
    pub state: &'static str,
    pub position: f64,
    pub duration: f64,
    pub error: Option<String>,
    pub error_seq: u64,
    pub ended_seq: u64,
    pub load_seq: u64,
}

struct Status {
    state: Transport,
    position: f64,
    duration: f64,
    error: Option<String>,
    error_seq: u64,
    ended_seq: u64,
}

/// State shared between the UI-facing handle, the audio thread and track
/// loaders.
struct Shared {
    status: Mutex<Status>,
    /// Bumped the moment a load is requested (before yt-dlp runs).
    load_seq: AtomicU64,
    /// Highest load sequence whose source has been handed to the player.
    appended_seq: AtomicU64,
    /// Handle of the reader that is currently downloaded/decoding.
    reader: Mutex<Option<ReaderControl>>,
    /// Shared with every `Normalized` source so the setting applies live.
    normalize: Arc<AtomicBool>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            status: Mutex::new(Status {
                state: Transport::Idle,
                position: 0.0,
                duration: 0.0,
                error: None,
                error_seq: 0,
                ended_seq: 0,
            }),
            load_seq: AtomicU64::new(0),
            appended_seq: AtomicU64::new(0),
            reader: Mutex::new(None),
            normalize: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Claim a new generation and put the UI into `loading`.
    fn begin_load(&self, start_at: f64) -> u64 {
        let seq = self.load_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let mut status = self.status.lock().expect("status poisoned");
        status.state = Transport::Loading;
        status.error = None;
        status.position = start_at.max(0.0);
        status.duration = 0.0;
        seq
    }

    /// Report a failure for `seq` (ignored if a newer load replaced it).
    fn fail(&self, seq: u64, message: String) {
        if self.load_seq.load(Ordering::SeqCst) != seq {
            return;
        }
        let mut status = self.status.lock().expect("status poisoned");
        status.state = Transport::Error;
        status.error = Some(message);
        status.error_seq += 1;
    }

    fn current_seq(&self) -> u64 {
        self.load_seq.load(Ordering::SeqCst)
    }

    fn set_reader(&self, seq: u64, control: ReaderControl) -> bool {
        let mut slot = self.reader.lock().expect("reader slot poisoned");
        if self.current_seq() != seq {
            return false;
        }
        *slot = Some(control);
        true
    }

    fn take_reader(&self) -> Option<ReaderControl> {
        self.reader.lock().expect("reader slot poisoned").take()
    }

    /// Take a reader-side stream failure, if any (reported at most once).
    fn take_stream_error(&self) -> Option<String> {
        let mut slot = self.reader.lock().expect("reader slot poisoned");
        let error = slot.as_ref().and_then(|r| r.stream_error());
        if error.is_some() {
            *slot = None;
        }
        error
    }

    fn snapshot(&self) -> PlayerStatus {
        let status = self.status.lock().expect("status poisoned");
        PlayerStatus {
            state: status.state.as_str(),
            position: status.position,
            duration: status.duration,
            error: status.error.clone(),
            error_seq: status.error_seq,
            ended_seq: status.ended_seq,
            load_seq: self.current_seq(),
        }
    }
}

enum Cmd {
    Load {
        seq: u64,
        url: String,
        video_id: String,
        start_at: f64,
        autoplay: bool,
    },
    Play,
    Pause,
    Seek(f64),
    SetVolume(f32),
    SetMuted(bool),
    Fade {
        target: f32,
        ms: u64,
    },
    SetNormalize(bool),
}

struct Fade {
    from: f32,
    to: f32,
    ms: f32,
    elapsed: f32,
    started: bool,
}

/// UI-facing handle. Cheap to clone through the global instance.
pub struct AudioEngine {
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
}

impl AudioEngine {
    /// The process-wide engine (one audio device, one player).
    pub fn global() -> &'static AudioEngine {
        static ENGINE: OnceLock<AudioEngine> = OnceLock::new();
        ENGINE.get_or_init(AudioEngine::spawn)
    }

    fn spawn() -> AudioEngine {
        let shared = Arc::new(Shared::new());
        let (tx, rx) = mpsc::channel::<Cmd>();
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("ytrium-audio".to_string())
            .spawn(move || audio_thread(rx, thread_shared))
            .expect("could not start the audio thread");
        AudioEngine { shared, tx }
    }

    /// Resolve and start a stream. Resolution failures surface through the
    /// status rather than as a rejected command, matching the web build where
    /// a failed stream surfaced on the media element.
    pub async fn load(&self, video_id: String, quality: String, start_at: f64, autoplay: bool) {
        let seq = self.shared.begin_load(start_at);
        match youtube::resolve_media_url(&video_id, &quality).await {
            Ok(source) => {
                let _ = self.tx.send(Cmd::Load {
                    seq,
                    url: source.url,
                    video_id,
                    start_at,
                    autoplay,
                });
            }
            Err(err) => self.shared.fail(seq, err.to_string()),
        }
    }

    pub fn play(&self) {
        let _ = self.tx.send(Cmd::Play);
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }

    pub fn seek(&self, seconds: f64) {
        let _ = self.tx.send(Cmd::Seek(seconds));
    }

    pub fn set_volume(&self, value: f32) {
        let _ = self.tx.send(Cmd::SetVolume(value));
    }

    pub fn set_muted(&self, muted: bool) {
        let _ = self.tx.send(Cmd::SetMuted(muted));
    }

    pub fn fade(&self, target: f32, ms: u64) {
        let _ = self.tx.send(Cmd::Fade { target, ms });
    }

    pub fn set_normalize(&self, enabled: bool) {
        let _ = self.tx.send(Cmd::SetNormalize(enabled));
    }

    pub fn status(&self) -> PlayerStatus {
        self.shared.snapshot()
    }
}

/* ------------------------------- audio thread ----------------------------- */

struct Engine {
    shared: Arc<Shared>,
    sink: Option<MixerDeviceSink>,
    player: Option<Arc<Player>>,
    seek_tx: Option<Sender<Duration>>,
    volume: f32,
    muted: bool,
    fade: Option<Fade>,
    append_lock: Arc<Mutex<()>>,
}

impl Engine {
    fn new(shared: Arc<Shared>) -> Engine {
        Engine {
            shared,
            sink: None,
            player: None,
            seek_tx: None,
            volume: 1.0,
            muted: false,
            fade: None,
            append_lock: Arc::new(Mutex::new(())),
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Load {
                seq,
                url,
                video_id,
                start_at,
                autoplay,
            } => self.start_load(seq, url, video_id, start_at, autoplay),
            Cmd::Play => {
                if let Some(player) = &self.player {
                    player.play();
                }
                let mut status = self.shared.status.lock().expect("status poisoned");
                if status.state == Transport::Paused {
                    status.state = Transport::Playing;
                }
            }
            Cmd::Pause => {
                if let Some(player) = &self.player {
                    player.pause();
                }
                let mut status = self.shared.status.lock().expect("status poisoned");
                if status.state == Transport::Playing {
                    status.state = Transport::Paused;
                }
            }
            Cmd::Seek(seconds) => {
                let seq = self.shared.current_seq();
                if seq > 0
                    && self.shared.appended_seq.load(Ordering::SeqCst) == seq
                    && seconds >= 0.0
                {
                    if let Some(tx) = &self.seek_tx {
                        let _ = tx.send(Duration::from_secs_f64(seconds));
                    }
                }
            }
            Cmd::SetVolume(value) => {
                self.volume = value.clamp(0.0, 4.0);
                self.fade = None;
                self.apply_volume();
            }
            Cmd::SetMuted(muted) => {
                self.muted = muted;
                self.apply_volume();
            }
            Cmd::Fade { target, ms } => {
                self.fade = Some(Fade {
                    from: self.volume,
                    to: target.clamp(0.0, 4.0),
                    ms: ms as f32,
                    elapsed: 0.0,
                    started: false,
                });
            }
            Cmd::SetNormalize(enabled) => {
                self.shared.normalize.store(enabled, Ordering::SeqCst);
            }
        }
    }

    fn start_load(
        &mut self,
        seq: u64,
        url: String,
        video_id: String,
        start_at: f64,
        autoplay: bool,
    ) {
        // Stop the outgoing stream first: the reader cancels, so a read that is
        // blocked in the audio callback returns promptly and `clear` finishes.
        if let Some(reader) = self.shared.take_reader() {
            reader.cancel();
        }

        self.ensure_player();
        let Some(player) = self.player.clone() else {
            self.shared
                .fail(seq, "no audio output device is available".to_string());
            return;
        };

        // `clear` also pauses, so playback is resumed explicitly below.
        if !player.empty() {
            player.clear();
        }

        let seek_tx = match self.seek_tx.clone() {
            Some(tx) => tx,
            None => return,
        };

        spawn_loader(
            self.shared.clone(),
            player,
            self.shared.normalize_flag(),
            self.append_lock.clone(),
            seek_tx,
            seq,
            url,
            video_id,
            start_at,
            autoplay,
        );
    }

    /// Create the device sink and player on first use (a desktop app should not
    /// grab the audio device until the user actually plays something).
    fn ensure_player(&mut self) {
        if self.player.is_some() {
            return;
        }
        match DeviceSinkBuilder::open_default_sink() {
            Ok(sink) => {
                let player = Arc::new(Player::connect_new(sink.mixer()));
                player.set_volume(self.applied_volume());
                self.seek_tx = Some(spawn_seek_worker(player.clone()));
                self.sink = Some(sink);
                self.player = Some(player);
            }
            Err(err) => {
                eprintln!("[audio] could not open the default output device: {err}");
            }
        }
    }

    fn applied_volume(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.volume
        }
    }

    fn apply_volume(&self) {
        if let Some(player) = &self.player {
            player.set_volume(self.applied_volume());
        }
    }

    fn tick(&mut self) {
        self.advance_fade();

        let seq = self.shared.current_seq();
        if seq == 0 || self.shared.appended_seq.load(Ordering::SeqCst) != seq {
            return;
        }
        let Some(player) = self.player.clone() else {
            return;
        };

        let state = self.shared.status.lock().expect("status poisoned").state;
        if !matches!(state, Transport::Playing | Transport::Paused) {
            return;
        }

        {
            let mut status = self.shared.status.lock().expect("status poisoned");
            status.position = player.get_pos().as_secs_f64();
        }

        // A stream that died mid-flight is a load failure, not track end: the
        // client retries with a freshly resolved URL.
        if let Some(error) = self.shared.take_stream_error() {
            self.shared.fail(seq, error);
            return;
        }

        if state == Transport::Playing && player.empty() {
            let _ = self.shared.take_reader();
            let mut status = self.shared.status.lock().expect("status poisoned");
            status.state = Transport::Ended;
            status.ended_seq += 1;
        }
    }

    fn advance_fade(&mut self) {
        let Some(fade) = self.fade.as_mut() else {
            return;
        };
        if !fade.started {
            let seq = self.shared.current_seq();
            let appended = self.shared.appended_seq.load(Ordering::SeqCst);
            if seq > 0 && appended == seq {
                fade.started = true;
            }
        }
        if !fade.started {
            return;
        }

        fade.elapsed += TICK_MS as f32;
        let progress = if fade.ms <= 0.0 {
            1.0
        } else {
            (fade.elapsed / fade.ms).min(1.0)
        };
        self.volume = fade.from + (fade.to - fade.from) * progress;
        let done = progress >= 1.0;
        if done {
            self.volume = fade.to;
            self.fade = None;
        }
        self.apply_volume();
    }
}

impl Shared {
    fn normalize_flag(&self) -> Arc<AtomicBool> {
        self.normalize.clone()
    }
}

fn audio_thread(rx: Receiver<Cmd>, shared: Arc<Shared>) {
    let mut engine = Engine::new(shared);
    loop {
        match rx.recv_timeout(Duration::from_millis(TICK_MS)) {
            Ok(cmd) => engine.handle(cmd),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(cmd) = rx.try_recv() {
            engine.handle(cmd);
        }
        engine.tick();
    }
}

fn spawn_seek_worker(player: Arc<Player>) -> Sender<Duration> {
    let (tx, rx) = mpsc::channel::<Duration>();
    std::thread::Builder::new()
        .name("ytrium-seek".to_string())
        .spawn(move || {
            while let Ok(first) = rx.recv() {
                // Coalesce bursts: only the last requested position matters.
                let mut target = first;
                while let Ok(next) = rx.try_recv() {
                    target = next;
                }
                if let Err(err) = player.try_seek(target) {
                    eprintln!("[audio] seek failed: {err}");
                }
            }
        })
        .expect("could not start the seek thread");
    tx
}

#[allow(clippy::too_many_arguments)]
fn spawn_loader(
    shared: Arc<Shared>,
    player: Arc<Player>,
    normalize: Arc<AtomicBool>,
    append_lock: Arc<Mutex<()>>,
    seek_tx: Sender<Duration>,
    seq: u64,
    url: String,
    video_id: String,
    start_at: f64,
    autoplay: bool,
) {
    let builder = std::thread::Builder::new().name("ytrium-decode".to_string());
    let spawned = {
        let shared = shared.clone();
        builder.spawn(move || {
        let opened = open_stream(&url, &video_id);
        let (decoder, control, duration) = match opened {
            Ok(parts) => parts,
            Err(err) => {
                shared.fail(seq, err.to_string());
                return;
            }
        };

        // Publish the control handle for cancellation, unless a newer load has
        // already claimed the engine.
        if !shared.set_reader(seq, control.clone()) {
            control.cancel();
            return;
        }

        let guard = append_lock.lock().expect("append lock poisoned");
        if shared.current_seq() != seq {
            drop(guard);
            control.cancel();
            return;
        }

        let source = Normalized::new(decoder, normalize);
        player.append(source);
        drop(guard);

        shared.appended_seq.store(seq, Ordering::SeqCst);
        {
            let mut status = shared.status.lock().expect("status poisoned");
            status.duration = duration;
            status.position = start_at.max(0.0);
            status.state = if autoplay {
                Transport::Playing
            } else {
                Transport::Paused
            };
        }

        if start_at > 0.0 {
            let _ = seek_tx.send(Duration::from_secs_f64(start_at));
        }
        if autoplay {
            player.play();
        } else {
            player.pause();
        }
        })
    };

    if let Err(err) = spawned {
        shared.fail(seq, format!("could not start the decode thread: {err}"));
    }
}
/// Download the stream and build the decoder (runs on the loader thread).
pub(crate) fn open_stream(
    url: &str,
    video_id: &str,
) -> ApiResult<(Decoder<RangeReader>, ReaderControl, f64)> {
    let reader = RangeReader::open(url, video_id)?;
    let control = reader.control();
    let byte_len = reader.total_len();

    let mut builder = Decoder::builder()
        .with_data(reader)
        .with_hint("m4a")
        .with_seekable(true);
    if let Some(len) = byte_len {
        builder = builder.with_byte_len(len);
    }

    let decoder = builder.build().map_err(|err| {
        crate::error::ApiError::extraction_failed(Some(format!(
            "could not decode the audio stream: {err}"
        )))
    })?;
    let duration = decoder
        .total_duration()
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    Ok((decoder, control, duration))
}
