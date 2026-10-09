# Dependency review

Secure phase, 8 October 2026, Telamon Store 0.4.0. What the Store depends on,
which of it reads data an attacker can write, and whether it should stay.
`cargo deny check` (advisories, bans, licenses, sources) is clean for this lock;
the settings are in `deny.toml` and CI runs them weekly and on every change to
a manifest or the lock (`.github/workflows/audit.yml`).

## What was measured, and how

| Number | Value | Source |
|---|---|---|
| Entries in `Cargo.lock` | **231** (2 workspace crates, 7 git, 222 crates.io) at the start of the phase; 242 with `proptest`, which is dev-only | `Cargo.lock` |
| Crates that ship in the RPM | fewer than the lock: the Windows, WASI and test-only ones are not built; `cargo deny` is set to `x86_64-unknown-linux-gnu` | `deny.toml` `[graph]` |
| Crates the fuzz crate adds | its own lock, 140 entries (the same versions for shared crates) | `fuzz/Cargo.lock` |
| Known advisories | none: 0 vulnerabilities, 0 unmaintained (direct dependencies), 0 yanked | `cargo deny check advisories` against the RustSec database of the day |
| Licences | MIT, Apache-2.0 (+ LLVM exception), BSD-3-Clause, ISC, Zlib, Unicode-3.0, BSL-1.0, CDLA-Permissive-2.0; nothing copyleft is *required* (r-efi offers LGPL as one of three choices) | `cargo deny list`, `deny.toml` |
| `unsafe` | counted by grep over each crate's `src` (comment lines left out): lines with the keyword, and files with a `forbid`/`deny(unsafe_code)` attribute. A rough size of the code a reviewer must trust, not a count of unsound code. | registry sources |

What could **not** be checked from here: download counts, publication dates and
maintainer lists (the registry index that Cargo keeps offline has none of them;
`cargo owner` and the crates.io API were not used), `cargo geiger` (not packaged
for Fedora; the grep above stands in for it), and `cargo audit` (not packaged
either, and it reads the same RustSec database as `cargo deny`, so it would add
nothing). A crate's age and maintainers are therefore described from what its
manifest says (repository, edition, rust-version), not from the registry.

## Crates that decode attacker-controlled data

These are the ones whose bugs matter most: a hostile AppImage, bundle,
GitHub answer, remote or `.flatpakref` reaches them. Each is behind a size or
count limit of the Store's own and is covered by a fuzz target (`fuzz/`) and a
property test (`crates/telamon-store-core/tests/prop_*.rs`).

