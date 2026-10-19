#!/usr/bin/env bash
#
# Ytrium installer for Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/vivek-502/ytrium/master/scripts/install.sh | bash
#
# Downloads the portable build of Ytrium from the GitHub releases and installs it
# for the current user (no root needed):
#
#   ~/.local/bin/ytrium                                    the app
#   ~/.local/share/icons/hicolor/256x256/apps/ytrium.png   the icon
#   ~/.local/share/applications/ytrium.desktop             the app-menu entry
#
# Options (pass after `bash -s --`):
#   v1.0.0           install a specific version instead of the latest
#   --with-deps      install yt-dlp (and ffmpeg) with the system package manager
#   --uninstall      remove everything this script installed (your library is kept)
#   --purge          with --uninstall: also delete your library
#
set -euo pipefail

REPO="vivek-502/ytrium"
TARBALL="ytrium-linux-x64.tar.gz"
BIN_NAME="ytrium"
APP_ID="com.ytrium.desktop"

BIN_DIR="$HOME/.local/bin"
ICON_DIR="$HOME/.local/share/icons/hicolor/256x256/apps"
APP_DIR="$HOME/.local/share/applications"
BIN_PATH="$BIN_DIR/$BIN_NAME"
ICON_PATH="$ICON_DIR/$BIN_NAME.png"
DESKTOP_PATH="$APP_DIR/$BIN_NAME.desktop"
DATA_DIR="$HOME/.local/share/$APP_ID"

say()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

WITH_DEPS=0
PURGE=0
VERSION="latest"
for arg in "$@"; do
  case "$arg" in
    --with-deps) WITH_DEPS=1 ;;
    --purge) PURGE=1 ;;
    --uninstall) VERSION="__uninstall__" ;;
    -h | --help)
      sed -n '3,17p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    -*) die "unknown option: $arg (see --help)" ;;
    *) VERSION="$arg" ;;
  esac
done

# ---------------------------------------------------------------- uninstall
if [ "$VERSION" = "__uninstall__" ]; then
  removed=0
  for f in "$BIN_PATH" "$ICON_PATH" "$DESKTOP_PATH"; do
    if [ -e "$f" ]; then rm -f "$f"; say "removed $f"; removed=1; fi
  done
  command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database "$APP_DIR" 2>/dev/null || true
  [ "$removed" -eq 1 ] || say "nothing to remove — Ytrium does not appear to be installed"
  if [ "$PURGE" -eq 1 ] && [ -d "$DATA_DIR" ]; then
    rm -rf "$DATA_DIR"
    say "deleted your library at $DATA_DIR"
  elif [ -d "$DATA_DIR" ]; then
    say "your library is kept at $DATA_DIR (pass --purge to delete it)"
  fi
  exit 0
fi

# ------------------------------------------------------------- preflight
[ "$(uname -s)" = "Linux" ] || die "this installer supports Linux only — on Windows use the .exe installer from https://github.com/$REPO/releases"

# The one place the package manager actually matters: yt-dlp (and ffmpeg) must be
# on the machine, or nothing can be searched or played. Note that yt-dlp is
# deliberately NOT auto-installed here: distro packages go stale for YouTube, and
# which route you want (distro, pipx, winget, self-updating binary) is your call.
install_deps() {
  local pm="" yes=""
  if   command -v pacman  >/dev/null 2>&1; then pm="pacman";  yes="--needed --noconfirm"
  elif command -v apt-get >/dev/null 2>&1; then pm="apt-get"; yes="-y"
  elif command -v dnf     >/dev/null 2>&1; then pm="dnf";     yes="-y"
  elif command -v zypper  >/dev/null 2>&1; then pm="zypper";  yes="--non-interactive"
  fi
  if [ -z "$pm" ]; then
    warn "unknown package manager — install yt-dlp and ffmpeg yourself"
    return 0
  fi
  say "installing yt-dlp and ffmpeg with $pm"
  if [ "$(id -u)" -eq 0 ]; then
    "$pm" $yes install yt-dlp ffmpeg
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$pm" $yes install yt-dlp ffmpeg
  elif command -v pkexec >/dev/null 2>&1; then
    pkexec "$pm" $yes install yt-dlp ffmpeg
  else
    warn "cannot elevate to root (no sudo/pkexec) — install yt-dlp and ffmpeg yourself"
  fi
}

case "$(uname -m)" in
  x86_64 | amd64) ;;
  *) die "no prebuilt Ytrium for $(uname -m) — build from source instead (see README)" ;;
esac

