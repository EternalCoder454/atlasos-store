#!/bin/bash
# Fails when an ELF executable or library lacks the linker hardening Fedora's
# flags and Telamon's Rust flags ask for. Reads the ELF headers only (readelf);
# nothing is run.
#   scripts/check-hardening.sh <elf>...
# Required (an error): position independent (ET_DYN), a PT_GNU_RELRO segment,
# BIND_NOW (full RELRO), a non-executable stack (PT_GNU_STACK without E),
# no text relocations, and the stack protector's __stack_chk_fail imported.
# Reported only (a notice): fortified libc calls (__*_chk) and the x86 CET
# property (IBT and SHSTK): they depend on what the program calls and on
# every linked object carrying the note. Set HARDENING_STRICT=1 to make the
# notices errors too.
# Used by the spec's %check and by CI's rpm job.
set -euo pipefail

fail=0
note() { printf '%s: %s\n' "$1" "$2" >&2; }
bad() { note "$1" "FAIL $2"; fail=1; }

check() {
    local elf=$1 hdr dyn seg syms
    if ! hdr=$(readelf -hW -- "$elf" 2>&1) || ! grep -q 'Class:.*ELF' <<<"$hdr"; then
        bad "$elf" "not an ELF file"
        return
    fi
    dyn=$(readelf -dW -- "$elf" 2>&1 || true)
    seg=$(readelf -lW -- "$elf" 2>&1 || true)
    syms=$(readelf --dyn-syms -W -- "$elf" 2>&1 || true)

    grep -Eq 'Type:[[:space:]]+DYN' <<<"$hdr" || bad "$elf" "not position independent (not ET_DYN)"
    grep -Eq '^[[:space:]]*GNU_RELRO[[:space:]]' <<<"$seg" || bad "$elf" "no PT_GNU_RELRO segment"
    # BIND_NOW: either the FLAGS bit or the FLAGS_1 NOW bit.
    grep -Eq '\((BIND_NOW)\)|\(FLAGS\).*BIND_NOW|\(FLAGS_1\).*Flags:.*\bNOW\b' <<<"$dyn" ||
        bad "$elf" "no BIND_NOW (RELRO is only partial)"
    # The stack: the line is "GNU_STACK <offs> <va> <pa> <fsz> <msz> RW  0x10".
    local stack
    stack=$(awk '$1 == "GNU_STACK" {print $0}' <<<"$seg")
    if [ -z "$stack" ]; then
        bad "$elf" "no PT_GNU_STACK header (the stack may be executable)"
    elif awk '{print $7 $8}' <<<"$stack" | grep -q E; then
        bad "$elf" "executable stack (PT_GNU_STACK has E)"
    fi
    if grep -Eq '\((TEXTREL)\)|\(FLAGS\).*TEXTREL' <<<"$dyn"; then
        bad "$elf" "text relocations (TEXTREL)"
    fi
    grep -q '__stack_chk_fail' <<<"$syms" || bad "$elf" "no __stack_chk_fail import (built without -fstack-protector-strong?)"

    local strict=${HARDENING_STRICT:-0} level=note
    [ "$strict" = 1 ] && level=bad
    grep -Eq ' __[a-z0-9_]+_chk(@| |$)' <<<"$syms" || "$level" "$elf" "no fortified libc calls (__*_chk) imported"
    if [ "$(readelf -hW -- "$elf" | awk '/Machine:/ {print $2}')" = Advanced ]; then
        # "Advanced Micro Devices X86-64"
        readelf -nW -- "$elf" | grep -q 'x86 feature:.*IBT' || "$level" "$elf" "no CET IBT property"
        readelf -nW -- "$elf" | grep -q 'x86 feature:.*SHSTK' || "$level" "$elf" "no CET SHSTK property"
    fi
    echo "$elf: hardening checked"
}

[ "$#" -gt 0 ] || { echo "usage: $0 <elf>..." >&2; exit 2; }
for f in "$@"; do check "$f"; done
exit "$fail"
