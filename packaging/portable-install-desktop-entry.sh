#!/usr/bin/env bash
# Add the Ytrium entry from a portable unpack to your app menu.
#
#   ./install-desktop-entry.sh              # user-wide (~/.local/share)
#   sudo ./install-desktop-entry.sh         # system-wide (/usr/local/share)
#   ./install-desktop-entry.sh --uninstall  # remove the entry again
#
# This only wires up the menu entry and icon - run ./ytrium to start the app.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_ID="com.ytrium.desktop"
UNINSTALL=0

for arg in "$@"; do
  case "$arg" in
    --uninstall) UNINSTALL=1 ;;
    -h | --help)
      sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown argument: $arg" >&2; exit 1 ;;
  esac
done

if [ "$(id -u)" -eq 0 ]; then
  PREFIX="/usr/local"
else
  PREFIX="$HOME/.local"
fi

BIN_DIR="$PREFIX/bin"
APP_DIR="$PREFIX/share/applications"
ICON_PATH="$PREFIX/share/icons/hicolor/256x256/apps/ytrium.png"
DESKTOP_PATH="$APP_DIR/ytrium.desktop"

if [ "$UNINSTALL" -eq 1 ]; then
  rm -f "$BIN_DIR/ytrium" "$DESKTOP_PATH" "$ICON_PATH"
  command -v update-desktop-database >/dev/null &&
    update-desktop-database "$APP_DIR" 2>/dev/null || true
  echo "Removed the Ytrium menu entry (your library at ~/.local/share/$APP_ID is kept)."
  exit 0
fi

install -d "$BIN_DIR" "$APP_DIR" "$(dirname "$ICON_PATH")"
# A symlink keeps the portable folder intact while the menu points at a PATH location.
ln -sfn "$HERE/ytrium" "$BIN_DIR/ytrium"
install -m 0644 "$HERE/ytrium.png" "$ICON_PATH"
sed "s#^Exec=.*#Exec=${BIN_DIR}/ytrium %U#" "$HERE/ytrium.desktop" >"$DESKTOP_PATH"
chmod 0644 "$DESKTOP_PATH"

command -v update-desktop-database >/dev/null &&
  update-desktop-database "$APP_DIR" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null &&
  gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true

echo "Installed the Ytrium menu entry:"
echo "  launcher: $BIN_DIR/ytrium  ->  $HERE/ytrium"
echo "  entry:    $DESKTOP_PATH"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "note: $BIN_DIR is not on your PATH - open Ytrium from the app menu instead." ;;
esac
