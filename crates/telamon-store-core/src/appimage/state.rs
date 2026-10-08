//! Which AppImages the Store has already told the user about, so a file gets
//! one notification and no more. A small JSON file in the Store's state
//! folder (`$XDG_STATE_HOME/telamon-store/appimage-seen.json`, folder 0700,
//! file 0600, written atomically, never through a link). It is bounded
//! (oldest entries go first) and untrusted when read: a damaged file reads
//! as empty and is replaced on the next write; a file that is a link or
//! belongs to someone else is refused, and then nothing is notified, since
//! the notification could not be remembered.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::fsutil;

/// Entries kept.
pub const MAX_ENTRIES: usize = 256;
/// Largest state file read, in bytes.
const MAX_FILE: u64 = 256 * 1024;
const FILE: &str = "appimage-seen.json";

/// One file that was announced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// Absolute path when it was announced.
    pub path: String,
    pub size: u64,
    /// Modification time, in nanoseconds since the epoch.
    pub mtime_ns: i64,
    /// SHA-256 of the file, lowercase hex, or "" when not computed.
    pub sha256: String,
    /// When it was announced, seconds since the epoch (to drop the oldest).
    pub at: i64,
}

#[derive(Serialize, Deserialize)]
struct Disk {
    v: u32,
    entries: Vec<Seen>,
}

/// Why the state could not be kept.
#[derive(Debug)]
pub struct StateError(pub String);

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<std::io::Error> for StateError {
    fn from(e: std::io::Error) -> StateError {
        StateError(e.to_string())
    }
}

/// The seen list, in memory.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SeenState {
    entries: Vec<Seen>,
    file: PathBuf,
}

fn valid(e: &Seen) -> bool {
    e.path.starts_with('/')
        && e.path.len() <= 4096
        && !e.path.chars().any(char::is_control)
        && (e.sha256.is_empty()
            || (e.sha256.len() == 64 && e.sha256.bytes().all(|b| b.is_ascii_hexdigit())))
}

impl SeenState {
    /// `$XDG_STATE_HOME/telamon-store/appimage-seen.json`.
    pub fn default_path() -> Option<PathBuf> {
        Some(fsutil::state_home()?.join(crate::legacy::NAME).join(FILE))
    }

    /// Reads the file at `path` (missing or damaged reads as empty). A link,
    /// another user's file or an unreadable one is an error.
    pub fn load(path: &Path) -> Result<SeenState, StateError> {
        let mut state = SeenState {
            entries: Vec::new(),
            file: path.to_path_buf(),
        };
        if let Some(dir) = path.parent() {
            match std::fs::symlink_metadata(dir) {
                Ok(md) if !md.is_dir() => {
                    return Err(StateError("the state folder is a link".into()));
                }
                Ok(_) | Err(_) => {}
            }
        }
        let Some(bytes) = fsutil::read_private(path, MAX_FILE)? else {
            return Ok(state);
        };
        if let Ok(disk) = serde_json::from_slice::<Disk>(&bytes)
            && disk.v == 1
        {
            state.entries = disk
                .entries
                .into_iter()
                .filter(valid)
                .take(MAX_ENTRIES)
                .collect();
        }
        Ok(state)
    }

    /// Whether this exact file (path, size, modification time) was announced,
    /// or one with the same size, time and content under another name (a
    /// rename).
    pub fn seen(&self, path: &str, size: u64, mtime_ns: i64, sha256: &str) -> bool {
        self.entries.iter().any(|e| {
            e.size == size
                && e.mtime_ns == mtime_ns
                && (e.path == path || (!sha256.is_empty() && e.sha256 == sha256))
        })
    }

    /// Notes a file as announced (replacing an older entry of the same path)
    /// and writes the file. The oldest entries beyond the cap go.
    pub fn remember(&mut self, entry: Seen) -> Result<(), StateError> {
        if !valid(&entry) {
            return Err(StateError("not a path to remember".into()));
        }
        self.entries.retain(|e| e.path != entry.path);
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            // Oldest first, as they were added.
            let over = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..over);
        }
        self.save()
    }

    fn save(&self) -> Result<(), StateError> {
        let dir = self
            .file
            .parent()
            .ok_or_else(|| StateError("no folder".into()))?;
        fsutil::private_dir(dir)?;
        let disk = Disk {
            v: 1,
            entries: self.entries.clone(),
        };
        let bytes = serde_json::to_vec(&disk).map_err(|e| StateError(e.to_string()))?;
        fsutil::write_atomic(&self.file, &bytes, 0o600)?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
