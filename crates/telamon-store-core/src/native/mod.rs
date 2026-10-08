//! Native Telamon apps: apps that are not part of the OS image and not
//! Flatpaks (Telamon Gates is the first), installed for the user from a
//! GitHub release, kept up to date by the Store. See docs/DESIGN.md, "Native
//! Telamon apps", and the framework's docs/BUNDLES.md for the bundle format.
//!
//! The pieces, each checking what it is given and nothing else:
//!
//! - [`catalog`]: the list of connected apps (`catalog/native-apps.json`);
//! - [`github`]: a repository's latest release, and which of its files are
//!   the bundle's;
//! - [`manifest`]: `telamon-bundle.json`, the outer one (a release file) and
//!   the inner one (in the archive);
//! - [`archive`]: the `.tar.zst`, checked and unpacked into a private folder;
//! - [`install`]: install, update (side by side, then the `current` link),
//!   rollback, uninstall, the list and Open;
//! - [`check`]: catalog + releases + what is installed = what the window shows;
//! - [`fetch`]: the network behind a trait, so tests and screenshots use
//!   recorded answers (`fake`, with the `fake-github` feature).
//!
//! Everything from the network or a bundle is untrusted. Nothing is installed
//! except by [`install::install_bundle`], which the window calls only after the
//! user's answer in the Store's own dialog.

pub mod archive;
pub mod catalog;
pub mod check;
pub mod desktop;
pub mod fetch;
pub mod github;
pub mod install;
pub mod manifest;
pub mod version;

#[cfg(any(test, feature = "fake-github", feature = "test-hooks"))]
pub mod fake;

/// The folder under `$XDG_DATA_HOME` holding every app: `<id>/<version>/`.
pub const APPS_DIR: &str = "telamon-apps";
/// Where the catalog is fetched from: the main branch of this repository.
pub const CATALOG_URL: &str =
    "https://raw.githubusercontent.com/EternalCoder454/atlasos-store/main/catalog/native-apps.json";
/// The GitHub accounts whose repositories may be connected. A catalog entry
/// for any other owner is ignored, so a mistaken or malicious line in the
/// catalog cannot make the Store install from a stranger. Adding an owner is
/// a Store release.
pub const ALLOWED_OWNERS: &[&str] = &["EternalCoder454"];

/// Why something did not happen, in plain words for the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub(crate) fn err(s: impl Into<String>) -> Error {
    Error(s.into())
}

pub(crate) fn io_err(what: &str, e: &std::io::Error) -> Error {
    err(format!(
        "{what}: {}",
        crate::text::clean(&e.to_string(), 120)
    ))
}

/// A valid app ID: reverse-DNS style (at least one dot), ASCII letters,
/// digits, `.`, `_` and `-`, at most 128 characters, no empty or leading-dot
/// parts. It becomes a folder and file names, so nothing else is accepted.
pub fn valid_app_id(id: &str) -> bool {
    id.len() <= 128
        && id.contains('.')
        && id
            .split('.')
            .all(|part| !part.is_empty() && !part.starts_with('-'))
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_are_names_not_paths() {
        for ok in ["net.eterneon.telamon.gates", "org.example.App-2", "a.b"] {
            assert!(valid_app_id(ok), "{ok}");
        }
        for bad in [
            "",
            "gates",
            ".hidden.app",
            "a..b",
            "a.b.",
            "../x.y",
            "a/b.c",
            "a.b c",
            "a.-b",
            "a.b\n",
            "é.app",
            &"a.".repeat(70),
        ] {
            assert!(!valid_app_id(bad), "{bad:?}");
        }
    }
}
