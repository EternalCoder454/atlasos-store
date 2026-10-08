//! The inspection runs in a process of its own. The squashfs reader, the
//! decompressors and the XML parser all work on bytes written by a stranger;
//! a bug in any of them must not be able to take the Store's window down or
//! use more than a little memory or time. So `telamon-store --appimage-inspect
//! <file>` (this module's [`child_main`], called before Qt starts) reads the
//! file under resource limits and prints [`Inspection::encode`]'s answer, and
//! [`run`] starts it, waits for it with a timeout and reads the answer back
//! as untrusted input.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::inspect::{InspectError, Inspection, inspect};
use super::squash::Limits;

/// Most output read from the helper: a header, and an icon of at most 1 MiB.
const MAX_OUTPUT: u64 = 2 << 20;
/// How long the helper may take (hashing a 4 GB file is the longest part).
pub const TIMEOUT: Duration = Duration::from_secs(180);
/// CPU seconds the helper may use.
const CPU_SECONDS: u64 = 150;
/// Memory the helper may add to what it has when it starts.
const EXTRA_MEMORY: u64 = 1 << 30;

fn set_limit(resource: libc::__rlimit_resource_t, value: u64) {
    let lim = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    // SAFETY: setrlimit reads the struct we pass; a failure only means the
    // limit is not applied.
    unsafe {
        libc::setrlimit(resource, &lim);
    }
}

/// The address space in use now, in bytes.
fn address_space() -> u64 {
    let pages = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok())
        })
        .unwrap_or(0);
    // SAFETY: sysconf has no side effects.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
    pages * page
}

/// No core files: a crash must not leave a dump of what was in memory.
pub fn limit_core() {
    set_limit(libc::RLIMIT_CORE, 0);
}

/// Puts the limits on this process: no core files, no big files written, a
/// bounded address space and CPU time, no new privileges.
pub fn limit_self() {
    set_limit(libc::RLIMIT_CORE, 0);
    // The only files it writes are the few small ones `gpgv` is given.
    set_limit(libc::RLIMIT_FSIZE, 1 << 20);
    set_limit(libc::RLIMIT_CPU, CPU_SECONDS);
    set_limit(libc::RLIMIT_AS, address_space() + EXTRA_MEMORY);
    // SAFETY: prctl with these arguments only sets a flag on this process.
    unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    }
}

/// The helper's `main`: inspects `path` and writes the answer to stdout.
/// Returns the exit code.
pub fn child_main(path: &Path) -> i32 {
    use std::io::Write;
    limit_self();
    let out = match inspect(path, &Limits::default()) {
        Ok(i) => i.encode(),
        Err(e) => format!("ERROR {e}\n").into_bytes(),
    };
    let mut so = std::io::stdout().lock();
    if so.write_all(&out).and_then(|()| so.flush()).is_err() {
        return 2;
    }
    0
}

/// Runs `exe --appimage-inspect <path>` and reads its answer.
pub fn run(exe: &Path, path: &Path, timeout: Duration) -> Result<Inspection, InspectError> {
    let mut child = Command::new(exe)
        .arg("--appimage-inspect")
        .arg(path)
        .env_remove("XDG_ACTIVATION_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| InspectError::Helper(format!("could not start: {}", e.kind())))?;
    let mut stdout = child.stdout.take().expect("piped");
    // Read on a thread so the wait below can time out.
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(MAX_OUTPUT).read_to_end(&mut buf);
        // Anything past the cap is dropped by closing; the child gets SIGPIPE.
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = reader.join().unwrap_or_default();
    match status {
        None => Err(InspectError::Helper("it took too long".into())),
        Some(_) if bytes.is_empty() => Err(InspectError::Helper(
            "it stopped before it finished; the file may be damaged or too large".into(),
        )),
        Some(_) => Inspection::decode(&bytes),
    }
}
