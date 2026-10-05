//! Installing, uninstalling and removing unused runtimes through libflatpak,
//! against the local test remote from `scripts/test-remote.sh`. Same guard as
//! `flatpak_ops.rs`: skipped unless `ATLAS_STORE_TEST_REMOTE` names a built
//! test dir, and refused unless every Flatpak and XDG directory (HOME too) is
//! under /work/. The flatpak CLI is test setup only. The flatpakref and
//! add-remote parts, which need `file://` URLs our parser refuses, are unit
//! tests in the module. The system scope cannot be tested here (it needs the
//! system helper and polkit).

mod common;

use std::path::PathBuf;

use atlas_store_core::flatpak::transaction::test_hooks;
use atlas_store_core::flatpak::{
    CancelToken, DataResult, Error, OpKind, OperationLock, Scope, install, list_unused,
    plan_install, uninstall, uninstall_unused,
};
use common::*;
use libflatpak::prelude::InstanceExt;

#[test]
fn plan_then_install_then_list() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();
    let r = app_ref();
    let plan = plan_install(Scope::User, "test", &r, &c).unwrap();
    assert_eq!(plan.scope, Scope::User);
    assert_eq!(plan.ref_, r);
    assert_eq!(plan.remote, "test");
    assert!(plan.remote_url.starts_with("file://") && !plan.remote_url.ends_with('/'));
    assert!(plan.gpg_verified);
    let nr: Vec<&str> = plan.new_runtimes.iter().map(|o| o.ref_.as_str()).collect();
    assert_eq!(nr, vec![runtime_ref()]);
    assert!(plan.new_runtimes.iter().all(|o| o.remote == "test"));
    assert!(
        plan.ops
            .iter()
            .all(|o| o.kind == OpKind::Install && o.remote == "test")
    );
    assert!(plan.ops.iter().any(|o| o.ref_ == r));
    assert!(plan.download_total > 0 && plan.installed_total > 0);
    assert!(plan.metadata.starts_with(b"[Application]"));
    // Planning installed nothing.
    assert!(installed().is_empty());

    let lock = OperationLock::try_acquire().unwrap();
    let mut events = Vec::new();
    let done = install(&plan, &lock, &c, |p| events.push(p)).unwrap();
    assert!(done.refs.contains(&r) && done.refs.contains(&runtime_ref()));
    // The deployed metadata matches the plan's, and the main ref was reported.
    assert!(done.warnings.is_empty(), "{:?}", done.warnings);
    assert!(events.iter().all(|p| !p.not_responding));
    assert!(!events.is_empty());
    assert!(
        events
            .iter()
            .all(|p| p.percent <= 100 && p.op >= 1 && p.status.chars().count() <= 120)
    );
    assert!(events.iter().any(|p| p.percent == 100));
    let refs = installed();
    let app = refs.iter().find(|x| x.id == APP).unwrap();
    assert_eq!(app.origin, "test");
    assert_eq!(app.version, "1.0");
    assert!(ids(&refs).contains(&RUNTIME));
}

#[test]
fn a_changed_remote_between_plan_and_install_is_plan_changed() {
    let Some((dir, _g)) = remote() else { return };
    let _restore = Rebuild(dir.clone());
    reset(&dir);
    let c = CancelToken::new();
    let r = app_ref();
    let old = plan_install(Scope::User, "test", &r, &c).unwrap();
    bump(&dir);
    let lock = OperationLock::try_acquire().unwrap();
    let e = install(&old, &lock, &c, |_| {}).unwrap_err();
    assert!(
        matches!(&e, Error::PlanChanged(c) if !c.missing.is_empty() && !c.unexpected.is_empty()),
        "{e:?}"
    );
    assert!(installed().is_empty(), "nothing was installed");

    let new = plan_install(Scope::User, "test", &r, &c).unwrap();
    let commit = |p: &atlas_store_core::flatpak::InstallPlan| {
        p.ops.iter().find(|o| o.ref_ == r).unwrap().commit.clone()
    };
    assert_ne!(commit(&old), commit(&new));
    install(&new, &lock, &c, |_| {}).unwrap();
    let refs = installed();
    assert_eq!(refs.iter().find(|x| x.id == APP).unwrap().version, "1.1");
}

