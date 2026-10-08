# Telamon Store

The app store of [Telamon OS](https://github.com/EternalCoder454/AtlasOS). It
finds, installs, updates and removes Flatpak apps from Flathub and your other
sources, and replaces KDE Discover.

- Browse Flathub's picks, categories and search, with screenshots, release
  notes, size, licence, age rating and the verified developer.
- See what an app may access before you install it and after.
- Install for everyone (administrators) or just for you; add-ons; updates;
  removing unused runtimes.
- Opens `appstream:` and `flatpak+https:` links and `.flatpakref`,
  `.flatpakrepo` and `.flatpak` files, always asking first.
- Installs Telamon's own apps that are not in the OS image (Telamon Gates)
  for you from their GitHub releases, with no Flatpak, and updates them. An
  owner connects an app with one line in `catalog/native-apps.json`:
  `docs/CONNECT-AN-APP.md`.

Nothing runs when the Store is closed: Telamon Updater checks for updates in the
background.

Built with Rust, Qt 6 Quick and Kirigami on
[atlas-framework](https://github.com/EternalCoder454/atlas-framework). See
`CLAUDE.md` for how to build and test it, and `docs/DESIGN.md` for how it
works.

Licence: MIT.
