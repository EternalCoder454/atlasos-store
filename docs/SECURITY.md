# Telamon Store: security

Secure phase of the F.S.R.P phases (docs/DESIGN.md has the design; "Trust" there
says where each input is checked). This page is the threat model: who can attack
what, what stops them, what does not, and where each defense is tested. It is
changed together with the code it describes.

## What the Store is, for an attacker

The Store installs software for the user, and each kind of software has a
different trust story:

| Kind | Code runs as | Sandbox | Where it comes from | What vouches for it |
|---|---|---|---|---|
| Flatpak apps | the user, inside Flatpak's sandbox | yes (the app's permissions, shown before install) | a remote (Flathub or one the user added) | the remote's OSTree/GPG signatures, checked by libflatpak; the Store shows the plan and permissions |
| Native Telamon apps | the user, no sandbox | none | a GitHub release of a repository whose owner the Store knows | the catalog entry (reviewed pull request) pins the repository and the minisign key(s); the release must carry a valid signature over its manifest, which holds the archive's SHA-256 |
| AppImages | the user, no sandbox | none | a file the user downloaded | nothing: the Store says so, in red, before Install |
| The Store itself | the user | none | the Telamon OS image (RPM) | the image's own update chain |

Nothing the Store does runs as root. System-wide Flatpak changes go through
flatpak's own polkit helper; the Store has no helper and no polkit action.

## Assets

1. The user's account: files, session, credentials. An installed native app or
   AppImage can reach all of it; so the Store's job is to make sure only what
   the user chose, from the publisher they chose, is installed.
2. Other software on the machine: the Store must not let an installed bundle
   shadow system components (menu entries, D-Bus services, notification
   configs) or another app's files.
3. The Store's own process and window: a hostile file or answer must not crash
   it, hang it, exhaust memory or disk, or show the user something other than
   the truth (spoofed text, rich text, bidi tricks).
4. Privacy: what the Store tells the network.

## Attackers and what stops them

Each section ends with the tests that pin the defense (test names, so a
future change that weakens one fails loudly).

### 1. A network attacker (on the path, or controlling DNS)

Goal: read or change what the Store fetches, make it connect inside the user's
network, exhaust it.