#[test]
fn cancel_during_install_leaves_no_half_installed_ref() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();
    let plan = plan_install(Scope::User, "test", &app_ref(), &c).unwrap();
    let lock = OperationLock::try_acquire().unwrap();
    let c2 = c.clone();
    // Cancel once an operation has started: it ends as Cancelled (the root of
    // a Partial when the runtime had already finished), or, if everything was
    // done by then, as Ok. Never half-installed.
    let res = install(&plan, &lock, &c, |_| c2.cancel());
    match &res {
        Err(e) => assert_eq!(e.root(), &Error::Cancelled, "{e:?}"),
        Ok(_) => assert!(ids(&installed()).contains(&APP)),
    }
    let refs = installed();
    if res.is_err() {
        assert!(!ids(&refs).contains(&APP), "{:?}", ids(&refs));
        if let Err(Error::Partial { completed, .. }) = &res {
            // What finished is installed, and only that.
            for r in completed {
                assert!(
                    refs.iter().any(|x| &x.full_ref() == r),
                    "{r} is not installed"
                );
            }
        }
    }
    // What is there is whole: its metadata reads, and a new plan works.
    let fresh = CancelToken::new();
    for r in &refs {
        r.metadata(&fresh).unwrap();
    }
    let again = plan_install(Scope::User, "test", &app_ref(), &fresh).unwrap();
    install(&again, &lock, &fresh, |_| {}).unwrap();
    assert!(ids(&installed()).contains(&APP));
    // Already cancelled: nothing runs.
    let gone = CancelToken::new();
    gone.cancel();
    assert_eq!(
        install(&again, &lock, &gone, |_| {}).unwrap_err(),
        Error::Cancelled
    );
}

#[test]
fn a_cancel_after_the_first_operation_is_a_partial_install() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();
    let plan = plan_install(Scope::User, "test", &app_ref(), &c).unwrap();
    assert!(plan.ops.len() >= 2, "the runtime and the app");
    let lock = OperationLock::try_acquire().unwrap();
    let hook = test_hooks::cancel_after_done(1);
    let e = install(&plan, &lock, &c, |_| {}).unwrap_err();
    drop(hook);
    match &e {
        Error::Partial { completed, cause } => {
            assert_eq!(completed.len(), 1, "{e:?}");
            assert_eq!(**cause, Error::Cancelled, "{e:?}");
            // What finished stays; the rest never started.
            let refs = installed();
            assert!(refs.iter().any(|x| x.full_ref() == completed[0]));
            assert!(!ids(&refs).contains(&APP));
            assert!(e.to_string().contains("Already done and kept"));
        }
        other => panic!("{other:?}"),
    }
    // A new plan sees the finished part and installs the rest.
    let fresh = CancelToken::new();
    let again = plan_install(Scope::User, "test", &app_ref(), &fresh).unwrap();
    assert!(again.ops.len() < plan.ops.len());
    install(&again, &lock, &fresh, |_| {}).unwrap();
    assert!(ids(&installed()).contains(&APP));
}

#[test]
fn a_cancel_in_ready_is_cancelled_with_nothing_done() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();
    let plan = plan_install(Scope::User, "test", &app_ref(), &c).unwrap();
    let lock = OperationLock::try_acquire().unwrap();
    let hook = test_hooks::cancel_in_ready();
    let e = install(&plan, &lock, &c, |_| {}).unwrap_err();
    drop(hook);
    assert_eq!(e, Error::Cancelled, "{e:?}");
    assert!(c.is_cancelled() && !c.is_timed_out());
    assert!(installed().is_empty());
}

#[test]
fn a_source_that_sends_more_than_announced_is_stopped() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();
    let plan = plan_install(Scope::User, "test", &app_ref(), &c).unwrap();
    let lock = OperationLock::try_acquire().unwrap();
    // Far more bytes than announced (the local remote reports none itself).
    let hook = test_hooks::extra_bytes(1 << 40);
    let e = install(&plan, &lock, &c, |_| {}).unwrap_err();
    drop(hook);
    assert_eq!(e.root(), &Error::SentTooMuch, "{e:?}");
    assert!(!ids(&installed()).contains(&APP));
    // With the real cap the same plan installs, and nothing is left over.
    let fresh = CancelToken::new();
    let plan = plan_install(Scope::User, "test", &app_ref(), &fresh).unwrap();
    let done = install(&plan, &lock, &fresh, |_| {}).unwrap();
    assert!(done.warnings.is_empty(), "{:?}", done.warnings);
}