| Crate | Reads | Written in | Store's checks before and around it |
|---|---|---|---|
| **backhand** 0.25.1 with deku 0.20.3 | the squashfs inside an AppImage | Rust (no `unsafe` of its own; deku 2 lines) | `appimage/squash.rs` checks the superblock and walks the metadata tables first, caps reads; runs in a helper process |
| **liblzma-sys** 0.4.9 (bundled xz) | xz-compressed squashfs blocks | **C** (xz-utils, built from the crate's copy) | via backhand only |
| **zstd-sys** 2.1.0+zstd.1.5.7 (bundled libzstd) | zstd squashfs blocks and the `.tar.zst` of native bundles | **C** (libzstd, built from the crate's copy) | decompressed size capped by `Capped` in `native/archive.rs`; squashfs reads capped |
| **flate2** 1.1.10 (miniz_oxide, zlib-rs) | gzip: squashfs blocks, `appstream.xml.gz` of remotes | Rust; zlib-rs is 456 lines of `unsafe` | decompressed size capped by `appstream::Limits` and the squashfs caps |
| **tar** 0.4.46 | the entries of a native bundle | Rust | read only: `native/archive.rs` writes every file itself; nothing is unpacked by the library |
| **quick-xml** 0.42.0 | AppStream XML (remotes, AppImage metainfo) | Rust, `#![forbid(unsafe_code)]` | `appstream::Limits` (depth, tokens, bytes, retained objects) |
| **serde_json** 1.0.151 | GitHub answers, the catalog, manifests, Flathub's API | Rust | size caps before parsing; serde_json's own recursion limit (128) |
| **ureq-proto** 0.6.4 / **ureq** 3.4.2, **rustls** 0.23.45, **rustls-webpki** 0.103.15, **ring** 0.17.14 | HTTP responses and TLS from the network | Rust; ring has C and assembly | `net.rs`: host allow-lists, redirects re-checked, public addresses only, size caps and timeouts |
| **libflatpak** 0.7.0 → libflatpak, ostree, GPGME (system libraries) | the refs, commits and appstream data flatpak itself reads | **C** (system libraries, not in this lock) | the Store parses and checks a `.flatpakref`/`.flatpakrepo` first and hands libflatpak a canonical rewrite |
| the Store's own code | PNG header and SVG screening, desktop entries (`keyfile`), URLs, launch arguments | Rust | the subject of `prop_*.rs` and `fuzz/` |

## Direct dependencies

`unsafe` is "lines / files with the keyword". "Parses untrusted input" is yes
when the crate sees bytes an attacker controls (the table above has the
detail).

### telamon-store-core

| Crate | Version | Licence | Why | Untrusted input | `unsafe` | Features and what can be switched off | Verdict |
|---|---|---|---|---|---|---|---|
| log | 0.4.34 | MIT/Apache-2.0 | logging facade | no | 6 / 1 | `std` only | keep |
| quick-xml | 0.42.0 | MIT | AppStream XML reader (streaming, no serde) | **yes** | 0 (`forbid`) | `default-features = false`: no serde, no encoding conversion | keep |
| flate2 | 1.1.10 | MIT/Apache-2.0 | gunzip `appstream.xml.gz` | **yes** | 36 / 3 | declared `default-features = false`, `rust_backend`; effective `miniz_oxide` **and** `zlib-rs` (backhand's `gzip` feature turns the second on) | keep with note |
| libc | 0.2.190 | MIT/Apache-2.0 | `geteuid`, `O_NOFOLLOW` and friends | no | 800 / 68 (FFI declarations) | none | keep |
| sha1, sha2 | 0.10.7, 0.10.9 | MIT/Apache-2.0 | OpenPGP key fingerprints; bundle and AppImage checksums | no (hashes untrusted bytes, parses nothing) | 4 / 3, 29 / 8 (CPU intrinsics) | `default-features = false` | keep |
| libflatpak | 0.7.0 | MIT | install, remove, update through the system's libflatpak | indirectly (flatpak parses) | 353 / 16 (+ glib 2306 / 85, gio 3174 / 222) | the bindings only; the C libraries are the system's | keep with note |
| ureq | 3.4.2 | MIT/Apache-2.0 | the one HTTPS client | **yes** | 0 (`forbid`) | `default-features = false`, `rustls`: no gzip, no JSON, no cookies, no `native-tls`, no SOCKS | keep |
| serde, serde_json | 1.0.229, 1.0.151 | MIT/Apache-2.0 | all JSON | **yes** | 2 / 2, 13 / 4 | no `preserve_order`, no `arbitrary_precision`, no `raw_value` | keep |
| backhand | 0.25.1 | MIT/Apache-2.0 | read the squashfs of an AppImage | **yes** | 0 | `default-features = false`, `gzip`, `zstd`, `xz` (the three compressions AppImages use) | keep with note (below) |
| tar | 0.4.46 | MIT/Apache-2.0 | read the entries of a bundle | **yes** | 26 / 3 | `default-features = false` (no `xattr`) | keep |
| zstd | 0.13.3 | MIT | decode `.tar.zst` | **yes** | 3 / 1 (+ zstd-safe 165 / 3, zstd-sys 18 / 4) | declared `default-features = false`, but backhand depends on zstd with its default features, so `legacy` (decoders for zstd's pre-1.0 formats), `zdict_builder` and `arrays` are compiled in and cannot be switched off from here | keep with note |
| proptest (dev) | 1.11.0 (exact) | MIT/Apache-2.0 | property tests | tests only | 7 / 2 | `default-features = false`, `std`: no `fork`, no `timeout` | keep |

### telamon-store (the application)

| Crate | Version | Licence | Why | Untrusted input | `unsafe` | Features | Verdict |
|---|---|---|---|---|---|---|---|
| telamon-store-core | path | MIT | everything above | | | | |
| telamon-framework-ui, -flatpak | git, tag `v2.0.0`, locked at `8e6a5356…` | MIT | startup, settings, logging, crash reports, Flatpak updates | no | not counted (not in the registry) | | keep (own code) |
| telamon-updater-core | git, rev `b481cf12…` | MIT | the app update engine | its own network reads | not counted | | keep (own code) |
| cxx, cxx-qt, cxx-qt-lib | 1.0.202, 0.10.0, 0.10.0 | MIT/Apache-2.0 | the Rust ↔ Qt bridge | no (strings from Qt) | 325 / 19, 24 / 5, 900 / 205 (FFI) | defaults | keep |
| cxx-qt-build (build) | 0.10.0 | MIT/Apache-2.0 | generates the bridge at build time | no | | | keep |
| zbus | 5.19.0 | MIT | the AppImage notification over D-Bus | the session bus's answers (the user's own bus) | 54 / 8 | `default-features = false`, `tokio` | keep |
| tokio | 1.53.2 | MIT | zbus's runtime | no | 1051 / 137 | declared `rt`, `time`; effective (unified with zbus and the framework crates) `fs`, `io-util`, `macros`, `net`, `process`, `rt-multi-thread`, `signal`, `sync`, `time` | keep |
| futures-util | 0.3.34 | MIT/Apache-2.0 | zbus signal streams | no | 135 / 21 | `default-features = false` | keep |
| libflatpak, libc, log, serde_json | as above | | | | | | |

### fuzz (not shipped)

`libfuzzer-sys` 0.4.13 (`(MIT OR Apache-2.0) AND NCSA`, LLVM's libFuzzer
runtime), `cargo-fuzz` 0.13.2 (installed in CI with `--locked`), plus `libc`,
`serde_json`, `tar` and `zstd` at the workspace's versions. Not part of the
workspace, not in the RPM, checked by `cargo deny` for advisories and sources
only (its NCSA licence is not the package's).

## Notable transitive dependencies

| Crate | Version | Pulled in by | Notes |
|---|---|---|---|
| deku, deku_derive | 0.20.3 | backhand | the bit-level reader/derive backhand parses squashfs structures with; 2 lines of `unsafe` each |
| miniz_oxide | 0.9.1 | flate2 | the pure-Rust inflate; `forbid(unsafe_code)` |
| zlib-rs | 0.6.8 | flate2 | pure-Rust zlib; 456 lines of `unsafe` (SIMD, manual memory). It is in the lock **together with** miniz_oxide: flate2 prefers zlib-rs when its feature is on, and backhand turns it on. The Store's `rust_backend` choice therefore does not decide which inflater runs. Both are memory-safe in intent; zlib-rs has the larger `unsafe` surface. |
| liblzma, liblzma-sys | 0.4.8, 0.4.9 | backhand | C xz decoder built from the crate's bundled source with the distro's flags; backhand asks for liblzma's `parallel` (threads) and `static` features, which the Store cannot turn off. A C decoder reading hostile data: the one reason to keep an eye on backhand's `xz` feature. Dropping it would make xz AppImages show "can't look inside", which they would then do for every xz-compressed image. Kept: real AppImages use xz. |
| zstd-safe, zstd-sys | 7.3.0, 2.1.0 | zstd, backhand | libzstd 1.5.7 built from the bundled source |
| xxhash-rust, solana-nohash-hasher | 0.8.19, 0.2.1 | backhand | hashing helpers; xxhash-rust 31 lines of `unsafe` |
| no_std_io2, thiserror, tracing | 0.9.4, 1 / 2, 0.1.44 | backhand | `tracing` is a normal dependency of backhand 0.25 even without a subscriber |
| ring | 0.17.14 | rustls, rustls-webpki, telamon-updater-core | the TLS crypto provider. **ureq's `rustls` feature uses `ring` here, not `aws-lc-rs`**: `aws-lc-rs` and `aws-lc-sys` are not in the lock, and `deny.toml` bans them (and OpenSSL) so a feature change cannot bring a second crypto stack in unnoticed. 228 lines of `unsafe` plus C and assembly. |
| rustls, rustls-webpki, rustls-pki-types | 0.23.45, 0.103.15, 1.15.1 | ureq | certificate verification in Rust; rustls-webpki has no `unsafe` |
| webpki-roots | 1.0.9 | ureq | the Mozilla CA list, **compiled in** (CDLA-Permissive-2.0). It does not follow the system trust store: a CA distrusted by Fedora stays trusted by the Store until the crate is updated. See "Recommendations". |
| ureq-proto, httparse, http, base64, percent-encoding, utf8-zero | | ureq | HTTP parsing; ureq-proto is `forbid(unsafe_code)`, httparse has SIMD `unsafe` |
| glib, gio, libflatpak-sys, gobject-sys, glib-sys, gio-sys | 0.21.5, 0.7.0 | libflatpak | the GLib object bindings; very large `unsafe` surface by nature (FFI) |
| crc32fast, simd-adler32, memchr | | flate2, miniz_oxide, glib, quick-xml, serde_json | checksum and search routines with SIMD `unsafe` |
| winnow, toml, toml_edit, zvariant, zbus_names, enumflags2, endi, ordered-stream | | zbus | D-Bus message and TOML handling |
| cxx-gen, codespan-reporting, syn, proc-macro2, quote, darling, clang-format | | cxx-qt (build time) | build-time code generation only |

## The two git dependencies

Both are first-party repositories and are pinned by an immutable reference, and
`Cargo.lock` agrees with `Cargo.toml` and with CI:

| Dependency | `Cargo.toml` | `Cargo.lock` source | CI |
|---|---|---|---|
| atlas-framework crates (`telamon-framework-ui`, `-flatpak`, and `-core`, `-system` below them) | `tag = "v2.0.0"` | `git+https://github.com/EternalCoder454/atlas-framework?tag=v2.0.0#8e6a53569363aec3d7e42920875d03ca113ef3eb` | `ci.yml` pins the reusable workflow and the RPM build to `8e6a53569363aec3d7e42920875d03ca113ef3eb # v2.0.0` |
| Telamon Updater (`telamon-updater-core` and its `-base`, `telamon-update-engine`) | `rev = "b481cf123ee314baad66f52b3f5414b34b92ebbb"` | `git+https://github.com/EternalCoder454/atlasos-updater?rev=b481cf123ee314baad66f52b3f5414b34b92ebbb#b481cf123ee314baad66f52b3f5414b34b92ebbb` | not used in CI directly |

A tag can be moved by whoever owns the repository, but `Cargo.lock` stores the
commit, and `--locked` (CI, the RPM build) fails when the two disagree, so a
moved tag cannot change a build without a visible change to the lock.
`deny.toml` allows exactly these two repositories (`unknown-git = "deny"`).

## Verdicts

Everything stays. The ones with a note:

- **backhand**: the squashfs reader has had a short history and a small
  maintainer base, and it is the parser with the most attacker-controlled
  structure in the Store. Kept because nothing else reads squashfs without
  mounting it; the Store puts its own superblock and table checks in front,
  reads in a helper process, and fuzzes it (`squashfs`, `appimage_file`).
  Fuzzing found that a block offset near `u64::MAX` in an id or fragment table
  overflows backhand's `offset + start` (reader.rs, `seek`); with
  `overflow-checks` on that is now a panic instead of a silent wrap. See the
  Secure-phase report for the fix to apply in `appimage/squash.rs`.
- **zstd / liblzma-sys**: C decoders reading hostile data. Memory-safety bugs
  in libzstd and xz-utils are real and the bundled copies are only as fresh as
  the crate version; `cargo deny` sees the Rust crates, not the C they carry.
  Recommended: build against the distro's libzstd and liblzma (`pkg-config`
  feature of `zstd-sys` and `liblzma-sys`) so Fedora's security updates apply
  without a Store release. This is a packaging decision (it needs
  `BuildRequires: pkgconfig(libzstd)` and `pkgconfig(liblzma)`) and was not
  made here.
- **flate2 with zlib-rs**: `cargo tree -e features` shows flate2 built with
  `miniz_oxide` and `zlib-rs` together; the `rust_backend` feature the
  workspace asks for does not pick the inflater, and the `unsafe`-heavy one
  wins. Not changed here (backhand decides).
- **Features that cannot be switched off from the workspace**: zstd `legacy`
  and `zdict_builder`, liblzma `parallel`, flate2 `zlib-rs`: backhand's own
  dependency declarations ask for them. Each is attack surface the Store never
  uses. The remedy is a change in (or a fork of) backhand, or dropping its
  `xz` and `zstd` features and decoding those blocks in the Store.
- **libflatpak**: the safety of install rests on libflatpak, ostree and GPGME,
  which are C and are updated with the OS. The Store's job is to hand them only
  what its own parser accepted.
- **webpki-roots**: the one dependency that holds *policy* data, not code.

## Recommendations (not done in this phase)

1. Link `zstd-sys` and `liblzma-sys` against the system libraries.
2. Consider `rustls-platform-verifier` or `rustls-native-certs` for ureq so TLS
   follows the OS trust store, and drop the compiled-in root list.
3. Pin the CI container by digest (`registry.fedoraproject.org/fedora:44@sha256:…`)
   once a digest can be looked up and refreshed on a schedule; a tag moves, and
   `dnf` inside it installs the current packages either way.
4. When the Performant phase starts, measure `overflow-checks = true`
   (`[profile.release]`), which this phase turned on.
