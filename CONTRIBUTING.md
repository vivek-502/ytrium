# Contributing to Ytrium

Thanks for the interest. Ytrium is a small, opinionated project; the notes below are what
keeps it that way.

## Getting set up

```bash
npm install
npm run app:dev        # Vite + the Tauri window
```

You need **Node 20+**, **Rust 1.80+**, the
[Tauri 2 system dependencies](https://tauri.app/start/prerequisites/) for your platform, and
`yt-dlp` + `ffmpeg` on `PATH` (`ffmpeg` recommended) — the app drives `yt-dlp` for search and
stream resolution, and decodes audio itself with `symphonia`/`rodio`.

## Before you open a pull request

```bash
npm run typecheck                  # TypeScript
cargo clippy --all-targets         # Rust, from src-tauri/
cargo test --release -- --test-threads=1   # network/decode integration tests
npm run app:build                  # the whole thing actually bundles
```

The integration tests in `src-tauri/src/devcheck.rs` talk to the real providers (YouTube via
`yt-dlp`, iTunes, LRCLIB), so they need a working network and a current `yt-dlp`. There is also
an `#[ignore]`d live-audio test:

```bash
cargo test --release engine_plays -- --ignored --nocapture
```

## House rules

- **The UI never opens a socket.** Every provider call goes through Rust. All `/api/*` paths
  live in `src-tauri/src/api.rs`; keep the route names, error codes and response shapes the
  frontend already expects.
- **No new runtime dependencies in the frontend** unless there is no reasonable alternative.
  The design system is plain CSS custom properties in `src/styles/`.
- **Match the surrounding style.** Rust: `rustfmt` defaults, no `unwrap()` on user input paths.
  TypeScript: the existing store/hook patterns, no `any`.
- **Keep it local-first.** Nothing may leave the machine except the provider API calls the app
  already makes; media URLs are never written to disk.
- **Accessibility is not optional** — keyboard reachability, focus trapping and `aria` labels
  ship with the component, not in a later pass.

## Reporting a bug

Open an issue with what you did, what you expected, what happened, and your platform
(`uname -a` or the Windows version), app version and `yt-dlp --version`. Screenshots help.