#[test]
fn a_panic_in_the_progress_callback_cancels_and_propagates() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();
    let plan = plan_install(Scope::User, "test", &app_ref(), &c).unwrap();
    let lock = OperationLock::try_acquire().unwrap();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = install(&plan, &lock, &c, |_| panic!("boom in the callback"));
    }));
    let payload = r.expect_err("the panic must propagate");
    assert_eq!(
        payload.downcast_ref::<&str>().copied(),
        Some("boom in the callback")
    );
    assert!(c.is_cancelled(), "the transaction was cancelled");
    assert!(!ids(&installed()).contains(&APP));
    // The installation still works.
    let fresh = CancelToken::new();
    let again = plan_install(Scope::User, "test", &app_ref(), &fresh).unwrap();
    install(&again, &lock, &fresh, |_| {}).unwrap();
    assert!(ids(&installed()).contains(&APP));
}

#[test]
fn a_remote_changed_after_the_plan_is_plan_changed() {
    let Some((dir, _g)) = remote() else { return };
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    for change in [
        &["remote-modify", "--no-gpg-verify", "test"][..],
        &["remote-modify", "--disable", "test"][..],
        &["remote-delete", "--force", "test"][..],
    ] {
        reset(&dir);
        let plan = plan_install(Scope::User, "test", &app_ref(), &c).unwrap();
        must(change);
        let e = install(&plan, &lock, &c, |_| {}).unwrap_err();
        match &e {
            Error::PlanChanged(p) => assert!(p.source.is_some(), "{change:?}: {p:?}"),
            other => panic!("{change:?}: {other:?}"),
        }
        assert!(installed().is_empty(), "{change:?}: nothing was installed");
    }
    // A disabled remote is not planned from either.
    reset(&dir);
    must(&["remote-modify", "--disable", "test"]);
    assert!(plan_install(Scope::User, "test", &app_ref(), &c).is_err());
}

fn make_data() -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let outside = home.join("tx-outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret"), b"keep").unwrap();
    let data = home.join(".var/app").join(APP);
    std::fs::create_dir_all(data.join("config")).unwrap();
    std::fs::write(data.join("config/settings"), b"x").unwrap();
    let _ = std::fs::remove_file(data.join("link"));
    std::os::unix::fs::symlink(&outside, data.join("link")).unwrap();
    data
}

#[test]
fn uninstall_keeps_the_data_unless_asked_and_never_follows_links() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let data = make_data();
    let outside = data
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tx-outside");

    let out = uninstall(Scope::User, &app_ref(), false, &lock, &c, |_| {}).unwrap();
    assert_eq!(out.data, DataResult::NotRequested);
    assert!(out.removed.contains(&app_ref()));
    // The app is gone, and its add-on with it; the data stays.
    let refs = installed();
    assert!(
        !ids(&refs).contains(&APP) && !ids(&refs).contains(&ADDON),
        "{:?}",
        ids(&refs)
    );
    assert!(data.join("config/settings").is_file());

    with_app(&dir);
    let out = uninstall(Scope::User, &app_ref(), true, &lock, &c, |_| {}).unwrap();
    assert_eq!(out.data, DataResult::Deleted);
    assert!(!data.exists());
    assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"keep");
    std::fs::remove_dir_all(&outside).unwrap();

    // A ref that is not installed fails and deletes nothing.
    let data = make_data();
    let e = uninstall(Scope::User, &app_ref(), true, &lock, &c, |_| {}).unwrap_err();
    assert!(matches!(e, Error::Flatpak { .. }), "{e:?}");
    assert!(data.exists());
    std::fs::remove_dir_all(
        data.parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tx-outside"),
    )
    .unwrap();
    std::fs::remove_dir_all(&data).unwrap();

    for bad in ["app/..", "app/org.test.Hello/x86_64/..", "nonsense"] {
        let e = uninstall(Scope::User, bad, true, &lock, &c, |_| {}).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{bad}: {e:?}");
    }
}

