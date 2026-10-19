/**
 * Audio engine (desktop).
 *
 * All decoding and playback happens in Rust (rodio + symphonia); the browser
 * never sees an audio element. This class is the control surface the rest of
 * the app already knows: load/play/pause/seek/volume/fade/normalize plus the
 * same event callbacks as the web build.
 *
 * Rust owns the transport state, so a 4 Hz status poll translates it into the
 * callback vocabulary the player store expects (timeupdate / playing / paused /
 * waiting / ended / error).
 */
import { invoke } from "@tauri-apps/api/core";

export interface EngineHandlers {
  onTime?: (seconds: number) => void;
  onDuration?: (seconds: number) => void;
  onPlaying?: () => void;
  onPaused?: () => void;
  onWaiting?: () => void;
  onCanPlay?: () => void;
  onEnded?: () => void;
  onError?: (message: string) => void;
  onStalled?: () => void;
}

type TransportState = "idle" | "loading" | "playing" | "paused" | "ended" | "error";

interface PlayerStatus {
  state: TransportState;
  /** Seconds. */
  position: number;
  /** Seconds; 0 while unknown. */
  duration: number;
  error: string | null;
  errorSeq: number;
  endedSeq: number;
}

const POLL_MS = 250;
/** Ignore stale positions for this long after a seek request. */
const SEEK_QUIET_MS = 600;

/** `/api/stream/<videoId>?q=<quality>` -> transport arguments. */
function parseSrc(src: string): { videoId: string; quality: string } | null {
  const path = /\/api\/stream\/([^/?#]+)/.exec(src);
  if (!path) return null;
  let videoId = path[1];
  try {
    videoId = decodeURIComponent(videoId);
  } catch {
    /* keep raw */
  }
  const quality = /[?&]q=([^&#]+)/.exec(src);
  return {
    videoId,
    quality: quality ? decodeURIComponent(quality[1]) : "auto",
  };
}

function errorMessage(err: unknown): string {
  if (err instanceof Error && err.message) return err.message;
  if (err && typeof err === "object" && typeof (err as { message?: unknown }).message === "string") {
    return (err as { message: string }).message;
  }
  return "Unable to play this track right now.";
}

export class AudioEngine {
  private handlers: EngineHandlers = {};
  private currentSrc: string | null = null;
  private timer: number | null = null;

  /* Last transport snapshot, used for edge detection. */
  private lastState: TransportState = "idle";
  private lastErrorSeq = 0;
  private lastEndedSeq = 0;
  private lastDuration = 0;
  private lastSentTime = -1;
  private pollBusy = false;
  private seekQuietUntil = 0;

  constructor() {
    if (typeof window !== "undefined") {
      this.timer = window.setInterval(() => void this.poll(), POLL_MS);
    }
  }

  setHandlers(handlers: EngineHandlers): void {
    this.handlers = handlers;
  }

  /** The path currently loaded, or null. */
  get src(): string | null {
    return this.currentSrc;
  }

  /* ------------------------------- transport ------------------------------ */

  /**
   * Load a media path (`/api/stream/<videoId>`). Rust resolves the media URL,
   * buffers the start of the stream and (optionally) begins playback.
   */
  load(src: string, opts: { startAt?: number; autoplay?: boolean } = {}): void {
    const parsed = parseSrc(src);
    this.currentSrc = src;
    if (!parsed) {
      this.currentSrc = null;
      this.handlers.onError?.("This track has no playable source.");
      return;
    }
    const { startAt = 0, autoplay = true } = opts;
    this.seekQuietUntil = 0;
    this.lastSentTime = -1;
    invoke("audio_load", {
      videoId: parsed.videoId,
      quality: parsed.quality,
      startAt: Math.max(0, startAt),
      autoplay,
    }).catch((err) => this.handlers.onError?.(errorMessage(err)));
  }

  /** Re-request the current source (used for stream retries). */
  reload(opts: { startAt?: number; autoplay?: boolean } = {}): void {
    if (!this.currentSrc) return;
    this.load(this.currentSrc, opts);
  }

  async play(): Promise<void> {
    try {
      await invoke("audio_play");
    } catch (err) {
      this.handlers.onError?.(errorMessage(err));
    }
  }

  pause(): void {
    invoke("audio_pause").catch(() => {
      /* ignore */
    });
  }

  seek(seconds: number): void {
    const target = Math.max(0, seconds);
    this.seekQuietUntil = Date.now() + SEEK_QUIET_MS;
    this.lastSentTime = target;
    this.handlers.onTime?.(target);
    invoke("audio_seek", { seconds: target }).catch(() => {
      /* ignore */
    });
  }

  setVolume(value: number): void {
    const v = Math.min(1, Math.max(0, value));
    invoke("audio_set_volume", { value: v }).catch(() => {
      /* ignore */
    });
  }

  setMuted(muted: boolean): void {
    invoke("audio_set_muted", { muted }).catch(() => {
      /* ignore */
    });
  }

  /** Smoothly ramp the effective volume (used by track fades). */
  fadeTo(value: number, ms: number): void {
    const target = Math.min(1, Math.max(0, value));
    invoke("audio_fade", { value: target, ms: Math.max(1, Math.round(ms)) }).catch(() => {
      /* ignore */
    });
  }

  /**
   * Toggle Rust-side peak/RMS normalisation on the live output. Available at
   * any time: the limiter is part of the playback chain, not a re-route.
   */
  async setNormalize(enabled: boolean): Promise<void> {
    try {
      await invoke("audio_set_normalize", { enabled });
    } catch {
      /* noop */
    }
  }

  /** Kept for API parity with the web build (no WebAudio context here). */
  async resumeContext(): Promise<void> {
    /* noop */
  }

  /* --------------------------------- poll -------------------------------- */

  private async poll(): Promise<void> {
    if (this.pollBusy) return;
    this.pollBusy = true;
    try {
      const status = await invoke<PlayerStatus>("audio_status");
      this.apply(status);
    } catch {
      /* engine not up yet - keep polling */
    } finally {
      this.pollBusy = false;
    }
  }

  private apply(status: PlayerStatus): void {
    const h = this.handlers;

    if (status.errorSeq !== this.lastErrorSeq) {
      this.lastErrorSeq = status.errorSeq;
      if (status.error) h.onError?.(status.error);
    }

    if (status.endedSeq !== this.lastEndedSeq) {
      this.lastEndedSeq = status.endedSeq;
      this.lastState = "ended";
      h.onEnded?.();
    }

    if (status.state !== this.lastState) {
      if (status.state === "loading") {
        h.onWaiting?.();
      } else if (status.state === "playing") {
        h.onCanPlay?.();
        h.onPlaying?.();
      } else if (status.state === "paused" && this.lastState === "playing") {
        h.onPaused?.();
      } else if (status.state === "error") {
        h.onStalled?.();
      }
      this.lastState = status.state;
    }

    if (status.duration > 0 && Math.abs(status.duration - this.lastDuration) > 0.25) {
      this.lastDuration = status.duration;
      h.onDuration?.(status.duration);
    }

    const timeKnown = status.state === "playing" || status.state === "paused";
    if (timeKnown && Date.now() >= this.seekQuietUntil) {
      if (Math.abs(status.position - this.lastSentTime) > 0.001) {
        this.lastSentTime = status.position;
        h.onTime?.(status.position);
      }
    }
  }

  /** Stop the poll timer (only used by tests / teardown). */
  dispose(): void {
    if (this.timer !== null) {
      window.clearInterval(this.timer);
      this.timer = null;
    }
  }
}

/** Global singleton - survives navigation, React re-renders, and route changes. */
export const audioEngine = new AudioEngine();
