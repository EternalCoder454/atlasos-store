#!/bin/bash
# Build the Telamon Store RPM inside a fedora:44 container, as root.
#   packaging/build-rpm.sh <out dir> [rpmbuild options]
# The binary RPM (no source, no debuginfo) is copied to <out dir>.
# Cargo needs network access.
# TELAMON_LOCAL_RPMS=<dir> installs the RPMs in <dir> first: atlas-framework's
# (telamon-ui), which the app builds against and no repository has.
set -euo pipefail

main() {
    out=${1:?usage: build-rpm.sh <out dir> [rpmbuild options]}
    shift

    here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
    src=$(dirname "$here")
    spec=$here/telamon-store.spec
    version=$(awk '/^Version:/ {print $2; exit}' "$spec")

    dnf -y install rpm-build dnf5-plugins tar gzip >&2
    # ATLAS_LOCAL_RPMS is the name before the rename and still works.
    TELAMON_LOCAL_RPMS=${TELAMON_LOCAL_RPMS:-${ATLAS_LOCAL_RPMS:-}}
    if [ -n "$TELAMON_LOCAL_RPMS" ]; then
        # Telamon.Ui and its fonts, not the gallery. dnf brings their
        # dependencies; rpm then puts these exact files in place even when a
        # build of the same version is installed already.
        local_rpms=("$TELAMON_LOCAL_RPMS"/telamon-ui-[0-9]*.rpm "$TELAMON_LOCAL_RPMS"/telamon-symbols-fonts-[0-9]*.rpm)
        dnf -y install "${local_rpms[@]}" >&2
        rpm -U --replacepkgs --replacefiles "${local_rpms[@]}" >&2
    fi
    dnf -y builddep "$spec" >&2

    top=$(mktemp -d)
    trap 'rm -rf "$top"' EXIT
    mkdir -p "$top"/{SOURCES,BUILD,RPMS,SRPMS,SPECS}
    tar -C "$src" \
        --exclude=./.git --exclude=./target --exclude=./out --exclude=./build \
        --transform "s,^\./,telamon-store-$version/," \
        -czf "$top/SOURCES/telamon-store-$version.tar.gz" .

    rpmbuild -bb "$@" --define "_topdir $top" "$spec"

    mkdir -p "$out"
    find "$top/RPMS" -name '*.rpm' ! -name '*.src.rpm' ! -name '*debuginfo*' ! -name '*debugsource*' \
        -exec cp -v {} "$out"/ \;
}

main "$@"
exit $?