#[test]
fn uninstall_unused_removes_exactly_the_confirmed_list() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    must(&["uninstall", "-y", "--noninteractive", APP]);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let unused: Vec<String> = list_unused(Scope::User, &c)
        .unwrap()
        .iter()
        .map(|r| r.full_ref())
        .collect();
    assert_eq!(unused, vec![runtime_ref()]);

    // A stale list (empty, wrong, or more than there is) stops, and nothing goes.
    for stale in [
        vec![],
        vec![format!(
            "runtime/org.test.Other/{}/stable",
            libflatpak::default_arch().unwrap()
        )],
        vec![runtime_ref(), app_ref()],
    ] {
        let e = uninstall_unused(Scope::User, &stale, &lock, &c, |_| {}).unwrap_err();
        assert!(matches!(e, Error::PlanChanged(_)), "{stale:?}: {e:?}");
    }
    assert!(ids(&installed()).contains(&RUNTIME));

    uninstall_unused(Scope::User, &unused, &lock, &c, |_| {}).unwrap();
    assert!(installed().is_empty());
    // Nothing left: the empty list is now the right one.
    uninstall_unused(Scope::User, &[], &lock, &c, |_| {}).unwrap();
}

fn beta_ref() -> String {
    format!("app/{APP}/{}/beta", libflatpak::default_arch().unwrap())
}

fn data_root() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap()).join(".var/app")
}

#[test]
fn data_is_kept_while_another_branch_is_installed() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    must(&["install", "-y", "--noninteractive", "test", &beta_ref()]);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let data = make_data();
    let out = uninstall(Scope::User, &app_ref(), true, &lock, &c, |_| {}).unwrap();
    assert!(matches!(out.data, DataResult::Kept(_)), "{:?}", out.data);
    assert!(data.join("config/settings").is_file());
    assert!(
        installed()
            .iter()
            .any(|r| r.id == APP && r.branch == "beta"),
        "the beta branch is still installed"
    );
    // The last branch takes the data with it.
    let out = uninstall(Scope::User, &beta_ref(), true, &lock, &c, |_| {}).unwrap();
    assert_eq!(out.data, DataResult::Deleted);
    assert!(!data.exists());
    let _ = std::fs::remove_dir_all(
        data_root()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tx-outside"),
    );
}

#[test]
fn a_runtime_in_use_is_not_uninstalled() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let e = uninstall(Scope::User, &runtime_ref(), false, &lock, &c, |_| {}).unwrap_err();
    match &e {
        Error::InUse(users) => assert!(users.contains(&app_ref()), "{users:?}"),
        other => panic!("{other:?}"),
    }
    assert!(ids(&installed()).contains(&RUNTIME));
    // Once nothing uses it, it can go.
    uninstall(Scope::User, &app_ref(), false, &lock, &c, |_| {}).unwrap();
    uninstall(Scope::User, &runtime_ref(), false, &lock, &c, |_| {}).unwrap();
    assert!(!ids(&installed()).contains(&RUNTIME));
}

#[test]
fn unused_runtime_goes_with_its_extension() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    must(&["install", "-y", "--noninteractive", "test", RTEXT]);
    must(&["uninstall", "-y", "--noninteractive", APP]);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let unused: Vec<String> = list_unused(Scope::User, &c)
        .unwrap()
        .iter()
        .map(|r| r.full_ref())
        .collect();
    assert!(unused.contains(&runtime_ref()), "{unused:?}");
    let out = uninstall_unused(Scope::User, &unused, &lock, &c, |_| {}).unwrap();
    assert!(out.removed.contains(&runtime_ref()), "{out:?}");
    assert!(installed().is_empty(), "{:?}", ids(&installed()));
}

/// A running instance as libflatpak finds one: `$XDG_RUNTIME_DIR/.flatpak/<id>`
/// with an `info` key file and a `.ref` file that is locked while the app
/// runs. The lock is an open-file-description lock, so libflatpak in this
/// very process sees it as held by someone else.
struct FakeInstance {
    dir: PathBuf,
    _ref: std::fs::File,
}

