//! Buffered, seekable media reader.
//!
//! The web build proxied byte ranges over HTTP and let the browser's media
//! element do its own buffering. Here the decoder pulls from this reader
//! instead, so the network work is moved off the audio path: a background
//! thread streams the whole response into memory and `read`/`seek` serve from
//! that buffer. Reads only block when the download genuinely falls behind.
//!
//! A 403/410 answer means YouTube rejected the resolved URL. The provider
//! cache entry is dropped so the client's retry loop re-resolves via yt-dlp,
//! which is exactly what the web build did when the proxy saw the same status.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::error::{ApiError, ApiResult};
use crate::http;
use crate::providers::youtube;

/// Hard cap on buffered media. Real audio tracks are a few megabytes; this
/// only exists so a bogus/malicious response cannot exhaust memory.
const MAX_BUFFER: usize = 256 * 1024 * 1024;
/// Bytes to gather before handing the reader to the decoder.
const PREFETCH_BYTES: usize = 256 * 1024;
/// Upper bound on that initial wait; playback may still start sooner.
const PREFETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest single block inside `read` before the cancel flag is re-checked.
const READ_WAIT: Duration = Duration::from_millis(200);

struct Buffer {
    data: Vec<u8>,
    /// No more bytes will arrive.
    done: bool,
    /// Set when the stream stopped early.
    error: Option<String>,
}

pub struct ReaderState {
    buf: Mutex<Buffer>,
    cv: Condvar,
    cancelled: AtomicBool,
    /// The media URL was rejected, so the caller must re-resolve.
    stale_url: AtomicBool,
    /// Total media size when the origin reported it.
    total: Mutex<Option<u64>>,
}

impl ReaderState {
    fn mark_done(&self, error: Option<String>) {
        let mut buf = self.buf.lock().expect("reader buffer poisoned");
        buf.done = true;
        if buf.error.is_none() {
            buf.error = error;
        }
        drop(buf);
        self.cv.notify_all();
    }
}

/// A `Read + Seek` view of a media response, backed by a growing buffer.
pub struct RangeReader {
    state: Arc<ReaderState>,
    pos: u64,
}

/// Cheap, shareable handle used by the engine to cancel a reader that the
/// decoder now owns, or to notice that its stream died.
#[derive(Clone)]
pub struct ReaderControl {
    state: Arc<ReaderState>,
}

impl ReaderControl {
    /// Ask the downloader to stop; blocked reads return an error promptly.
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::SeqCst);
        self.state.cv.notify_all();
    }

    /// Stream failure that happened after playback started (`None` = healthy).
    pub fn stream_error(&self) -> Option<String> {
        self.state
            .buf
            .lock()
            .expect("reader buffer poisoned")
            .error
            .clone()
    }
}

impl RangeReader {
    /// Start downloading `url`. Returns once a usable amount of the stream has
    /// been buffered, so the decoder can parse container headers immediately.
    pub fn open(url: &str, video_id: &str) -> ApiResult<RangeReader> {
        let res = http::media_agent()
            .get(url)
            .header("range", "bytes=0-")
            .header("accept", "*/*")
            .call()
            .map_err(|err| {
                ApiError::extraction_failed(Some(format!("media request failed: {err}")))
            })?;

        let status = res.status().as_u16();
        if status == 403 || status == 410 {
            // The URL is no longer valid: drop it so the next attempt resolves
            // a fresh one, and tell the client to retry.
            youtube::invalidate_stream(video_id);
            return Err(ApiError::video_unavailable(Some(format!(
                "media url rejected with HTTP {status}"
            ))));
        }
        if !(200..300).contains(&status) {
            return Err(ApiError::extraction_failed(Some(format!(
                "media request answered HTTP {status}"
            ))));
        }

        let total = res
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .or_else(|| {
                res.headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.rsplit('/').next())
                    .filter(|t| t.chars().all(|c| c.is_ascii_digit()))
            })
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0);

        let body = res.into_body().into_reader();
        let state = Arc::new(ReaderState {
            buf: Mutex::new(Buffer {
                data: Vec::with_capacity(PREFETCH_BYTES * 2),
                done: false,
                error: None,
            }),
            cv: Condvar::new(),
            cancelled: AtomicBool::new(false),
            stale_url: AtomicBool::new(false),
            total: Mutex::new(total),
        });

        let worker = state.clone();
        let owned_url = url.to_string();
        let owned_id = video_id.to_string();
        std::thread::Builder::new()
            .name("ytrium-media".to_string())
            .spawn(move || download_loop(worker, body, owned_url, owned_id))
            .map_err(|err| {
                ApiError::internal(Some(format!("could not start media reader: {err}")))
            })?;

        let reader = RangeReader { state, pos: 0 };
        reader.wait_for_prefetch()?;
        Ok(reader)
    }

    /// Block until enough data (or the end of the stream) has arrived.
    fn wait_for_prefetch(&self) -> ApiResult<()> {
        let deadline = Instant::now() + PREFETCH_TIMEOUT;
        let mut buf = self.state.buf.lock().expect("reader buffer poisoned");
        while buf.data.len() < PREFETCH_BYTES && !buf.done {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (guard, _) = self
                .state
                .cv
                .wait_timeout(buf, deadline - now)
                .expect("reader buffer poisoned");
            buf = guard;
        }
        if buf.data.is_empty() && buf.done {
            let message = buf
                .error
                .clone()
                .unwrap_or_else(|| "media stream returned no data".to_string());
            return Err(ApiError::extraction_failed(Some(message)));
        }
        Ok(())
    }

    /// A shareable control handle for this reader.
    pub fn control(&self) -> ReaderControl {
        ReaderControl {
            state: self.state.clone(),
        }
    }

    /// Total size once known.
    pub fn total_len(&self) -> Option<u64> {
        *self.state.total.lock().expect("reader total poisoned")
    }
}

