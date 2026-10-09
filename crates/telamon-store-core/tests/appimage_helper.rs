//! The inspection helper and its sandbox, for real.
//!
//! This test has its own `main` (`harness = false` in Cargo.toml) because it
//! is also the helper: `helper::run` starts `<this program> --appimage-inspect
//! <file>`, exactly as the Store starts itself, and the first thing `main`
//! does with those arguments is what `telamon-store`'s `main` does
//! (`appimage_cli::early`): `helper::child_main`. So the helper process in
//! these tests is the real code path, seccomp filter included.
//!
//! The syscall tests fork a child, put the real filter on it, make one
//! forbidden system call and check that the kernel killed it (`SIGSYS`). They
//! only run on x86-64, the only architecture the filter is built for.
#[path = "common/appimage.rs"]
mod build;

use std::path::{Path, PathBuf};
use std::time::Duration;

use backhand::compression::Compressor;
use telamon_store_core::appimage::helper;
use telamon_store_core::appimage::inspect::{InspectError, Inspection, inspect};
use telamon_store_core::appimage::sandbox::{self, Filter, Rule};
use telamon_store_core::appimage::sign::Signature;
use telamon_store_core::appimage::squash::Limits;

type Test = (&'static str, fn());

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // As the Store's main does.
    if let [_, opt, path] = args.as_slice()
        && opt == "--appimage-inspect"
    {
        helper::exit_now(helper::child_main(Path::new(path)));
    }
    // Verifies a signature with the program given as `gpgv` (a stand-in), for
    // as long as it takes.
    if let [_, opt, script] = args.as_slice()
        && opt == "--verify-with"
    {
        let got = telamon_store_core::appimage::sign::verify_within(
            Path::new(script),
            &[0x89, 1, 2, 3],
            &[0x99, 1, 2, 3],
            "x",
            Duration::from_secs(300),
        );
        std::process::exit(if got == Signature::Unchecked { 0 } else { 1 });
    }
    // Sandboxed, then waits for its input to close (the test looks at
    // /proc/<pid>/status meanwhile).
    if args.get(1).map(String::as_str) == Some("--sandboxed-wait") {
        helper::close_inherited_fds();
        sandbox::enter().expect("the sandbox");
        let mut byte = [0u8; 1];
        // SAFETY: read on stdin into a buffer we own; the call is allowed.
        unsafe { libc::read(0, byte.as_mut_ptr().cast(), 1) };
        helper::exit_now(0);
    }
    let want: Vec<&String> = args[1..].iter().filter(|a| !a.starts_with('-')).collect();
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, test) in tests() {
        if !want.is_empty() && !want.iter().any(|w| name.contains(w.as_str())) {
            continue;
        }
        ran += 1;
        match std::panic::catch_unwind(test) {
            Ok(()) => println!("test {name} ... ok"),
            Err(_) => {
                println!("test {name} ... FAILED");
                failed.push(name);
            }
        }
    }
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        ran - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        std::process::exit(1);
    }
}

fn tests() -> Vec<Test> {
    let mut t: Vec<Test> = vec![
        (
            "every_compression_gives_the_same_answer_through_the_helper",
            every_compression_gives_the_same_answer_through_the_helper,
        ),
        (
            "a_helper_that_lies_in_its_second_record_changes_nothing_stage_one_computed",
            a_helper_that_lies_in_its_second_record_changes_nothing_stage_one_computed,
        ),
        (
            "without_a_filter_the_contents_are_not_read",
            without_a_filter_the_contents_are_not_read,
        ),
        (
            "gpgv_dies_with_the_helper_that_started_it",
            gpgv_dies_with_the_helper_that_started_it,
        ),
        (
            "a_big_image_is_read_under_the_sandbox_too",
            a_big_image_is_read_under_the_sandbox_too,
        ),
        (
            "a_signed_file_is_checked_before_the_sandbox_and_answers_the_same",
            a_signed_file_is_checked_before_the_sandbox_and_answers_the_same,
        ),
        (
            "the_helper_refuses_bad_files_in_plain_words",
            the_helper_refuses_bad_files_in_plain_words,
        ),
        (
            "the_helper_does_not_inherit_the_stores_descriptors",
            the_helper_does_not_inherit_the_stores_descriptors,
        ),
        (
            "a_helper_that_hangs_is_killed_with_its_children",
            a_helper_that_hangs_is_killed_with_its_children,
        ),
    ];
    if sandbox::AVAILABLE {
        t.extend([
            (
                "the_sandboxed_helper_has_a_seccomp_filter_and_no_new_privs",
                the_sandboxed_helper_has_a_seccomp_filter_and_no_new_privs as fn(),
            ),
            (
                "forbidden_system_calls_kill_the_process",
                forbidden_system_calls_kill_the_process,
            ),
            (
                "the_calls_the_parsers_need_still_work",
                the_calls_the_parsers_need_still_work,
            ),
            (
                "gpgv_gets_no_sockets_tracing_or_keys_and_everything_else",
                gpgv_gets_no_sockets_tracing_or_keys_and_everything_else,
            ),
            (
                "an_abort_ends_with_sigabrt_not_sigsys",
                an_abort_ends_with_sigabrt_not_sigsys,
            ),
            (
                "an_errno_rule_fails_the_call_without_killing",
                an_errno_rule_fails_the_call_without_killing,
            ),
        ]);
    }
    t
}

