# Telamon Store: design

What this file fixes: the layout, the threading rule, what is trusted, who
owns what, and the budgets. Change it together with the code that changes
them. The full plan and its reasons are the Atlas Notes note
"AtlasOS/Store/Plan".

## Scope

The Store replaces KDE Discover on Telamon OS, a bootc image with no PackageKit:
Discover's only backends there are Flatpak and fwupd.

| Discover today | Who covers it |
|---|---|
| Browse, search, app pages (all enabled remotes, Flathub first) | Store |
| Install, remove, update Flatpaks, system and user, with add-ons | Store (updates run Telamon Updater's engine, see "App updates") |
| Sources: remotes, enable, add from `.flatpakrepo` (file or link), remove (priority is not editable) | Store |
| `application/vnd.flatpak.ref`, `.repo`, `vnd.flatpak` bundles | Store |
| `application/x-rpm` | Store explains that RPMs aren't installed on Telamon OS and points to toolbox |
| `appstream:` and `flatpak+https:` links (Kicker, KRunner) | Store |
| AppImages (not in Discover): noticed in Downloads, looked into without running, installed for the user after a plain warning | Store (see "AppImages") |
| Telamon's own apps that are not in the image (Telamon Gates is the first): installed for the user from a GitHub release, updated by the Store | Store (see "Native Telamon apps") |
| Background update checks, automatic updates, notifications | Telamon Updater (unchanged) |
| Firmware (fwupd) | Telamon Updater |
| Launcher and menu entries, `mimeapps.list`, removing Discover | The Telamon OS image |

No ratings or reviews (no ODRS). The extra network services are the Flathub
API (`flathub.org/api/v2`) for the lists on Home and the category pages (see
"Home and category lists"), and GitHub for the Telamon apps (see "Native
Telamon apps": the catalog from `raw.githubusercontent.com`, the latest
release from `api.github.com`, its files from `github.com`), while the Store
is open. A `.flatpakrepo` link the user pastes into Add Source is the only
other thing the Store fetches itself.

## Layout

- `crates/telamon-store-core`: no Qt. Launch arguments, AppStream parsing and
  the on-disk index, search, flatpakref and flatpakrepo parsing, AppStream
  markup to blocks, the image fetcher and cache, the Flatpak job queue, the
  AppImage install code and the native Telamon apps (`native/`).
- `apps/telamon-store`: the CXX-Qt backend (`src/backend.rs`), `cpp/main.cpp`
  (Qt start, single instance) and `qml/`.

## Threads

The GUI thread never blocks. Parsing, search, libflatpak calls, downloads and
image decoding run on worker threads and hand results back with
`qt_thread().queue`. One Flatpak job runs at a time; each takes Telamon Updater's
lock (`$XDG_RUNTIME_DIR/telamon-updater-apps.lock`, flock; also `atlas-updater-apps.lock`, see
"Names before the rename") blocking, on its
worker, and the Store shows "Another update is running" while it waits.

## Sources

`flatpak/remotes.rs` (core) and `src/sources.rs` (one worker, one job at a time;
jobs that change something take the `OperationLock`). The list shows both
installations (reads open with no interaction; an unreadable system
installation does not hide the user one). A source file comes from
`fetch_repo` (`net::get`: https, public addresses, 256 KiB, 15 s) or
`read_repo_file` (the launch path rules, a regular file of at most 256 KiB),
then `parse_flatpakrepo`. `preview_repo` builds the "Add Source" confirmation
before anything is added: title, address, key fingerprint or "not signed", the
free name for each installation, and whether the address is already a source.
`confirmAdd` adds exactly the previewed file's own rewrite (`add_source`); a
file without a key is added with signature checking off, only after the
dialog's extra acknowledgement. It then refreshes the new source's app list; a
failed refresh keeps the source. `remove_source` refuses, naming them, while any
app or runtime from that remote is installed; it never forces and needs the URL
the user saw. Enabling or disabling only flips the remote's disabled flag.
System changes go through flatpak's polkit helper. After any change the window
reloads the catalog and the installed list.

## App updates

The Updates place drives Telamon Updater's engine (`telamon-updater-core`,
pinned to the revision Telamon Settings uses; `apps` and `apphistory`) from
one worker thread, so the Store, the tray and Settings share the logic, the
history file and the lock. It holds the Store's `OperationLock`, which includes
the Updater's apps lock, and never calls the engine's own `lock::take`. A check
is `apps::list(refresh)` then `apps::check`; an update is the `apps::unseen`
guard, `apps::update`, `apphistory::record`, then a fresh list. Updates that
ask for new permissions show them on their row and are never installed
unseen: when any waiting update asks for new permissions, Update All first asks
in the Store's own dialog (every such app with its full list, Cancel the
default, input ignored for 0.5 s), which also offers "Update Without These"
(the engine's `hold_new_permissions`). A check starts only when the page is opened with no list this session
or one older than 10 minutes, or when the user asks. The engine updates all
or nothing, so there is no per-app Update, and Cancel works only while waiting
for the lock (a ref filter and a cancel token in `telamon-framework-flatpak`
and `apps::update` would add both). "Last checked" is the later of the Store's
own record (`$XDG_STATE_HOME/telamon-store/updates-checked`) and the Updater's
`RoundAt`. The settings (background updates on or off) stay in Telamon
Settings; quitting waits up to 10 s for a running update; "Update Settings" starts `/usr/bin/telamon-settings updates apps`.

## Home and category lists

Home shows Popular Apps, New & Updated and Editor's Picks; a category page
shows "Popular in <Category>" (hidden while a filter is on) above its grid.
They come from `collection/popular`, `collection/recently-updated`,
`app-picks/apps-of-the-week/<UTC date>` and `collection/category/<name>` of
the Flathub API. A request carries nothing but the list's path and a page
size. Each answer is capped at 1 MiB and parsed into app IDs only; IDs are
validated, deduplicated, capped at 64 and matched against the local AppStream
library, and the Store shows the local name, summary, developer and icon,
never API text. Each list is cached as
`$XDG_CACHE_HOME/telamon-store/flathub/<list>.json` with its fetch time (a 0700
folder, written atomically, never through a link, re-validated on read). A
list is refreshed when older than 24 h (recently-updated: 6 h) and only while
Home or a category page is shown; a failed fetch is not retried for 10
minutes; an expired file is still shown, so the lists work offline.

## Single instance

`main.cpp` uses `KDBusService::Unique`. A second launch hands its arguments
and working directory to the first, which raises its window (with the
launcher's activation token) and passes them to `Backend.activate`. The
arguments are parsed in Rust (`telamon_store_core::launch`), never in C++ or
QML:

- `--app <id>`, `--search <text>`, `--page home|installed|updates|sources`
- `--remove <id>`: the app's page with its Remove confirmation open (the
  launcher's Uninstall; it starts `telamon-store --remove <id>` with
  `XDG_ACTIVATION_TOKEN` set). The user still confirms there.
- `--appimage-install <file>`: the install confirmation for an AppImage (the
  notification's Install and Show in Store buttons; the file's name does not
  matter, its bytes are checked)
- `--install-bundle <file>.tar.zst`: the install confirmation for a native
  Telamon app bundle you built yourself, to try it before publishing (see
  "Native Telamon apps"). Only this option opens a bundle: a `.tar.zst` given
  as a plain argument is refused.
- `appstream://<id>`, `appstream:<id>`, `flatpak+https://...`
- `.flatpakref`, `.flatpakrepo`, `.flatpak`, `.rpm` and `.AppImage` paths or
  `file:` URLs (a file with another name that starts like an AppImage is
  taken too, which is how the file manager hands over `application/vnd.appimage`)

Two more options never reach the window: `main.cpp` hands `--appimage-check
<folder>` and `--appimage-inspect <file>` to Rust before Qt starts
(`telamon_store_early`, read with `launch::internal_path`: exactly one plain
absolute path), and they run to completion with no window and no
single-instance service. Sent to a running Store (`Open`, a second launch)
they are "unknown option" like any other.

Anything else is refused with a reason, logged, and shown in the window as
plain text. Per launch at most 64 arguments are read, 8 requests acted on and
8 refusals listed. A bare relaunch (the launcher icon) only raises the window.
`org.freedesktop.Application.Open` (any process in the session) is handled
the same way, after a `--`, so nothing it sends is read as an option, and
with no working directory, so relative paths are refused. File paths must be
plain (no `..`, `//`, `/./` or hidden characters). `https` links are checked
by name: a DNS host with a letters-only top level that isn't kept for local
networks, tests or Tor (no IP address, `localhost`, `.local`, `.lan`,
`.internal`, `.home.arpa`, `.onion`...), port 443, no user name, plain ASCII,
no `.` or `..` path segment. International names arrive as `xn--` punycode
and are shown that way. A name can still resolve to a local address, so the
fetcher refuses loopback, private and link-local addresses when it connects
and runs every redirect through the same check. Refusals show a link without
its user name, query or fragment, and an option without its value.

Once requests open dialogs: one launch opens at most one confirmation (the
last request that needs one), a new launch never replaces a dialog the user
is answering, the dialog's default button is never Install, Add or Remove,
and it ignores input for its first half second.

## Trust

The threat model (attackers, defenses, tests, what is not done) is `docs/SECURITY.md`.

Everything from the network or a file is untrusted, and checked where it
enters, in `telamon-store-core`:

- AppStream XML: size cap before parsing, depth and count limits, text
  cleaned of control and bidi characters, length caps, IDs validated.
- AppStream markup becomes blocks of plain text. Nothing is shown as QML
  RichText or HTML. Links open through TelamonPortal, https only. The QML is
  held to that by `scripts/check-qml-plaintext.sh` (`cargo test` runs it
  too): every `Text` and `Label` sets `textFormat: Text.PlainText`; no
  `Qt.openUrlExternally`, `Qt.createQmlObject`, `eval` or `XMLHttpRequest`; an
  `Image` source is a `file:` URL (the backend's `iconSource`) or a literal.
  The one place Telamon.Ui shows data as Qt's default `AutoText` is the
  navigation stack's header, which shows a page's title: a name like
  `<img src=...>` would be fetched as an image. `AppPage` and `NativeAppPage`
  pass their title through `headerTitle()`, which swaps `<`, `>` and `&` for
  look-alike characters, and the script fails on a data-built `TelamonPage`
  title without it.
- "Verified" is Flathub's word: an app is marked Verified (and counted by the
  verified filter) only when its catalog is Flathub's own (`flathub` or
  `flathub-beta`); the same custom values in another remote's AppStream mean
  nothing.
