# Telamon Store (Telamon OS)

Rust + Qt 6.11 + Kirigami (CXX-Qt) app store for Telamon OS, a Fedora Kinoite 44
bootc image (repo `~/Documents/Projects/AtlasOS/AtlasOS`). It replaces KDE
Discover: Flatpak apps from every enabled remote (Flathub first), their
add-ons and updates, and the flatpak/appstream links and files Discover opens.
Read `docs/DESIGN.md` first: it fixes the layout, the threading rule, what
comes from the network and how it is checked, and who owns what. Change it
only together with the code that implements the change.
The plan and roadmap are the Atlas Notes notes "AtlasOS/Store/Plan" and
"AtlasOS/Store/Roadmap".

The stack, build and look are Telamon Monitor's
(`~/Documents/Projects/AtlasOS/AtlasOS Monitor`). When in doubt, do what it
does, except for what atlas-framework provides (startup, settings file,
logging, crash reports, Flatpak updates), which the Store takes from there.

## Hard rules

- **Build and test inside the `fedora:44` dev container**, never on the host:
  `scripts/dev.sh <command>`. The repo is at `/src`; all build output goes to
  `/work` (`~/.cache/claude-builds/telamon-store` on the host), never into the
  repo or `/tmp`. Use a separate target dir per agent or task
  (`CARGO_TARGET_DIR=/work/target/<name> scripts/dev.sh ...`).
- **Never touch a real Flatpak installation.** Tests and smoke runs set
  `FLATPAK_USER_DIR` and `FLATPAK_SYSTEM_DIR` under `/work/flatpak/<name>` and
  use the local test remotes; never the host's `/var/lib/flatpak` or
  `~/.local/share/flatpak`, and never Flathub for installs.
- **Never run the GUI on the user's display.** Smoke runs use
  `QT_QPA_PLATFORM=offscreen`, or `xvfb-run -a -s "-screen 0 1920x1080x24"`,
  inside `dbus-run-session`, with `XDG_*_HOME` pointed under `/work`. Real
  end-to-end tests happen in the Telamon OS test VM, which the AtlasOS session
  runs.
- **Everything from the network is untrusted**: AppStream XML and its markup,
  icons, screenshots, the Flathub API, flatpakrefs, flatpakrepos, bundles and
  remote definitions, and every launch argument. Parse it in Rust
  (`crates/telamon-store-core`), with limits, never as QML RichText or a
  remote URL handed to QML. Every `Text` or `Label` that shows catalog,
  file or launch text sets `textFormat: Text.PlainText`: Qt's default,
  `AutoText`, turns a decoded `&lt;b&gt;` into rich text. Downloads have a timeout and a size cap.
- **Nothing is installed, removed or added as a source without the user's
  confirmation in the Store's own dialog**, showing what will happen
  (permissions, size, the remote and its key). System-wide changes go through
  flatpak's polkit actions; nothing else asks for privilege.
- **No background work when closed**: no timer, autostart, D-Bus activation
  or notification. One exception, in docs/DESIGN.md (AppImages): a systemd
  user path unit on `~/Downloads` starts a short-lived `telamon-store
  --appimage-check` that may send one notification per new AppImage and
  exits; it never runs the file. Telamon Updater owns background checks, auto-updates,
  update notifications and firmware. The Store takes Updater's lock
  (`$XDG_RUNTIME_DIR/telamon-updater-apps.lock`, and for this release
  `atlas-updater-apps.lock`) from a worker before changing
  installations.
- **Security**: the threat model, what is enforced and where each defense is
  tested is `docs/SECURITY.md`; change it with the code. A release of a native
  app is installed only with a minisign signature from a key its catalog entry
  lists (`signers`).
- **Native Telamon apps** (`crates/telamon-store-core/src/native/`,
  `docs/DESIGN.md`, "Native Telamon apps"): everything from GitHub and every
  bundle is untrusted. Nothing is unpacked by the tar library; a bundle may
  write only files that carry its own app ID; the catalog
  (`catalog/native-apps.json`) is fetched at run time, so a line in it is live
  when merged: keep `docs/CONNECT-AN-APP.md` right. Tests use the fake GitHub
  (`native::fake`), never the real one. Screenshots:
  `scripts/dev.sh scripts/native-shots.sh`.
