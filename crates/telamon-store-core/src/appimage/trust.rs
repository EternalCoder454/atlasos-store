//! What the Store tells the user about an AppImage before it is installed,
//! in plain words. Nothing here is a verdict that a file is safe: an AppImage
//! is never sandboxed and never checked by Telamon, and the first line says
//! so every time. The lines that follow are findings; a finding that should
//! make the user stop and think is `Danger` and makes the whole notice
//! `strong` (the window draws it red).

use serde::Serialize;

use super::format::Format;
use super::inspect::Inspection;
use super::origin::Origin;
use super::sign::Signature;
use crate::catalog::{Filter, Library};

/// Always shown first.
pub const ALWAYS: &str = "This app isn't sandboxed and isn't checked by Telamon.";

/// "What it can access", shown with every AppImage.
pub const ACCESS: &str = "AppImages are not sandboxed. This app can read and change all your files, use the network and see everything you can.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A fact.
    Info,
    /// Worth knowing.
    Caution,
    /// A reason to stop and think.
    Danger,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    pub severity: Severity,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Trust {
    pub lines: Vec<Line>,
    /// Some finding is `Danger`.
    pub strong: bool,
}

fn line(severity: Severity, text: impl Into<String>) -> Line {
    Line {
        severity,
        text: text.into(),
    }
}

/// The findings for `insp`.
pub fn assess(insp: &Inspection) -> Trust {
    let mut lines = vec![line(Severity::Caution, ALWAYS)];

    if !insp.inspected {
        let why = if insp.format == Format::Type1 {
            "This is an old kind of AppImage that Telamon can't look inside, so it can't tell what is in it."
        } else {
            "Telamon couldn't look inside this file, so it can't tell what is in it."
        };
        lines.push(line(Severity::Danger, why));
    }

    match &insp.signature {
        Signature::None => lines.push(line(
            Severity::Danger,
            "Not signed. Nothing shows who made this file or that it is unchanged.",
        )),
        Signature::Wrong => lines.push(line(
            Severity::Danger,
            "The signature is wrong (the file was changed).",
        )),
        Signature::Unchecked => lines.push(line(
            Severity::Danger,
            "It carries a signature, but this computer couldn't check it.",
        )),
        Signature::Signed { fingerprint } => lines.push(line(
            Severity::Caution,
            format!(
                "Signed by {}, but this key isn't one Telamon knows.",
                Signature::grouped(fingerprint)
            ),
        )),
    }

    match &insp.origin {
        Origin::Https { host } => lines.push(line(
            Severity::Info,
            format!("The browser recorded {host} as where it came from."),
        )),
        Origin::Http { host } => lines.push(line(
            Severity::Danger,
            format!(
                "Downloaded from {host} without encryption, so it may have been changed on the way."
            ),
        )),
        Origin::Unknown | Origin::Other => lines.push(line(
            Severity::Danger,
            "We can't tell where this file came from.",
        )),
    }

    let strong = lines.iter().any(|l| l.severity == Severity::Danger);
    Trust { lines, strong }
}

/// An app of the same name or ID that is on Flathub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlathubMatch {
    pub app_id: String,
    pub name: String,
}

/// Lowercase letters and digits only, to compare names.
fn flat(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether the local catalog has this app from the Flathub remote: first by
/// the AppStream ID the AppImage carries, else by an exact name (ignoring
/// case and spacing) that exactly one app has. Nothing is fetched.
pub fn flathub_match(lib: &Library, insp: &Inspection) -> Option<FlathubMatch> {
    let on_flathub = |id| {
        lib.source(id).remote == "flathub"
            || lib.alternatives(id).iter().any(|s| s.remote == "flathub")
    };
    let found = |id| {
        let c = lib.component(id);
        FlathubMatch {
            app_id: c.id_bare().to_string(),
            name: c.name.clone(),
        }
    };
    if !insp.app_id.is_empty()
        && let Some(id) = lib.find(&insp.app_id)
        && on_flathub(id)
    {
        return Some(found(id));
    }
    let want = flat(&insp.name);
    if want.len() < 3 {
        return None;
    }
    let mut hits = lib
        .search(&insp.name, Filter::default(), 50)
        .into_iter()
        .filter(|&id| flat(&lib.component(id).name) == want && on_flathub(id));
    let first = hits.next()?;
    hits.next().is_none().then(|| found(first))
}
