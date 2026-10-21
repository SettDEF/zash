#!/usr/bin/env bash
# visual.sh: what zash draws, compared with kept images (tests/visual): each case's bytes rendered
# headless at a frozen effects clock (ZASH_FX_AT), with zash's built-in look (no user config, no
# theme file), against tests/visual/<name>.png.
#   scripts/visual.sh            check every case (exit 1 when one differs)
#   scripts/visual.sh --update   keep the new images (after a change that is meant to look different)
#   ZASH_BIN=path                the binary (default: the release build)
set -uo pipefail
cd "$(dirname "$0")/.."
dir=tests/visual
ZASH_BIN=${ZASH_BIN:-${CARGO_TARGET_DIR:-/mnt/storage/dev-cache/targets}/release/zash}
update=0
[ "${1:-}" = --update ] && update=1
home=$(mktemp -d)
trap 'rm -rf "$home"' EXIT
: > "$home/zash.toml"
fail=0
while read -r name bytes size at _; do
    case "$name" in ''|'#'*) continue ;; esac
    cols=${size%x*} rows=${size#*x}
    out="$home/$name.png"
    # the bytes, then a pause: the snapshot is taken while the program still runs
    HOME=$home ZASH_CONFIG=$home/zash.toml ZASH_FX_AT=$at env -u ZEIT -u ZEIT_PANE \
        "$ZASH_BIN" --snapshot "$out" --size "${cols}x${rows}" --wait 700 -- sh -c "cat '$PWD/$dir/$bytes'; sleep 3" >/dev/null 2>&1 < /dev/null
    if [ ! -s "$out" ]; then echo "✗ $name: nothing rendered"; fail=1; continue; fi
    keep="$dir/$name.png"
    if [ $update = 1 ] || [ ! -f "$keep" ]; then
        cp -f "$out" "$keep"; echo "• $name: kept"
        continue
    fi
    # pixels that differ by more than a GPU's rounding (4 %)
    diff=$(magick compare -metric AE -fuzz 4% "$keep" "$out" null: 2>&1 | awk '{print int($1)}')
    if [ "${diff:-1}" -le 5 ]; then echo "✓ $name"; else echo "✗ $name: $diff pixels differ (see $dir/$name.new.png)"; cp -f "$out" "$dir/$name.new.png"; fail=1; fi
done < "$dir/cases"
exit $fail
