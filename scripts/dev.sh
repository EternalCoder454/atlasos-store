#!/bin/bash
# Run a command in the fedora:44 build container, with the repo at /src, the
# build output at /work and the cargo and dnf caches in named podman volumes
# (shared with the other Atlas apps).
#   scripts/dev.sh <command...>     e.g. scripts/dev.sh cargo test --workspace
#   scripts/dev.sh                  an interactive shell
# /work is $ATLAS_STORE_WORK, by default ~/.cache/claude-builds/atlas-store:
# on disk, outside the repo (cargo's target dirs, CMake build dirs, the test
# Flatpak installation and remotes). Set CARGO_TARGET_DIR to /work/target/<name>
# to keep one target dir per task.
# The first run installs the build dependencies from the spec (cached after).
# They include atlas-ui, which no repository has: that run needs
# ATLAS_LOCAL_RPMS=<dir> holding atlas-framework's RPMs (atlas-ui and
# atlas-symbols-fonts), one version only. Delete the image after changing the
# spec's BuildRequires or to take a newer atlas-ui.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
image=localhost/atlas-store-dev:44
work=${ATLAS_STORE_WORK:-$HOME/.cache/claude-builds/atlas-store}
mkdir -p "$work"

if ! podman image exists "$image"; then
    rpms=${ATLAS_LOCAL_RPMS:?the dev image needs atlas-framework RPMs: set ATLAS_LOCAL_RPMS=<dir>}
    rpms=$(cd "$rpms" && pwd)
    # The dir is mounted with an SELinux relabel (:z): refuse anything but a
    # dir of RPMs, so a wrong value can't relabel $HOME.
    if [ -n "$(find "$rpms" -mindepth 1 ! -name '*.rpm' -print -quit)" ] ||
        ! compgen -G "$rpms/atlas-ui-[0-9]*.rpm" >/dev/null; then
        echo "ATLAS_LOCAL_RPMS=$rpms must hold only atlas-framework's RPMs (atlas-ui-*.rpm and atlas-symbols-fonts-*.rpm)" >&2
        exit 1
    fi
    ctr=$(podman run -d -v "$repo/packaging":/packaging:ro,Z \
        -v "$rpms":/atlas-rpms:ro,z \
        -v atlas-dnf:/var/cache/libdnf5 \
        registry.fedoraproject.org/fedora:44 sleep infinity)
    trap 'podman rm -f "$ctr" >/dev/null' EXIT
    # flatpak, ostree and appstream make the test installation and the local
    # test remotes (scripts/test-remote.sh).
    podman exec "$ctr" bash -c '
        echo keepcache=True >>/etc/dnf/dnf.conf
        dnf -y install dnf5-plugins rpm-build clippy rustfmt xorg-x11-server-Xvfb \
            dbus-daemon qt6-qtbase-gui kf6-qqc2-desktop-style breeze-icon-theme \
            ImageMagick xdotool flatpak ostree appstream \
            /atlas-rpms/atlas-ui-[0-9]*.rpm /atlas-rpms/atlas-symbols-fonts-[0-9]*.rpm &&
        dnf -y builddep /packaging/atlas-store.spec' >&2
    podman commit "$ctr" "$image" >/dev/null
    podman rm -f "$ctr" >/dev/null
    trap - EXIT
fi

tty=()
[ -t 0 ] && tty=(-it)
exec podman run --rm "${tty[@]}" \
    -v "$repo":/src:Z -w /src \
    -v "$work":/work:Z \
    -v atlas-cargo:/root/.cargo/registry \
    -v atlas-cargo-git:/root/.cargo/git \
    -e CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/work/target/dev}" \
    "$image" "${@:-bash}"