impl FakeInstance {
    fn start(app: &str) -> FakeInstance {
        use std::os::fd::AsRawFd;
        let run = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap());
        assert!(run.starts_with("/work/"));
        let dir = run.join(".flatpak").join("4242424242");
        // A leftover of a failed run would count as running.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("info"),
            format!("[Application]\nname={app}\n\n[Instance]\ninstance-id=4242424242\n"),
        )
        .unwrap();
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(dir.join(".ref"))
            .unwrap();
        let lock = libc::flock {
            l_type: libc::F_RDLCK as _,
            l_whence: libc::SEEK_SET as _,
            l_start: 0,
            l_len: 0,
            l_pid: 0,
        };
        // SAFETY: `f` is an open file for the whole call and `lock` is a
        // valid, initialized `flock` that fcntl only reads.
        let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_OFD_SETLK, &lock) };
        assert_eq!(r, 0, "locking the fake instance failed");
        FakeInstance { dir, _ref: f }
    }
}

impl Drop for FakeInstance {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_running_app_stops_the_uninstall_and_keeps_its_data() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let data = make_data();
    let outside = data_root()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tx-outside");
    let running = FakeInstance::start(APP);
    assert!(libflatpak::Instance::all().iter().any(|i| i.is_running()));

    let e = uninstall(Scope::User, &app_ref(), true, &lock, &c, |_| {}).unwrap_err();
    assert_eq!(e, Error::AppRunning);
    // Nothing was removed.
    assert!(ids(&installed()).contains(&APP));
    assert!(data.join("config/settings").is_file());

    // Once it has stopped, the same call goes through.
    drop(running);
    assert!(!libflatpak::Instance::all().iter().any(|i| i.is_running()));
    let out = uninstall(Scope::User, &app_ref(), true, &lock, &c, |_| {}).unwrap();
    assert_eq!(out.data, DataResult::Deleted);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn the_check_before_deleting_data_keeps_it_while_the_app_runs() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let data = make_data();
    let outside = data_root()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tx-outside");
    // The app starts while the uninstall runs: the first check (before) saw
    // nothing, the one after the uninstall does.
    let started = std::cell::RefCell::new(None);
    let out = uninstall(Scope::User, &app_ref(), true, &lock, &c, |_| {
        if started.borrow().is_none() {
            *started.borrow_mut() = Some(FakeInstance::start(APP));
        }
    })
    .unwrap();
    assert!(
        started.borrow().is_some(),
        "the uninstall reported no progress"
    );
    assert!(matches!(out.data, DataResult::Kept(_)), "{:?}", out.data);
    assert!(data.join("config/settings").is_file());
    drop(started);
    std::fs::remove_dir_all(&data).unwrap();
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn an_extension_is_not_uninstalled_alone_while_its_parent_is_installed() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    must(&["install", "-y", "--noninteractive", "test", RTEXT]);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let ext = format!(
        "runtime/{RTEXT}/{}/stable",
        libflatpak::default_arch().unwrap()
    );
    let e = uninstall(Scope::User, &ext, false, &lock, &c, |_| {}).unwrap_err();
    assert_eq!(e, Error::InUse(vec![runtime_ref()]));
    assert!(e.to_string().contains("Remove Unused"), "{e}");
    assert!(ids(&installed()).contains(&RTEXT));
}

