#!/bin/bash
# Headless screenshots of the Telamon apps in the window, without GitHub: the
# Store is built with its `fake-github` feature and reads recorded answers from
# a folder (crates/telamon-store-core/examples/make_native_fake.rs makes it,
# and installs an older version of one app). Runs in the dev container:
#   scripts/dev.sh scripts/native-shots.sh [shots dir]
# Screenshots land in /work/shots/native (or the folder given):
#   home updates installed app-available install-dialog app-installed
#   update-dialog updated uninstall-dialog uninstalled local-dialog local-installed
# The clicks are at fixed places of a 1152x756 window (the Store's default
# size on a 1400x900 screen); a change to those pages may move them. Dialogs
# are answered with the keyboard (Cancel has the focus; Tab, then Return).
# Nothing here reaches the network, the real home or the real Flatpak.
set -euo pipefail
shots=${1:-/work/shots/native}
build=/work/cmake/fake
work=/work/native-shots
mkdir -p "$shots" "$work"

if [ -z "${SKIP_BUILD:-}" ]; then
    cmake -S /src/apps/telamon-store -B "$build" -G Ninja -DTELAMON_STORE_FAKE_GITHUB=ON >/dev/null 2>&1
    cmake --build "$build" >/dev/null 2>&1
fi
bin=$build/telamon-store

# A fresh world: fixtures, an older Gates installed, empty caches and an
# empty Flatpak installation.
fresh() {
    rm -rf "$work/world"
    mkdir -p "$work/world/fake" "$work/world/run"
    chmod 700 "$work/world/run"
    (cd /src && cargo run -q --example make_native_fake --features fake-github -p telamon-store-core -- \
        "$work/world/fake" "$work/world/home/.local/share" "$work/world/home")
}

# shot <scenario> <app args...>; the scenario is a function of the same name
# that holds the xdotool steps (`snap <name>`, `click <x> <y>`, `pause <s>`).
shot() {
    local scenario=$1
    shift
    local w=$work/world
    cat >"$w/inner.sh" <<INNER
#!/bin/bash
export HOME=$w/home XDG_CONFIG_HOME=$w/home/.config XDG_CACHE_HOME=$w/home/.cache \
    XDG_STATE_HOME=$w/home/.local/state XDG_DATA_HOME=$w/home/.local/share \
    XDG_RUNTIME_DIR=$w/run FLATPAK_USER_DIR=$w/flatpak-user \
    TELAMON_STORE_FAKE_GITHUB=$w/fake QT_QPA_PLATFORM=xcb
mkdir -p "\$HOME" "\$FLATPAK_USER_DIR"
flatpak --user remotes >/dev/null 2>&1 || true
"$bin" $* >"$shots/$scenario.log" 2>&1 &
pid=\$!
for _ in \$(seq 60); do
    xdotool search --onlyvisible --name Telamon >/dev/null 2>&1 && break
    sleep 0.25
done
win=\$(xdotool search --onlyvisible --name Telamon | head -1)
xdotool windowmove \$win 0 0 windowsize \$win 1152 756
snap() { import -window \$win "$shots/\$1.png"; }
click() { xdotool mousemove \$1 \$2 click 1; }
# Cancel has the focus in every dialog: Tab moves to the confirming button.
confirm() { xdotool key Tab; sleep 0.3; xdotool key Return; }
pause() { sleep \$1; }
pause 3
$(declare -f "$scenario" | sed '1,2d;$d')
kill \$pid 2>/dev/null || true
wait \$pid 2>/dev/null || true
INNER
    chmod +x "$w/inner.sh"
    timeout 150 xvfb-run -a -s "-screen 0 1400x900x24" dbus-run-session -- "$w/inner.sh" >/dev/null 2>&1 || true
}

# ---- the scenarios ----

pages() {
    snap home
    click 76 106; pause 2; snap updates
    click 78 66; pause 2; snap installed
}

install_new() {
    pause 1
    click 502 167; pause 1.5; snap install-dialog
    confirm; pause 4; snap app-installed
}

update_old() {
    click 977 170; pause 1.5; snap update-dialog
    confirm; pause 4; snap updated
}

uninstall() {
    click 977 166; pause 1.5; snap uninstall-dialog
    confirm; pause 3; snap uninstalled
}

local_bundle() {
    pause 2; snap local-dialog
    confirm; pause 4; snap local-installed
}

fresh; shot pages --page home
fresh; shot install_new --app net.eterneon.telamon.scratch
fresh; shot update_old --page updates
fresh; shot uninstall --page installed
fresh; shot local_bundle --install-bundle "$work/world/local-bundle.tar.zst"
echo "screenshots in $shots"
