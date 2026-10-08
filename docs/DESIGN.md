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
| Background update checks, automatic updates, notifications | Telamon Updater (unchanged) |
| Firmware (fwupd) | Telamon Updater |
| Launcher and menu entries, `mimeapps.list`, removing Discover | The Telamon OS image |

No ratings or reviews (no ODRS). The only extra network service is the Flathub
API (`flathub.org/api/v2`) for the lists on Home and the category pages (see
"Home and category lists"), while the Store is open. A `.flatpakrepo` link the
user pastes into Add Source is the only other thing the Store fetches itself.

## Layout

- `crates/telamon-store-core`: no Qt. Launch arguments, AppStream parsing and
  the on-disk index, search, flatpakref and flatpakrepo parsing, AppStream
  markup to blocks, the image fetcher and cache, the Flatpak job queue.
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

Everything from the network or a file is untrusted, and checked where it
enters, in `telamon-store-core`:

- AppStream XML: size cap before parsing, depth and count limits, text
  cleaned of control and bidi characters, length caps, IDs validated.
- AppStream markup becomes blocks of plain text. Nothing is shown as QML
  RichText or HTML. Links open through TelamonPortal, https only.
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
polkit helper; the Store has no helper and no polkit actions of its own.
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

## Opening an app

Open asks the window system for an XDG activation token on the GUI thread
(`cpp/activation_token.cpp`, KWaylandExtras, a signal and a 1 s timeout, none
on X11), then the worker runs `flatpak run --user|--system --arch --branch <id>`
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
told the user (or after at most 6 minutes). No daemon, no timer, no polling
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
icon (a PNG up to 2048 px, or a small SVG without scripts, entities, `<use>`
or `<image>`). Texts go through `text::clean`; the embedded `Exec=` is never
used (the Store writes its own). All of it is shown as `Text.PlainText`.

**The helper process.** Inspection runs in `telamon-store --appimage-inspect
<file>` (the same binary, before Qt starts) with `RLIMIT_AS` (its size at
start plus 1 GiB), `RLIMIT_CPU` 150 s, `RLIMIT_CORE` 0, `RLIMIT_FSIZE` 1 MiB
and no new privileges, and a 180 s timeout on the Store's side. It prints one
line of JSON and then the icon's bytes; the Store reads at most 2 MiB of it
and treats it as untrusted again (`Inspection::sanitize`: texts cleaned again,
IDs, hash, host and fingerprint checked, an icon that is not an image
dropped). A helper that fails (out of memory, too long) means "Telamon
couldn't look at this file" and no Install.

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
  SHA-256 of the file with both sections zeroed. It runs `gpgv` by argv with
  an empty environment, a keyring made of the embedded key alone in a 0700
  temporary folder that is removed after, and a 10 s timeout. The user's
  keyring is never read or changed and the key is trusted nowhere.
- Where it came from (`origin.rs`): the browser's `user.xdg.origin.url`
  (or `referrer.url` when the origin is not a web address) read with
  `fgetxattr` from the file that was inspected, at most 2 KiB. `https` from a
  public host: "Downloaded from <host>" (info). `http`: danger, naming the
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
(created 0755; refused when it is a link or not the user's), hashed while
copying and compared with the hash the user was shown (a changed file is not
installed), set 0755 and renamed to `<Name>.AppImage` with `RENAME_NOREPLACE`.
`<Name>` is the app's name reduced to letters, digits, `.`, `_`, `-`. A name
that is taken is numbered (`-2`, `-3`); only the Store's own earlier install
of the same app (its entry carries the marker and the path) is replaced. The
icon goes to `$XDG_DATA_HOME/icons/hicolor/<size>/apps/appimage-<id>.png` (or
`scalable/.../.svg`), and last the desktop entry
`$XDG_DATA_HOME/applications/appimage-<id>.desktop`:
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
token only when valid ASCII). Uninstall asks first, then removes exactly the
three files the entry records, each only if it is a regular file of the user
(never through a link; anything else is left and named), the entry last. An
entry that is not the Store's, or whose path or icon reference does not check
out, removes nothing.

**The Downloads watcher** (`check.rs`, `appimage_cli.rs`, `data/systemd`).
`telamon-store-appimage.path` (`PathChanged=%h/Downloads`; a path unit cannot
expand the XDG download folder, so another download folder is not watched)
starts `telamon-store-appimage.service` (oneshot: `telamon-store
--appimage-check %h/Downloads`, `NoNewPrivileges`, `AF_UNIX` only,
`MemoryMax=2G`). The RPM installs both in `/usr/lib/systemd/user` and
`90-telamon-store.preset` (`enable telamon-store-appimage.path`) in
`/usr/lib/systemd/user-preset`, and runs `%systemd_user_post`. A preset only
says what `systemctl preset` should do: the image must run `systemctl
--global preset telamon-store-appimage.path` (the RPM scriptlet does it at
image build when the package is installed there) for it to start at every
login. The check looks one level deep at regular files (not links, not
hidden, not `.part`, `.crdownload`, `.download`, `.partial`, `.opdownload`,
`.tmp`) that arrived in the last 15 minutes and are named `*.AppImage` or
start like one, waits until a file has not changed for 3 s (at most 2
minutes), inspects it through the helper, and remembers path, size,
modification time and SHA-256 in
`$XDG_STATE_HOME/telamon-store/appimage-seen.json` (folder 0700, file 0600,
atomic, at most 256 entries, a damaged file reads as empty, a link or another
user's file is refused) before it tells the user: if that cannot be saved,
nobody is told. The same file is never announced twice (same path, size and
time; or the same size, time and content under another name); a changed file
is. At most 2 notices per run. The notification
(`org.freedesktop.Notifications` through `telamon-updater-core`'s notifier, event
`appimageFound` in `telamon-store.notifyrc`, so Plasma's settings apply) says
"Install <name>?" with plain text only (cleaned fields, markup escaped),
buttons Install, Not Now and Show in Store, and waits at most 60 s for one.
Install and Show in Store both start the Store with the install dialog open
(`systemd-run --user --collect --no-block telamon-store --appimage-install
<file>`, so the window is not killed with the service) and pass the
notification server's activation token when it sends one; the dialog is where
the user decides. Not Now remembers nothing beyond the de-duplication.


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