| Threat | Defense | Test |
|---|---|---|
| Plain HTTP, downgrade | `net.rs` is https only (rustls via ureq, no OpenSSL, no `http_status_as_error`), port 443 only, a public DNS name only (`launch::https_url`); no proxy from the environment; `https_only` also in the HTTP library | `net::tests::urls_are_checked_before_any_connection` |
| Redirect to another host or scheme | Redirects are followed by the Store, at most 3, each target re-run through `https_url`; with a `Hosts::Only` policy (GitHub's hosts for native apps; `flathub.org` / `dl.flathub.org` for the Flathub API) a redirect off the list is refused before any connection or name lookup | `net::tests::a_host_list_binds_the_first_address_and_every_redirect`, `redirects_stay_on_https_and_public_names` |
| DNS rebinding / a name that points into the LAN (`169.254.169.254`, `10/8`, `::1`, NAT64 and 6to4 wrappers of private v4, CGNAT, ULA...) | The resolver is replaced (`PublicOnly`): non-global addresses are dropped from the answer the connection is made with, so there is no check-then-connect gap | `net::tests::only_global_addresses_are_public` |
| Oversized or endless answers | A body cap per request (`max_bytes`: 256 KiB catalog, 1 MiB API/manifest, 4 KiB signature, 256 MiB bundle) enforced while reading; a global time limit per request (30 s small, 30 min archive) that includes redirects and the body | `net` limits, `native` fetch tests with the fake |
| Slow-loris | The same global deadline; nothing waits without one | as above |
| Tampered content on a TLS-intercepted path | TLS to the real host or nothing; for bundles, an Ed25519 signature on top (section 2) | |

What a network attacker with a valid certificate for github.com cannot do
either way: install an unsigned or wrongly signed bundle. What it can do:
withhold answers (the Store then uses its cache and says so) and replay old,
validly signed releases (see "Not done": freshness).

### 2. A hijacked GitHub repository, release asset, CI secret or catalog PR

Goal: get the user to install attacker code as a "Telamon app".

Trust roots, in the order the Store applies them:

1. The Store binary (the RPM in the image): `ALLOWED_OWNERS`, the host list,
   the reserved name spaces.
2. The catalog, `catalog/native-apps.json` on the `main` branch of this
   repository, fetched over https from `raw.githubusercontent.com`: which
   repository and which minisign key(s) may provide each app ID. It changes
   only by a reviewed pull request (`CODEOWNERS`; branch protection is the
   owner's setting).
3. The app owner's signing key. It should live off the repository (offline
   signing, `docs/BUNDLES.md` in the framework); a key stored as a repository
   secret is exposed to anyone who can change that repository's workflows.

| Compromise | Result |
|---|---|
| A release asset is replaced (bundle, manifest, or both) | Refused: the manifest must carry a valid minisign signature (`telamon-bundle.json.minisig`) from a key the catalog entry lists; the signature covers the archive's SHA-256 and size through the manifest |
| The app repository is taken over, no access to the signing key | Same: new releases cannot be signed; the old signed ones stay valid |
| The signing key is stolen | The attacker can publish a "valid" release. Mitigation: rotate in the catalog (list the new key, then drop the old one); installed apps keep working. The dialog tells the user when the signing key of an update changed |
| A malicious catalog pull request | Review. Bounded by `ALLOWED_OWNERS` (the repository must belong to a known owner), by the app-ID rules (reserved namespaces, no system collisions), and by the signature requirement. A key added to an entry is shown by key ID in the install dialog |
| The Store repository `main` is taken over | The attacker controls the pinned keys, but only for repositories of allowed owners, so they also need a release in such a repository. This is the weakest link of the model; the migration path is a root key compiled into the Store that signs the catalog |
| A validly signed old release served as "latest" | An installed app is never moved to a lower version (checked when listing, before downloading, and again under the install lock). A first install has no version to compare: see "Not done" |
| One key signs two apps; a manifest of app A is offered under app B | Refused: the manifest's id must equal the catalog entry's, after the signature check |
| An old signature replayed under a newer tag | Refused: the tag must equal the signed manifest's version |
| A cache file edited on disk | The cache holds the release, manifest and signature and is verified again on every read with the catalog's current keys |
| A local file (`--install-bundle`) | Never claims a signer; the dialog says in red that it was not checked by Telamon and shows its SHA-256 |

Sigstore / GitHub artifact attestations were evaluated and are not used yet:
verifying one offline needs the Fulcio certificate chain, a Rekor
inclusion proof (the certificate lives ten minutes) and a pinned, rotating
trusted root, plus X.509 and ECDSA code in the Store; that is more attack
surface than the problem needs today. The catalog's typed `signers` list
(`{"type": "minisign", "key": ...}`; unknown types are ignored) is the
extension point: a `sigstore` signer naming the expected workflow identity can
be added without a schema change.

### 3. A malicious bundle (what the signed publisher, or a local file, can do)

A bundle is code that runs as the user; the Store cannot make that safe and
says so. What it does enforce is that **installing** it cannot do more than
that, and cannot damage other software.

| Threat | Defense | Test (tests/native.rs, tests/native_secure.rs) |
|---|---|---|
| Path traversal, absolute names, `.`/empty parts, hidden/bidi/control characters, over-deep nesting | Every tar entry is validated before anything is written; the tar library only reads | `names_that_leave_or_repeat_are_refused`, `what_the_tar_library_resolves_is_checked_like_everything_else` |
| Symlink and hardlink games, devices, FIFOs, sparse files, PAX/GNU size lies | Only folders, regular files and relative links; links are created last and resolved by hand inside the tree; everything is created through open directory descriptors with `O_NOFOLLOW`/`O_EXCL` | `an_entry_cannot_be_written_through_a_link_planted_in_the_staging_folder`, `link_chains_are_followed_by_hand_and_must_end_inside`, `sparse_and_other_special_entries_are_refused`, `a_pax_size_that_disagrees_with_the_header_is_not_believed_either_way` |
| setuid/setgid/sticky, odd modes | Modes come from the manifest only (0644/0755) | |
| Decompression bombs, huge headers | Window <= 128 MiB, 1 GiB unpacked, 40,000 entries, 1 MiB for any header the tar library reads, only zero padding after the tar | `the_decompressor_is_held_to_a_window_and_to_the_end_of_the_tar`, `headers_that_claim_more_than_can_be_held_are_stopped` |
| `Exec` injection in the copied desktop entry or D-Bus service | First word must be a bare name of a program in the bundle's `bin/`; rewritten to an absolute path, quoted by the Desktop Entry rules (round-trip property test); control characters, odd line ends, repeated groups/keys and odd key names are refused instead of normalised; keys that load or run other things (`X-KDE-Library`, `X-KDE-ServiceTypes`, `Implements`, ...) are dropped | `exec_values_round_trip`, `control_characters_and_odd_line_ends_are_refused`, `keys_that_load_or_run_something_are_not_copied` |
| Shadowing system software through the user data dir (it comes first in every search path) | A bundle may write only files that carry its app ID; no export may have the same relative path as a file in any system data dir; the app ID and every D-Bus name are refused if a system `dbus-1/services` file names them or if they are in a reserved namespace (`org.freedesktop.`, `org.kde.`, `org.gnome.`, ...); `telamon-<x>.notifyrc` cannot replace a system one | `nothing_is_copied_over_a_system_file_with_the_same_path`, `a_dbus_name_a_system_service_claims_is_not_taken`, `the_notification_file_of_a_system_app_cannot_be_shadowed`, `ids_in_the_desktops_name_spaces_are_refused` |
| Shadowing a command through `~/.local/bin` (it comes before `/usr/bin` on `PATH`) | A command must be named after the app ID's last part (`gates`, `telamon-gates`, `gates-*`); a name that is in any folder of the Store's own `PATH`, `/usr/{,local/}{bin,sbin}`, Homebrew's, mise's shims, `~/.cargo/bin` or `~/bin` is skipped (a folder only a shell profile adds and none of these is not seen); nothing but the Store's own link for that app is replaced or removed | `a_command_never_shadows_the_system_or_takes_a_name_not_its_own`, `a_file_or_link_that_is_not_ours_is_never_replaced` (tests/native_commands.rs) |
| A notification file that runs a command or writes a log | `Execute`/`Logfile` keys and actions are refused (escapes decoded first) | `a_notification_file_may_not_run_a_command_or_write_a_log` |
| metainfo that replaces or extends another component | Parsed (no DOCTYPE/entities, depth and size limits); one `<component>` whose `<id>` is the app ID; elements outside an allow-list are refused | `metainfo_is_one_component_with_the_apps_own_id` |
| Icons | PNG header pixel cap or a plain SVG (same validator as AppImages) at install and again when listed | `an_icon_must_be_what_qt_can_safely_decode` |
| Local attackers racing the install (TOCTOU in `~/.local/share`) | `telamon-apps` and below are opened by descriptor, never through a link, owned by the user, mode 0700; replaced/removed export files are checked on the very object (RENAME_EXCHANGE / move aside, hash through the fd); undo and cleanup are `Drop`s so a panic leaves nothing half done | `a_link_in_place_of_the_apps_folder_is_never_used`, `a_file_changed_between_the_check_and_the_write_is_not_replaced`, `a_panic_in_the_worker_leaves_no_half_installed_app` |
| Leaking descriptors or tokens into the launched app | `close_range(3, ~0, CLOEXEC)` before exec; the activation token is passed only when valid | `the_program_gets_no_descriptor_of_the_stores` |

Accepted residual: a bundle's desktop entry may claim `MimeType`s and
`x-scheme-handler`s. The default handler stays the user's choice
(`mimeapps.list`); the Store does not rewrite them.

### 4. A malicious AppImage, and a file dropped in `~/Downloads`

An AppImage is not sandboxed and not checked by Telamon; the install dialog
says so and the strongest warnings appear when it is unsigned, from an
unknown origin, or in an old format.

| Threat | Defense |
|---|---|
| A file in `~/Downloads` is executed or opened by anything but the bounded inspector | Never executed. The systemd path unit only starts `telamon-store --appimage-check`; the check looks one level deep at regular files by `lstat` + `O_NOFOLLOW|O_NONBLOCK` + `fstat` (a FIFO, device, socket or symlink named `x.AppImage` never blocks or passes), skips names with control/bidi characters, handles at most a bounded number per run, and inspects through the helper. It opens no network (`AF_UNIX` only) |
| A parser bug in the squashfs/decompressor/XML code gives the file's author code execution | The inspection runs in a helper process: rlimits (address space, CPU, file size, no core), `NO_NEW_PRIVS`, and, after the file is open and hashed and `gpgv` has run, a **seccomp-bpf allowlist** (read/pread/write on already open fds, memory management, futex, exit): no `open`, `exec`, `socket`, `clone`, `ptrace`, `ioctl`. A filter that cannot be installed on x86-64 means no inspection (fail closed). The helper's answer is untrusted again in the parent; the facts the sandboxed stage must not influence (origin, signature, hash, size) are taken from a record the unsandboxed stage wrote before the filter was installed |
| `gpgv` parsing an attacker's embedded key and signature | Fixed path (`/usr/bin/gpgv`), empty environment, private keyring dir, rlimits, `NO_NEW_PRIVS`, a syscall denylist (no sockets, ptrace, ...), its own process group killed on timeout, status read with a cap, temp dir removed on every path |
| Malicious SVG/PNG icon (scripts, external references, entity tricks, huge images) | A tag-by-tag SVG tokenizer that refuses what it does not understand (scripts, styles, `use`, `image`, `foreignObject`, animation of `href`, `on*` attributes, non-fragment `href`/`url()`, any entity but the five predefined ones, any non-UTF-8 encoding); PNG header checked (dimensions, depth/colour combinations); re-validated in the parent |
| Install of a different file than the one shown | The file is opened once; the same descriptor is hashed and copied; the hash is compared with the one the user was shown |
| Forged Store entries in `~/.local/share/applications` | An entry counts only if it carries the marker, its path is a plain `*.AppImage` directly in `~/Applications`, its icon reference matches the install layout; Open runs only that path; Uninstall removes only what the entry records, each only if it is a regular file of the user |
| Hostile names (newlines, quotes, `$`, `%`, bidi) | File names reduced to `[A-Za-z0-9._-]`; `Exec` quoting round-trip tested with random hostile names; notifications are plain text |
| The unit | `NoNewPrivileges`, `RestrictAddressFamilies=AF_UNIX`, `RestrictNamespaces`, `RestrictSUIDSGID`, `LockPersonality`, `RestrictRealtime`, `KeyringMode=private`, `UMask=0077`, `LimitCORE=0`, `TasksMax`, `MemoryMax`. Not set because they could not be verified against a real user manager: `ProtectSystem`/`ProtectHome`, a `SystemCallFilter` (the helper installs its own) and `MemoryDenyWriteExecute` (the binary links Qt) |

### 5. A malicious Flatpak remote, `.flatpakrepo` or `.flatpakref`

Flatpak apps are sandboxed; what the Store must get right is **what it adds as
a source**, **what it shows as the plan**, and **that it asks before doing it**.

| Threat | Defense |
|---|---|
| A repo file configures more than the user was shown | The file is read as a GLib key file with limits, known keys only; the Store builds the remote from its own `RemoteProposal` fields. A hostile file passed to libflatpak would have set `xa.filter=<local file>`, authenticator keys, `xa.subset`, `xa.nodeps`: tested not to reach the config |
| Non-https, odd ports, credentials, IPs, `file:`, `oci+https`, dot segments | Refused (`https_url`) |
| A key that is not the one shown | The GPG key is parsed (exactly one primary key, no secret key packets, size limited), and its fingerprint is shown in the Add Source dialog; no key means an extra acknowledgement |
| A source named like a trusted one | The Store picks the name (slug, reserved names such as `flathub`, `fedora`, `kde` refused unless the URL is that remote's own) |
| A source that claims "Verified" | Only the Flathub remote (by URL) can mark an app Verified |
| `RuntimeRepo` | Never followed without its own confirmation |
| Spoofed text in dialogs (bidi, newlines, rich text) | `text::clean` on everything remote; every `Text`/`Label` plain; `scripts/check-qml-plaintext.sh` enforces it (also page titles, which Telamon.Ui draws as AutoText) |
| Removing from the wrong installation | A removal carries the installation (`user`/`system`) the dialog named |

**System scope and polkit.** The Store asks for no privilege and has no polkit
action or helper. A system-wide change is libflatpak talking to
`flatpak-system-helper`, which asks polkit. Fedora's rules (flatpak 1.18.4 on
Fedora 44) make several of those passwordless for wheel members, so **the
Store's own confirmation dialog is the control**, not polkit:

| Store operation | polkit action | Fedora default | Store confirmation |
|---|---|---|---|
| Install app / runtime (with runtimes, add-ons) | `app-install` / `runtime-install`, `modify-repo` | wheel: yes, no password; others: auth_admin_keep | Install dialog: plan, sources, sizes, permissions; re-checked against a fresh transaction (`PlanChanged`) |
| Update | `app-update` / `runtime-update` | yes | Update All; review dialog when permissions are new |
| Uninstall, remove unused, prune | `app-uninstall` / `runtime-uninstall`, `modify-repo` | wheel: yes, no password | Remove / Unused dialogs |
| Add, remove, enable/disable a system source | `configure-remote` | password, even for wheel | Add / Remove Source dialogs, then the polkit password |
| AppStream and summary refresh | `appstream-update`, `update-remote`/`metadata-update` | yes | none (metadata only) |
| Install a `.flatpak` bundle, change flatpak config | `install-bundle`, `configure` | password | not wired in the Store |
| Everything in the user installation | none | none | the dialog |

**Permissions display.** `permissions.rs` was compared with flatpak's
`flatpak-context.c`: every key maps to a known item; unknown keys, wrong case,
`[Policy *]`, `[USB Devices]` show as unknown/High; `devices=usb` and
`filesystems=host-root` are High.

### 6. Other untrusted input

Launch arguments (`flatpak:` links, files, `--install-bundle`,
`--appimage-install`), AppStream XML, the Flathub JSON, the cache files: all
parsed in Rust with limits (`docs/DESIGN.md`, Trust), all fuzzed (section 7).
`flatpak` and `systemd-run` are started from `/usr/bin` or `/bin`, never found
through `PATH` (a session `PATH` has user-writable folders), with `--` before
an ID; IDs may not start with `-`.

### 7. Privacy

Every Store request goes through `net.rs`: no cookies, no credentials, no
referer, no proxy settings, `User-Agent: telamon-store/<version>`, `Accept`.

| Request | When | Sent |
|---|---|---|
| `flathub.org/api/v2/collection/popular`, `recently-updated`, `category/<c>`, `app-picks/apps-of-the-week/<UTC date>` | Home / category pages, cache older than 6-24 h | the list name, page size, the UTC date. **No installed-app list, no IDs, no locale, hostname or machine ID** |
| the `.flatpakrepo` URL the user pasted | Add Source | the UA |
| `raw.githubusercontent.com` (catalog), `api.github.com/repos/<repo>/releases/latest` for every catalog app, `github.com` release files | Telamon Apps page / check / install | the UA. The check asks for every catalog app, not only installed ones |
| Remotes (libflatpak/libostree) | refresh, install, update | flatpak's own user agent and the remote's paths, as the `flatpak` tool would |

Not present: telemetry, screenshots or remote icons fetched by the Store,
`XMLHttpRequest`, remote `Image` sources (the QML check forbids them).
GitHub and Flathub see the user's IP address like any web client.

### 8. Parsers under test

Every parser of untrusted input has property tests (`tests/prop_*.rs`,
bounded, run by `cargo test`; `PROPTEST_CASES` raises them) and a libFuzzer
target (`fuzz/`, 18 targets over the native manifest, catalog and GitHub
release answers, the desktop-entry and D-Bus rewrites, the install plan, the
bundle unpacker on arbitrary bytes and on structurally valid hostile tars, key
files, `.flatpakref`, URLs and launch arguments, text cleaning, the AppImage
ELF and squashfs readers and their metadata, icons, the helper's answer and
AppStream metainfo), all sharing the invariants in
`tests/harness/checks.rs`. Seeds, dictionaries and the inputs that once
crashed a target are checked in; `.github/workflows/fuzz.yml` runs every
target for 15 s on a pull request that touches `crates/` or `fuzz/` and 120 s
weekly. The first runs found: `squash.rs` shifting by a file-controlled
amount, backhand's unchecked `offset + start` and a 4 GiB allocation from a
block size field (both now refused before backhand sees the image), a
redirect-target slice that could panic, and a trailing space in a search text.

## Build and supply chain

* **Hardening flags.** The RPM is built with Fedora's defaults (checked in the
  spec's macros): `_FORTIFY_SOURCE=3`, stack protector strong, stack clash
  protection, `-fcf-protection`, PIE, `-z relro -z now`, non-executable
  stack. `scripts/check-hardening.sh` (run by CI on the built RPM) fails the
  build when the installed binary lacks PIE, BIND_NOW, RELRO or a non-exec
  stack, or when the package holds a setuid/setgid/world-writable file.
  Observed: DYN with `NOW PIE`, `PT_GNU_RELRO`, GNU_STACK `RW`, no TEXTREL,
  fortify and `__stack_chk_fail` imported, SHSTK note; annocheck passes. Not
  available on stable Rust: stack protector and IBT notes for the Rust
  objects (reported, not enforced). The Rust part is compiled with `-C
  relocation-model=pic` and, new in the Secure phase, **`overflow-checks =
  true`** in the release profile, so an arithmetic overflow from file data is
  a contained panic instead of a silent wrap (cost to be measured in the
  Performant phase). Workers catch panics, and the AppImage inspector runs in
  its own process.
* **cargo-deny** (`audit.yml`, on dependency changes and weekly): advisories,
  bans (no OpenSSL / native-tls / aws-lc: rustls with `ring` only; wildcards
  denied), licenses (a short allow-list, each entry explained in `deny.toml`)
  and sources (crates.io and the two pinned git repositories). `cargo audit`
  was not added: it reads the same RustSec database as deny and is not packaged
  for Fedora.
* **Dependabot** (cargo and github-actions, weekly, grouped) and
  **CODEOWNERS** for `catalog/`, `.github/`, `packaging/` and `deny.toml`.
* **CI hygiene** (reviewed): `contents: read`, `persist-credentials: false`,
  actions pinned by commit, no `pull_request_target`, no secrets, no
  `${{ }}` inside `run:`. Recommended, not done: `ubuntu-24.04` instead of
  `ubuntu-latest`, and the Fedora container by digest.
* **Dependency review**: `docs/DEPENDENCY-REVIEW.md` lists every direct
  dependency and the notable transitive ones (licence, purpose, whether it
  parses untrusted input, `unsafe`, features). New for this phase:
  `minisign-verify =0.3.0` (no dependencies, no `unsafe`, MIT; read in full),
  `proptest` (dev only), and, only behind the `test-hooks` / `fake-github`
  features, `ed25519-compact` and `blake2` (the fake GitHub's test signer; not
  compiled into the package).

## Not done, deliberately

* **Freshness.** No signed timestamp or minimum version: a first install can
  be offered an old signed release that is still "latest", and an attacker who
  blocks or replays GitHub answers can hold a computer on an old signed
  release. Needs a catalog `min_version` or a signed timestamp (product
  decision).
* **A compiled-in root key** that signs the catalog (see section 2).
* **Sigstore verification** (section 2).
* **Landlock / namespaces** for `gpgv` and the inspector, beyond the syscall
  filters.
* `ProtectSystem`/`ProtectHome`/`SystemCallFilter` on the AppImage unit
  (needs a real user manager to verify).
* A same-user process can forge entries it could write anyway (it can already
  write `~/.local/share/applications` and `~/.bashrc`); the Store makes sure
  it cannot turn *its* privileges into more.
