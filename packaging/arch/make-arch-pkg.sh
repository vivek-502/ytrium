#!/usr/bin/env bash
# Produce a pacman package for Ytrium from this repository (Arch Linux).
#
#   bash packaging/arch/make-arch-pkg.sh
#
# It stages the release binary plus the desktop entry and icon into a package
# layout, writes .PKGINFO, and bundles everything with makepkg --repackage.
# The result lands in packaging/dist/ and can be installed with pacman -U.
# Requires base-devel (the PKGBUILD's makedepends) and a built release binary
# (npm run app:build -- --no-bundle).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

PKGVER="$(node -p "require('./package.json').version")"
PKGREL=1
ARCH="$(uname -m)"
OUT_DIR="$ROOT/packaging/dist"
BIN="${ROOT}/src-tauri/target/release/ytrium"

[ -x "$BIN" ] || {
  echo "no release binary at src-tauri/target/release/ytrium - run: npm run app:build -- --no-bundle" >&2
  exit 1
}
command -v makepkg >/dev/null || { echo "makepkg is required (pacman -S base-devel)" >&2; exit 1; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/ytrium-arch.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
STAGE="$WORK/pkg/ytrium"
mkdir -p "$STAGE/usr/bin" "$STAGE/usr/share/applications" \
         "$STAGE/usr/share/icons/hicolor/256x256/apps"

install -m 0755 "$BIN" "$STAGE/usr/bin/ytrium"
install -m 0644 "$ROOT/packaging/ytrium.png" "$STAGE/usr/share/icons/hicolor/256x256/apps/ytrium.png"
install -m 0644 "$ROOT/packaging/ytrium.desktop" "$STAGE/usr/share/applications/ytrium.desktop"

SIZE="$(du -sb "$STAGE" | cut -f1)"
cat >"$WORK/pkg/.PKGINFO" <<EOF
pkgname = ytrium
pkgbase = ytrium
pkgver = ${PKGVER}-${PKGREL}
pkgdesc = Local-first desktop music player (Tauri 2 + Rust)
url = https://github.com/vivek-502/ytrium
builddate = $(date +%s)
packager = Ytrium
size = ${SIZE}
arch = ${ARCH}
license = MIT
depend = webkit2gtk-4.1
depend = gtk3
depend = yt-dlp
optdepend = ffmpeg: format remuxing for yt-dlp
EOF

# makepkg repackages an existing layout (the PKGBUILD is only a vehicle).
cp "$ROOT/packaging/arch/PKGBUILD" "$WORK/PKGBUILD"
mkdir -p "$OUT_DIR"
( cd "$WORK" && makepkg --repackage --noconfirm )
cp "$WORK"/ytrium-"${PKGVER}"-"${PKGREL}"-"${ARCH}".pkg.tar.* "$OUT_DIR/"
echo
echo "==> done"
ls -lh "$OUT_DIR"/ytrium-"${PKGVER}"-"${PKGREL}"-"${ARCH}".pkg.tar.*
