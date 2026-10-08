#!/bin/bash
# Headless check that typing a whole word into Home's search field keeps working
# (the field once lost the keyboard after the first letter): types "hello" in one
# go with xdotool, then looks at the screenshot: the typed text must be as wide
# as five letters, and a result card for the test app must be on screen.
# Runs in the dev container, on a fixture installation, offline:
#   scripts/dev.sh scripts/smoke-search.sh [/work/cmake/dev/telamon-store]
# (build the app first, see CLAUDE.md). Exit status 0 = pass. Screenshot:
# /work/shots/search/typed.png
set -euo pipefail
bin=${1:-/work/cmake/dev/telamon-store}
base=/work/flatpak/smoke-search
shots=/work/shots/search
home=/work/home/smoke-search
/src/scripts/test-remote.sh build "$base" >/dev/null 2>&1
. "$base/env"
export HOME=$home XDG_CONFIG_HOME=$home/.config XDG_CACHE_HOME=$home/.cache \
    XDG_STATE_HOME=$home/.local/state XDG_DATA_HOME=$home/.local/share \
    XDG_RUNTIME_DIR=/work/run/smoke-search
mkdir -p "$HOME" "$XDG_RUNTIME_DIR" "$shots"
chmod 700 "$XDG_RUNTIME_DIR"
flatpak --user remote-add --if-not-exists --gpg-import="$base/key.gpg" test "file://$base/repo/"
flatpak --user update --appstream test >/dev/null 2>&1 || true

inner=$XDG_RUNTIME_DIR/inner.sh
cat >"$inner" <<INNER
#!/bin/bash
QT_QPA_PLATFORM=xcb "$bin" --page home >"$shots/app.log" 2>&1 &
pid=\$!
for _ in \$(seq 40); do
    xdotool search --onlyvisible --name Telamon >/dev/null 2>&1 && break
    sleep 0.25
done
xdotool search --onlyvisible --name Telamon windowsize 1152 756
sleep 4
xdotool mousemove 690 88 click 1
sleep 1
xdotool type --delay 120 hello
sleep 3
import -window root "$shots/typed.png"
kill \$pid 2>/dev/null || true
INNER
chmod +x "$inner"
xvfb-run -a -s "-screen 0 1400x900x24" dbus-run-session -- "$inner" >/dev/null 2>&1

# The text in the field: its dark pixels' width (one letter is about 6 px).
width=$(magick "$shots/typed.png" -crop 800x20+255+48 +repage -colorspace Gray \
    -threshold 45% -negate -trim -format '%w' info:)
# A result card: the grid's first cell is not the page background there.
card=$(magick "$shots/typed.png" -crop 1x1+400+126 +repage -format '%[hex:u.p{0,0}]' info:)
bg=$(magick "$shots/typed.png" -crop 1x1+700+400 +repage -format '%[hex:u.p{0,0}]' info:)
echo "typed text width: ${width}px, card pixel $card, background pixel $bg"
if [ "${width:-0}" -lt 25 ]; then
    echo "FAIL: the field holds only the first letter(s)" >&2
    exit 1
fi
if [ "$card" = "$bg" ]; then
    echo "FAIL: no result card on screen" >&2
    exit 1
fi
echo PASS