- Images (screenshots, remote icons): https, an allowlisted host per remote
  (Flathub: `dl.flathub.org`), 15 s timeout, 8 MB cap, redirect cap, magic
  bytes and a pixel cap checked, decoded off the GUI thread, stored in a 200 MB
  LRU disk cache. QML only ever gets `file:` URLs.
- Flathub API JSON: size cap, schema checked, every ID matched against local
  AppStream before it is shown.
- flatpakref and flatpakrepo: read as GLib key files with limits (256 KiB,
  4096 lines, 64 KiB per value), known keys only, translated and unknown keys
  refused, https only, the GPG key parsed and its fingerprint shown,
  `RuntimeRepo=` never followed without its own confirmation
  (`Error::NeedsRuntimeRepo` names it). libflatpak never gets a
  `.flatpakref` at all: it adds the file's remote while planning, even if the
  install is then abandoned. The Store resolves the source itself
  (`resolve_ref_source`): an enabled remote with the same normalized URL is
  used as it is and the file's key is ignored; otherwise a `RemoteProposal`
  (name, URL, key fingerprint, flatpak's origin-remote settings) goes to an
  "Add Source" confirmation, and `add_ref_remote` builds the remote from the
  proposal's own fields. A `.flatpakrepo` goes to libflatpak only as the
  Store's own `to_bytes()` rewrite.
- Cache files live under `~/.cache/telamon-store` (0700), written atomically,
  files that are symlinks refused, and the cache folder itself must not be
  one (folders above it may be, for a moved `~/.cache`). A cache folder that is not
  the user's own is not used; one that group or others can write to (Fedora's
  umask 002) is set back to 0700 with a warning.
  The index's checksum catches corruption, not tampering: a process running
  as the user can change any of the user's files, so the reader treats the
  index as untrusted input like the XML. It repeats the parser's per-field
  checks (characters, IDs, URLs, icon files, bundle references, runtimes and
  SDKs, text lengths and whitespace, the bundle matching the component, a
  bundle where one is required, no duplicate IDs), caps sizes and the total
  decoded, and anything wrong means a rebuild. Its caps are the parser's own
  (derived from `Limits` and the constants beside it, so they cannot drift),
  and the parser stops, with an error, when what it keeps would not fit the
  index's file and decode caps. It cannot re-check what only the XML shows
  (such as which of several languages was chosen).
  The header holds the layout `FORMAT` (bumped on any change to `Catalog`,
  `Component` or the encoding) and the `PARSER_REV` (bumped when the parser
  returns something different for the same XML); either one changing means a
  rebuild. A read never changes the cache folder (a group- or other-writable
  one is refused, and the next write sets it to 0700); no file is `Missing`,
  not a fault. After a write only index files last written before it began are
  removed, so a slower writer cannot delete a newer index.

