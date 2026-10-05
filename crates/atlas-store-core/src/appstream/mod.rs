//! AppStream catalogs: the parser for a remote's `appstream.xml(.gz)` and the
//! on-disk index the Store reads at start. The model holds what the Store
//! shows and nothing more; every field has passed the checks in [`crate::text`]
//! and the caps in [`parse::Limits`], because the input is untrusted.

pub mod index;
pub mod lang;
pub mod parse;

pub use index::{IndexError, IndexKey};
pub use parse::{Limits, ParseError, ParseOptions, parse, parse_gz_file};

/// The components of one remote.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Catalog {
    /// The remote this came from (`flathub`).
    pub origin: String,
    /// The components kept, in file order, first of each ID.
    pub components: Vec<Component>,
    /// How many components were dropped: invalid or duplicate ID, no name, or
    /// no Flatpak bundle.
    pub skipped: u32,
}

/// What a component is, from `<component type>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    /// `desktop-application` or `desktop`.
    DesktopApp,
    /// `console-application`.
    ConsoleApp,
    /// `addon`.
    Addon,
    /// `runtime`.
    Runtime,
    /// Anything else, or no type.
    #[default]
    Other,
}

/// A themed icon the remote cached: a bare file name and the sizes it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    /// File name, without a path, ending `.png` or `.svg`.
    pub file: String,
    /// Pixel sizes at scale 1, ascending.
    pub sizes: Vec<u16>,
}

/// What a link is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlKind {
    Homepage,
    Bugtracker,
    Help,
    Donation,
    Translate,
    Contact,
    Contribute,
    VcsBrowser,
    Faq,
}

/// One picture of a screenshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// True for a scaled-down copy, false for the source.
    pub thumbnail: bool,
    /// Width in pixels, 0 when not given.
    pub width: u32,
    /// Height in pixels, 0 when not given.
    pub height: u32,
    /// An `https://` URL.
    pub url: String,
}

/// A screenshot with its caption and sizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    /// True for the one to show first.
    pub default: bool,
    /// Caption in the chosen language; empty when none.
    pub caption: String,
    /// At least one.
    pub images: Vec<Image>,
}

/// How a release is labelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseKind {
    Stable,
    Development,
    Snapshot,
    Other,
}

/// One entry of a component's changelog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// Seconds since the epoch; 0 when the release has no date.
    pub timestamp: i64,
    pub kind: ReleaseKind,
    pub description: Vec<Block>,
}

/// The OARS scheme a rating follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatingScheme {
    Oars10,
    Oars11,
    Other,
}

/// How strong a content attribute is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intensity {
    None,
    Mild,
    Moderate,
    Intense,
}

/// An age rating. An empty `attrs` means suitable for all ages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRating {
    pub scheme: RatingScheme,
    /// OARS attribute ids with their intensity.
    pub attrs: Vec<(String, Intensity)>,
}

/// How a Flatpak is fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// `app|runtime/ID/arch/branch`.
    pub reference: String,
    /// The runtime it needs, `ID/arch/branch`.
    pub runtime: Option<String>,
    /// The SDK, `ID/arch/branch`.
    pub sdk: Option<String>,
}

/// Flathub's check that the publisher owns the app's ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    pub method: String,
    pub website: String,
    pub login_name: String,
    pub login_provider: String,
    /// True when the login is an organization.
    pub organization: bool,
    pub timestamp: i64,
}

/// The app's brand colors as RGB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branding {
    pub light: Option<[u8; 3]>,
    pub dark: Option<[u8; 3]>,
}

/// How a run of text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Emphasis,
    Code,
}

/// A run of text in one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// A part of a description. Plain text only: nothing is ever kept as markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Span>),
    List {
        ordered: bool,
        items: Vec<Vec<Span>>,
    },
}

/// One app, add-on or runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// Valid ID, possibly with a `.desktop` suffix.
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub summary: String,
    pub description: Vec<Block>,
    pub developer: String,
    pub license: String,
    pub categories: Vec<String>,
    pub keywords: Vec<String>,
    pub icon: Option<Icon>,
    pub urls: Vec<(UrlKind, String)>,
    pub screenshots: Vec<Screenshot>,
    /// At most 10, newest first.
    pub releases: Vec<Release>,
    pub content_rating: Option<ContentRating>,
    pub bundle: Option<Bundle>,
    /// IDs of the components this extends, as given.
    pub extends: Vec<String>,
    /// The desktop file ID.
    pub launchable: Option<String>,
    /// Present only when Flathub verified the publisher.
    pub verification: Option<Verification>,
    pub branding: Option<Branding>,
}

impl Component {
    /// [`Component::extends`] without a trailing `.desktop`, to compare with
    /// component IDs.
    pub fn extends_ids(&self) -> impl Iterator<Item = &str> {
        self.extends
            .iter()
            .map(|e| e.strip_suffix(".desktop").unwrap_or(e))
    }
}
