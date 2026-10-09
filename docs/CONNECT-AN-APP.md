# Connecting a Telamon app to the Store

A Telamon app that is not part of the OS image (Telamon Gates, say) is not
installed by the system and has no Flatpak. Connect it to the Store and users
install it with one button, and it updates from the Store's Updates page.

Four steps, in this order. Nothing in the Store itself changes.

## 1. Add the bundle workflow to the app's repository

Create `.github/workflows/bundle.yml` in the app's repository:

```yaml
name: Telamon bundle

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
    uses: EternalCoder454/atlas-framework/.github/workflows/bundle.yml@FRAMEWORK_SHA # v2.0.4
    with:
      framework-ref: FRAMEWORK_SHA # v2.0.4
```

`FRAMEWORK_SHA` is the 40-character commit of the framework release:
`gh api repos/EternalCoder454/atlas-framework/commits/v2.0.4 --jq .sha`. Pin by
commit, as the other workflows do.

The app needs a spec (`packaging/<app>.spec`) whose `BuildRequires` build it (the
workflow installs them, and its `telamon-ui >=` becomes the bundle's minimum)
and a `project(<name> VERSION x.y.z)` in its CMake. The workflow builds the app
in a `fedora:44` container like the one its CI uses,
against `telamon-ui` from that framework release, and makes
`<app-id>-<version>-x86_64.tar.zst` and `telamon-bundle.json`. Your app needs
only what any Telamon app has: the CMake project under `apps/<name>`, its
`.desktop` file named `<app-id>.desktop` whose `Exec` is the bare program name,
a metainfo file, and an icon named after the app ID. (The app template has all
of it; the rules are in the framework's `docs/BUNDLES.md`.) If the app reads
files at run time, it finds them next to its program, at `../share/<app-id>/`:
nothing sets an environment for it.

Try it before publishing: run the workflow by hand (Actions, Telamon bundle,
Run workflow; run it once on the default branch so its cache is shared), download the artifact, and install it with
`telamon-store --install-bundle <file>.tar.zst`. The Store looks inside without
running anything, says in red that the file is not from Telamon's list (and is
not signed), and installs it after you answer.

## 2. Make a signing key

The Store installs a release only when its `telamon-bundle.json` is signed with
a key that the app's catalog entry lists. Make the key once, on your own
computer, never in the repository:

```
minisign -G -p telamon-gates.pub -s telamon-gates.key
```

`minisign` is in Fedora (`dnf install minisign`; in the Store's dev container
too). Give the key a password. The `.key` file is the secret: keep it off every
repository and put it, with its password, in the app repository's Actions secrets
as the framework's `docs/BUNDLES.md` names them, so the bundle workflow signs
`telamon-bundle.json` and attaches the signature as
`telamon-bundle.json.minisig`. The `.pub` file is public; its second line (it
starts with `RW`) is what goes in the catalog. The key ID in its first line is
what users see in the install dialog. Nothing but the signature of that one file
is needed: it names the archive's SHA-256, so it covers the archive too.

## 3. Tag a release

The version in the app's CMake `project()` must equal the tag without the `v`:

```
git tag v0.1.0
git push origin v0.1.0
```

The workflow runs on the tag and attaches the three files to the GitHub release
of that tag (it creates the release when there is none). Check that the release
lists `telamon-bundle.json`, `telamon-bundle.json.minisig` and the `.tar.zst`. A new tag is a new version:
users get it as an update the next time the Store checks.

## 4. Add an entry to the catalog

In this repository, add an entry to `catalog/native-apps.json` and open a pull
request:

```json
{
  "id": "net.eterneon.telamon.gates",
  "repo": "EternalCoder454/telamon-gates",
  "channel": "releases",
  "signers": [
    { "type": "minisign", "key": "RW<the second line of telamon-gates.pub>" }
  ]
}
```

`id` is the app ID (the same as in the bundle's manifest; the Store refuses a
release whose manifest says another), `repo` is `Owner/name`, `channel` is
`releases`, and `signers` is required: one to four keys, each `{ "type":
"minisign", "key": "RW..." }` with the public key exactly as the `.pub` file
has it. An entry with no usable signer is skipped, as is one whose key is not a
minisign public key, so a mistake hides the app and never installs it unchecked.
The pull request is what decides which key may ship the app, so review it like
code. Merged, it is live: the Store reads this file from the main branch
at run time (and keeps it for 6 hours), so there is no Store release to wait
for. Home then shows the app under Telamon Apps.

Only owners the Store knows are accepted (`ALLOWED_OWNERS` in
`crates/telamon-store-core/src/native/mod.rs`, today `EternalCoder454`). Another
account is a Store release that adds it. A test (`cargo test`) reads the file,
so a typo fails the pull request instead of hiding the app.

## When something does not show up

- Not on Home: the catalog line is not merged yet, or the latest release has no
  `telamon-bundle.json` (a draft or prerelease does not count), or the tag does
  not equal the bundle's version. If the release has the manifest but not
  `telamon-bundle.json.minisig`, or the signature is by a key the entry does not
  list, the Store says so under Telamon Apps and skips the release.
- The Store never installs a release older than the version the user has, even
  when it is signed and is the latest: re-releasing an old version does nothing
  for users who are ahead. Publish a higher version. Pressing Check for Updates on the Updates page
  asks GitHub again at once.
- "Not installable here": the bundle needs a newer Telamon.Ui or Fedora than the
  computer has (`min_telamon_ui`, `min_os_version`); the app page says which.
- The Store refused the bundle: the page says why (a checksum that differs, a
  file that is not listed, an icon not named after the app ID...). Run the
  bundle tool locally; it makes the same checks.

## Changing the key

Keep the old key until the new one is in use. To rotate: add the new key to the
entry's `signers` (a pull request; both keys then work), sign new releases with
the new key, and, later, remove the old key in a second pull request. If the key
leaks, remove it at once: the Store stops accepting what it signs the next time
it reads the catalog (within 6 hours, or when a user presses Check for Updates),
though nothing already installed is removed. Users who update across a rotation
see in the install dialog that the signing key changed; that is expected.

## What users see and what they are told

The Store asks before every install and update, names the repository the app
comes from and the key that signed the release ("Signed with key <ID> that
Telamon's list names for this app"), and says what is true: the app is not
sandboxed and runs as the user. A file opened with `--install-bundle` is shown
as not signed and not from Telamon's list. GitHub artifact attestations
(Sigstore) are a later step; the `signers` list is typed so that they can be
added beside minisign keys. The details and what each kind of compromise can do
are in `docs/DESIGN.md`, "Native Telamon apps".