**Confirmation:** nothing is installed, removed or added as a source without
the Store's own dialog, showing the app, the remote, sizes and permissions.
Fedora's polkit gives wheel members Flatpak installs without a password, so
polkit is not the confirmation. System-wide changes go through flatpak's own
polkit helper; the Store has no helper and no polkit actions of its own, and
asks for no privilege itself. What the helper asks polkit for (flatpak 1.18,
Fedora's `org.freedesktop.Flatpak.rules`): an install (`app-install`,
`runtime-install`), an uninstall (`app-uninstall`, `runtime-uninstall`) and
the repository upkeep around them (`modify-repo`) need no password for a
wheel member in an active local session; adding, removing, enabling or
disabling a system-wide source (`configure-remote`) always asks for one;
updates (`app-update`, `runtime-update`) and the refreshes of AppStream and
summaries (`appstream-update`, `metadata-update`, `update-remote`) need
none for anyone in an active session. The scope of an operation is never
read from a file or a link: it is the installation of the catalog entry's
remote, of the installed ref, or the one the person picked in a dialog. A
removal names its installation as well as its ref (the same ref can be
installed in both), so the one the dialog showed is the one removed.
A file's source is its own step: "Add Source" (remote, URL, key fingerprint or
a warning that it is unsigned) comes before the install confirmation, and a
declined install can take the source back (`remove_remote`: exact name and
URL, nothing installed from it). An install runs only what its confirmed plan
lists: the source is re-checked (same URL and signing, still enabled) and a
fresh transaction is compared with the plan before anything downloads, else
`PlanChanged` says what differs and the user is asked again. Uninstalling an
app deletes its data only on request, for the current user, never while it
runs (the dialog then offers "Close and Remove": SIGTERM, 3 s, SIGKILL, on the
worker, after the user confirms) and never while another branch of it is installed; a runtime an
installed app uses is refused (only "remove unused" removes runtimes).

**Permissions shown:** the install dialog lists everything flatpak applies
from the app's metadata, read the way flatpak reads it (`permissions.rs`);
what the Store does not know, whether flatpak reads it or not, is shown as
unknown and High, never dropped. `host-root` (new in flatpak 1.18) and raw USB
access (`devices=usb` binds all of `/dev/bus/usb`) are High. A runtime's own
permissions are not inherited by its apps in flatpak (only environment
variables are), so the dialog lists the app's metadata alone.

## Opening an app

Open asks the window system for an XDG activation token on the GUI thread
(`cpp/activation_token.cpp`, KWaylandExtras, a signal and a 1 s timeout, none
on X11), then the worker runs `flatpak run --user|--system --arch --branch -- <id>`
(the program is `/usr/bin/flatpak`, or `/bin/flatpak`, never found through
`PATH`, which in a user session holds folders any process of the user can write
to; the ID never starts with a dash and follows `--`)
with `XDG_ACTIVATION_TOKEN` and `DESKTOP_STARTUP_ID` set, in its own process
group, and does not wait for the app. Without the token Wayland can leave the
app's window behind the Store.

## Lifetime

The Store does nothing when closed: no timer, autostart, D-Bus activation or
notification. AppStream is refreshed only while it is open (when older than
6 h), on a worker, while the cached data is shown.

The one exception is the AppImage notice (see "AppImages"): a systemd user path
unit holds an inotify watch on `~/Downloads`, and when that folder changes
starts a short-lived `telamon-store --appimage-check` that exits when it has
told the user (or after at most 10 minutes). No daemon, no timer, no polling
while nothing changes, no network.

## Budgets

| What | Budget |
|---|---|
| Cold start to a usable home (cached index) | ≤ 500 ms here, ≤ 1.2 s on the T480 profile |
| First run (index built from 50 MB XML) | ≤ 2 s to home |
| Search, keystroke to results painted | ≤ 50 ms (core query ≤ 5 ms) |
| Scrolling a 3,000-app grid | no frame over 16 ms |
| RSS | ≤ 200 MB on home, ≤ 260 MB after 200 app pages |
| Idle CPU, window open | 0 % |
| Disk | index ≤ 15 MB, screenshots ≤ 200 MB |
| RPM | ≤ 15 MB |

## AppImages

An AppImage is a program in one file. It is not sandboxed, it runs as the user
and can read and change all the user's files, use the network and see
everything the user can, and Telamon does not check it. The Store never calls
one "safe": the first line of every confirmation is "This app isn't sandboxed
and isn't checked by Telamon", and "What It Can Access" says what that means.
It is the Store's job to say what it can find out, in plain words, and to
install only after the user answers in its own dialog.

