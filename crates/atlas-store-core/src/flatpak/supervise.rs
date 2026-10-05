//! Running a blocking libflatpak call on a thread of its own, with its
//! progress handed to the caller's thread and a stall watchdog: the caller's
//! `FnMut` never needs to be `'static` or `Send`, and the libflatpak objects
//! never leave their thread.
//!
//! # Stall limits, by phase
//!
//! - **Start** (before the first sign of life, and before libflatpak's `ready`
//!   step): [`START_LIMIT`], 300 s, then the call is cancelled as
//!   [`Error::TimedOut`]. Resolving a plan or reading a remote's details makes
//!   no progress reports at all, so this is their only limit.
//! - **Downloading**: [`STALL_LIMIT`], 120 s without any byte moving, then
//!   cancelled as timed out.
//! - **Quiet** (deploy, an uninstall, and anything waiting for a person: a
//!   polkit password, a sign-in): in the system scope never cancelled by the
//!   watchdog, because a person taking their time must never be reported as a
//!   timeout. After [`STALL_LIMIT`] without news the caller's progress
//!   callback gets a `not_responding` notice (and again every
//!   [`STALL_LIMIT`]). In the user scope (no prompt is possible there) a
//!   quiet phase is also cancelled, as [`Error::TimedOut`], after
//!   [`QUIET_CEILING`] without news. An uninstall enters this phase at its
//!   start (it reports no bytes, and a system-scope polkit prompt can come
//!   inside it). A download that reports 100% is quiet too, but if its bytes
//!   grow again the phase goes back to Downloading ([`OpPhase`]), so a real
//!   stall after a premature 100% is still caught.
//!
//! # After a cancel
//!
//! The calling thread **always waits for the libflatpak thread to finish**,
//! however long that takes: the caller holds the `OperationLock` while this
//! runs, and libflatpak may still be writing to the installation, so
//! abandoning the thread would let another process start on a half-written
//! one. When a cancel (the watchdog's, the panic's or the user's) has not made
//! the call return after [`CANCEL_GRACE`], the callback gets one
//! `not_responding` notice and the wait goes on.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use super::transaction::Progress;
use super::{CancelToken, Error, Scope};

/// Before the first beat and before `ready`: nothing for this long is a stall.
pub(crate) const START_LIMIT: Duration = Duration::from_secs(300);
/// A download that has not moved a byte for this long is stopped; a quiet
/// phase is reported as not responding after this long.
pub(crate) const STALL_LIMIT: Duration = Duration::from_secs(120);
/// How long a cancelled call may take to return before the caller is told.
pub(crate) const CANCEL_GRACE: Duration = Duration::from_secs(30);
/// The longest a quiet phase may go without news in the user scope.
pub(crate) const QUIET_CEILING: Duration = Duration::from_secs(30 * 60);
/// How often the watchdog looks.
const POLL: Duration = Duration::from_millis(500);

/// The limits [`supervise`] works to.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) start: Duration,
    pub(crate) stall: Duration,
    pub(crate) grace: Duration,
    pub(crate) poll: Duration,
    /// The longest a quiet phase may go without news before the call is
    /// cancelled; `None` for none (the system scope, where a person may be
    /// typing a password).
    pub(crate) quiet_ceiling: Option<Duration>,
}

impl Limits {
    pub(crate) const DEFAULT: Limits = Limits {
        start: START_LIMIT,
        stall: STALL_LIMIT,
        grace: CANCEL_GRACE,
        poll: POLL,
        quiet_ceiling: None,
    };

    /// The default limits for a run in `scope`: the quiet ceiling applies
    /// where no prompt is possible, in the user scope.
    pub(crate) fn for_scope(scope: Scope) -> Limits {
        Limits {
            quiet_ceiling: match scope {
                Scope::User => Some(QUIET_CEILING),
                Scope::System => None,
            },
            ..Limits::DEFAULT
        }
    }
}

/// What the call is doing, which decides what a silence means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Nothing has happened yet.
    Start,
    /// Bytes are moving; a silence is a stall.
    Downloading,
    /// Deploy or waiting for a person: a silence is only reported.
    Quiet,
}

