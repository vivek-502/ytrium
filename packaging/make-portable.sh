#!/usr/bin/env bash
# Build the portable Linux tarball for Ytrium.
#
#   bash packaging/make-portable.sh                       # host architecture
#   bash packaging/make-portable.sh --target aarch64-unknown-linux-gnu
#
# The tarball is self-contained - the app binary, its icon, an example desktop
# entry and a small helper that installs that entry - and needs no installer or
# root: unpack it anywhere and run ./ytrium. It is what the Release workflow
# uploads as ytrium-linux-<arch>.tar.gz.
#
# The binary still needs `yt-dlp` on PATH (and ffmpeg recommended), exactly like
# every other Ytrium install.
set -euo pipefail

TARGET=""
OUT_DIR="packaging/dist"
while [ $# -gt 0 ]; do
  case "$1" in
    --target)
      TARGET="${2:?--target needs a triple}"
      shift 2
      ;;
    --out-dir)
      OUT_DIR="${2:?--out-dir needs a path}"
      shift 2
      ;;
    -h | --help)
      sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# ---------------------------------------------------------------- arch naming
case "${TARGET:-$(rustc -vV | awk '/^host:/{print $2}')}" in
  x86_64-unknown-linux-*) ARCH="x64" ;;
  aarch64-unknown-linux-*) ARCH="arm64" ;;
  *) echo "unsupported target for a Linux bundle: ${TARGET:-$(rustc -vV | awk '/^host:/{print $2}')}" >&2; exit 1 ;;
esac

# --------------------------------------------------------------- build binary
TARGET_ARGS=()
if [ -n "$TARGET" ]; then
  TARGET_ARGS=(--target "$TARGET")
fi

echo "==> building the release binary (${ARCH})"
npm run --silent build
npm exec -- tauri build --no-bundle "${TARGET_ARGS[@]}"

if [ -n "$TARGET" ]; then
  BIN="src-tauri/target/${TARGET}/release/ytrium"
else
  BIN="src-tauri/target/release/ytrium"
fi
[ -x "$BIN" ] || { echo "build did not produce $BIN" >&2; exit 1; }

# ------------------------------------------------------------------- assemble
NAME="ytrium-linux-${ARCH}"
# Stage under a subdirectory so that only the tarball itself lands in $OUT_DIR:
# a sibling "ytrium-linux-x64/" directory next to "ytrium-linux-x64.tar.gz"
# makes release-upload globs ambiguous.
STAGE="$OUT_DIR/stage/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE"

install -m 0755 "$BIN" "$STAGE/ytrium"
install -m 0644 packaging/ytrium.png "$STAGE/ytrium.png"
cat >"$STAGE/ytrium.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Ytrium
GenericName=Music Player
Comment=Local-first music player
Exec=ytrium %U
Icon=ytrium
Terminal=false
Categories=AudioVideo;Audio;Player;Music;
Keywords=music;player;audio;streaming;local;ytrium;
StartupWMClass=ytrium
StartupNotify=true
EOF
install -m 0755 packaging/portable-install-desktop-entry.sh "$STAGE/install-desktop-entry.sh"
printf 'Ytrium %s portable (linux-%s)\n\nRun ./ytrium - no installer and no root required.\nThe app needs `yt-dlp` on PATH (ffmpeg recommended).\n' \
  "$(node -p "require('./package.json').version")" "$ARCH" >"$STAGE/README.txt"

# --------------------------------------------------------------------- tarball
echo "==> packing ${NAME}.tar.gz"
tar -C "$OUT_DIR/stage" -czf "$OUT_DIR/${NAME}.tar.gz" "$NAME"
sha256sum "$OUT_DIR/${NAME}.tar.gz" | sed "s#${OUT_DIR}/##" >"$OUT_DIR/${NAME}.tar.gz.sha256"
rm -rf "$OUT_DIR/stage"

echo
echo "==> done"
ls -lh "$OUT_DIR/${NAME}.tar.gz" "$OUT_DIR/${NAME}.tar.gz.sha256"