**Facts about the format** (`appimage/format.rs`, `squash.rs`). A type 2 file
is an ELF runtime followed by a squashfs; the squashfs starts at the end of the
ELF (`e_shoff + e_shentsize * e_shnum`). The marker `AI` and the type byte are
at offset 8 of the ELF header (`AI\x02` type 2, `AI\x01` type 1, an ISO 9660
payload). The ELF sections `.sha256_sig` (1 KiB) and `.sig_key` (8 KiB) hold
the signature and the signer's key, all zeros when unsigned. A type 1 file is
recognized and not parsed: it gets the strongest warning ("can't look
inside"). A file is an AppImage by its first bytes, never by its name.

**Looking inside never runs it.** The Store reads the squashfs with the
`backhand` crate (gzip, xz and zstd; MIT or Apache-2.0; no `parallel`), never
mounts it and never uses `--appimage-extract` or any other `--appimage-*`
option. Everything in the file is untrusted and is capped where it is read:
the superblock is checked before the reader gets it (version, block size,
counts, offsets, and the inode and directory tables are walked block by block
so a table of thousands of tiny blocks is refused before anything is
decompressed); only the top folder, `usr/share/metainfo`, `applications`,
`pixmaps` and the hicolor icons are indexed; a file over its cap (2 MiB) is
refused, not cut; all reads together have a budget (8 MiB); links are
resolved inside the image only. From the desktop entry (parsed with the
Store's key file reader), the AppStream metainfo (parsed by the Store's own
AppStream parser, `parse_metainfo`: same limits and cleaning as a catalog) and
the icon the Store takes name, version, publisher, summary, an app ID and an
icon (a PNG up to 2048 px with a real header, or a small SVG: see below).
Texts go through `text::clean`; the embedded `Exec=` is never
used (the Store writes its own). All of it is shown as `Text.PlainText`.

**The SVG rule** (`meta::icon_kind`). The SVG is read tag by tag by a small
tokenizer, not searched for words: it is accepted only when the whole document
is understood, so what a real XML parser would read differently is refused.
The entities are the five predefined ones (no `&#..;` that could spell a
name, no DOCTYPE); the only declaration is `<?xml ...?>` first, with no
encoding but UTF-8 (a UTF-7 parser would read other tags); no element
`script`, `style`, `image`, `use`, `a`, `feImage`, `foreignObject`, `iframe`,
`animate`, `set`, `handler` and the like, whatever its prefix or case; no
event-handler attribute (`on...`), no `xml:base`, no backslash (a CSS
escape), `javascript:` or `data:`; every `href` is `#id` and every CSS
`url(...)` points at `#id`; one root, the `svg`; nesting at most 64 deep and
20,000 elements. The PNG header must be 13 bytes of `IHDR` with a bit depth
its colour type allows and the only compression and filter methods PNG has.

**The helper process.** Inspection runs in `telamon-store --appimage-inspect
<file>` (the same binary, before Qt starts) with `RLIMIT_AS` (its size at
start plus 1 GiB), `RLIMIT_CPU` 150 s, `RLIMIT_CORE` 0, `RLIMIT_FSIZE` 1 MiB
and no new privileges, started with an empty environment (but the runtime
folder, `TMPDIR` and the language), in a process group of its own, and a 180 s
timeout on the Store's side that kills the whole group. It prints one
line of JSON and then the icon's bytes; the Store reads at most 2 MiB of it
and treats it as untrusted again (`Inspection::sanitize`: texts cleaned again,
IDs, hash, host and fingerprint checked, an icon that is not an image
dropped). A helper that fails (out of memory, too long) means "Telamon
couldn't look at this file" and no Install.

It works in two stages (`appimage/sandbox.rs`, `inspect::prepare` and
`Prepared::finish`). Stage 1 needs more than the file: it closes every
descriptor but 0 to 2 (whatever the Store left open without close-on-exec),
opens the file, reads the ELF headers and the signature sections, hashes the
whole file (and the variant with the signature sections zeroed) and runs
`gpgv`, the one child it ever starts. Then it puts a **seccomp-bpf
allowlist** on itself (hand-written, no crate; `SECCOMP_RET_KILL_PROCESS` for
everything else, the architecture checked first; x86-64 only, elsewhere it
logs and goes on with the limits above) and only then does stage 2: the
squashfs walk, the decompressors (zlib, liblzma, libzstd) and the XML,
desktop-entry and icon parsing, and writes its answer. After the filter it can
read the file it holds (`read`, `pread64`, `lseek`, `fcntl` to duplicate or
read the flags of a descriptor), write to standard output and error, manage
memory (`brk`, `mmap` that is never executable, `munmap`, `mremap`, `madvise`),
get random bytes and end (`exit_group`, and `tgkill` of itself with `SIGABRT`
for `abort()`). It cannot open a file, run a program, make a socket, a
process or a thread, trace, signal another process, `ioctl`, `unlink`,
`rename`, load a program, or map executable memory. Each entry in the list
has its reason in a comment, found with `strace -f` under the tests. If the
filter cannot be installed on x86-64 the helper does not read the file's
contents (no Install). A bug in a parser that takes over the process gets the
file's author a process that can do nothing.

**What the user is told** (`appimage/trust.rs`, always, before Install). The
findings are `Info`, `Caution` or `Danger`; any `Danger` makes the whole box
red and the Install button the red kind:

- Signature (`sign.rs`): none in the file, "Not signed" (danger); present
  and good, "Signed by <fingerprint>, but this key isn't one Telamon knows"
  (caution: Telamon knows no keys, and a key that comes in the file proves
  nothing about who made it); present and wrong, "The signature is wrong (the
  file was changed)" (danger); present but not checkable (no `gpgv`, an
  unreadable key), danger. The check is what `appimagetool --sign` makes: a
  detached armored OpenPGP signature of the 64-character lowercase hex
  SHA-256 of the file with both sections zeroed. It runs `gpgv` (only `/usr/bin/gpgv` or `/bin/gpgv`: a `PATH` in a
  user session holds folders the user can write to) by argv with an empty
  environment, a keyring made of the embedded key alone in a 0700
  temporary folder that is removed after (a timeout included), and a 10 s
  timeout that kills its whole process group. It runs in a process group of
  its own with `RLIMIT_CPU` 20 s, `RLIMIT_AS` 1 GiB, `RLIMIT_FSIZE` 1 MiB,
  `RLIMIT_NOFILE` 64, no core file and no new privileges, and dies with the
  helper that started it (`PR_SET_PDEATHSIG`). `gpgv` has no configuration
  file, no agent and no network; its status output is read as it comes and at
  most 64 KiB of it. A signature that is not detached (`gpgv` says so) reads as
  wrong, never as signed. The user's keyring is never read or changed and the
  key is trusted nowhere.
- Where it came from (`origin.rs`): the browser's `user.xdg.origin.url`
  (or `referrer.url` when the origin is not a web address) read with
  `fgetxattr` from the file that was inspected, at most 2 KiB. `https` from a
  public host (any port): "The browser recorded <host> as where it came
  from" (info). `http`: danger, naming the
  host. None, or anything else: danger, "We can't tell where this file came
  from". Any program can write these attributes; they are a hint, not proof.
- A file that cannot be looked into (type 1, damaged, over a limit): danger.
- Flathub: when the file's AppStream ID, or its exact name (ignoring case and
  spacing, and only when one app has it), is in the local catalog from the
  `flathub` remote, "Get Flathub Version" is the main button and the default,
  and "Install AppImage Anyway" the other. Otherwise the default is Cancel.
  Nothing is fetched to decide this.

**Installing** (`appimage/install.rs`; the Store's dialog is the only way).
The file is copied (never moved) to a temporary name in `~/Applications`
(created 0755; refused when it is a link, not the user's, or writable by
everyone; refused too when its path has a control character or bytes that are
not text, which a menu entry would write as another path), hashed while
copying and compared with the hash the user was shown (a changed file is not
installed), set 0755 and renamed to `<Name>.AppImage` with `RENAME_NOREPLACE`.
`<Name>` is the app's name reduced to letters, digits, `.`, `_`, `-`. A name
that is taken is numbered (`-2`, `-3`); only the Store's own earlier install
of the same app (its entry carries the marker and the path) is replaced. The
icon goes to `$XDG_DATA_HOME/icons/hicolor/<size>/apps/appimage-<id>.png` (or
`scalable/.../.svg`), and last the desktop entry
`$XDG_DATA_HOME/applications/appimage-<id>.desktop` (a new one is renamed in
with `RENAME_NOREPLACE`, so a file that appeared since the check is never
replaced):
`Type=Application`, `X-Telamon-AppImage=true`,
`X-Telamon-AppImage-Path=<path>`, `X-Telamon-AppImage-Icon=<path under
icons>`, the Store's own `Exec` with every argument quoted by the Desktop
Entry rules (`%` as `%%`; tested to round trip). If the FUSE 2 library
(`libfuse.so.2`) is missing when installing, `Exec` is `env
APPIMAGE_EXTRACT_AND_RUN=1 <path>` (the app unpacks itself on every start; the
Telamon OS image should carry `fuse-libs` so AppImages start fast), and Open
makes the same choice when it starts the app.

**Installed, Open, Uninstall.** Installed lists the Store's AppImages (entries
that carry the marker, whose recorded path is a plain `*.AppImage` file
directly in `~/Applications`) below the Flatpak apps. Open runs the recorded
file directly (no shell, no arguments, an own process group, the activation
token only when valid ASCII, every descriptor above 2 marked close-on-exec
before it starts, and only a regular file of the user). Uninstall asks first, then removes exactly the
three files the entry records, each only if it is a regular file of the user
(never through a link; anything else is left and named), the entry last. An
entry that is not the Store's, or whose path or icon reference does not check
out, removes nothing.

