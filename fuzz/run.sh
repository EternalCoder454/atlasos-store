#!/bin/bash
# Runs the fuzz targets for a while each, on nightly with cargo-fuzz.
#   fuzz/run.sh [seconds per target, default 30] [target...]
# The seeds in fuzz/corpus/<target> and the inputs that crashed a target once
# (fuzz/regressions/<target>, each a fixed bug or a finding waiting for its fix)
# are only read: what the fuzzer finds goes
# to $FUZZ_WORK/corpus/<target> (default fuzz/work, not checked in) and a
# crash to $FUZZ_WORK/artifacts/<target>/ (CI uploads that folder). Exits
# non-zero when any target crashed, and after trying all of them.
#   FUZZ_SANITIZER=none   skips AddressSanitizer (faster, no memory errors in unsafe code)
set -uo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
secs=${1:-30}
shift || true
work=${FUZZ_WORK:-$here/work}
sanitizer=${FUZZ_SANITIZER:-address}

# target:dictionary:max_len
table=(
    native_manifest:manifest:65536
    native_catalog:manifest:65536
    github_release:manifest:65536
    native_desktop:desktop:8192
    native_dbus:desktop:4096
    native_plan:desktop:16384
    native_unpack:tar:65536
    native_unpack_tar:tar:32768
    keyfile:keyfile:16384
    flatpakref:flatpakref:16384
    urls:urls:2048
    text:urls:2048
    launch_args:urls:4096
    squashfs:squashfs:32768
    appimage_file:squashfs:65536
    icon:urls:8192
    inspection_decode:manifest:16384
    appstream_metainfo:manifest:32768
)

want=("$@")
failed=()
cd "$here"
for row in "${table[@]}"; do
    IFS=: read -r target dict max_len <<<"$row"
    if [ "${#want[@]}" -gt 0 ] && [[ ! " ${want[*]} " == *" $target "* ]]; then
        continue
    fi
    mkdir -p "$work/corpus/$target" "$work/artifacts/$target"
    extra=()
    [ -d "$here/regressions/$target" ] && extra=("$here/regressions/$target")
    echo "=== $target (${secs}s)"
    if ! cargo fuzz run --sanitizer "$sanitizer" "$target" "$work/corpus/$target" "$here/corpus/$target" "${extra[@]}" -- \
        -max_total_time="$secs" -dict="$here/dictionaries/$dict.dict" -max_len="$max_len" \
        -timeout=10 -rss_limit_mb=2048 -artifact_prefix="$work/artifacts/$target/" -print_final_stats=1; then
        failed+=("$target")
    fi
done
if [ "${#failed[@]}" -gt 0 ]; then
    echo "FAILED: ${failed[*]}" >&2
    exit 1
fi