/// Which phase one operation is in, from its progress reports: the single
/// place that decides it (a pure state machine, so it can be tested).
///
/// - An uninstall is quiet from the start: it reports no bytes, and a
///   system-scope polkit prompt can come inside it.
/// - Anything else downloads until it reports 100%, then deploys (quiet).
/// - In a quiet phase, if `bytes` still grow the phase goes back to
///   Downloading, so a stall after a premature 100% is still caught. It
///   becomes quiet again once a 100% report comes with no new bytes.
pub(crate) struct OpPhase {
    uninstall: bool,
    phase: Phase,
    /// The bytes when the quiet phase was entered.
    quiet_at: u64,
    /// The bytes of the previous report.
    prev: u64,
    /// A quiet phase was entered before.
    quiet_seen: bool,
}

impl OpPhase {
    pub(crate) fn new(uninstall: bool) -> OpPhase {
        OpPhase {
            uninstall,
            phase: if uninstall {
                Phase::Quiet
            } else {
                Phase::Downloading
            },
            quiet_at: 0,
            prev: 0,
            quiet_seen: uninstall,
        }
    }

    /// The phase the operation starts in.
    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }

    /// Takes one report; `Some(phase)` when the phase changed.
    pub(crate) fn report(&mut self, percent: u8, bytes: u64) -> Option<Phase> {
        let want = if self.uninstall {
            Phase::Quiet
        } else if percent < 100 {
            Phase::Downloading
        } else if self.phase == Phase::Quiet {
            if bytes > self.quiet_at {
                Phase::Downloading
            } else {
                Phase::Quiet
            }
        } else if self.quiet_seen && bytes > self.prev {
            Phase::Downloading
        } else {
            Phase::Quiet
        };
        self.prev = bytes;
        if want == self.phase {
            return None;
        }
        if want == Phase::Quiet {
            self.quiet_at = bytes;
            self.quiet_seen = true;
        }
        self.phase = want;
        Some(want)
    }
}

pub(crate) enum Msg {
    Progress(Progress),
    /// Something moved (even if the callback is throttled). The first one
    /// ends the start phase.
    Beat,
    /// The call entered another phase (which counts as movement).
    Phase(Phase),
    Done,
}

#[cfg(test)]
thread_local! {
    /// Beats and progress reports the supervisor on this thread has received.
    pub(crate) static BEATS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Tracks when something last moved. Time is passed in, so a test can use
/// made-up instants.
pub(crate) struct Stall {
    start: Duration,
    stall: Duration,
    phase: Phase,
    quiet_ceiling: Option<Duration>,
    last: Instant,
    noticed: Option<Instant>,
}

impl Stall {
    pub(crate) fn new(start: Duration, stall: Duration, now: Instant) -> Stall {
        Stall {
            start,
            stall,
            phase: Phase::Start,
            quiet_ceiling: None,
            last: now,
            noticed: None,
        }
    }

    /// Also cancels a quiet phase after `ceiling` without news.
    pub(crate) fn with_quiet_ceiling(mut self, ceiling: Option<Duration>) -> Stall {
        self.quiet_ceiling = ceiling;
        self
    }

    pub(crate) fn beat(&mut self, now: Instant) {
        self.last = now;
        self.noticed = None;
        if self.phase == Phase::Start {
            self.phase = Phase::Downloading;
        }
    }

    pub(crate) fn set_phase(&mut self, phase: Phase, now: Instant) {
        self.phase = phase;
        self.last = now;
        self.noticed = None;
    }

    /// Whether the silence is long enough to cancel (in a quiet phase only
    /// past the ceiling, if there is one).
    pub(crate) fn expired(&self, now: Instant) -> bool {
        let silent = now.saturating_duration_since(self.last);
        match self.phase {
            Phase::Start => silent >= self.start,
            Phase::Downloading => silent >= self.stall,
            Phase::Quiet => self.quiet_ceiling.is_some_and(|c| silent >= c),
        }
    }

    /// True once per `stall` of silence in a quiet phase.
    pub(crate) fn quiet_notice(&mut self, now: Instant) -> bool {
        if self.phase != Phase::Quiet {
            return false;
        }
        let since = now.saturating_duration_since(self.noticed.unwrap_or(self.last));
        if since >= self.stall {
            self.noticed = Some(now);
            true
        } else {
            false
        }
    }
}

pub(crate) struct Supervised {
    pub(crate) panic: Option<Box<dyn Any + Send>>,
    pub(crate) timed_out: bool,
    /// How many `not_responding` notices were delivered.
    pub(crate) notices: u32,
}

/// Hands one report to the caller's callback. A panic in it cancels the token
/// and is kept to be re-raised; after one, the callback is not called again.
fn deliver(
    out: &mut Supervised,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(Progress),
    p: Progress,
) {
    if out.panic.is_some() {
        return;
    }
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| progress(p))) {
        log::error!(
            "the progress callback panicked ({}): stopping the operation",
            describe(payload.as_ref())
        );
        cancel.cancel();
        out.panic = Some(payload);
    }
}