**The Downloads watcher** (`check.rs`, `appimage_cli.rs`, `data/systemd`).
`telamon-store-appimage.path` (`PathChanged=%h/Downloads`, no trigger or start limit so a busy folder does not stop the watch; a path unit cannot
expand the XDG download folder, so another download folder is not watched)
starts `telamon-store-appimage.service` (oneshot: `telamon-store
--appimage-check %h/Downloads`, `NoNewPrivileges`, `AF_UNIX` only,
`MemoryMax=2G`, and the hardening that needs no namespace: `RestrictNamespaces`,
`RestrictSUIDSGID`, `KeyringMode=private`, `UMask=0077`, `LimitCORE=0`,
`TasksMax=64`; `systemd-analyze verify` is clean and its exposure score goes
from 8.0 to 6.9). The unit leaves out `ProtectSystem`, `ProtectHome`,
`PrivateTmp`, `ProtectKernel*`, `ProtectClock`, `ProtectHostname`,
`CapabilityBoundingSet`, `SystemCallFilter` and `MemoryDenyWriteExecute`: the
first ones set up namespaces that a user unit gets only through an implied
user namespace (a unit that failed to start would stop the notice, silently),
a user unit has no capability to give up, the filter would be inherited by the
helper that installs its own, and the binary links Qt. The RPM installs both in `/usr/lib/systemd/user` and
`90-telamon-store.preset` (`enable telamon-store-appimage.path`) in
`/usr/lib/systemd/user-preset`, and runs `%systemd_user_post`. A preset only
says what `systemctl preset` should do: the image must run `systemctl
--global preset telamon-store-appimage.path` (the RPM scriptlet does it at
image build when the package is installed there) for it to start at every
login. The check looks one level deep (at most 2000 entries) at regular files (not
links, pipes, devices or sockets, not hidden, not `.part`, `.crdownload`,
`.download`, `.partial`, `.opdownload`, `.tmp`, and no name with a control or
bidi character, which the notice could show as another name) that arrived in the last 15 minutes (at most 8 are waited for and
inspected per round) and are named `*.AppImage` or
start like one, waits until a file has not changed for 3 s (at most 2
minutes), inspects it through the helper, and remembers path, size,
modification time and SHA-256 in
`$XDG_STATE_HOME/telamon-store/appimage-seen.json` (folder 0700, file 0600,
atomic, at most 256 entries, a damaged file reads as empty, a link or another
user's file is refused) before it tells the user: if that cannot be saved,
nobody is told. The same file is never announced twice (same path, size and
time; or the same size, time and content under another name); a changed file
is. A run looks at the folder again after each round (at most 4 rounds, so files that
arrived while it ran, or past the 8, are found; the path unit does not start a
service that is still running), announces at most 4 files, starts no new file
after 8 minutes and ends itself after 10 (the unit's timeout is 11). The notification
(`org.freedesktop.Notifications` through `telamon-updater-core`'s notifier, event
`appimageFound` in `telamon-store.notifyrc`, so Plasma's settings apply) says
"Install <name>?" with plain text only (cleaned fields, markup escaped),
buttons Install, Not Now and Show in Store, and waits at most 60 s for one.
Install and Show in Store both start the Store with the install dialog open
(`systemd-run --user --collect --no-block telamon-store --appimage-install
<file>`, so the window is not killed with the service) and pass the
notification server's activation token when it sends one; the dialog is where
the user decides. `systemd-run` is the program in `/usr/bin` or `/bin` (never
found through `PATH`) and gets an empty environment but the runtime folder, the
session bus address and `HOME`, and the display variables it is told to copy.
Not Now remembers nothing beyond the de-duplication.

**What a file dropped in Downloads can and cannot do.** It is never run, and
nothing opens it but the bounded inspector: the check only `lstat`s names and
reads 16 bytes of a regular file (opened `O_NOFOLLOW|O_NONBLOCK` and `fstat`ed,
so a pipe or a device named `x.AppImage` is skipped and cannot block it), and
the inspector is the sandboxed helper above. A link, a pipe, a device, a
socket, a folder, a hidden file, a download in progress and a name with
control characters are skipped; a file swapped between the check and the
inspection is `fstat`ed again by the inspector (a device or a pipe is "not a
regular file", over 4 GiB is refused) and compared by size and time after;
nothing is announced unless the state can be kept. It can make the watcher
start the check many times (the path unit has no trigger limit on purpose),
each of which looks at no more than 2000 entries and exits at once when
nothing is new. It cannot make the Store install anything: only the user's
answer in the Store's own dialog does, and the install hashes the bytes it
copies against the hash the user was shown.


## Native Telamon apps

Telamon's own apps that are not part of the OS image (Telamon Gates is the
first) are installed for the user by the Store from the GitHub release of
their repository, and updated by it, with no Flatpak. System apps stay in the
image; an app is "native" when its owner connects it here. Core:
`crates/telamon-store-core/src/native/`; window: `apps/telamon-store/src/native.rs`
and the `Native*.qml` pages. The bundle format and the producer side (the tool
and the reusable workflow) are the framework's, `docs/BUNDLES.md` there; how to
connect an app is `docs/CONNECT-AN-APP.md`.

**Connecting an app is one line in `catalog/native-apps.json`** of this
repository (a pull request; no Store release). The Store fetches the file from
the main branch (`raw.githubusercontent.com`, through `net::get`), caches it for
6 hours and parses it:
`{ "schema": 1, "apps": [ { "id", "repo", "channel", "signers" } ] }`.
`repo` is `Owner/name` with an owner in `native::ALLOWED_OWNERS` (today
`EternalCoder454`; adding one is a Store release), `channel` is `releases`
(the latest published release that is not a draft or a prerelease), `signers`
is 1 to 4 keys that may sign the app's releases: `{ "type": "minisign", "key":
"RW..." }`, the public key as `minisign -G` writes it. A `type` the Store does
not know is ignored (see "Integrity"), a `minisign` key that cannot be read
skips the entry, and an entry left with no signer is skipped with the reason
"no signer": a release is never installed or offered without a signature check.
An entry that is malformed, for another owner, with another channel or listed
twice is skipped and logged, never half-used. At most 200 entries. `signers` is
an addition to schema 1, so `schema` stays 1; a Store from before it ignores
the field and the signature file, and keeps the behavior it was released with.

**What the Store checks about a release** (`github.rs`, `check.rs`): the
answer of `api.github.com/repos/<repo>/releases/latest` (1 MiB cap); a release
file counts only when its address is exactly
`https://github.com/<repo>/releases/download/<tag>/<name>`, so the files are
the catalog's repository's own and the same release's; the release has a
`telamon-bundle.json` (1 MiB cap), the outer manifest, and a
`telamon-bundle.json.minisig` (4 KiB cap) signing it, and the signature is
verified over the manifest's bytes, as downloaded, with the entry's keys
**before the manifest is read for anything** (see "Integrity"). A release with a
manifest and no signature is an error shown for that app ("not signed"). The
manifest's `id` must equal
the catalog entry's, its archive name must be `<id>-<version>-x86_64.tar.zst`
and exist in the release with the manifest's size (and GitHub's own `digest`,
when the API gives one, must equal the manifest's SHA-256), and its version
must match the tag (`v<version>` or `<version>`). A release older than the
installed version is ignored (see "Integrity"). Anything else is "no bundle
in this release" or an error shown for that app only; the others still list.
No release at all (404) is not an error: the app is simply not shown yet.
Answers (release, manifest, signature) are cached under
`$XDG_CACHE_HOME/telamon-store/native/` for 6 hours (a check by hand ignores
the cache); with no network the cache is used however old and the page says so.
The cache is read back through the same checks, signature included, against
the keys the catalog lists now: a cache file that was edited, or an answer
signed by a key the catalog has since dropped, is not used.

**What the Store checks about a bundle** (`manifest.rs`, `archive.rs`,
`desktop.rs`). The download is streamed to a private file (`net::download`,
https, public addresses only, capped at the size the manifest declares and
256 MiB), and its size and SHA-256 must equal the manifest's before it is
opened. Then every tar entry is checked before anything is written, and the
tar library only reads, it never unpacks: names are relative, plain, UTF-8, with
no `..`, `.`, empty or hidden-character parts, at most 32 parts deep
(`manifest::MAX_PATH_PARTS`, also for a link's target, so every tree the Store
unpacks can be walked and removed again), unique; only folders, regular
files and symbolic links exist (hard links, sparse files, devices, FIFOs and
anything else refuse the bundle); at most 40,000 entries, 512 MiB per file
(the size the tar library will read, so a PAX `size` counts, not the header's)
and 1 GiB unpacked, with the decompressor cut off at its cap and its window at
128 MiB (`window_log_max(27)`); what the tar library keeps in memory for an
entry (long-name and PAX headers) is capped at 1 MiB, and only zero padding may
follow the end of the tar. Everything is written through an open folder
(`native/dirfd.rs`): each file and folder is made by `openat`/`mkdirat` from its
parent's descriptor with `O_NOFOLLOW` (files `O_EXCL`, folders 0700), so a
name in the archive, or a link another process plants in the staging folder
meanwhile, cannot lead a write outside the tree. Files get the mode the manifest
says (0644, or 0755 when `executable`), never what the archive says; links are
created last, each must be relative with a target that stays inside the folder
and is then followed by hand, name by name, to something that exists inside it.
The unpacked tree must then be exactly what the inner manifest lists (same
files, sizes, SHA-256, same links), and the inner manifest must say what the
release's outer one says. The bundle must fit this system (`min_os_version`
against `/etc/os-release`, `min_telamon_ui` against `rpm -q telamon-ui`; a
minimum that cannot be looked up is not held against the app). All of this runs
in the install worker, so a failure leaves nothing: the private folder is
removed (also when the worker panics: the cleanup and the undo are `Drop`s).

**Integrity.** A release is used only when its `telamon-bundle.json` carries a
valid [minisign](https://jedisct1.github.io/minisign/) signature (Ed25519 over
the BLAKE2b-512 hash, `sign.rs`, the `minisign-verify` crate pinned to an exact
version) by one of the keys the app's catalog entry lists. The manifest names
the archive's SHA-256, size and name, so the signature covers the archive; the
download is then checked against the manifest as before. Enforced:

- The signature is verified over exactly the manifest bytes as downloaded,
  before the manifest is parsed or any of its content decides anything (only
  the release's file list is read to find the two files). A good signature by
  a key the entry does not list is a failure, as are a missing, oversize
  (over 4 KiB), non-UTF-8, truncated or edited signature file, and the legacy
  signature kind (`Ed`, the file itself signed). The trusted comment is signed
  but is never read: the version, file name and time it carries mean nothing.
- The signed manifest binds what the Store acts on: `id` must equal the
  catalog entry's (a manifest signed for app A is refused at app B's entry,
  even when one key signs both), the archive name, size and SHA-256 are the
  download's, and the tag must equal the version (a signature of an older
  version cannot be replayed under a newer tag, and a signature does not
  verify a different manifest).
- **No downgrade.** A release older than the installed version is never
  offered as an update, never shown as a candidate, and never installed: the
  check ignores it (logged), `install_candidate` refuses it before downloading,
  and `install_bundle` refuses a release over a newer version under the lock.
  So a validly signed old release served as "latest" does not roll an app
  back. (A bundle file the user opens is their own decision and may replace a
  newer version; the dialog says what it replaces.)
- The cache is verified again on every read (above).
- `install.json` records the key ID (16 hex digits, as `minisign` prints it)
  the release was installed on (`origin.signer`; absent for a local file and
  for installs from before releases were signed). The install dialog says
  "Signed with key <ID> that Telamon's list names for this app" in plain text;
  on an update whose signing key differs from the recorded one it says so
  (still allowed: the catalog lists the key, and rotating is a catalog
  decision). A file opened with `--install-bundle` is not signed: the dialog
  keeps its red warning, shows the SHA-256 and says it is not signed.
- The dialog never calls an app safe: it says the app is not sandboxed and runs
  as the user, as any program does.

**Trust roots.** Three places decide what the Store will install, and a
compromise of one is not a compromise of the others:

1. *The Store binary:* `native::ALLOWED_OWNERS` (which GitHub accounts a
   catalog entry may name; adding one is a Store release) and the signature
   code.
2. *The catalog on this repository's main branch:* which repository supplies
   an app, and which keys may sign it. It is fetched at run time, so a merged
   line is live; changes to it are pull requests to this repository.
3. *The app owner's signing key,* generated off the repository, kept in the
   release workflow's secrets (or on the owner's machine), never in a
   repository.

