#!/usr/bin/env bash
# Builds zash and installs it for this user: ~/.local/bin/zash, the launcher
# entry and the icon. No root needed. Re-run to update.
set -euo pipefail
cd "$(dirname "$0")"
say() { printf '\033[1;34m::\033[0m %s\n' "$*"; }

say "building (release, low priority)"
nice -n 19 cargo build --release --quiet -j "${JOBS:-4}"
target="${CARGO_TARGET_DIR:-target}/release/zash"

install -Dm755 "$target" "$HOME/.local/bin/zash"
icons="$HOME/.local/share/icons/hicolor"
install -Dm644 packaging/zash.svg "$icons/scalable/apps/zash.svg"
install -Dm644 shell/zash-popup.plugin.zsh "$HOME/.local/share/zash/shell/zsh/zash-popup.plugin.zsh"
if command -v rsvg-convert >/dev/null; then
    for s in 16 24 32 48 64 128 256; do
        install -dm755 "$icons/${s}x${s}/apps"
        rsvg-convert -w $s -h $s packaging/zash.svg -o "$icons/${s}x${s}/apps/zash.png"
    done
fi
# Exec as an absolute path: a session's PATH may not include ~/.local/bin
sed "s|^Exec=zash|Exec=$HOME/.local/bin/zash|" packaging/zash.desktop > "$HOME/.local/share/applications/zash.desktop"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -f -t "$icons" 2>/dev/null || true
say "installed: run 'zash', or zash in your launcher"