// ---- through the real helper process ----

fn this_program() -> PathBuf {
    std::env::current_exe().unwrap()
}

fn via_helper(path: &Path) -> Result<Inspection, InspectError> {
    helper::run(&this_program(), path, Duration::from_secs(120))
}

/// What the in-process inspection says, cleaned the way the Store cleans the
/// helper's answer.
fn in_process(path: &Path) -> Inspection {
    let mut i = inspect(path, &Limits::default()).unwrap();
    i.sanitize();
    i
}

fn squash_with(c: Compressor) -> build::Squash {
    build::Squash {
        compressor: c,
        ..build::normal_squash()
    }
}

fn every_compression_gives_the_same_answer_through_the_helper() {
    let dir = build::scratch("helper-compress");
    for (name, c) in [
        ("gzip", Compressor::Gzip),
        ("xz", Compressor::Xz),
        ("zstd", Compressor::Zstd),
    ] {
        let bytes = build::type2(&squash_with(c).build(), &[], &[]);
        let p = build::write(&dir, &format!("Sample-{name}.AppImage"), &bytes);
        let got = via_helper(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(got.inspected, "{name}: {}", got.note);
        assert_eq!(got.name, "Sample Draw", "{name}");
        assert_eq!(got.app_id, "org.example.Sample", "{name}");
        assert!(got.icon.is_some(), "{name}: the icon came through");
        assert_eq!(
            got,
            in_process(&p),
            "{name}: same answer as without the sandbox"
        );
    }
}

/// A "helper": a program that prints `bytes` whatever it is asked.
fn fake_helper(dir: &Path, bytes: &[u8]) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let out = dir.join("answer");
    std::fs::write(&out, bytes).unwrap();
    let script = dir.join("helper.sh");
    std::fs::write(&script, format!("#!/bin/sh\ncat '{}'\n", out.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn a_helper_that_lies_in_its_second_record_changes_nothing_stage_one_computed() {
    let dir = build::scratch("helper-liar");
    let p = build::write(&dir, "Sample.AppImage", &build::normal());
    let real = in_process(&p);
    let ask = |bytes: &[u8]| {
        let script = fake_helper(&dir, bytes);
        helper::run(&script, &p, Duration::from_secs(20))
    };
    let facts = real.encode_facts();
    let contents = real.encode_contents();
    // As it should be.
    assert_eq!(
        ask(&[facts.clone(), contents.clone()].concat()).unwrap(),
        real
    );
    // Stage 2 claims another hash, size, format, origin and signature,
    // inside its own record: the fields are not read.
    let nl = contents.iter().position(|b| *b == b'\n').unwrap();
    let (line, icon_and_rest) = contents.split_at(nl + 1);
    let line = std::str::from_utf8(&line[..nl]).unwrap();
    let forged_line = line.trim_end_matches('}').to_string()
        + &format!(
            r#","sha256":"{}","size":7,"format":"type1","file_name":"x","origin":{{"kind":"https","host":"trusted.example"}},"signature":{{"state":"signed","fingerprint":"{}"}}}}"#,
            "0".repeat(64),
            "A".repeat(40)
        );
    let forged = [
        facts.clone(),
        format!("{forged_line}\n").into_bytes(),
        icon_and_rest.to_vec(),
    ]
    .concat();
    assert_eq!(ask(&forged).unwrap(), real);
    // It writes a whole other first record where the second goes, or a third
    // one after: the facts are the first line's.
    let other = {
        let mut i = real.clone();
        i.sha256 = "0".repeat(64);
        i.origin = telamon_store_core::appimage::origin::Origin::Https {
            host: "trusted.example".into(),
        };
        i.signature = Signature::Signed {
            fingerprint: "A".repeat(40),
        };
        i
    };
    let got = ask(&[facts.clone(), other.encode_facts()].concat()).unwrap();
    assert!(!got.inspected);
    assert_eq!(
        (got.sha256.as_str(), &got.origin, &got.signature),
        (real.sha256.as_str(), &real.origin, &real.signature)
    );
    let got = ask(&[facts.clone(), contents.clone(), other.encode_facts()].concat()).unwrap();
    assert_eq!(
        (got.sha256.as_str(), &got.origin, &got.signature),
        (real.sha256.as_str(), &real.origin, &real.signature)
    );
    // Cut short or missing: not looked into, the facts kept. No facts: fails.
    for cut in [0, 1, 10, contents.len() / 2] {
        let got = ask(&[facts.clone(), contents[..cut].to_vec()].concat()).unwrap();
        assert!(!got.inspected, "cut at {cut}");
        assert_eq!(got.sha256, real.sha256);
        assert_eq!(got.origin, real.origin);
    }
    assert!(ask(&contents).is_err());
    assert!(ask(&facts[..facts.len() / 2]).is_err());
}

fn without_a_filter_the_contents_are_not_read() {
    use telamon_store_core::appimage::inspect::prepare;
    let dir = build::scratch("helper-nofilter");
    let p = build::write(&dir, "Sample.AppImage", &build::normal());
    let decode = |entered| {
        let prepared = prepare(&p).unwrap();
        let facts = prepared.facts_record();
        Inspection::decode(&[facts, helper::second_record(prepared, entered)].concat()).unwrap()
    };
    // A filter that could not be made: never read, on any processor.
    let got = decode(Err("no seccomp".into()));
    assert!(!got.inspected);
    assert!(got.note.contains("safe place"), "{}", got.note);
    assert_eq!(got.sha256, in_process(&p).sha256, "the facts are kept");
    // A processor without a filter: read only with the feature that says so.
    let got = decode(Ok(sandbox::Entered::Skipped));
    if cfg!(feature = "unsandboxed-inspector") {
        assert!(got.inspected);
    } else {
        assert!(!got.inspected);
        assert!(got.note.contains("kind of processor"), "{}", got.note);
        assert_eq!(got.sha256, in_process(&p).sha256);
        assert!(got.icon.is_none() && got.app_id.is_empty());
    }
    // With the filter on, it is read.
    let got = decode(Ok(sandbox::Entered::Yes));
    assert!(got.inspected);
    assert_eq!(got.name, "Sample Draw");
}

fn gpgv_dies_with_the_helper_that_started_it() {
    use std::os::unix::fs::PermissionsExt;
    let dir = build::scratch("gpgv-orphan");
    let pidfile = dir.join("gpgv.pid");
    let script = dir.join("gpgv");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec /usr/bin/sleep 300\n",
            pidfile.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // This program as a stand-in for the helper, waiting for "gpgv".
    let mut helper_like = std::process::Command::new(this_program())
        .arg("--verify-with")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut pid = 0i32;
    for _ in 0..200 {
        if let Ok(t) = std::fs::read_to_string(&pidfile)
            && let Ok(n) = t.trim().parse()
        {
            pid = n;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(pid > 1, "gpgv did not start");
    // gpgv is in a group of its own, so killing the helper's group (what the
    // timeout does) would not reach it: it has to die on its own.
    // SAFETY: getpgid asks about a process we know.
    assert_ne!(unsafe { libc::getpgid(pid) }, unsafe { libc::getpgid(0) });
    helper_like.kill().unwrap();
    let _ = helper_like.wait();
    let gone = (0..200).any(|_| {
        std::thread::sleep(Duration::from_millis(25));
        std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |s| s.contains(") Z"))
    });
    assert!(gone, "gpgv outlived the helper that started it");
}

fn gpgv_gets_no_sockets_tracing_or_keys_and_everything_else() {
    use telamon_store_core::appimage::sandbox::gpgv_filter;
    fn run(body: fn()) -> End {
        // SAFETY: single threaded; the child allocates its own filter.
        match unsafe { libc::fork() } {
            -1 => panic!("fork"),
            0 => {
                let code = std::panic::catch_unwind(|| {
                    gpgv_filter()
                        .expect("a filter")
                        .install()
                        .expect("installed");
                    body();
                })
                .map_or(99, |()| 0);
                helper::exit_now(code)
            }
            pid => {
                let mut status = 0;
                // SAFETY: waitpid on our own child.
                unsafe { libc::waitpid(pid, &mut status, 0) };
                if libc::WIFSIGNALED(status) {
                    End::Signal(libc::WTERMSIG(status))
                } else {
                    End::Exit(libc::WEXITSTATUS(status))
                }
            }
        }
    }
    fn refused(nr: i64, a: [usize; 4]) {
        let r = sys(nr, a);
        assert_eq!(r, -1, "call {nr} went through");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM),
            "call {nr}"
        );
    }
    // Refused with EPERM, and the process lives on.
    assert_eq!(
        run(|| {
            refused(
                libc::SYS_socket,
                [libc::AF_INET as usize, libc::SOCK_STREAM as usize, 0, 0],
            );
            refused(
                libc::SYS_socket,
                [libc::AF_UNIX as usize, libc::SOCK_STREAM as usize, 0, 0],
            );
            refused(libc::SYS_connect, [3, 0, 0, 0]);
            refused(libc::SYS_ptrace, [libc::PTRACE_TRACEME as usize, 0, 0, 0]);
            refused(libc::SYS_keyctl, [0, 0, 0, 0]);
            refused(libc::SYS_mount, [0, 0, 0, 0]);
            refused(libc::SYS_unshare, [libc::CLONE_NEWUSER as usize, 0, 0, 0]);
            refused(libc::SYS_io_uring_setup, [4, 0, 0, 0]);
            refused(libc::SYS_bpf, [0, 0, 0, 0]);
            refused(libc::SYS_perf_event_open, [0, 0, 0, 0]);
            refused(libc::SYS_userfaultfd, [0, 0, 0, 0]);
            refused(libc::SYS_personality, [0xffff_ffff, 0, 0, 0]);
        }),
        End::Exit(0)
    );
    // What gpgv does still works: open and read a file, map memory, run a
    // program (it execs itself through the filter), make a pipe.
    assert_eq!(
        run(|| {
            let t = std::fs::read_to_string("/proc/self/stat").unwrap();
            assert!(!t.is_empty());
            let mut v = vec![0u8; 1 << 20];
            v[5] = 1;
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", "echo ok"])
                .output()
                .unwrap();
            assert_eq!(out.stdout, b"ok\n");
        }),
        End::Exit(0)
    );
    // The other ABIs would get around a denylist: killed.
    assert_eq!(
        run(|| {
            sys(libc::SYS_getpid | 0x4000_0000, [0; 4]);
        }),
        End::Signal(libc::SIGSYS)
    );
}

fn a_big_image_is_read_under_the_sandbox_too() {
    // Thousands of entries (the tables grow the heap), an icon near the cap
    // (a large block that is allocated, read into and freed), and a long
    // metainfo: the calls the allowlist has to cover when the work is not
    // tiny.
    let dir = build::scratch("helper-big");
    let mut sq = build::normal_squash();
    sq.compressor = Compressor::Zstd;
    for i in 0..4000 {
        sq.files.push((
            format!("/usr/share/doc/pkg-{}/file-{i}", i % 50),
            format!("doc {i}").into_bytes(),
        ));
    }
    let mut icon = build::fake_png(256, 256);
    icon.resize(900_000, 7);
    sq.files.retain(|(p, _)| p != "/sample-draw.png");
    sq.files.push(("/sample-draw.png".into(), icon.clone()));
    let bytes = build::type2(&sq.build(), &[], &[]);
    let p = build::write(&dir, "Big.AppImage", &bytes);
    let got = via_helper(&p).unwrap();
    assert!(got.inspected, "{}", got.note);
    assert_eq!(got.icon.as_ref().map(|i| i.bytes.len()), Some(900_000));
    assert_eq!(got, in_process(&p));
}

/// A throw-away key, as `appimage.rs` makes one. `None` where gpg is missing.
mod signing {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::process::{Command, Stdio};

    pub struct Key {
        home: PathBuf,
        pub fingerprint: String,
    }

    impl Drop for Key {
        fn drop(&mut self) {
            let _ = Command::new("gpgconf")
                .arg("--homedir")
                .arg(&self.home)
                .args(["--kill", "gpg-agent"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    fn gpg(home: &Path, args: &[&str], stdin: Option<&[u8]>) -> Option<Vec<u8>> {
        use std::io::Write;
        let mut child = Command::new("gpg")
            .arg("--homedir")
            .arg(home)
            .args(["--batch", "--yes", "--pinentry-mode", "loopback"])
            .args(["--passphrase", ""])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        if let Some(data) = stdin {
            child.stdin.take().unwrap().write_all(data).ok()?;
        } else {
            drop(child.stdin.take());
        }
        let out = child.wait_with_output().ok()?;
        out.status.success().then_some(out.stdout)
    }

    pub fn make_key(tag: &str) -> Option<Key> {
        use std::os::unix::fs::PermissionsExt;
        let home = build::scratch(tag);
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).ok()?;
        gpg(
            &home,
            &[
                "--quick-generate-key",
                "Test Signer <test@example.org>",
                "ed25519",
                "sign",
                "never",
            ],
            None,
        )?;
        let colons =
            String::from_utf8(gpg(&home, &["--list-keys", "--with-colons"], None)?).ok()?;
        let fingerprint = colons.lines().find_map(|l| {
            l.strip_prefix("fpr:::::::::")
                .map(|r| r.trim_end_matches(':').to_string())
        })?;
        Some(Key { home, fingerprint })
    }

    /// What appimagetool does: sign the hex digest of the file with empty
    /// sections, put signature and key in the sections.
    pub fn signed(squash: &[u8], k: &Key) -> Vec<u8> {
        let unsigned = build::type2(squash, &[], &[]);
        let digest: String = Sha256::digest(&unsigned)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let sig = gpg(
            &k.home,
            &[
                "--armor",
                "--detach-sign",
                "--local-user",
                &k.fingerprint,
                "--output",
                "-",
            ],
            Some(digest.as_bytes()),
        )
        .unwrap();
        let key = gpg(&k.home, &["--armor", "--export", &k.fingerprint], None).unwrap();
        build::type2(squash, &sig, &key)
    }
}

fn a_signed_file_is_checked_before_the_sandbox_and_answers_the_same() {
    if telamon_store_core::appimage::sign::find_gpgv().is_none() {
        eprintln!("skipped: no gpgv");
        return;
    }
    let Some(key) = signing::make_key("helper-sig") else {
        eprintln!("skipped: no gpg to make a test key");
        return;
    };
    let dir = build::scratch("helper-signed");
    let bytes = signing::signed(&squash_with(Compressor::Zstd).build(), &key);
    let p = build::write(&dir, "Signed.AppImage", &bytes);
    let got = via_helper(&p).unwrap();
    assert_eq!(
        got.signature,
        Signature::Signed {
            fingerprint: key.fingerprint.to_ascii_uppercase()
        }
    );
    assert!(got.inspected, "{}", got.note);
    assert_eq!(got, in_process(&p));
    // Changed after signing: wrong, and still no panic in the sandbox.
    let mut changed = bytes.clone();
    let last = changed.len() - 1;
    changed[last] ^= 1;
    let p2 = build::write(&dir, "Changed.AppImage", &changed);
    assert_eq!(via_helper(&p2).unwrap().signature, Signature::Wrong);
}

fn the_helper_refuses_bad_files_in_plain_words() {
    let dir = build::scratch("helper-bad");
    let say = |name: &str, bytes: &[u8]| {
        let p = build::write(&dir, name, bytes);
        via_helper(&p).map(|_| ()).unwrap_err().to_string()
    };
    assert!(
        say("noise.AppImage", &[7u8; 10_000]).contains("isn't an AppImage"),
        "not an AppImage"
    );
    assert!(
        say("tiny.AppImage", b"\x7fELF").contains("isn't an AppImage"),
        "too small"
    );
    // A folder and a pipe are not files.
    let folder = dir.join("folder.AppImage");
    std::fs::create_dir(&folder).unwrap();
    let msg = via_helper(&folder).map(|_| ()).unwrap_err().to_string();
    assert!(msg.contains("not a regular file"), "{msg}");
    let fifo = dir.join("fifo.AppImage");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: mkfifo with a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let started = std::time::Instant::now();
    let msg = via_helper(&fifo).map(|_| ()).unwrap_err().to_string();
    assert!(msg.contains("not a regular file"), "{msg}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "a pipe must not block"
    );
    // Missing.
    let msg = via_helper(&dir.join("missing.AppImage"))
        .map(|_| ())
        .unwrap_err()
        .to_string();
    assert!(msg.contains("could not be read"), "{msg}");
    // An AppImage whose squashfs is damaged is described, not a failure.
    let mut bytes = build::normal();
    let at = build::runtime(&[], &[], 2).len();
    bytes[at + 4] = 0xff; // the inode count
    bytes[at + 5] = 0xff;
    bytes[at + 6] = 0xff;
    bytes[at + 7] = 0x7f;
    let p = build::write(&dir, "damaged.AppImage", &bytes);
    let got = via_helper(&p).unwrap();
    assert!(!got.inspected);
    assert!(got.note.contains("couldn't look inside"), "{}", got.note);
    assert_eq!(got.sha256.len(), 64);
}

fn the_helper_does_not_inherit_the_stores_descriptors() {
    // A descriptor the Store left open without close-on-exec, as a careless
    // library would: the helper's first act closes it, and leaves 0, 1 and 2.
    let dir = build::scratch("helper-fds");
    let secret = build::write(&dir, "secret", b"secret");
    let f = std::fs::File::open(&secret).unwrap();
    let fd = std::os::fd::AsRawFd::as_raw_fd(&f);
    // SAFETY: fcntl on our own descriptor.
    unsafe { libc::fcntl(fd, libc::F_SETFD, 0) };
    FD.store(fd, std::sync::atomic::Ordering::SeqCst);
    let end = forked(|| {
        let fd = FD.load(std::sync::atomic::Ordering::SeqCst);
        // SAFETY: fcntl on descriptors; a closed one only fails.
        unsafe {
            assert_ne!(libc::fcntl(fd, libc::F_GETFD), -1, "open before");
            helper::close_inherited_fds();
            assert_eq!(libc::fcntl(fd, libc::F_GETFD), -1, "closed after");
            for std_fd in 0..3 {
                assert_ne!(libc::fcntl(std_fd, libc::F_GETFD), -1, "fd {std_fd} stays");
            }
        }
    });
    assert_eq!(end, End::Exit(0));
}

fn a_helper_that_hangs_is_killed_with_its_children() {
    // A "helper" that starts a child of its own and sleeps: the timeout must
    // end both.
    let dir = build::scratch("helper-hang");
    let pidfile = dir.join("child.pid");
    let script = dir.join("hang.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n/usr/bin/sleep 300 &\necho $! > '{}'\nwait\n",
            pidfile.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let err = helper::run(&script, &dir.join("x.AppImage"), Duration::from_secs(2))
        .map(|_| ())
        .unwrap_err();
    assert!(err.to_string().contains("took too long"), "{err}");
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Gone (or a zombie of init's reaping): signal 0 fails.
    std::thread::sleep(Duration::from_millis(300));
    let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.contains(") Z"))
        .unwrap_or(false);
    assert!(!alive, "the helper's child outlived the timeout");
}

// ---- the filter itself ----

/// The number after `name:` in a /proc status text.
fn status_number(text: &str, name: &str) -> u32 {
    text.lines()
        .find_map(|l| l.strip_prefix(name)?.trim().parse().ok())
        .unwrap_or(0)
}

fn the_sandboxed_helper_has_a_seccomp_filter_and_no_new_privs() {
    use std::process::{Command, Stdio};
    // A container may have filters of its own: the helper must add one.
    let mine = std::fs::read_to_string("/proc/self/status").unwrap();
    let before = status_number(&mine, "Seccomp_filters:");
    let mut child = Command::new(this_program())
        .arg("--sandboxed-wait")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let status = format!("/proc/{}/status", child.id());
    let mut text = String::new();
    for _ in 0..400 {
        text = std::fs::read_to_string(&status).unwrap_or_default();
        if status_number(&text, "Seccomp_filters:") > before {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        status_number(&text, "Seccomp_filters:"),
        before + 1,
        "{text}"
    );
    assert_eq!(status_number(&text, "Seccomp:"), 2);
    assert_eq!(status_number(&text, "NoNewPrivs:"), 1);
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
}

/// How a forked child ended.
#[derive(Debug, PartialEq, Eq)]
enum End {
    Signal(i32),
    Exit(i32),
}

/// Forks; the child puts the inspector's filter on itself and runs `body`;
/// when `body` returns the child exits 0 ("survived").
fn sandboxed(body: fn()) -> End {
    run_forked(true, body)
}

/// The same without the filter.
fn forked(body: fn()) -> End {
    run_forked(false, body)
}

fn run_forked(filter: bool, body: fn()) -> End {
    // SAFETY: the harness is single threaded here, so the child may allocate.
    match unsafe { libc::fork() } {
        -1 => panic!("fork failed"),
        0 => {
            let code = std::panic::catch_unwind(|| {
                if filter {
                    sandbox::enter().expect("the filter");
                }
                body();
            })
            .map_or(99, |()| 0);
            helper::exit_now(code)
        }
        pid => {
            let mut status = 0;
            // SAFETY: waitpid on our own child.
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            if libc::WIFSIGNALED(status) {
                End::Signal(libc::WTERMSIG(status))
            } else {
                End::Exit(libc::WEXITSTATUS(status))
            }
        }
    }
}

fn sys(nr: i64, a: [usize; 4]) -> i64 {
    // SAFETY: only used for calls the filter must stop before they run, or
    // harmless ones.
    unsafe { libc::syscall(nr, a[0], a[1], a[2], a[3]) }
}

fn cstr(s: &'static [u8]) -> usize {
    s.as_ptr() as usize
}

fn forbidden_system_calls_kill_the_process() {
    let dir = build::scratch("filter-forbidden");
    // The file `unlink` and friends are aimed at must still be there.
    let victim = dir.join("victim");
    std::fs::write(&victim, b"x").unwrap();
    VICTIM
        .set(std::ffi::CString::new(victim.to_str().unwrap()).unwrap())
        .ok();
    let cases: Vec<(&str, fn())> = vec![
        ("socket", || {
            sys(
                libc::SYS_socket,
                [libc::AF_UNIX as usize, libc::SOCK_STREAM as usize, 0, 0],
            );
        }),
        ("connect", || {
            sys(libc::SYS_connect, [3, cstr(b"\x01\x00/nope\0"), 8, 0]);
        }),
        ("execve", || {
            sys(libc::SYS_execve, [cstr(b"/bin/true\0"), 0, 0, 0]);
        }),
        ("execveat", || {
            sys(
                libc::SYS_execveat,
                [libc::AT_FDCWD as usize, cstr(b"/bin/true\0"), 0, 0],
            );
        }),
        ("openat", || {
            sys(
                libc::SYS_openat,
                [libc::AT_FDCWD as usize, cstr(b"/etc/passwd\0"), 0, 0],
            );
        }),
        ("open", || {
            sys(libc::SYS_open, [cstr(b"/etc/passwd\0"), 0, 0, 0]);
        }),
        ("creat", || {
            sys(
                libc::SYS_creat,
                [cstr(b"/tmp/telamon-test-creat\0"), 0o600, 0, 0],
            );
        }),
        ("ptrace", || {
            sys(libc::SYS_ptrace, [libc::PTRACE_TRACEME as usize, 0, 0, 0]);
        }),
        ("clone", || {
            sys(libc::SYS_clone, [libc::SIGCHLD as usize, 0, 0, 0]);
        }),
        ("clone3", || {
            sys(libc::SYS_clone3, [0, 0, 0, 0]);
        }),
        ("fork", || {
            sys(libc::SYS_fork, [0; 4]);
        }),
        ("vfork", || {
            sys(libc::SYS_vfork, [0; 4]);
        }),
        ("unlink", || {
            sys(
                libc::SYS_unlink,
                [VICTIM.get().unwrap().as_ptr() as usize, 0, 0, 0],
            );
        }),
        ("unlinkat", || {
            sys(
                libc::SYS_unlinkat,
                [
                    libc::AT_FDCWD as usize,
                    VICTIM.get().unwrap().as_ptr() as usize,
                    0,
                    0,
                ],
            );
        }),
        ("rename", || {
            sys(
                libc::SYS_rename,
                [cstr(b"/nope-a\0"), cstr(b"/nope-b\0"), 0, 0],
            );
        }),
        ("mkdir", || {
            sys(
                libc::SYS_mkdir,
                [cstr(b"/tmp/telamon-test-mkdir\0"), 0o700, 0, 0],
            );
        }),
        ("chmod", || {
            sys(libc::SYS_chmod, [cstr(b"/nope\0"), 0o777, 0, 0]);
        }),
        ("symlink", || {
            sys(
                libc::SYS_symlink,
                [cstr(b"/a\0"), cstr(b"/tmp/telamon-test-link\0"), 0, 0],
            );
        }),
        ("ioctl", || {
            sys(libc::SYS_ioctl, [1, libc::TIOCSTI as usize, 0, 0]);
        }),
        ("kill", || {
            sys(libc::SYS_kill, [1, 0, 0, 0]);
        }),
        ("mount", || {
            sys(libc::SYS_mount, [0, 0, 0, 0]);
        }),
        ("setuid", || {
            sys(libc::SYS_setuid, [0, 0, 0, 0]);
        }),
        ("prctl", || {
            sys(libc::SYS_prctl, [libc::PR_SET_DUMPABLE as usize, 1, 0, 0]);
        }),
        ("bpf", || {
            sys(libc::SYS_bpf, [0, 0, 0, 0]);
        }),
        ("io_uring_setup", || {
            sys(libc::SYS_io_uring_setup, [4, 0, 0, 0]);
        }),
        ("memfd_create", || {
            sys(libc::SYS_memfd_create, [cstr(b"x\0"), 0, 0, 0]);
        }),
        ("process_vm_readv", || {
            sys(libc::SYS_process_vm_readv, [1, 0, 0, 0]);
        }),
        ("pipe2", || {
            sys(libc::SYS_pipe2, [0, 0, 0, 0]);
        }),
        ("dup", || {
            sys(libc::SYS_dup, [0, 0, 0, 0]);
        }),
        ("fcntl, any other command", || {
            sys(libc::SYS_fcntl, [0, libc::F_SETFL as usize, 0, 0]);
        }),
        ("a second filter", || {
            sys(libc::SYS_seccomp, [1, 0, 0, 0]);
        }),
        ("write to a descriptor that is not stdout or stderr", || {
            sys(libc::SYS_write, [5, cstr(b"x"), 1, 0]);
        }),
        ("mmap that is executable", || {
            sys(
                libc::SYS_mmap,
                [0, 4096, (libc::PROT_READ | libc::PROT_EXEC) as usize, 0],
            );
        }),
        ("mprotect that makes memory executable", || {
            let page = [0u8; 8192];
            let at = (page.as_ptr() as usize + 4095) & !4095;
            sys(
                libc::SYS_mprotect,
                [at, 4096, (libc::PROT_READ | libc::PROT_EXEC) as usize, 0],
            );
        }),
        ("tgkill to another process", || {
            sys(libc::SYS_tgkill, [1, 1, libc::SIGABRT as usize, 0]);
        }),
        ("tgkill with another signal", || {
            let me = std::process::id() as usize;
            sys(libc::SYS_tgkill, [me, me, libc::SIGKILL as usize, 0]);
        }),
        ("a call through the 32-bit ABI", || {
            // SAFETY: on x86-64 `int 0x80` is the 32-bit entry; eax=1 is exit.
            #[cfg(target_arch = "x86_64")]
            unsafe {
                std::arch::asm!(
                    "xchg {tmp:r}, rbx",
                    "int 0x80",
                    "xchg {tmp:r}, rbx",
                    tmp = inout(reg) 0u64 => _,
                    in("eax") 1,
                    options(nostack),
                );
            }
        }),
        ("a call through the x32 ABI", || {
            sys(libc::SYS_getpid | 0x4000_0000, [0; 4]);
        }),
    ];
    for (name, body) in cases {
        assert_eq!(
            sandboxed(body),
            End::Signal(libc::SIGSYS),
            "{name} must kill the process"
        );
    }
    assert!(victim.exists(), "unlink must not have run");
    for left in [
        "telamon-test-creat",
        "telamon-test-mkdir",
        "telamon-test-link",
    ] {
        assert!(!Path::new("/tmp").join(left).exists(), "{left} was made");
    }
}

static VICTIM: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();

fn the_calls_the_parsers_need_still_work() {
    // The harness has no threads here; this is what the squashfs walk and the
    // decompressors do after the filter: allocate, hash maps, read and seek a
    // file they hold, write the answer.
    let dir = build::scratch("filter-allowed");
    let p = build::write(&dir, "f", &[1u8; 70_000]);
    let f = std::fs::File::open(&p).unwrap();
    // Passed to the child through a descriptor number.
    let fd = std::os::fd::AsRawFd::as_raw_fd(&f);
    FD.store(fd, std::sync::atomic::Ordering::SeqCst);
    let end = sandboxed(|| {
        use std::collections::HashMap;
        use std::io::{Read, Seek, SeekFrom, Write};
        use std::os::fd::FromRawFd;
        use std::os::unix::fs::FileExt;
        let fd = FD.load(std::sync::atomic::Ordering::SeqCst);
        // SAFETY: the descriptor is the test's open file, inherited by fork.
        let mut f = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        let mut m: HashMap<String, Vec<u8>> = HashMap::new();
        for i in 0..2000 {
            m.insert(format!("k{i}"), vec![i as u8; 4096]);
        }
        let mut one = [0u8; 10];
        f.read_exact_at(&mut one, 100).unwrap();
        f.seek(SeekFrom::Start(5)).unwrap();
        let mut buf = vec![0u8; 60_000];
        f.read_exact(&mut buf).unwrap();
        let copy = f.try_clone().unwrap();
        drop(copy);
        // A big allocation (mmap), grown and freed (mremap, munmap, madvise).
        let mut big: Vec<u8> = Vec::new();
        for _ in 0..40 {
            big.extend(std::iter::repeat_n(7u8, 1 << 20));
        }
        big.shrink_to_fit();
        drop(big);
        let mut so = std::io::stdout().lock();
        so.write_all(b"answer\n").unwrap();
        so.flush().unwrap();
        assert_eq!(m.len(), 2000);
    });
    assert_eq!(end, End::Exit(0));
}

static FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

fn an_abort_ends_with_sigabrt_not_sigsys() {
    assert_eq!(
        sandboxed(|| std::process::abort()),
        End::Signal(libc::SIGABRT)
    );
}

fn an_errno_rule_fails_the_call_without_killing() {
    // SAFETY: single threaded; the child only makes two system calls.
    match unsafe { libc::fork() } {
        -1 => panic!("fork"),
        0 => {
            let rules = vec![
                Rule::Allow(libc::SYS_exit_group),
                Rule::Allow(libc::SYS_write),
                Rule::Errno(libc::SYS_getppid, libc::EPERM),
            ];
            Filter::new(&rules).unwrap().install().unwrap();
            let r = sys(libc::SYS_getppid, [0; 4]);
            let code =
                if r == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) {
                    0
                } else {
                    1
                };
            helper::exit_now(code)
        }
        pid => {
            let mut status = 0;
            // SAFETY: waitpid on our own child.
            unsafe { libc::waitpid(pid, &mut status, 0) };
            assert!(libc::WIFEXITED(status), "status {status}");
            assert_eq!(libc::WEXITSTATUS(status), 0);
        }
    }
}