What each compromise can and cannot do:

| Compromised | Can | Cannot |
|---|---|---|
| The app's repository (code, workflows) without the signing key | Publish releases, which are refused (no valid signature); change the source of future releases | Get any release installed or offered. It can only deny updates. |
| A release asset (manifest, archive, signature replaced by an attacker) | Make the Store refuse the release (deny updates) | Make the Store install code:  the signature must verify the manifest, the manifest names the archive's hash. An older signed release can be re-served; the Store never installs one over a newer version |
| The CI secret (the signing key) | Sign and publish a malicious release that installs everywhere the key is listed | Do it silently for ever: the owner removes the key from the catalog (a pull request) and Stores stop trusting releases signed by it once they refetch the catalog (at most 6 hours, or at once on Check for Updates). Nothing installed earlier is removed. |
| A catalog pull request (merged) | Change which repository and which keys supply an app, so whoever holds a newly listed key can have a release installed | Add an owner outside `ALLOWED_OWNERS`, or get anything installed without a signature by a key the entry lists. A catalog change is as safe as the review of its pull request |
| `raw.githubusercontent.com` / the main branch | The same as a catalog change, for as long as it lasts | Anything on a computer whose Store has not refetched, and nothing outside the allowlisted owners |

Limits worth knowing: a first install of an app has nothing to compare with, so
a validly signed old release that is still the latest can be offered to a
computer that has none installed (it is no downgrade there); there is no
freshness signal (an attacker who can block or replay GitHub's answers can keep
a computer on an old signed release). Neither lets an attacker install code the
owner did not sign.

