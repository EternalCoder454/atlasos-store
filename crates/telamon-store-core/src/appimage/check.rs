//! `telamon-store --appimage-check <folder or file>`: what a systemd path unit
//! runs when the Downloads folder changes. It finds AppImages that arrived
//! lately, waits until each has stopped growing, looks inside it (through the
//! inspection helper, never running it), tells the user once, and does what
//! the notification's button says. Then it exits: nothing stays running.
//!
//! The parts that touch the world (the inspection, the notification, starting
//! the Store) are traits, so the rules here are tested without a desktop.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::fsutil;
use super::inspect::{InspectError, Inspection, MAX_SIZE};
use super::origin::Origin;
use super::state::{Seen, SeenState};
use crate::text;

/// Names a browser uses for a download that is not finished.
const TEMP_SUFFIXES: [&str; 7] = [
    ".part",
    ".crdownload",
    ".download",
    ".partial",
    ".opdownload",
    ".tmp",
    ".crswap",
];

/// What a check may do and how long it may take.
#[derive(Debug, Clone)]
pub struct Config {
    /// A file must not have changed for this long.
    pub stable_for: Duration,
    /// How often a growing file is looked at.
    pub poll: Duration,
    /// Give up on a file that is still changing after this long.
    pub max_wait: Duration,
    /// Only files that arrived (changed or were renamed in) this lately.
    pub recent: Duration,
    /// Most folder entries looked at.
    pub max_entries: usize,
    /// Most notifications in one run.
    pub max_notices: usize,
    /// Most files waited for and inspected in one round, announced or not.
    pub max_files: usize,
    /// Most rounds in one run. A round looks at the folder again, so files
    /// that arrived while the last one ran (the path unit does not start a
    /// service that is still running) and files past `max_files` are found.
    pub max_rounds: usize,
    /// No new file is started after this long; the process's own alarm and the
    /// unit's timeout are longer (see `DEADLINE`).
    pub budget: Duration,
}

/// How long a check may run at most: the process ends itself after this
/// (SIGALRM), and the unit's `TimeoutStartSec` is a minute more. The work
/// budget (`Config::budget`) ends first, so the alarm is only a backstop.
pub const DEADLINE: Duration = Duration::from_secs(10 * 60);

impl Default for Config {
    fn default() -> Config {
        Config {
            stable_for: Duration::from_secs(3),
            poll: Duration::from_millis(500),
            max_wait: Duration::from_secs(120),
            recent: Duration::from_secs(15 * 60),
            max_entries: 2000,
            max_notices: 4,
            max_files: 8,
            max_rounds: 4,
            budget: Duration::from_secs(8 * 60),
        }
    }
}

/// A file that may be an AppImage that just arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
}

/// What the user answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Install,
    ShowInStore,
    NotNow,
    /// Dismissed, or nobody answered in time.
    None,
}

/// The answer and the activation token the notification server gave with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub answer: Answer,
    pub token: Option<String>,
}

/// One notification: plain text, escaped by whoever sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub path: PathBuf,
}

pub trait Inspector {
    fn inspect(&self, path: &Path) -> Result<Inspection, InspectError>;
}

pub trait Notifier {
    /// Shows the notice and waits for the user (or a timeout).
    fn notify(&mut self, notice: &Notice) -> Result<Reply, String>;
}

pub trait Launcher {
    /// Starts the Store with its install dialog open for `path`.
    fn open_install(&mut self, path: &Path, token: Option<&str>) -> Result<(), String>;
}

/// What a run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub notified: Vec<PathBuf>,
    pub launched: Vec<PathBuf>,
    pub skipped: usize,
    pub errors: Vec<String>,
}

fn ns(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_nanos()).unwrap_or(i64::MAX),
    }
}

fn mtime_ns(md: &std::fs::Metadata) -> i64 {
    md.mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(md.mtime_nsec())
}

fn skippable_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.starts_with('.')
        || TEMP_SUFFIXES.iter().any(|s| lower.ends_with(s))
        // A name with a newline, a bidi override or another character that
        // hides what a name says is never announced: the Store would refuse to
        // open it (`launch::internal_path`) and the notice could show it as
        // another name. (A name that is not UTF-8 never gets this far.)
        || name.chars().any(crate::launch::hidden)
}

