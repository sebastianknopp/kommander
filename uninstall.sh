#!/usr/bin/env bash
# Removes what install.sh installed, and nothing else. Use the same PREFIX
# (if any) as for installing. Your settings are left alone.
set -euo pipefail

APP_ID=io.github.sebastianknopp.Kommander

if [ -n "${PREFIX:-}" ]; then
    BIN_DIR="$PREFIX/bin"
    DATA_DIR="$PREFIX/share"
else
    BIN_DIR="$HOME/.local/bin"
    DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
fi
APPS_DIR="$DATA_DIR/applications"
ICON_DIR="$DATA_DIR/icons/hicolor"

FILES=(
    "$BIN_DIR/kommander"
    "$APPS_DIR/$APP_ID.desktop"
    "$ICON_DIR/scalable/apps/$APP_ID.svg"
    "$ICON_DIR/symbolic/apps/$APP_ID-symbolic.svg"
)
removed=0
for file in "${FILES[@]}"; do
    if [ -e "$file" ]; then
        rm -f "$file"
        echo "Removed $file"
        removed=1
    fi
done
[ "$removed" = 1 ] || echo "Nothing to remove: Kommander was not installed there."

# Icon folders that are now empty go too (other apps' icons stay).
for dir in "$ICON_DIR/scalable/apps" "$ICON_DIR/scalable" \
    "$ICON_DIR/symbolic/apps" "$ICON_DIR/symbolic"; do
    [ -d "$dir" ] && rmdir --ignore-fail-on-non-empty "$dir"
done

# Same cache refresh as install.sh: only caches that already exist.
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
[ -d "$ICON_DIR" ] && touch "$ICON_DIR"

CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/kommander"
echo "Kommander is uninstalled."
echo "Your settings in $CONFIG/ were left alone."
echo "To remove them too: rm -r \"$CONFIG\""
