# Connecting a Telamon app to the Store

A Telamon app that is not part of the OS image (Telamon Gates, say) is not
installed by the system and has no Flatpak. Connect it to the Store and users
install it with one button, and it updates from the Store's Updates page.

Three steps, in this order. Nothing in the Store itself changes.

## 1. Add the bundle workflow to the app's repository

Create `.github/workflows/bundle.yml` in the app's repository:

```yaml
name: Bundle

on:
  push:
    tags: ['v*']
  workflow_dispatch:

permissions:
  contents: read

jobs:
  bundle:
    permissions:
      contents: write   # attaches the bundle to the release
    uses: EternalCoder454/atlas-framework/.github/workflows/bundle.yml@FRAMEWORK_SHA # v2.0.3
    with:
      framework-ref: FRAMEWORK_SHA # v2.0.3
```

`FRAMEWORK_SHA` is the 40-character commit of the framework release:
`gh api repos/EternalCoder454/atlas-framework/commits/v2.0.3 --jq .sha`. Pin by
commit, as the other workflows do.

The workflow builds the app in the same `fedora:44` container its CI uses,
against `telamon-ui` from that framework release, and makes
`<app-id>-<version>-x86_64.tar.zst` and `telamon-bundle.json`. Your app needs
only what any Telamon app has: the CMake project under `apps/<name>`, its
`.desktop` file named `<app-id>.desktop` whose `Exec` is the bare program name,
a metainfo file, and an icon named after the app ID. (The app template has all
of it; the rules are in the framework's `docs/BUNDLES.md`.) If the app reads
files at run time, it finds them next to its program, at `../share/<app-id>/`:
nothing sets an environment for it.

Try it before publishing: run the workflow by hand (Actions, Bundle, Run
workflow), download the artifact, and install it with
`telamon-store --install-bundle <file>.tar.zst`. The Store looks inside without
running anything, says in red that the file is not from Telamon's list, and
installs it after you answer.

## 2. Tag a release

The version in the app's CMake `project()` must equal the tag without the `v`:

```
git tag v0.1.0
git push origin v0.1.0
```

The workflow runs on the tag and attaches the two files to the GitHub release
of that tag (it creates the release when there is none). Check that the release
lists both `telamon-bundle.json` and the `.tar.zst`. A new tag is a new version:
users get it as an update the next time the Store checks.

## 3. Add one line to the catalog

In this repository, add an entry to `catalog/native-apps.json` and open a pull
request:

```json
{ "id": "net.eterneon.telamon.gates", "repo": "EternalCoder454/telamon-gates", "channel": "releases" }
```

`id` is the app ID (the same as in the bundle's manifest; the Store refuses a
release whose manifest says another), `repo` is `Owner/name`, `channel` is
`releases`. Merged, it is live: the Store reads this file from the main branch
at run time (and keeps it for 6 hours), so there is no Store release to wait
for. Home then shows the app under Telamon Apps.

Only owners the Store knows are accepted (`ALLOWED_OWNERS` in
`crates/telamon-store-core/src/native/mod.rs`, today `EternalCoder454`). Another
account is a Store release that adds it. A test (`cargo test`) reads the file,
so a typo fails the pull request instead of hiding the app.

## When something does not show up

- Not on Home: the catalog line is not merged yet, or the latest release has no
  `telamon-bundle.json` (a draft or prerelease does not count), or the tag does
  not equal the bundle's version. Pressing Check for Updates on the Updates page
  asks GitHub again at once.
- "Not installable here": the bundle needs a newer Telamon.Ui or Fedora than the
  computer has (`min_telamon_ui`, `min_os_version`); the app page says which.
- The Store refused the bundle: the page says why (a checksum that differs, a
  file that is not listed, an icon not named after the app ID...). Run the
  bundle tool locally; it makes the same checks.

## What users see and what they are told

The Store asks before every install and update, names the repository the app
comes from, and says what is true: the app is not sandboxed, it runs as the
user, and the only check beyond HTTPS is the SHA-256 published with the
release. Signing (GitHub artifact attestations or a minisign key per app) is a
later step; the manifest and the catalog are versioned for it.