- **Telamon.Ui is the installed `telamon-ui` package** from atlas-framework
  (`~/Documents/Atlas Framework`, read-only from here).
  Never fork Telamon.Ui components into this repo: ask the "AtlasOS Framework"
  session. Pieces it hasn't shipped yet live in `qml/` with the requested API
  shape, and move upstream later.
- **The GUI thread never blocks.** Parsing, search, libflatpak calls,
  downloads and image decoding run on worker threads; results come back with
  `qt_thread().queue`.
- Tests assert invariants and use fixtures in
  `crates/telamon-store-core/tests/fixtures`, never this machine's Flatpaks.
- Commits are authored as
  `EternalHell <77252745+EternalCoder454@users.noreply.github.com>`. Commit
  only the paths you own (`git commit -- <paths>`). Don't push unless the
  lead asked.
- Licence: MIT. App ID `net.eterneon.telamon.store`. Wording follows KDE:
  Title Case buttons and titles, US spelling.

## Commands

| Task | Command (from the repo root on the host) |
|---|---|
| Format | `scripts/dev.sh cargo fmt --all --check` |
| Lint | `scripts/dev.sh cargo clippy --workspace --all-targets --locked -- -D warnings` |
| Tests | `scripts/dev.sh cargo test --workspace --locked` |
| App build | `scripts/dev.sh bash -c 'cmake -S apps/telamon-store -B /work/cmake/dev -G Ninja && cmake --build /work/cmake/dev'` |
| Smoke run | `scripts/dev.sh dbus-run-session -- env QT_QPA_PLATFORM=offscreen /work/cmake/dev/telamon-store` |
| Search smoke | `STORE_OFFLINE=1`-style offline container run: `scripts/dev.sh scripts/smoke-search.sh /work/cmake/dev/telamon-store` (types a whole word into Home's search with xdotool; fails if the field keeps only the first letter) |
| RPM | `podman run --rm --security-opt label=disable -v "$PWD":/src -v <framework rpms>:/telamon-rpms:ro -e TELAMON_LOCAL_RPMS=/telamon-rpms -v telamon-store-cargo:/root/.cargo/registry -v telamon-store-cargo-git:/root/.cargo/git -e CARGO_HOME=/root/.cargo registry.fedoraproject.org/fedora:44 /src/packaging/build-rpm.sh /src/out` |
| Native app screenshots | `scripts/dev.sh scripts/native-shots.sh` (builds with `-DTELAMON_STORE_FAKE_GITHUB=ON`, answers GitHub from recorded files, writes `/work/shots/native`) |
| Hardening of the built RPM | `scripts/check-hardening.sh <elf>...` (PIE, BIND_NOW, RELRO, non-exec stack; the spec's %check and CI's rpm job run it) |
| QML plain-text check | `scripts/check-qml-plaintext.sh` (no rich text, no remote sources, no unchecked titles; `tests/qml_plaintext.rs` runs it) |
| Supply chain | `cargo deny --locked check` (advisories, bans, licenses, sources; CI: audit.yml) |
| Fuzz the parsers | `fuzz/run.sh [seconds] [target...]` on nightly with cargo-fuzz (CI: fuzz.yml); the same invariants run as property tests in `cargo test` (`PROPTEST_CASES=100000` for a soak) |
| Telamon checks | `git -C ~/Documents/Atlas\ Framework archive v2.0.0 tools ui \| tar -x -C <dir>`, then `<dir>/tools/lint-app.sh apps/telamon-store` and `<dir>/tools/check-app-names.sh apps/telamon-store` |

`<framework rpms>` is the out dir of atlas-framework's `packaging/build-rpm.sh`:
no repository has telamon-ui. `scripts/dev.sh` builds
`localhost/telamon-store-dev:44` on first use, which needs
`TELAMON_LOCAL_RPMS=<dir>` holding them.

## Moving the atlas-framework pin

1. Change `tag` in `Cargo.toml`, then
   `scripts/dev.sh cargo update -p telamon-framework-ui -p telamon-framework-flatpak`.
2. Move the pin in `.github/workflows/ci.yml` (app-checks, its
   `framework-ref` and the framework RPM job): CI pins by the tag's commit
   SHA, with the tag in a comment (`gh api repos/EternalCoder454/atlas-framework/commits/vX.Y.Z --jq .sha`), and when the app uses something new in Telamon.Ui, `ui:`
   in `src/lib.rs` and `telamon-ui >=` in the spec (Requires and BuildRequires).
3. Rebuild the dev image against that release's RPMs.
4. Commit `Cargo.toml` and `Cargo.lock` together.
