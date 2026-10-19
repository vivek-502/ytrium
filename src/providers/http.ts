/**
 * API client.
 *
 * The desktop build has no HTTP backend at all: the whole `/api/*` surface is
 * implemented in Rust and reached through a single Tauri command
 * (`api_request`). This module keeps the web build's tiny client shape - same
 * `get`/`post`/`delete` helpers, same query-param handling, same structured
 * `ApiError` - so every provider and page above it is unchanged.
 */
import { invoke } from "@tauri-apps/api/core";

export class ApiError extends Error {
  readonly code: string;
  readonly status: number;

  constructor(code: string, status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.code = code;
    this.status = status;
  }
}

/** Guard against a wedged backend promise; Rust enforces its own timeouts. */
const GUARD_SLACK_MS = 30_000;
const DEFAULT_TIMEOUT = 45_000;

interface RequestOptions {
  params?: Record<string, string | number | undefined | null>;
  signal?: AbortSignal;
  timeoutMs?: number;
}

function buildPath(path: string, params?: RequestOptions["params"]): string {
  if (!params) return path;
  const usp = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === null || v === "") continue;
    usp.set(k, String(v));
  }
  const qs = usp.toString();
  return qs ? `${path}${path.includes("?") ? "&" : "?"}${qs}` : path;
}

function defaultMessageForStatus(status: number): string {
  if (status === 429) return "Too many requests. Please slow down.";
  if (status === 404) return "Not found.";
  if (status >= 500) return "The player had a problem. Please try again.";
  return "Something went wrong. Please try again.";
}

/** Shape of the structured error Rust commands reject with. */
interface RawApiError {
  code?: string;
  status?: number;
  message?: string;
  detail?: string | null;
}

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
  opts: RequestOptions = {},
): Promise<T> {
  if (opts.signal?.aborted) throw new ApiError("ABORTED", 0, "The request was cancelled.");

  const guardMs = (opts.timeoutMs ?? DEFAULT_TIMEOUT) + GUARD_SLACK_MS;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const guard = new Promise<never>((_, reject) => {
    timer = setTimeout(
      () => reject(new ApiError("TIMEOUT", 0, "The request timed out. Please try again.")),
      guardMs,
    );
  });

  try {
    return await Promise.race([
      invoke<T>("api_request", {
        method,
        path: buildPath(path, opts.params),
        body: body ?? null,
      }),
      guard,
    ]);
  } catch (err) {
    if (err instanceof ApiError) throw err;
    const e = (err ?? {}) as RawApiError;
    const status = typeof e.status === "number" ? e.status : 0;
    const code = typeof e.code === "string" && e.code ? e.code : "IPC_ERROR";
    const message =
      typeof e.message === "string" && e.message
        ? e.message
        : status
          ? defaultMessageForStatus(status)
          : "The Ytrium engine is unavailable. Try restarting the app.";
    throw new ApiError(code, status, message);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

export const http = {
  get: <T>(path: string, opts?: RequestOptions) => request<T>("GET", path, undefined, opts),
  post: <T>(path: string, body?: unknown, opts?: RequestOptions) =>
    request<T>("POST", path, body, opts),
  delete: <T>(path: string, body?: unknown, opts?: RequestOptions) =>
    request<T>("DELETE", path, body, opts),
};