for cmd in curl tar; do
  command -v "$cmd" >/dev/null 2>&1 || die "'$cmd' is required. Install it first:
    Debian/Ubuntu : sudo apt install $cmd
    Fedora        : sudo dnf install $cmd
    Arch          : sudo pacman -S $cmd"
done

# WebKitGTK 4.1 is the one real runtime dependency of a Tauri app on Linux.
# (grep on a variable, not a pipe: `pipefail` + `grep -q` racing ldconfig's
# SIGPIPE produces false negatives.)
if command -v ldconfig >/dev/null 2>&1; then
  ld_out="$(ldconfig -p 2>/dev/null || :)"
  if ! grep -q 'libwebkit2gtk-4\.1' <<<"$ld_out"; then
    warn "the WebKitGTK 4.1 runtime was not found — Ytrium will not start without it:"
    warn "    Debian/Ubuntu : sudo apt install libwebkit2gtk-4.1-0"
    warn "    Fedora        : sudo dnf install webkit2gtk4.1"
    warn "    Arch          : sudo pacman -S webkit2gtk-4.1"
  fi
fi

if [ "$WITH_DEPS" -eq 1 ]; then
  install_deps
elif ! command -v yt-dlp >/dev/null 2>&1; then
  warn "yt-dlp was not found — Ytrium uses it to search and play, and will report a"
  warn "provider error until it is installed:"
  warn "    Arch          : sudo pacman -S yt-dlp ffmpeg"
  warn "    Debian/Ubuntu : sudo apt install yt-dlp ffmpeg"
  warn "    Fedora        : sudo dnf install yt-dlp ffmpeg"
  warn "    any distro    : pipx install yt-dlp   (or https://github.com/yt-dlp/yt-dlp#installation)"
  warn "  or re-run this installer with --with-deps to do it now."
fi

# ------------------------------------------------------------- download
if [ "$VERSION" = "latest" ]; then
  URL="https://github.com/$REPO/releases/latest/download/$TARBALL"
else
  URL="https://github.com/$REPO/releases/download/v${VERSION#v}/$TARBALL"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
say "downloading $URL"
curl -fSL --progress-bar -o "$TMP/$TARBALL" "$URL" ||
  die "download failed — check the version tag at https://github.com/$REPO/releases"

tar -xzf "$TMP/$TARBALL" -C "$TMP"

# Archives have shipped both flat and wrapped in a top-level folder — find the
# binary wherever it lives instead of assuming.
BIN_IN_TAR="$(find "$TMP" -maxdepth 3 -type f -name "$BIN_NAME" | head -n1)"
[ -n "$BIN_IN_TAR" ] || die "unexpected archive layout: no '$BIN_NAME' inside the tarball:
$(find "$TMP" -maxdepth 2 | sed 's/^/    /')"
PAYLOAD="$(dirname "$BIN_IN_TAR")"

# -------------------------------------------------------------- install
mkdir -p "$BIN_DIR" "$ICON_DIR" "$APP_DIR"
install -m 0755 "$PAYLOAD/$BIN_NAME" "$BIN_PATH"
if [ -f "$PAYLOAD/$BIN_NAME.png" ]; then
  install -m 0644 "$PAYLOAD/$BIN_NAME.png" "$ICON_PATH"
fi

cat >"$DESKTOP_PATH" <<EOF
[Desktop Entry]
Type=Application
Name=Ytrium
GenericName=Music Player
Comment=Local-first music player
Exec=$BIN_PATH %U
Icon=$BIN_NAME
Terminal=false
StartupNotify=true
StartupWMClass=$BIN_NAME
Categories=AudioVideo;Audio;Player;Music;
Keywords=music;player;audio;streaming;local;ytrium;
EOF
chmod 0644 "$DESKTOP_PATH"

command -v update-desktop-database >/dev/null 2>&1 &&
  update-desktop-database "$APP_DIR" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 &&
  gtk-update-icon-cache -q -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

# --------------------------------------------------------------- finish
say "Ytrium installed to $BIN_PATH"
# `case` instead of a pipe: grep -q can exit on the first PATH entry and the same
# pipefail/SIGPIPE race would print a bogus warning.
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)  warn "$BIN_DIR is not on your PATH — add this to your shell profile:"
      warn "    export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
esac
if command -v yt-dlp >/dev/null 2>&1; then
  say "yt-dlp: $(yt-dlp --version 2>/dev/null || echo 'present')"
fi
say "Launch with:  $BIN_NAME     (or find 'Ytrium' in your app menu)"
say "Uninstall:    curl -fsSL https://raw.githubusercontent.com/$REPO/master/scripts/install.sh | bash -s -- --uninstall"
