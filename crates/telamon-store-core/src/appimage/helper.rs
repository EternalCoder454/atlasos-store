//! The inspection runs in a process of its own. The squashfs reader, the
//! decompressors and the XML parser all work on bytes written by a stranger;
//! a bug in any of them must not be able to take the Store's window down or
//! use more than a little memory or time. So `telamon-store --appimage-inspect
//! <file>` (this module's [`child_main`], called before Qt starts) reads the
//! file under resource limits and prints [`Inspection::encode`]'s answer, and
//! [`run`] starts it, waits for it with a timeout and reads the answer back
//! as untrusted input.
//!
//! The helper works in two stages (see [`super::sandbox`]). First it needs
//! more than the file: it opens it, reads the ELF headers, hashes it and runs
//! `gpgv`. Then it puts a seccomp allowlist on itself, and only then does the
//! squashfs walk, the decompression and the parsing of the file's contents.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::inspect::{self, InspectError, Inspection};
use super::sandbox;
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

/// Closes every descriptor above the standard three: whatever the Store held
/// open and did not mark close-on-exec (a socket, a pipe) must not be usable
/// by the helper, which is about to read a stranger's file. Done first, before
/// the file is opened.
pub fn close_inherited_fds() {
    // SAFETY: close_range only closes descriptors of this process.
    let rc = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) };
    if rc != 0 {
        // A kernel without close_range: the descriptors that can exist.
        for fd in 3..4096 {
            // SAFETY: closing a descriptor number; a bad one only fails.
            unsafe { libc::close(fd) };
        }
    }
}

/// Ends this process at once with `code`, without running exit handlers or
/// the destructors of the libraries the Store links (the sandboxed helper may
/// not make the system calls they would).
pub fn exit_now(code: i32) -> ! {
    // SAFETY: _exit ends the process.
    unsafe { libc::_exit(code) }
}

/// The helper's `main`: inspects `path` and writes the answer to stdout.
/// Returns the exit code. On x86-64 the process is sandboxed when this
/// returns (use [`exit_now`] to end it).
pub fn child_main(path: &Path) -> i32 {
    use std::io::Write;
    close_inherited_fds();
    limit_self();
    // Stage 1: everything that needs more than the open file.
    let out = match inspect::prepare(path) {
        Err(e) => format!("ERROR {e}\n").into_bytes(),
        // Stage 2: the file's contents, from inside the sandbox.
        Ok(prepared) => match sandbox::enter() {
            Ok(_) => prepared.finish(&Limits::default()).encode(),
            Err(e) => format!("ERROR {e}\n").into_bytes(),
        },
    };
    let mut so = std::io::stdout().lock();
    if so.write_all(&out).and_then(|()| so.flush()).is_err() {
        return 2;
    }
    0
}

/// The running program itself, for starting the helper: the kernel keeps the
/// executable that is running even when a package upgrade replaced its file,
/// so the helper is the same version as the Store that asks.
pub const SELF: &str = "/proc/self/exe";

/// The path of this program's file for starting something that must be the
/// installed (possibly newer) one: what `/proc/self/exe` points to, without
/// the " (deleted)" the kernel adds when the file was replaced.
pub fn installed_path() -> Option<std::path::PathBuf> {
    let link = std::fs::read_link(SELF).ok()?;
    let text = link.to_str()?;
    Some(std::path::PathBuf::from(
        text.strip_suffix(" (deleted)").unwrap_or(text),
    ))
}

/// Kills every process of the group a child leads (`process_group(0)` made
/// its pid the group's).
pub fn kill_group(leader: u32) {
    if let Ok(pid) = i32::try_from(leader)
        && pid > 1
    {
        // SAFETY: kill with a negative pid signals that process group.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
}

/// Runs `exe --appimage-inspect <path>` and reads its answer.
pub fn run(exe: &Path, path: &Path, timeout: Duration) -> Result<Inspection, InspectError> {
    let mut cmd = Command::new(exe);
    cmd.arg("--appimage-inspect").arg(path).env_clear();
    // Only what the helper reads: where `gpgv` may make its private folder,
    // and the language for the texts it picks. Nothing else of the Store's
    // environment (no activation token, no session secrets).
    for var in [
        "XDG_RUNTIME_DIR",
        "TMPDIR",
        "LANG",
        "LANGUAGE",
        "LC_ALL",
        "LC_MESSAGES",
    ] {
        if let Some(v) = std::env::var_os(var) {
            cmd.env(var, v);
        }
    }
    // Its own process group: a timeout ends `gpgv` too, not only the helper.
    let mut child = cmd
        .process_group(0)
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
                kill_group(child.id());
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