#[test]
fn a_runtime_used_from_the_other_scope_is_not_unused() {
    let Some((dir, _g)) = remote() else { return };
    let _clean = SystemCleanup;
    reset(&dir);
    let gpg = format!("--gpg-import={}", dir.join("key.gpg").display());
    let url = format!("file://{}/repo/", dir.display());
    flatpak_system(&["uninstall", "-y", "--noninteractive", "--all"]);
    clear_pins_system();
    flatpak_system(&["remote-delete", "--force", "test"]);
    must_system(&["remote-add", &gpg, "test", &url]);
    // The runtime lives in the system installation, the app that needs it in
    // the user installation (without its dependencies). The runtime is
    // installed as a dependency, which does not pin it: a runtime installed
    // on its own is pinned and never unused.
    must_system(&["install", "-y", "--noninteractive", "test", &app_ref()]);
    must_system(&["uninstall", "-y", "--noninteractive", APP]);
    must(&[
        "install",
        "-y",
        "--noninteractive",
        "--no-deps",
        "test",
        &app_ref(),
    ]);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let refs = |scope| -> Vec<String> {
        list_unused(scope, &c)
            .unwrap()
            .iter()
            .map(|r| r.full_ref())
            .collect()
    };
    // The list does not offer the system runtime while the user app needs it
    // (libflatpak sees the other installation itself; the Store's own filter
    // is unit-tested), and uninstall_unused agrees with the list: a list that
    // still has the runtime in it is a stale plan, never a removal.
    assert!(refs(Scope::System).is_empty(), "{:?}", refs(Scope::System));
    let e = uninstall_unused(Scope::System, &[runtime_ref()], &lock, &c, |_| {}).unwrap_err();
    assert!(
        matches!(e, Error::InUse(_) | Error::PlanChanged(_)),
        "{e:?}"
    );
    uninstall_unused(Scope::System, &[], &lock, &c, |_| {}).unwrap();
    assert!(flatpak_system(&["info", &runtime_ref()]));

    // Without the app it is unused, listed, and goes.
    must(&["uninstall", "-y", "--noninteractive", APP]);
    assert_eq!(refs(Scope::System), vec![runtime_ref()]);
    uninstall_unused(Scope::System, &[runtime_ref()], &lock, &c, |_| {}).unwrap();
    assert!(!flatpak_system(&["info", &runtime_ref()]));
}

#[test]
fn a_runtime_remote_that_changes_after_the_plan_is_plan_changed() {
    let Some((dir, _g)) = remote() else { return };
    let _clean = RemoteCleanup("rt");
    reset(&dir);
    let gpg = format!("--gpg-import={}", dir.join("key.gpg").display());
    let url = format!("file://{}/repo/", dir.display());
    flatpak(&["remote-delete", "--force", "rt"]);
    must(&["remote-add", &gpg, "rt", &url]);
    // The runtime comes from "rt" (higher priority), the app from "test".
    must(&["remote-modify", "--prio=10", "rt"]);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let plan = plan_install(Scope::User, "test", &app_ref(), &c).unwrap();
    let rt = plan.ops.iter().find(|o| o.ref_ == runtime_ref()).unwrap();
    assert_eq!(rt.remote, "rt", "{:?}", plan.ops);
    assert!(rt.signed);
    let names: Vec<&str> = plan.remotes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["rt", "test"]);
    assert!(plan.remotes.iter().all(|r| r.gpg_verified && !r.disabled));

    // The runtime remote's address changes: nothing is installed.
    must(&["remote-modify", "--url=file:///work/nowhere/repo/", "rt"]);
    let e = install(&plan, &lock, &c, |_| {}).unwrap_err();
    match &e {
        Error::PlanChanged(p) => assert!(
            p.source.as_deref().is_some_and(|s| s.contains("\"rt\"")),
            "{p:?}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(installed().is_empty());
    // Put back, the same plan installs.
    must(&["remote-modify", &format!("--url={url}"), "rt"]);
    install(&plan, &lock, &c, |_| {}).unwrap();
    assert!(ids(&installed()).contains(&APP));
}

#[test]
fn an_app_whose_metadata_differs_from_the_plan_is_rolled_back() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let mut plan = plan_install(Scope::User, "test", &app_ref(), &c).unwrap();
    // What the user was shown is not what the remote deploys.
    plan.metadata =
        b"[Application]\nname=org.test.Hello\ncommand=other\n[Context]\nshared=network;\n".to_vec();
    let e = install(&plan, &lock, &c, |_| {}).unwrap_err();
    assert_eq!(e, Error::MetadataMismatch { rolled_back: true }, "{e:?}");
    assert!(e.to_string().contains("removed again"), "{e}");
    let refs = installed();
    assert!(!ids(&refs).contains(&APP), "{:?}", ids(&refs));
    // Its runtime stays, whole.
    assert!(ids(&refs).contains(&RUNTIME));
    let fresh = CancelToken::new();
    for r in &refs {
        r.metadata(&fresh).unwrap();
    }
    // The real plan installs it.
    let plan = plan_install(Scope::User, "test", &app_ref(), &c).unwrap();
    let done = install(&plan, &lock, &c, |_| {}).unwrap();
    assert!(done.warnings.is_empty(), "{:?}", done.warnings);
    assert!(ids(&installed()).contains(&APP));
}