**Key rotation.** A catalog entry may list up to four keys. To rotate: add the
new public key to `signers` (pull request, live when merged), sign new releases
with it, and when the old key is no longer needed (or is lost or leaked) remove
it in a second pull request; Stores that refetch stop accepting what the old
key signs, including their cached answers. While both are listed either may
sign. Users updating across a rotation see "signed with a different key" in the
dialog; that is expected and not a refusal.

**Sigstore, later.** GitHub artifact attestations (Sigstore) were weighed
against pinned keys. Verifying one offline needs the Fulcio certificate chain,
a Rekor inclusion proof or signed entry timestamp, a pinned and rotating
Sigstore trusted root, and x509 and ECDSA code: too much code and moving
trust data to ship now. The extension point is in the catalog: `signers` is a
typed list, and `{ "type": "sigstore", ... }` entries (issuer, repository or
workflow identity) can sit beside `minisign` ones, because a `type` the Store
does not know is ignored. A Store that learns `sigstore` can then require it
in addition to, or instead of, minisign for entries that list it, without a new
catalog schema, and an older Store keeps verifying the minisign signers it
knows.

**Where things go** (`install.rs`, all under `$XDG_DATA_HOME`, never a
system path, no privilege):

```
telamon-apps/<id>/<version>/    the bundle's tree (bin/, share/...)
telamon-apps/<id>/current       link to <version>, switched in one rename
telamon-apps/<id>/install.json  the record: what the Store installed
applications/<id>.desktop       copied out of the bundle, Exec rewritten
icons/hicolor/<size>/apps/..    metainfo/..  dbus-1/services/..  knotifications6/..
```

What is copied out of a bundle, and under which names, is fixed in
`desktop.rs`: a bundle can only ever write files that carry its own app ID
(`<id>.desktop`, `<id>*.png|svg` icons, `<id>.metainfo.xml`, D-Bus services
named `<id>` or `<id>.*`, `telamon-<last part of the ID>.notifyrc`), because the user's folders
come before `/usr` in every search path and a bundle must not shadow anything
else. The same reason gives more rules, all checked before anything is written:

- **Nothing is copied over what is not the Store's own:** a file the Store did
  not put there, or that the user changed since (an update stops and says so),
  or a link.