/// Receives until `Done` (or the sender is gone): progress goes to
/// `progress`, a stall cancels the token as timed out (see the module notes
/// for the limits), and a quiet phase or a cancel that is slow to take effect
/// is reported to `progress` as not responding. Never gives up waiting.
pub(crate) fn supervise(
    rx: &Receiver<Msg>,
    lim: &Limits,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(Progress),
) -> Supervised {
    let mut stall =
        Stall::new(lim.start, lim.stall, Instant::now()).with_quiet_ceiling(lim.quiet_ceiling);
    let mut out = Supervised {
        panic: None,
        timed_out: false,
        notices: 0,
    };
    let mut last: Option<Progress> = None;
    let mut cancelled_at: Option<Instant> = None;
    let mut grace_told = false;
    loop {
        match rx.recv_timeout(lim.poll) {
            Ok(Msg::Done) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Msg::Beat) => {
                #[cfg(test)]
                BEATS.with(|b| b.set(b.get() + 1));
                stall.beat(Instant::now());
            }
            Ok(Msg::Phase(p)) => stall.set_phase(p, Instant::now()),
            Ok(Msg::Progress(p)) => {
                #[cfg(test)]
                BEATS.with(|b| b.set(b.get() + 1));
                stall.beat(Instant::now());
                last = Some(p.clone());
                deliver(&mut out, cancel, progress, p);
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        if cancelled_at.is_none() && cancel.is_cancelled() {
            cancelled_at = Some(now);
        }
        if !out.timed_out && stall.expired(now) {
            log::warn!(
                "no progress for {} s: stopping the operation",
                now.saturating_duration_since(stall.last).as_secs()
            );
            out.timed_out = true;
            cancel.cancel_timed_out();
            cancelled_at.get_or_insert(now);
        } else if stall.quiet_notice(now) {
            log::warn!(
                "no news for {} s while deploying or waiting for a person",
                lim.stall.as_secs()
            );
            out.notices += 1;
            deliver(
                &mut out,
                cancel,
                progress,
                Progress::not_responding(last.as_ref()),
            );
        }
        if let Some(t) = cancelled_at
            && !grace_told
            && now.saturating_duration_since(t) >= lim.grace
        {
            grace_told = true;
            log::error!(
                "the operation has not stopped {} s after it was cancelled; \
                 still waiting for it, as it may be writing",
                lim.grace.as_secs()
            );
            out.notices += 1;
            deliver(
                &mut out,
                cancel,
                progress,
                Progress::not_responding(last.as_ref()),
            );
        }
    }
    out
}

pub(crate) fn describe(p: &(dyn Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        crate::text::clean(s, 200)
    } else if let Some(s) = p.downcast_ref::<String>() {
        crate::text::clean(s, 200)
    } else {
        "no message".into()
    }
}

struct DoneGuard(Sender<Msg>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        let _ = self.0.send(Msg::Done);
    }
}

/// Runs `f` on a thread of its own, supervised, with the default limits. A
/// panic in `progress` is re-raised here after the thread has finished (the
/// operation was cancelled first); a panic in `f` is logged and becomes an
/// error.
pub(crate) fn run_supervised<T: Send>(
    cancel: &CancelToken,
    progress: &mut dyn FnMut(Progress),
    f: impl FnOnce(Sender<Msg>) -> T + Send,
) -> Result<(T, bool), Error> {
    let (value, sup) = run_supervised_with(cancel, &Limits::DEFAULT, progress, f)?;
    if let Some(p) = sup.panic {
        resume_unwind(p);
    }
    Ok((value, sup.timed_out))
}

