//! Gentle volume levelling, mirroring the web build's `DynamicsCompressor`
//! settings (threshold -26 dB, knee 24 dB, ratio 4:1, 4 ms attack, 220 ms
//! release). The web build routed the media element through a WebAudio
//! compressor; here the compressor is a `Source` wrapper in front of the
//! decoder, before rodio's volume gain so the user's volume control still
//! behaves exactly as before.
//!
//! It is a no-op until enabled, and it ramps back to unity when disabled so
//! toggling the setting never clicks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, Sample, SampleRate, Source};

const THRESHOLD_DB: f32 = -26.0;
const KNEE_DB: f32 = 24.0;
const RATIO: f32 = 4.0;
const ATTACK_S: f32 = 0.004;
const RELEASE_S: f32 = 0.22;
/// Frames needed for the gain to glide back to unity once disabled.
const DISABLE_COEFF: f32 = 0.05;

pub struct Normalized<S> {
    inner: S,
    enabled: Arc<AtomicBool>,
    channels: u16,
    sample_rate: u32,
    /// Channel index inside the current frame.
    frame_pos: u16,
    /// Peak of the frame currently being produced.
    frame_peak: f32,
    /// Applied gain in dB (0 = unity).
    gain_db: f32,
}

impl<S: Source> Normalized<S> {
    pub fn new(inner: S, enabled: Arc<AtomicBool>) -> Self {
        let channels = inner.channels().get();
        let sample_rate = inner.sample_rate().get();
        Self {
            inner,
            enabled,
            channels: channels.max(1),
            sample_rate: sample_rate.max(1),
            frame_pos: 0,
            frame_peak: 0.0,
            gain_db: 0.0,
        }
    }

    #[inline]
    fn current_gain(&self) -> f32 {
        if self.gain_db == 0.0 {
            1.0
        } else {
            10f32.powf(self.gain_db / 20.0)
        }
    }

    /// Update the smoothed gain from the finished frame's peak level.
    fn advance(&mut self, peak: f32) {
        if !self.enabled.load(Ordering::Relaxed) {
            self.gain_db += (0.0 - self.gain_db) * DISABLE_COEFF;
            if self.gain_db.abs() < 0.01 {
                self.gain_db = 0.0;
            }
            return;
        }

        let level_db = 20.0 * peak.max(1e-9).log10();
        let over = level_db - THRESHOLD_DB;
        let reduction_db = if over <= -KNEE_DB / 2.0 {
            0.0
        } else if over >= KNEE_DB / 2.0 {
            over * (1.0 - 1.0 / RATIO)
        } else {
            // Quadratic soft knee between the two breakpoints.
            let x = over + KNEE_DB / 2.0;
            (1.0 - 1.0 / RATIO) * x * x / (2.0 * KNEE_DB)
        };
        let target = -reduction_db;

        // Attack applies while gain is falling, release while it rises.
        let time = if target < self.gain_db {
            ATTACK_S
        } else {
            RELEASE_S
        };
        let coeff = 1.0 - (-1.0 / (time * self.sample_rate as f32)).exp();
        self.gain_db += (target - self.gain_db) * coeff;
    }
}

impl<S: Source> Iterator for Normalized<S> {
    type Item = Sample;

    #[inline]
    fn next(&mut self) -> Option<Sample> {
        let sample = self.inner.next()?;
        let gain = self.current_gain();

        let magnitude = sample.abs();
        if magnitude > self.frame_peak {
            self.frame_peak = magnitude;
        }
        self.frame_pos += 1;
        if self.frame_pos >= self.channels {
            self.frame_pos = 0;
            let peak = self.frame_peak;
            self.frame_peak = 0.0;
            self.advance(peak);
        }

        Some(sample * gain)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S: Source> Source for Normalized<S> {
    #[inline]
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    #[inline]
    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    #[inline]
    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    #[inline]
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    #[inline]
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.inner.try_seek(pos)
    }
}