fn download_loop<B: Read>(state: Arc<ReaderState>, mut body: B, url: String, video_id: String) {
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        if state.cancelled.load(Ordering::SeqCst) {
            break;
        }
        match body.read(&mut chunk) {
            Ok(0) => {
                state.mark_done(None);
                break;
            }
            Ok(n) => {
                let over_capacity = {
                    let mut buf = state.buf.lock().expect("reader buffer poisoned");
                    if buf.data.len() + n > MAX_BUFFER {
                        true
                    } else {
                        buf.data.extend_from_slice(&chunk[..n]);
                        false
                    }
                };
                state.cv.notify_all();
                if over_capacity {
                    state.mark_done(Some("media stream exceeded buffer limit".to_string()));
                    break;
                }
            }
            Err(err) => {
                // Mid-stream failure: keep what we have, but flag the URL so a
                // retry re-resolves (an expired URL surfaces exactly like this
                // once the headers are already gone).
                state.stale_url.store(true, Ordering::SeqCst);
                youtube::invalidate_stream(&video_id);
                state.mark_done(Some(format!("media stream interrupted: {err}")));
                let _ = url;
                break;
            }
        }
    }
    state.cv.notify_all();
}

impl Read for RangeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let mut buf = self.state.buf.lock().expect("reader buffer poisoned");
        loop {
            if self.state.cancelled.load(Ordering::SeqCst) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "read cancelled"));
            }
            let available = buf.data.len() as u64;
            if self.pos < available {
                let start = self.pos as usize;
                let n = std::cmp::min(out.len(), buf.data.len() - start);
                out[..n].copy_from_slice(&buf.data[start..start + n]);
                self.pos += n as u64;
                return Ok(n);
            }
            if buf.done {
                // End of stream (or the download stopped early; the engine
                // notices that separately and reports it).
                return Ok(0);
            }
            let (guard, _) = self
                .state
                .cv
                .wait_timeout(buf, READ_WAIT)
                .expect("reader buffer poisoned");
            buf = guard;
        }
    }
}

impl Seek for RangeReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target: i64 = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.pos as i64 + n,
            SeekFrom::End(n) => {
                let total = self.total_len().or_else(|| {
                    // Unknown length: wait (briefly) for the download to finish
                    // so the end is well defined.
                    let deadline = Instant::now() + Duration::from_secs(2);
                    let mut buf = self.state.buf.lock().expect("reader buffer poisoned");
                    while !buf.done && Instant::now() < deadline {
                        let (guard, _) = self
                            .state
                            .cv
                            .wait_timeout(buf, Duration::from_millis(50))
                            .expect("reader buffer poisoned");
                        buf = guard;
                    }
                    Some(buf.data.len() as u64)
                });
                match total {
                    Some(total) => total as i64 + n,
                    None => return Err(io::Error::other("unknown stream length")),
                }
            }
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start of stream",
            ));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}
