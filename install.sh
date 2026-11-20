#!/usr/bin/env bash
# Builds Kommander and installs it for the current user only (no sudo).
#
#   ./install.sh                 installs into ~/.local
#   PREFIX=/some/folder ./install.sh   installs into /some/folder instead
#
# What goes where:
#   the program    ~/.local/bin/kommander
#   menu entry     ~/.local/share/applications/io.github.sebastianknopp.kommander.desktop
#   icons          ~/.local/share/icons/hicolor/{scalable,symbolic}/apps/
# (~/.local/share follows $XDG_DATA_HOME if set; PREFIX overrides both.)
# Run ./uninstall.sh with the same PREFIX to remove it all again.
set -euo pipefail

APP_ID=io.github.sebastianknopp.kommander
cd "$(dirname "$0")"

if [ -n "${PREFIX:-}" ]; then
    BIN_DIR="$PREFIX/bin"
    DATA_DIR="$PREFIX/share"
else
    BIN_DIR="$HOME/.local/bin"
    DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
fi
APPS_DIR="$DATA_DIR/applications"
ICON_DIR="$DATA_DIR/icons/hicolor"

if ! command -v cargo >/dev/null; then
    echo "Rust is missing. Install it first:  sudo pacman -S --needed rust pkgconf" >&2
    exit 1
fi

echo "Building Kommander (this takes a few minutes the first time)..."
cargo build --release --quiet

install -Dm755 target/release/kommander "$BIN_DIR/kommander"
install -Dm644 "data/icons/hicolor/scalable/apps/$APP_ID.svg" \
    "$ICON_DIR/scalable/apps/$APP_ID.svg"
install -Dm644 "data/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg" \
    "$ICON_DIR/symbolic/apps/$APP_ID-symbolic.svg"

# The menu entry starts the program by its full path, so it works even if
# ~/.local/bin is not on the PATH.
mkdir -p "$APPS_DIR"
while IFS= read -r line; do
    # The arguments (" %U": folders to open) stay as they are.
    [[ $line == Exec=* ]] && line="Exec=\"$BIN_DIR/kommander\"${line#Exec=kommander}"
    printf '%s\n' "$line"
done <"data/$APP_ID.desktop" >"$APPS_DIR/$APP_ID.desktop"

# Let the app launcher and the icon theme notice. Only caches that already
# exist are refreshed; without one, they pick up the change by themselves.
if [ -f "$APPS_DIR/mimeinfo.cache" ] && command -v update-desktop-database >/dev/null; then
    update-desktop-database -q "$APPS_DIR" || true
fi
if [ -f "$ICON_DIR/icon-theme.cache" ]; then
    for tool in gtk4-update-icon-cache gtk-update-icon-cache; do
        if command -v "$tool" >/dev/null; then
            "$tool" -q -t -f "$ICON_DIR" || true
            break
        fi
    done
fi
touch "$ICON_DIR"

echo "Installed:"
echo "  $BIN_DIR/kommander"
echo "  $APPS_DIR/$APP_ID.desktop"
echo "  $ICON_DIR/scalable/apps/$APP_ID.svg (and the -symbolic one)"
echo "Start it from the app launcher (\"Kommander\") or run: $BIN_DIR/kommander"