- **Nothing shadows the system.** No file may be copied to a relative path that
  exists in any system data folder (`XDG_DATA_DIRS`, `/usr/share`,
  `/usr/local/share`, Flatpak's exports); an ID with a menu entry or a D-Bus
  service of that name there is refused; and the app ID and every D-Bus name the
  bundle declares must not be the `Name` of any system `dbus-1/services/*.service`
  file (whatever the file is called; at most 2,000 are read, 64 KiB each, and
  more than that is a refusal). An ID or D-Bus name below `org.freedesktop.`,
  `org.kde.`, `org.gnome.`, `org.gtk.`, `org.mate.`, `org.xfce.`, `org.flatpak.`,
  `org.fedoraproject.`, `org.mozilla.` or `com.canonical.` is refused, and so
  is an ID with fewer than three parts (`org.kde`).
- **The text files are read strictly.** A desktop entry, a D-Bus service or a
  notification file with a control character other than TAB, a `\r` that is not
  part of a line end, a hidden or line-separating Unicode character (the
  left-to-right and right-to-left marks, U+FE0F and the soft hyphen are allowed
  only in the value of a translated key, `Name[ar]=`), a line that
  starts with white space, a key that is not ASCII letters, digits and `-`
  (plus `[locale]`), or a repeated group or key is refused, not normalized:
  GLib, KDE's KConfig and the D-Bus daemon would not all read it the same way.
- **The desktop entry is rewritten.** The first word of every `Exec` (in any
  group) must be a plain program name that is in the bundle's `bin/`; it becomes
  the absolute path under `current/bin/` (quoted by the Desktop Entry rules; a
  folder name that cannot be quoted so that it reads back the same is refused),
  `TryExec` and `Path` are dropped, and so are the keys that make another
  component load or run something of the bundle's choosing or hand it
  privileges: `X-KDE-Library`, `X-KDE-ServiceTypes`, `X-KDE-Protocols`,
  `X-KDE-Init`, `Implements`, `X-KDE-SubstituteUID`, `X-KDE-Username`,
  `X-KDE-Wayland-Interfaces`, `X-KDE-DBUS-Restricted-Interfaces`, and the
  markers of other tools (`X-Telamon-*`, `X-Flatpak*`, `X-Snap*`, `X-AppImage*`,
  `X-KDE-PluginInfo*`); the Store adds `X-Telamon-Native-App` and `-Version`.
  Plain metadata is kept, `MimeType` included: a bundle can offer itself as a
  handler for a file type (the default stays the user's choice in
  `mimeapps.list`). That is accepted.
- **Notification files** may not run a command (`Execute`, `Action=Execute`) or
  write a log file (`Logfile`): the notification service would do it whenever
  the app notifies. Values are decoded as KConfig reads them (`\x65` is `e`)
  before they are looked at.
- **Metainfo** is read with quick-xml under limits (1 MiB, 32 levels, no
  DOCTYPE, no entity but the five predefined, no processing instruction, no
  encoding but UTF-8): exactly one `<component>` whose `<id>` is the app ID (or
  `<id>.desktop`), only the elements AppStream metainfo for a desktop app uses,
  each in its place (`desktop.rs`, `metainfo_children`; anything else, such as
  `<replaces>`, `<extends>`, `<bundle>`, `<pkgname>`, is refused with its
  name), a `<launchable>` only as `desktop-id` with the app's own
  `<id>.desktop`, and nothing provided that is not the app's own.
- **Icons** are decoded by Qt, in the Store and in the desktop shell: a PNG of
  at most 2048 pixels each way by its header, or a plain SVG without scripts,
  entities or references to other files (`appimage::meta::icon_kind`); the file
  extension must match. The icon the window shows is checked again when listed
  (a regular file of this user, at most 1 MiB, the same kind of check), through
  the open folders, then given to QML as a `file:` URL like a catalog icon.

Everything below `telamon-apps` goes through folders held open by descriptor
(`native/dirfd.rs`), not paths walked by name, because a process of the same
user with less privilege (a Flatpak app with access to the home folder) could
swap a folder for a link between a check and its use. `$XDG_DATA_HOME` and the
export roots (`applications`, `icons`, `metainfo`, `dbus-1`, `knotifications6`)
may be links (a dotfile manager makes them): they are opened following the link.
`telamon-apps` and everything below it is opened with `O_NOFOLLOW`, must belong
to the user, and `telamon-apps` and the staging folders are 0700 (nothing needs
other users); a link in their place, or in place of a folder below an export
root (`icons/hicolor`), refuses the install and names the path. A file the Store
replaces is swapped in with `renameat2(RENAME_EXCHANGE)` (or `RENAME_NOREPLACE`
when nothing should be there) and the old file, now under a temporary name, is
read and hashed through its descriptor: if it is not the content the Store wrote
the swap is undone (if a writer put a file at the name meanwhile, as an editor
saving does, that file is kept as `<name>.orig-<pid>`, never unlinked). A file
it removes is first renamed aside, hashed there and unlinked, or renamed back.
A file system that cannot do `RENAME_NOREPLACE`/`RENAME_EXCHANGE` (NFS, some
FUSE, vfat) gets a hard link plus unlink, or a check followed by a rename: the
same result with a short window the flags would not have. An error of the
file system is reported as such, not as "changed since the Store wrote it".
Temporary files a killed Store left (`.<name>.<pid>.<n>.tmp`, `.current.<pid>-<n>`,
only the exact pattern, only the user's regular files and links) are swept by
the next update and by an uninstall. A data folder that appears in
`XDG_DATA_DIRS` is not counted among the system's. Removing a tree never follows a link
(`unlinkat`/`O_NOFOLLOW` level by level). The app's program is started by its
path, as the menu does, after the same folders were checked; it gets no
descriptor of the Store's (`close_range(CLOSE_RANGE_CLOEXEC)`) and the
activation token only if it is a valid one. The app finds its own
data relative to its program (`../share/<id>`, the framework's convention);
the Store sets no environment. The program runs through the `current` link, so
an update needs no change to the menu entry.

**Update** (the same code as install): download the new version, verify it,
unpack beside the old one, write the copied files (each one remembered so it can
be put back), switch `current`, write the record, and only then tidy. The old
version stays until the next update (older ones go), so an app that is running
keeps every file it may read later; it needs a restart to use the new one. Any
failure before the record is written is undone in the reverse order: the copied
files are as they were, `current` still names the old version, the new folder is
removed (**rollback**; tested with a failure injected after each step).
**Uninstall** removes the files the record lists, each only if it still has the
content the Store wrote (an edited file is left and named), and the app's
folder, the record last: a removal that stops half way leaves an app that is
still listed and can be removed again, never a hidden one. The app's own data and settings are never touched. A tampered record
cannot reach other files: only paths under the five export folders pass, and the
content must match its recorded SHA-256. One install runs at a time (`flock` on
`telamon-apps/.lock`, opened `O_NOFOLLOW|O_CLOEXEC`).

**Local bundles** (`--install-bundle`): for trying a bundle before it is
published. The file is looked into without installing (unpacked into a scratch
folder under the cache, checked, removed), and the dialog says in red that it
did not come from Telamon's list and was not checked by Telamon, that it is
not signed, with its SHA-256. After the user's answer it installs like any bundle; an app installed
this way has no source and is updated only if the catalog also lists its ID.

**In the window.** Home shows a "Telamon Apps" shelf (and a Telamon Apps tile
among the categories, which opens the list of them) when any connected app has
a release; an app page has Install, Update, Open and Uninstall, each asking in
the Store's own dialog (Cancel is the default; input is ignored for the first
half second); Installed lists them below the Flatpak apps with Open and
Uninstall; Updates shows the ones with a newer release in a "Telamon Apps"
card with an Update button each. They do not go through Telamon Updater's
engine (it knows Flatpak only), so "Update All" is the Flatpak engine's. A
check runs when the window opens (the cache decides whether GitHub is asked),
when Updates is opened, when Check for Updates is pressed, and once a day while
the window stays open; nothing runs when the Store is closed. A launch's
request that comes while a check runs (a local bundle) waits for it.

**Not done yet.** Telamon Updater's tray and Telamon Settings' Updates page do
not know these apps, so no background notification or Settings entry; they
would need the engine (`telamon-updater-core`) to learn the catalog. Sigstore
attestations (above). A freshness signal for the "latest" release. More than
one release channel.

## Names before the rename (0.2.0)

The app was `atlas-store` (`net.eterneon.atlas.store`) until 0.2.0. Because
the apps move to the Telamon names one at a time, this release keeps what
another app or the image may still use, and takes both names of what it
shares. To be removed once every app has moved:

- **Locks** (`flatpak/lock.rs`). The Updater's apps lock is taken as
  `telamon-updater-apps.lock` and as `atlas-updater-apps.lock`; the Store's
  own operations lock as `telamon-flatpak.lock` and `atlas-flatpak.lock`.
  Either name held is "busy", so an Updater of either generation, and a Store
  of the old name, exclude this one.
- **Launch.** The package obsoletes and provides `atlas-store`;
  `/usr/bin/atlas-store` is a link to `telamon-store` (`atlas-store --app <id>`
  from an Updater that has not moved), and a hidden
  `net.eterneon.atlas.store.desktop` starts the new binary for what launches
  by the old desktop ID. The old icon name is a link to the new icon.
  The single-instance D-Bus name is the new app ID's (`KDBusService` takes it
  from the app ID): a process of the old name running next to a new one is not
  expected, since an image update applies at a reboot.
- **User data** (`legacy.rs`): `$XDG_CACHE_HOME/atlas-store` (the catalog
  cache) and `$XDG_STATE_HOME/atlas-store` (the pending-sources journal) move
  to `telamon-store` on first use, once, with one `renameat2(RENAME_NOREPLACE)`:
  atomic, never replaces, and a new folder that exists wins. A link or a file
  under the old name stays. The settings file, crash state and notification
  choices are the framework's (`~/.config/atlas-storerc` is copied to
  `telamon-storerc`).
- The index file's magic (`ATLASIDX`) and the seed of the OCI source hash are
  not names the user sees; they stay, so the moved cache is still valid.