/// Like [`run_supervised`], but a panic in `progress` is handed back in the
/// result instead of being re-raised, so the caller can log what it knows
/// first, and the limits are given.
pub(crate) fn run_supervised_with<T: Send>(
    cancel: &CancelToken,
    lim: &Limits,
    progress: &mut dyn FnMut(Progress),
    f: impl FnOnce(Sender<Msg>) -> T + Send,
) -> Result<(T, Supervised), Error> {
    let (tx, rx) = mpsc::channel::<Msg>();
    std::thread::scope(|s| {
        let handle = std::thread::Builder::new()
            .name("atlas-flatpak".into())
            .spawn_scoped(s, move || {
                let _done = DoneGuard(tx.clone());
                f(tx)
            })
            .map_err(|e| {
                log::error!("could not start the operation thread: {e}");
                Error::Io {
                    action: "start the operation",
                    message: e.kind().to_string(),
                }
            })?;
        let sup = supervise(&rx, lim, cancel, progress);
        match handle.join() {
            Ok(v) => Ok((v, sup)),
            Err(payload) => {
                log::error!(
                    "the operation thread panicked: {}",
                    describe(payload.as_ref())
                );
                if let Some(p) = sup.panic {
                    resume_unwind(p);
                }
                Err(Error::Flatpak {
                    action: "run the operation",
                    message: "an internal error stopped it".into(),
                })
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lim(start_ms: u64, stall_ms: u64, grace_ms: u64) -> Limits {
        Limits {
            start: Duration::from_millis(start_ms),
            stall: Duration::from_millis(stall_ms),
            grace: Duration::from_millis(grace_ms),
            poll: Duration::from_millis(10),
            quiet_ceiling: None,
        }
    }

    fn sample() -> Progress {
        Progress {
            op: 1,
            ops: 1,
            ref_: "app/a.b/x/s".into(),
            kind: super::super::transaction::OpKind::Install,
            percent: 1,
            bytes: 0,
            status: String::new(),
            not_responding: false,
        }
    }

    #[test]
    fn the_limits_are_the_documented_ones() {
        assert_eq!(START_LIMIT, Duration::from_secs(300));
        assert_eq!(STALL_LIMIT, Duration::from_secs(120));
        assert_eq!(CANCEL_GRACE, Duration::from_secs(30));
    }

    #[test]
    fn stall_limits_depend_on_the_phase() {
        let t0 = Instant::now();
        let s = |n| Duration::from_secs(n);
        let mut st = Stall::new(s(300), s(120), t0);
        // Start: 300 s.
        assert!(!st.expired(t0 + s(299)));
        assert!(st.expired(t0 + s(300)));
        // The first beat ends the start phase: 120 s from then on.
        st.beat(t0 + s(10));
        assert!(!st.expired(t0 + s(129)));
        assert!(st.expired(t0 + s(130)));
        // A clock that went back is not an expiry.
        assert!(!st.expired(t0));
        // Quiet: never expires, notices once per 120 s.
        st.set_phase(Phase::Quiet, t0 + s(200));
        assert!(!st.expired(t0 + s(10_000)));
        assert!(!st.quiet_notice(t0 + s(319)));
        assert!(st.quiet_notice(t0 + s(320)));
        assert!(!st.quiet_notice(t0 + s(321)));
        assert!(st.quiet_notice(t0 + s(440)));
        // A beat does not leave the quiet phase but restarts the count.
        st.beat(t0 + s(450));
        assert!(!st.expired(t0 + s(10_000)));
        assert!(!st.quiet_notice(t0 + s(569)));
        assert!(st.quiet_notice(t0 + s(570)));
        // Back to downloading.
        st.set_phase(Phase::Downloading, t0 + s(600));
        assert!(st.expired(t0 + s(720)));
        assert!(!st.quiet_notice(t0 + s(10_000)));
    }

    #[test]
    fn a_silent_source_is_cancelled_as_timed_out() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let h = std::thread::spawn(move || {
            while !c2.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = tx.send(Msg::Done);
        });
        let sup = supervise(&rx, &lim(150, 150, 5000), &cancel, &mut |_| {});
        h.join().unwrap();
        assert!(sup.timed_out && sup.panic.is_none() && sup.notices == 0);
        assert!(cancel.is_cancelled() && cancel.is_timed_out());
    }

    #[test]
    fn the_start_phase_is_longer_than_the_download_limit() {
        // 100 ms of silence at the start is fine with a 400 ms start limit,
        // but 100 ms of silence after the first beat is a stall.
        let (tx, rx) = mpsc::channel::<Msg>();
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            assert!(!c2.is_cancelled(), "the start limit is longer");
            let _ = tx.send(Msg::Beat);
            while !c2.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = tx.send(Msg::Done);
        });
        let sup = supervise(&rx, &lim(400, 100, 5000), &cancel, &mut |_| {});
        h.join().unwrap();
        assert!(sup.timed_out);
    }

    #[test]
    fn a_quiet_phase_is_never_cancelled_and_is_reported() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let cancel = CancelToken::new();
        let h = std::thread::spawn(move || {
            tx.send(Msg::Progress(sample())).unwrap();
            tx.send(Msg::Phase(Phase::Quiet)).unwrap();
            std::thread::sleep(Duration::from_millis(500));
            let _ = tx.send(Msg::Done);
        });
        let mut seen = Vec::new();
        let sup = supervise(&rx, &lim(100, 100, 5000), &cancel, &mut |p| seen.push(p));
        h.join().unwrap();
        assert!(!sup.timed_out && !cancel.is_cancelled());
        assert!(sup.notices >= 2, "{}", sup.notices);
        let notes: Vec<_> = seen.iter().filter(|p| p.not_responding).collect();
        assert_eq!(notes.len() as u32, sup.notices);
        // The notice keeps the last known operation.
        assert!(notes.iter().all(|p| p.ref_ == "app/a.b/x/s"));
        assert!(!seen[0].not_responding);
    }

    #[test]
    fn an_uninstall_is_quiet_from_the_start_and_stays_quiet() {
        let mut p = OpPhase::new(true);
        assert_eq!(p.phase(), Phase::Quiet);
        // Reports without bytes, at any percent, never leave the quiet phase.
        for pc in [0, 0, 50, 100, 100] {
            assert_eq!(p.report(pc, 0), None);
        }
        assert_eq!(p.phase(), Phase::Quiet);
        // A source silent for longer than the stall limit is not cancelled.
        let (tx, rx) = mpsc::channel::<Msg>();
        let cancel = CancelToken::new();
        let h = std::thread::spawn(move || {
            tx.send(Msg::Phase(OpPhase::new(true).phase())).unwrap();
            std::thread::sleep(Duration::from_millis(400));
            let _ = tx.send(Msg::Done);
        });
        let sup = supervise(&rx, &lim(100, 100, 5000), &cancel, &mut |_| {});
        h.join().unwrap();
        assert!(!sup.timed_out && !cancel.is_cancelled());
        assert!(sup.notices >= 2, "{}", sup.notices);
    }

    #[test]
    fn a_download_is_quiet_at_100_percent_and_goes_back_when_bytes_grow() {
        let mut p = OpPhase::new(false);
        assert_eq!(p.phase(), Phase::Downloading);
        assert_eq!(p.report(10, 100), None);
        assert_eq!(p.report(99, 990), None);
        // 100%: deploy.
        assert_eq!(p.report(100, 1000), Some(Phase::Quiet));
        assert_eq!(p.report(100, 1000), None);
        // The bytes grow again: it was a premature 100%.
        assert_eq!(p.report(100, 1500), Some(Phase::Downloading));
        assert_eq!(p.report(100, 2000), None);
        // The real end: no new bytes, so deploy.
        assert_eq!(p.report(100, 2000), Some(Phase::Quiet));
        assert_eq!(p.report(100, 2000), None);
        // Falling below 100% is downloading.
        assert_eq!(p.report(40, 2100), Some(Phase::Downloading));

        // Through the watchdog: quiet, then bytes grow (Downloading), then
        // silence is a stall and ends as a timeout.
        let (tx, rx) = mpsc::channel::<Msg>();
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let h = std::thread::spawn(move || {
            tx.send(Msg::Phase(Phase::Quiet)).unwrap();
            std::thread::sleep(Duration::from_millis(250));
            assert!(!c2.is_cancelled(), "quiet is not cancelled");
            tx.send(Msg::Phase(Phase::Downloading)).unwrap();
            while !c2.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = tx.send(Msg::Done);
        });
        let sup = supervise(&rx, &lim(100, 100, 5000), &cancel, &mut |_| {});
        h.join().unwrap();
        assert!(sup.timed_out && cancel.is_timed_out());
    }

    #[test]
    fn the_user_scope_cancels_a_quiet_phase_at_the_ceiling_and_the_system_scope_does_not() {
        assert_eq!(QUIET_CEILING, Duration::from_secs(30 * 60));
        assert_eq!(
            Limits::for_scope(Scope::User).quiet_ceiling,
            Some(QUIET_CEILING)
        );
        assert_eq!(Limits::for_scope(Scope::System).quiet_ceiling, None);
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut st = Stall::new(s(300), s(120), t0).with_quiet_ceiling(Some(s(1800)));
        st.set_phase(Phase::Quiet, t0);
        assert!(!st.expired(t0 + s(1799)));
        assert!(st.expired(t0 + s(1800)));
        // News restarts the count.
        st.beat(t0 + s(1000));
        assert!(!st.expired(t0 + s(2799)));
        assert!(st.expired(t0 + s(2800)));
        // No ceiling: never.
        let mut st = Stall::new(s(300), s(120), t0);
        st.set_phase(Phase::Quiet, t0);
        assert!(!st.expired(t0 + s(100_000)));

        // Through the watchdog, with a short ceiling.
        let (tx, rx) = mpsc::channel::<Msg>();
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let h = std::thread::spawn(move || {
            tx.send(Msg::Phase(Phase::Quiet)).unwrap();
            while !c2.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = tx.send(Msg::Done);
        });
        let mut l = lim(100, 100, 5000);
        l.quiet_ceiling = Some(Duration::from_millis(300));
        let sup = supervise(&rx, &l, &cancel, &mut |_| {});
        h.join().unwrap();
        assert!(sup.timed_out && cancel.is_timed_out());
        assert!(sup.notices >= 1, "notices come before the ceiling");
    }

    #[test]
    fn a_cancel_that_is_slow_to_land_is_reported_and_waited_for() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let h = std::thread::spawn(move || {
            while !c2.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            // Ignores the cancel for a while, then finishes.
            std::thread::sleep(Duration::from_millis(400));
            let _ = tx.send(Msg::Done);
        });
        let started = Instant::now();
        let mut seen = Vec::new();
        let c3 = cancel.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            c3.cancel();
        });
        let sup = supervise(&rx, &lim(5000, 5000, 100), &cancel, &mut |p| seen.push(p));
        h.join().unwrap();
        canceller.join().unwrap();
        assert!(started.elapsed() >= Duration::from_millis(400), "it waited");
        assert!(!sup.timed_out);
        assert_eq!(sup.notices, 1);
        assert_eq!(seen.len(), 1);
        assert!(seen[0].not_responding);
    }

    #[test]
    fn beats_keep_it_alive_and_a_user_cancel_is_not_a_timeout() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let cancel = CancelToken::new();
        let h = std::thread::spawn(move || {
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(20));
                let _ = tx.send(Msg::Beat);
            }
            let _ = tx.send(Msg::Done);
        });
        let sup = supervise(&rx, &lim(100, 100, 5000), &cancel, &mut |_| {});
        h.join().unwrap();
        assert!(!sup.timed_out && !cancel.is_cancelled());
        let c = CancelToken::new();
        c.cancel();
        c.cancel_timed_out();
        assert!(c.is_cancelled() && !c.is_timed_out());
    }

    #[test]
    fn a_panic_in_progress_cancels_and_is_kept() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let cancel = CancelToken::new();
        let p = sample();
        tx.send(Msg::Progress(p.clone())).unwrap();
        tx.send(Msg::Progress(p)).unwrap();
        tx.send(Msg::Done).unwrap();
        let mut calls = 0;
        let sup = supervise(&rx, &lim(5000, 5000, 5000), &cancel, &mut |_| {
            calls += 1;
            panic!("boom");
        });
        assert_eq!(calls, 1, "no more callbacks after a panic");
        assert!(cancel.is_cancelled() && !cancel.is_timed_out());
        assert!(sup.panic.is_some());
    }

    #[test]
    fn a_panic_in_the_operation_thread_is_an_error() {
        let cancel = CancelToken::new();
        let r: Result<((), bool), Error> = run_supervised(&cancel, &mut |_| {}, |_| {
            panic!("inside the operation thread");
        });
        assert!(matches!(r, Err(Error::Flatpak { .. })), "{r:?}");
    }
}
