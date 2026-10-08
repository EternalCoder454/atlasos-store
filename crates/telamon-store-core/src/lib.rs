//! Telamon Store's core, with no Qt: everything here runs on worker threads and
//! is tested on its own. Everything it reads from a remote, a file or a URL is
//! untrusted and is checked where it enters (see docs/DESIGN.md, Security).

pub mod appimage;
pub mod appstream;
pub mod catalog;
pub mod flathub;
pub mod flatpak;
pub mod flatpakref;
pub mod keyfile;
pub mod launch;
pub mod legacy;
pub mod native;
pub mod net;
pub mod permissions;
pub mod text;
