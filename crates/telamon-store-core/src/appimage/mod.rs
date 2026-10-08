//! AppImages: finding one the user downloaded, looking inside it without
//! running it, warning about what can't be trusted, and installing it for the
//! user. See docs/DESIGN.md, "AppImages".

pub mod check;
pub mod format;
pub mod fsutil;
pub mod helper;
pub mod inspect;
pub mod install;
pub mod meta;
pub mod origin;
pub mod sign;
pub mod squash;
pub mod state;
pub mod trust;

pub use format::Format;
pub use inspect::{InspectError, Inspection};
