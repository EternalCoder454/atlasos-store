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
- `appstream://<id>`, `appstream:<id>`, `flatpak+https://...`
- `.flatpakref`, `.flatpakrepo`, `.flatpak` and `.rpm` paths or `file:` URLs

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