fn plain_file(path: &Path) -> Option<std::fs::Metadata> {
    std::fs::symlink_metadata(path).ok().filter(|m| m.is_file())
}

/// Whether the file starts like an AppImage (ELF with the `AI` marker). Reads
/// 16 bytes, without following a link.
fn looks_like_appimage(path: &Path) -> bool {
    fsutil::open_regular(path)
        .ok()
        .and_then(|f| super::format::sniff_file(&f))
        .is_some()
}

/// The files in `target` (a folder, looked at one level deep; or a single
/// file) that arrived lately, are not a browser's unfinished download, and
/// are named `*.AppImage` or start like an AppImage.
pub fn candidates(target: &Path, cfg: &Config, now: SystemTime) -> Vec<Candidate> {
    let mut paths: Vec<PathBuf> = Vec::new();
    match std::fs::symlink_metadata(target) {
        Ok(md) if md.is_dir() => {
            if let Ok(rd) = std::fs::read_dir(target) {
                paths.extend(
                    rd.filter_map(|e| e.ok())
                        .take(cfg.max_entries)
                        .map(|e| e.path()),
                );
            }
        }
        Ok(md) if md.is_file() => paths.push(target.to_path_buf()),
        _ => {}
    }
    let single = target.is_file();
    let oldest = now
        .checked_sub(cfg.recent)
        .map_or(i64::MIN, |t| ns(t) / 1_000_000_000);
    let mut out = Vec::new();
    for path in paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if skippable_name(name) {
            continue;
        }
        let Some(md) = plain_file(&path) else {
            continue;
        };
        if md.len() < 4096 || md.len() > MAX_SIZE {
            continue;
        }
        // A file named in the request is looked at whenever it changed; in a
        // folder only what arrived lately.
        if !single && md.ctime().max(md.mtime()) < oldest {
            continue;
        }
        if !looks_like_appimage(&path) {
            continue;
        }
        out.push(Candidate {
            path,
            size: md.len(),
            mtime_ns: mtime_ns(&md),
        });
    }
    out.sort_by(|a, b| {
        b.mtime_ns
            .cmp(&a.mtime_ns)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// Waits until the file has not changed for `cfg.stable_for` and returns its
/// size and time then; `None` when it vanished, stopped being a plain file,
/// or kept changing for `cfg.max_wait`.
pub fn wait_stable(path: &Path, cfg: &Config, now: SystemTime) -> Option<Candidate> {
    let stat = |p: &Path| plain_file(p).map(|m| (m.len(), mtime_ns(&m)));
    let mut last = stat(path)?;
    // A file whose last change is old is only confirmed once.
    let age = now
        .duration_since(UNIX_EPOCH + Duration::from_nanos(last.1.max(0) as u64))
        .unwrap_or_default();
    let quiet = if age >= cfg.stable_for {
        Duration::ZERO
    } else {
        cfg.stable_for
    };
    let start = Instant::now();
    let mut changed = Instant::now();
    loop {
        std::thread::sleep(cfg.poll);
        let cur = stat(path)?;
        if cur != last {
            last = cur;
            changed = Instant::now();
        } else if changed.elapsed() >= quiet {
            return Some(Candidate {
                path: path.to_path_buf(),
                size: cur.0,
                mtime_ns: cur.1,
            });
        }
        if start.elapsed() > cfg.max_wait {
            return None;
        }
    }
}

/// The notification for an inspected file. Built from the inspection's
/// cleaned fields only.
pub fn notice(path: &Path, insp: &Inspection) -> Notice {
    let name = text::clean(&insp.name, 60);
    let file = text::clean(&insp.file_name, 80);
    let mut body =
        format!("{file} is an AppImage. Telamon does not check it, and it is not sandboxed.");
    match &insp.origin {
        Origin::Http { .. } => body.push_str("\nIt was downloaded without encryption."),
        Origin::Unknown | Origin::Other => body.push_str("\nWe cannot tell where it came from."),
        Origin::Https { .. } => {}
    }
    if !insp.inspected {
        body.push_str("\nTelamon can't look inside it.");
    }
    Notice {
        title: format!("Install {name}?"),
        body,
        path: path.to_path_buf(),
    }
}

/// Runs one check of `target`: rounds over the folder until a round finds
/// nothing new to look at (or the caps and the budget are reached).
pub fn run(
    target: &Path,
    cfg: &Config,
    state: &mut SeenState,
    inspector: &dyn Inspector,
    notifier: &mut dyn Notifier,
    launcher: &mut dyn Launcher,
) -> Report {
    let mut report = Report::default();
    let started = Instant::now();
    for _ in 0..cfg.max_rounds.max(1) {
        if report.notified.len() >= cfg.max_notices || started.elapsed() >= cfg.budget {
            break;
        }
        if !round(
            target,
            cfg,
            state,
            inspector,
            notifier,
            launcher,
            &mut report,
            started,
        ) {
            break;
        }
    }
    report
}

/// One look at the folder. True when it did something a later round could
/// change the picture of (looked at a file); false when there was nothing new.
#[allow(clippy::too_many_arguments)]
fn round(
    target: &Path,
    cfg: &Config,
    state: &mut SeenState,
    inspector: &dyn Inspector,
    notifier: &mut dyn Notifier,
    launcher: &mut dyn Launcher,
    report: &mut Report,
    started: Instant,
) -> bool {
    let now = SystemTime::now();
    let at = ns(now) / 1_000_000_000;
    let mut looked_at = 0;
    let mut state_failed = false;
    for c in candidates(target, cfg, now) {
        if report.notified.len() >= cfg.max_notices
            || looked_at >= cfg.max_files
            || started.elapsed() >= cfg.budget
        {
            break;
        }
        let path = c.path.to_string_lossy().into_owned();
        if state.seen(&path, c.size, c.mtime_ns, "") {
            continue;
        }
        looked_at += 1;
        let Some(stable) = wait_stable(&c.path, cfg, SystemTime::now()) else {
            report.skipped += 1;
            continue;
        };
        if (stable.size, stable.mtime_ns) != (c.size, c.mtime_ns)
            && state.seen(&path, stable.size, stable.mtime_ns, "")
        {
            report.skipped += 1;
            continue;
        }
        let mut entry = Seen {
            path: path.clone(),
            size: stable.size,
            mtime_ns: stable.mtime_ns,
            sha256: String::new(),
            at,
        };
        let insp = match inspector.inspect(&c.path) {
            Ok(i) => i,
            Err(e) => {
                // Not remembered as an AppImage, but not looked at again.
                report.errors.push(format!("{}: {e}", c.path.display()));
                if let Err(e) = state.remember(entry) {
                    report.errors.push(e.to_string());
                    state_failed = true;
                }
                continue;
            }
        };
        // A file that changed while it was looked at is left for the next
        // round: its size and time would not match its hash.
        let after = plain_file(&c.path).map(|m| (m.len(), mtime_ns(&m)));
        if after != Some((stable.size, stable.mtime_ns)) {
            report.skipped += 1;
            continue;
        }
        entry.sha256 = insp.sha256.clone();
        let duplicate = state.seen(&path, stable.size, stable.mtime_ns, &insp.sha256);
        // Remembered before the user is told: if it can't be, nobody is.
        if let Err(e) = state.remember(entry) {
            report
                .errors
                .push(format!("not announced, the state can't be kept: {e}"));
            state_failed = true;
            continue;
        }
        if duplicate {
            report.skipped += 1;
            continue;
        }
        let n = notice(&c.path, &insp);
        report.notified.push(c.path.clone());
        match notifier.notify(&n) {
            Ok(Reply {
                answer: Answer::Install | Answer::ShowInStore,
                token,
            }) => match launcher.open_install(&c.path, token.as_deref()) {
                Ok(()) => report.launched.push(c.path.clone()),
                Err(e) => report.errors.push(format!("could not open the Store: {e}")),
            },
            Ok(_) => {}
            Err(e) => report.errors.push(format!("notification: {e}")),
        }
    }
    looked_at > 0 && !state_failed
}
