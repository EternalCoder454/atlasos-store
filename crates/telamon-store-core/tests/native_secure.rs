//! Native Telamon apps, Secure phase: what a bundle, a hostile archive or
//! another process of the same user can and cannot make the Store do while it
//! unpacks and installs. Everything is in scratch trees; nothing touches the
//! network, the real home folder or the real Flatpak.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use telamon_store_core::native::archive::{self, hex};
use telamon_store_core::native::fake::{Built, BundleBuilder};
use telamon_store_core::native::install::test_hooks::{FAIL_AFTER, ON_STEP, PANIC_AFTER};
use telamon_store_core::native::install::{self, Dirs, Options, Origin};
use telamon_store_core::native::manifest::{FileEntry, Host, LinkEntry, Manifest};
use telamon_store_core::native::version::Version;

const ID: &str = "net.eterneon.telamon.gates";

fn scratch(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "secure-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn dirs(name: &str) -> (Dirs, PathBuf) {
    let root = scratch(name);
    let data = root.join("data");
    let home = root.join("home");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(&home).unwrap();
    (
        Dirs {
            data,
            home,
            system: Vec::new(),
        },
        root,
    )
}

fn host() -> Host {
    Host {
        os_version: Some(44),
        telamon_ui: Version::parse("2.0.2"),
        arch: "x86_64".into(),
    }
}

fn gates(version: &str) -> BundleBuilder {
    BundleBuilder::new(ID, "Telamon Gates", version).exe("telamon-gates")
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b
}

fn write_archive(root: &Path, built: &Built) -> PathBuf {
    let p = root.join(format!("bundle-{}.tar.zst", &built.sha256[..8]));
    fs::write(&p, &built.archive).unwrap();
    p
}

fn install_local(d: &Dirs, root: &Path, built: &Built) -> Result<install::Done, String> {
    let h = host();
    let f = write_archive(root, built);
    install::install_bundle(
        d,
        &f,
        &Options {
            expect_id: None,
            outer: None,
            origin: Origin::local(),
            host: &h,
        },
    )
    .map_err(|e| e.0)
}

/// A folder outside the Store's, with one file that must stay as it is.
fn outside(root: &Path) -> PathBuf {
    let o = root.join("outside");
    fs::create_dir_all(&o).unwrap();
    fs::write(o.join("sentinel"), b"keep").unwrap();
    o
}

fn untouched(o: &Path) {
    assert_eq!(fs::read(o.join("sentinel")).unwrap(), b"keep");
    let names: Vec<_> = fs::read_dir(o)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["sentinel"], "something was made in {}", o.display());
}

fn files_under(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
            if e.file_type().unwrap().is_dir() {
                walk(base, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn staging_in(d: &Dirs) -> Option<PathBuf> {
    fs::read_dir(d.apps()).ok()?.flatten().find_map(|e| {
        e.file_name()
            .to_string_lossy()
            .starts_with(".staging-")
            .then(|| e.path())
    })
}

// ---- (a) nothing below the apps folder follows a link ----

#[test]
fn a_link_in_place_of_the_apps_folder_is_never_used() {
    let (d, root) = dirs("apps-link");
    let o = outside(&root);
    symlink(&o, d.apps()).unwrap();
    let e = install_local(&d, &root, &gates("0.1.0").build()).unwrap_err();
    assert!(e.contains("link"), "{e}");
    untouched(&o);
    assert!(install::list(&d).is_empty());
    assert!(install::read_record(&d, ID).is_none());
    assert!(install::uninstall(&d, ID).is_err());
    assert!(install::launch(&d, ID, None).is_err());
    untouched(&o);
}

#[test]
fn an_apps_folder_that_others_can_use_is_set_to_private() {
    let (d, root) = dirs("apps-mode");
    fs::create_dir_all(d.apps()).unwrap();
    fs::set_permissions(d.apps(), fs::Permissions::from_mode(0o755)).unwrap();
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().mode() & 0o777;
    assert_eq!(mode(&d.apps()), 0o700);
    assert_eq!(mode(&d.app(ID)), 0o700);
    assert_eq!(mode(&d.app(ID).join("0.1.0")), 0o700);
    assert_eq!(mode(&d.app(ID).join("0.1.0/share")), 0o700);
    assert_eq!(mode(&d.app(ID).join("0.1.0/bin")), 0o700);
    // A new apps folder is made 0700 too.
    let (d2, root2) = dirs("apps-mode-new");
    install_local(&d2, &root2, &gates("0.1.0").build()).unwrap();
    assert_eq!(mode(&d2.apps()), 0o700);
}

#[test]
fn a_link_in_place_of_an_apps_own_folder_is_never_used() {
    let (d, root) = dirs("app-link");
    let o = outside(&root);
    fs::create_dir_all(d.apps()).unwrap();
    symlink(&o, d.app(ID)).unwrap();
    let e = install_local(&d, &root, &gates("0.1.0").build()).unwrap_err();
    assert!(e.contains("already exists"), "{e}");
    untouched(&o);
    assert!(
        fs::symlink_metadata(d.app(ID))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_link_in_place_of_a_version_folder_is_replaced_not_followed() {
    let (d, root) = dirs("version-link");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    let o = outside(&root);
    symlink(&o, d.app(ID).join("0.2.0")).unwrap();
    install_local(&d, &root, &gates("0.2.0").build()).unwrap();
    untouched(&o);
    let md = fs::symlink_metadata(d.app(ID).join("0.2.0")).unwrap();
    assert!(md.is_dir() && !md.file_type().is_symlink());
    assert_eq!(install::read_record(&d, ID).unwrap().version, "0.2.0");
}

#[test]
fn a_link_in_place_of_the_lock_is_never_used() {
    let (d, root) = dirs("lock-link");
    let o = outside(&root);
    fs::create_dir_all(d.apps()).unwrap();
    symlink(o.join("sentinel"), d.apps().join(".lock")).unwrap();
    assert!(install_local(&d, &root, &gates("0.1.0").build()).is_err());
    assert!(install::uninstall(&d, ID).is_err());
    untouched(&o);
}

#[test]
fn links_planted_at_staging_names_are_not_followed_and_are_swept() {
    let (d, root) = dirs("staging-link");
    let o = outside(&root);
    fs::create_dir_all(d.apps()).unwrap();
    for n in 0..400 {
        symlink(
            &o,
            d.apps()
                .join(format!(".staging-{}-{n}", std::process::id())),
        )
        .unwrap();
    }
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    untouched(&o);
    let left: Vec<_> = files_under(&d.apps())
        .into_iter()
        .filter(|p| p.starts_with(".staging-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_link_at_an_export_or_above_it_is_never_written_through() {
    // At the file.
    let (d, root) = dirs("export-file-link");
    let o = outside(&root);
    let icon = d.data.join(format!("icons/hicolor/scalable/apps/{ID}.svg"));
    fs::create_dir_all(icon.parent().unwrap()).unwrap();
    symlink(o.join("sentinel"), &icon).unwrap();
    let e = install_local(&d, &root, &gates("0.1.0").build()).unwrap_err();
    assert!(e.contains("didn't put it there"), "{e}");
    untouched(&o);
    assert!(install::list(&d).is_empty());

    // At a folder below the export root.
    let (d, root) = dirs("export-parent-link");
    let o = outside(&root);
    fs::create_dir_all(d.data.join("icons")).unwrap();
    symlink(&o, d.data.join("icons/hicolor")).unwrap();
    let e = install_local(&d, &root, &gates("0.1.0").build()).unwrap_err();
    assert!(e.contains("link or a file"), "{e}");
    untouched(&o);
    assert!(install::list(&d).is_empty());
    assert!(!d.data.join(format!("applications/{ID}.desktop")).exists());

    // At the folder the desktop entry goes in.
    let (d, root) = dirs("export-applications-file");
    fs::write(d.data.join("applications"), b"a file").unwrap();
    assert!(install_local(&d, &root, &gates("0.1.0").build()).is_err());
    assert_eq!(fs::read(d.data.join("applications")).unwrap(), b"a file");
}

#[test]
fn an_export_root_may_be_a_link_as_a_dotfile_manager_makes_them() {
    let (d, root) = dirs("export-root-link");
    let real = root.join("dotfiles/applications");
    fs::create_dir_all(&real).unwrap();
    symlink(&real, d.data.join("applications")).unwrap();
    let icons = root.join("dotfiles/icons");
    fs::create_dir_all(&icons).unwrap();
    symlink(&icons, d.data.join("icons")).unwrap();
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    assert!(real.join(format!("{ID}.desktop")).is_file());
    assert!(
        icons
            .join(format!("hicolor/scalable/apps/{ID}.svg"))
            .is_file()
    );
    assert!(install::list(&d)[0].icon.is_some());
    install::uninstall(&d, ID).unwrap();
    assert!(!real.join(format!("{ID}.desktop")).exists());
    // Their own folders are left.
    assert!(real.is_dir() && icons.is_dir());
}

#[test]
fn a_record_whose_folder_became_a_link_does_not_make_uninstall_touch_the_target() {
    let (d, root) = dirs("record-swap");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    // The app's folder (record, versions and all) is moved, and a link to it
    // stands in its place.
    let moved = root.join("moved");
    fs::create_dir_all(&moved).unwrap();
    fs::rename(d.app(ID), moved.join(ID)).unwrap();
    symlink(moved.join(ID), d.app(ID)).unwrap();
    let before = files_under(&moved);
    assert!(install::read_record(&d, ID).is_none());
    assert!(install::list(&d).is_empty());
    assert!(install::uninstall(&d, ID).is_err());
    assert!(install::launch(&d, ID, None).is_err());
    assert_eq!(files_under(&moved), before);
    assert!(moved.join(ID).join("install.json").is_file());
    // The files it copied out are untouched as well.
    assert!(d.data.join(format!("applications/{ID}.desktop")).is_file());
}

#[test]
fn uninstall_does_not_follow_a_link_that_replaced_an_export_folder() {
    let (d, root) = dirs("uninstall-export-link");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    // `icons/hicolor` becomes a link to a folder that holds a file with the
    // very name and content the Store wrote.
    let o = root.join("lookalike");
    fs::create_dir_all(o.join("scalable/apps")).unwrap();
    let name = format!("scalable/apps/{ID}.svg");
    let real = d.data.join("icons/hicolor").join(&name);
    fs::copy(&real, o.join(&name)).unwrap();
    fs::rename(d.data.join("icons/hicolor"), root.join("hicolor-moved")).unwrap();
    symlink(&o, d.data.join("icons/hicolor")).unwrap();
    let r = install::uninstall(&d, ID).unwrap();
    assert_eq!(r.left.len(), 1, "{:?}", r.left);
    assert!(
        o.join(&name).is_file(),
        "the file behind the link was removed"
    );
    assert!(!d.app(ID).exists());
}

#[test]
fn an_entry_cannot_be_written_through_a_link_planted_in_the_staging_folder() {
    use telamon_store_core::native::archive::test_hooks::ON_ENTRY;
    let built = gates("0.1.0").build();
    let data_path = format!("share/{ID}/data.txt");

    // A folder swapped for a link after the unpack has started.
    let (d, root) = dirs("race-folder");
    let o = outside(&root);
    let staging_of = |d: &Dirs| staging_in(d).expect("the staging folder");
    {
        let d2 = d.clone();
        let o2 = o.clone();
        let target = data_path.clone();
        ON_ENTRY.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |path: &str| {
                if path == target {
                    let st = staging_of(&d2);
                    fs::rename(st.join("share"), st.join("share.real")).unwrap();
                    symlink(&o2, st.join("share")).unwrap();
                }
            }))
        });
    }
    let r = install_local(&d, &root, &built);
    ON_ENTRY.with(|h| *h.borrow_mut() = None);
    let e = r.expect_err("installed through a swapped folder");
    assert!(e.contains("needs a folder"), "{e}");
    untouched(&o);
    assert!(install::list(&d).is_empty());

    // A file swapped for a link before its mode is set.
    let (d, root) = dirs("race-file");
    let o = outside(&root);
    fs::set_permissions(o.join("sentinel"), fs::Permissions::from_mode(0o600)).unwrap();
    {
        let d2 = d.clone();
        let o2 = o.clone();
        let last = data_path.clone();
        ON_ENTRY.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |path: &str| {
                if path == last {
                    let st = staging_in(&d2).unwrap();
                    fs::remove_file(st.join("bin/telamon-gates")).unwrap();
                    symlink(o2.join("sentinel"), st.join("bin/telamon-gates")).unwrap();
                }
            }))
        });
    }
    let r = install_local(&d, &root, &built);
    ON_ENTRY.with(|h| *h.borrow_mut() = None);
    let e = r.expect_err("installed with a swapped file");
    assert!(e.contains("mode"), "{e}");
    untouched(&o);
    assert_eq!(
        fs::metadata(o.join("sentinel")).unwrap().mode() & 0o777,
        0o600
    );

    // A link planted where the archive needs a folder, from the first entry on.
    let (d, root) = dirs("race-plant");
    let o = outside(&root);
    {
        let d2 = d.clone();
        let o2 = o.clone();
        ON_ENTRY.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |_path: &str| {
                let st = staging_in(&d2).unwrap();
                if !st.join("share").exists() && fs::symlink_metadata(st.join("share")).is_err() {
                    symlink(&o2, st.join("share")).unwrap();
                }
            }))
        });
    }
    let r = install_local(&d, &root, &built);
    ON_ENTRY.with(|h| *h.borrow_mut() = None);
    let e = r.expect_err("installed through a planted link");
    assert!(e.contains("needs a folder"), "{e}");
    untouched(&o);
}

#[test]
fn a_file_changed_between_the_check_and_the_write_is_not_replaced() {
    // The user edits the desktop entry after the Store looked at it.
    let (d, root) = dirs("edit-race");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    let desktop = d.data.join(format!("applications/{ID}.desktop"));
    let before_files = files_under(&d.data);
    {
        let desktop = desktop.clone();
        ON_STEP.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |step: &str| {
                if step == "planned" {
                    fs::write(
                        &desktop,
                        b"[Desktop Entry]\nType=Application\nName=Mine\nExec=true\n",
                    )
                    .unwrap();
                }
            }))
        });
    }
    let r = install_local(&d, &root, &gates("0.2.0").build());
    ON_STEP.with(|h| *h.borrow_mut() = None);
    let e = r.unwrap_err();
    assert!(e.contains("changed while"), "{e}");
    assert_eq!(
        fs::read(&desktop).unwrap(),
        b"[Desktop Entry]\nType=Application\nName=Mine\nExec=true\n"
    );
    assert_eq!(install::read_record(&d, ID).unwrap().version, "0.1.0");
    assert_eq!(files_under(&d.data), before_files);

    // The file is swapped for a link to another file.
    let (d, root) = dirs("link-race");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    let o = outside(&root);
    let desktop = d.data.join(format!("applications/{ID}.desktop"));
    {
        let (desktop, o) = (desktop.clone(), o.clone());
        ON_STEP.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |step: &str| {
                if step == "planned" {
                    fs::remove_file(&desktop).unwrap();
                    symlink(o.join("sentinel"), &desktop).unwrap();
                }
            }))
        });
    }
    let r = install_local(&d, &root, &gates("0.2.0").build());
    ON_STEP.with(|h| *h.borrow_mut() = None);
    assert!(r.is_err());
    untouched(&o);
    assert!(
        fs::symlink_metadata(&desktop)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(install::read_record(&d, ID).unwrap().version, "0.1.0");

    // A first install, and someone makes the file just before it is written.
    let (d, root) = dirs("create-race");
    let desktop = d.data.join(format!("applications/{ID}.desktop"));
    {
        let desktop = desktop.clone();
        ON_STEP.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |step: &str| {
                if step == "planned" {
                    fs::create_dir_all(desktop.parent().unwrap()).unwrap();
                    fs::write(&desktop, b"theirs").unwrap();
                }
            }))
        });
    }
    let r = install_local(&d, &root, &gates("0.1.0").build());
    ON_STEP.with(|h| *h.borrow_mut() = None);
    assert!(r.unwrap_err().contains("changed while"));
    assert_eq!(fs::read(&desktop).unwrap(), b"theirs");
    assert!(install::list(&d).is_empty());
    assert!(!d.app(ID).exists());
}

#[test]
fn a_panic_in_the_worker_leaves_no_half_installed_app() {
    let (d, root) = dirs("panic");
    install_local(
        &d,
        &root,
        &gates("0.1.0")
            .file(
                &format!("share/icons/hicolor/48x48/apps/{ID}.png"),
                &png(48, 48),
                false,
            )
            .build(),
    )
    .unwrap();
    let desktop = d.data.join(format!("applications/{ID}.desktop"));
    let before_desktop = fs::read(&desktop).unwrap();
    let before_files = files_under(&d.data);
    let before_record = fs::read(d.app(ID).join("install.json")).unwrap();
    for step in ["exports", "switch", "record"] {
        PANIC_AFTER.with(|f| f.set(Some(step)));
        let new = gates("0.2.0")
            .file(
                &format!("share/icons/hicolor/48x48/apps/{ID}.png"),
                &png(49, 49),
                false,
            )
            .file(
                &format!("share/icons/hicolor/64x64/apps/{ID}.png"),
                &png(64, 64),
                false,
            )
            .build();
        let r = catch_unwind(AssertUnwindSafe(|| install_local(&d, &root, &new)));
        PANIC_AFTER.with(|f| f.set(None));
        assert!(r.is_err(), "{step}: no panic");
        assert_eq!(
            fs::read_link(d.app(ID).join("current")).unwrap(),
            Path::new("0.1.0"),
            "{step}"
        );
        assert!(!d.app(ID).join("0.2.0").exists(), "{step}");
        assert_eq!(fs::read(&desktop).unwrap(), before_desktop, "{step}");
        assert_eq!(
            fs::read(d.app(ID).join("install.json")).unwrap(),
            before_record,
            "{step}"
        );
        let mut now = files_under(&d.data);
        now.retain(|p| !p.starts_with("telamon-apps/.staging-"));
        assert_eq!(now, before_files, "{step}");
        assert!(staging_in(&d).is_none(), "{step}: staging left");
    }
    // The lock was let go and the same update goes through.
    install_local(&d, &root, &gates("0.2.0").build()).unwrap();

    // A panic in a first install leaves nothing either.
    let (d, root) = dirs("panic-first");
    PANIC_AFTER.with(|f| f.set(Some("switch")));
    let r = catch_unwind(AssertUnwindSafe(|| {
        install_local(&d, &root, &gates("0.1.0").build())
    }));
    PANIC_AFTER.with(|f| f.set(None));
    assert!(r.is_err());
    assert!(!d.app(ID).exists());
    assert!(!d.data.join(format!("applications/{ID}.desktop")).exists());
    assert!(install::list(&d).is_empty());
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
}

#[test]
fn a_failure_after_each_step_still_rolls_back() {
    // The rollback of the earlier rounds, now through the open folders: an
    // update that fails at each step, with an icon replaced and one added.
    let (d, root) = dirs("rollback-fd");
    install_local(
        &d,
        &root,
        &gates("0.1.0")
            .file(
                &format!("share/icons/hicolor/48x48/apps/{ID}.png"),
                &png(48, 48),
                false,
            )
            .build(),
    )
    .unwrap();
    let before = files_under(&d.data);
    for step in ["exports", "switch", "record"] {
        FAIL_AFTER.with(|f| f.set(Some(step)));
        let new = gates("0.2.0")
            .file(
                &format!("share/icons/hicolor/48x48/apps/{ID}.png"),
                &png(49, 49),
                false,
            )
            .file(
                &format!("share/icons/hicolor/128x128/apps/{ID}.png"),
                &png(128, 128),
                false,
            )
            .build();
        let e = install_local(&d, &root, &new).unwrap_err();
        FAIL_AFTER.with(|f| f.set(None));
        assert!(e.contains(&format!("after {step}")), "{e}");
        assert_eq!(files_under(&d.data), before, "{step}");
        assert_eq!(
            fs::read(d.data.join(format!("icons/hicolor/48x48/apps/{ID}.png"))).unwrap(),
            png(48, 48)
        );
    }
}

// ---- (b) a bundle cannot stand in for something the system provides ----

fn system_dir(root: &Path) -> PathBuf {
    let sys = root.join("usr-share");
    fs::create_dir_all(&sys).unwrap();
    sys
}

fn put(sys: &Path, rel: &str, bytes: &[u8]) {
    let p = sys.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

const RC: &[u8] = b"[Global]\nIconName=x\n\n[Event/m]\nName=Message\nAction=Popup\n";

fn service(name: &str) -> Vec<u8> {
    format!("[D-BUS Service]\nName={name}\nExec=telamon-gates --service\n").into_bytes()
}

#[test]
fn nothing_is_copied_over_a_system_file_with_the_same_path() {
    let with_service = |b: BundleBuilder| {
        b.file(
            &format!("share/dbus-1/services/{ID}.Daemon.service"),
            &service(&format!("{ID}.Daemon")),
            false,
        )
    };
    let cases: Vec<(&str, &str, BundleBuilder)> = vec![
        (
            "an icon",
            "icons/hicolor/scalable/apps/net.eterneon.telamon.gates.svg",
            gates("1.0.0"),
        ),
        (
            "a metainfo file",
            "metainfo/net.eterneon.telamon.gates.metainfo.xml",
            gates("1.0.0"),
        ),
        (
            "a service",
            "dbus-1/services/net.eterneon.telamon.gates.Daemon.service",
            with_service(gates("1.0.0")),
        ),
        (
            "a notification file",
            "knotifications6/telamon-gates.notifyrc",
            gates("1.0.0").file("share/knotifications6/telamon-gates.notifyrc", RC, false),
        ),
    ];
    for (what, rel, builder) in cases {
        let (mut d, root) = dirs("shadow");
        let sys = system_dir(&root);
        d.system = vec![sys.clone()];
        // Without the system's file it installs.
        let built = builder.build();
        install_local(&d, &root, &built).unwrap_or_else(|e| panic!("{what}: {e}"));
        install::uninstall(&d, ID).unwrap();
        // With it, it does not.
        put(&sys, rel, b"x");
        let e = install_local(&d, &root, &built).unwrap_err();
        assert!(e.contains("already provided by the system"), "{what}: {e}");
        assert!(install::list(&d).is_empty(), "{what}");
        assert!(!d.app(ID).exists(), "{what}");
    }
}

#[test]
fn the_notification_file_of_a_system_app_cannot_be_shadowed() {
    // `telamon-<last part>.notifyrc` of an ID that ends in `settings`.
    let (mut d, root) = dirs("shadow-notifyrc");
    let sys = system_dir(&root);
    put(&sys, "knotifications6/telamon-settings.notifyrc", RC);
    d.system = vec![sys];
    let id = "x.y.settings";
    let b = BundleBuilder::new(id, "Settings", "1.0.0")
        .file("share/knotifications6/telamon-settings.notifyrc", RC, false)
        .build();
    let e = install_local(&d, &root, &b).unwrap_err();
    assert!(e.contains("already provided by the system"), "{e}");
    assert!(
        !d.data
            .join("knotifications6/telamon-settings.notifyrc")
            .exists()
    );
}

#[test]
fn a_dbus_name_a_system_service_claims_is_not_taken() {
    let b = |name: &str| {
        gates("1.0.0").file(
            &format!("share/dbus-1/services/{name}.service"),
            &service(name),
            false,
        )
    };
    // The system's file is named differently from the name inside it.
    for (what, claimed, bundle) in [
        ("the app ID itself", ID.to_string(), gates("1.0.0")),
        (
            "a name the bundle declares",
            format!("{ID}.Daemon"),
            b(&format!("{ID}.Daemon")),
        ),
        ("the ID, by a bundle with a service", ID.to_string(), b(ID)),
    ] {
        let (mut d, root) = dirs("dbus-claim");
        let sys = system_dir(&root);
        put(
            &sys,
            "dbus-1/services/something.else.service",
            &service(&claimed),
        );
        d.system = vec![sys];
        let e = install_local(&d, &root, &bundle.build()).unwrap_err();
        assert!(e.contains("already used by a service"), "{what}: {e}");
        assert!(install::list(&d).is_empty(), "{what}");
    }
    // Other names, unreadable files and files that are not services claim nothing.
    let (mut d, root) = dirs("dbus-claim-ok");
    let sys = system_dir(&root);
    put(
        &sys,
        "dbus-1/services/org.example.Other.service",
        &service("org.example.Other"),
    );
    put(
        &sys,
        "dbus-1/services/garbage.service",
        b"\xff\x00 not a key file",
    );
    put(&sys, "dbus-1/services/readme.txt", &service(ID));
    put(&sys, "dbus-1/services/big.service", &vec![b'#'; 70 * 1024]);
    d.system = vec![sys];
    install_local(&d, &root, &gates("1.0.0").build()).unwrap();
}

#[test]
fn too_many_system_services_to_check_is_a_refusal() {
    let (mut d, root) = dirs("dbus-many");
    let sys = system_dir(&root);
    fs::create_dir_all(sys.join("dbus-1/services")).unwrap();
    for n in 0..2001 {
        fs::write(
            sys.join(format!("dbus-1/services/org.example.s{n}.service")),
            b"",
        )
        .unwrap();
    }
    d.system = vec![sys];
    let e = install_local(&d, &root, &gates("1.0.0").build()).unwrap_err();
    assert!(e.contains("too many D-Bus services"), "{e}");
}

#[test]
fn ids_in_the_desktops_name_spaces_are_refused() {
    for id in [
        "org.freedesktop.portal",
        "org.kde.dolphin",
        "org.gnome.Shell",
        "org.gtk.Settings",
        "org.mate.x",
        "org.xfce.x",
        "org.flatpak.Helper",
        "org.fedoraproject.x",
        "org.mozilla.firefox",
        "com.canonical.x",
    ] {
        let (d, root) = dirs("reserved");
        let b = BundleBuilder::new(id, "X", "1.0.0").build();
        let e = install_local(&d, &root, &b).unwrap_err();
        assert!(e.contains("name space"), "{id}: {e}");
        assert!(install::list(&d).is_empty());
    }
    // A service of the portal's name, by a bundle whose ID is not under it.
    let (d, root) = dirs("reserved-service");
    let b = gates("1.0.0").file(
        "share/dbus-1/services/org.freedesktop.portal.Desktop.service",
        &service("org.freedesktop.portal.Desktop"),
        false,
    );
    assert!(install_local(&d, &root, &b.build()).is_err());
    assert!(
        !d.data
            .join("dbus-1/services/org.freedesktop.portal.Desktop.service")
            .exists()
    );
}

#[test]
fn a_notification_file_that_runs_a_command_is_refused() {
    for (what, rc) in [
        (
            "Execute",
            &b"[Event/m]\nAction=Execute\nExecute=/usr/bin/konsole\n"[..],
        ),
        ("Action list", b"[Event/m]\nAction=Popup|Execute\n"),
        (
            "Logfile",
            b"[Event/m]\nAction=Logfile\nLogfile=/home/u/.bashrc\n",
        ),
        ("not a key file", b"x"),
    ] {
        let (d, root) = dirs("notifyrc-exec");
        let b = gates("1.0.0").file("share/knotifications6/telamon-gates.notifyrc", rc, false);
        assert!(install_local(&d, &root, &b.build()).is_err(), "{what}");
        assert!(
            !d.data
                .join("knotifications6/telamon-gates.notifyrc")
                .exists(),
            "{what}"
        );
    }
}

#[test]
fn a_metainfo_file_for_another_component_is_refused() {
    let m = |body: &str| {
        format!(
            "<?xml version=\"1.0\"?>\n<component type=\"desktop-application\">{body}</component>\n"
        )
        .into_bytes()
    };
    for (what, xml) in [
        ("another id", m("<id>org.mozilla.firefox</id>")),
        (
            "replaces",
            m(&format!(
                "<id>{ID}</id><replaces><id>org.kde.dolphin</id></replaces>"
            )),
        ),
        (
            "provides",
            m(&format!(
                "<id>{ID}</id><provides><id>org.kde.dolphin</id></provides>"
            )),
        ),
        (
            "a doctype",
            format!("<!DOCTYPE c SYSTEM \"http://evil/x\"><component><id>{ID}</id></component>")
                .into_bytes(),
        ),
    ] {
        let (d, root) = dirs("metainfo");
        let b = gates("1.0.0").file(&format!("share/metainfo/{ID}.metainfo.xml"), &xml, false);
        assert!(install_local(&d, &root, &b.build()).is_err(), "{what}");
        assert!(
            !d.data.join(format!("metainfo/{ID}.metainfo.xml")).exists(),
            "{what}"
        );
    }
}

// ---- (e) icons ----

#[test]
fn an_icon_must_be_what_qt_can_safely_decode() {
    let name = |ext: &str| format!("share/icons/hicolor/48x48/apps/{ID}.{ext}");
    for (what, path, bytes) in [
        ("a huge PNG", name("png"), png(60_000, 60_000)),
        ("not a PNG", name("png"), b"not an image".to_vec()),
        (
            "an SVG that loads a file",
            name("svg"),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><image href=\"file:///etc/passwd\"/></svg>"
                .to_vec(),
        ),
    ] {
        let (d, root) = dirs("icon-bad");
        let b = gates("1.0.0").file(&path, &bytes, false);
        let e = install_local(&d, &root, &b.build()).unwrap_err();
        assert!(e.contains("icon"), "{what}: {e}");
        assert!(install::list(&d).is_empty());
    }
}

#[test]
fn the_icon_handed_to_the_window_is_checked_again_when_listed() {
    let (d, root) = dirs("icon-list");
    install_local(&d, &root, &gates("1.0.0").build()).unwrap();
    let icon = d.data.join(format!("icons/hicolor/scalable/apps/{ID}.svg"));
    assert_eq!(install::list(&d)[0].icon.as_deref(), Some(icon.as_path()));
    let good = fs::read(&icon).unwrap();
    let o = outside(&root);
    // A link, a file that is not an icon, an oversize one, a folder.
    fs::remove_file(&icon).unwrap();
    symlink(o.join("sentinel"), &icon).unwrap();
    assert!(install::list(&d)[0].icon.is_none(), "a link");
    fs::remove_file(&icon).unwrap();
    fs::write(&icon, b"MZ not an image").unwrap();
    assert!(install::list(&d)[0].icon.is_none(), "junk");
    fs::write(&icon, vec![b' '; 2 * 1024 * 1024]).unwrap();
    assert!(install::list(&d)[0].icon.is_none(), "oversize");
    fs::remove_file(&icon).unwrap();
    fs::create_dir(&icon).unwrap();
    assert!(install::list(&d)[0].icon.is_none(), "a folder");
    fs::remove_dir(&icon).unwrap();
    fs::write(&icon, &good).unwrap();
    assert!(install::list(&d)[0].icon.is_some());
    // A better icon that went bad: the next best is used.
    let big = d.data.join(format!("icons/hicolor/256x256/apps/{ID}.png"));
    fs::create_dir_all(big.parent().unwrap()).unwrap();
    fs::write(&big, png(60_000, 60_000)).unwrap();
}

// ---- (f) launching ----

fn script_app(root: &Path, d: &Dirs, body: &str) {
    let script = format!("#!/bin/sh\n{body}\n");
    install_local(
        d,
        root,
        &gates("0.1.0")
            .file("bin/telamon-gates", script.as_bytes(), true)
            .build(),
    )
    .unwrap();
}

#[test]
fn an_activation_token_is_passed_only_when_it_is_valid() {
    let (d, root) = dirs("token");
    let out = root.join("env");
    script_app(
        &root,
        &d,
        &format!(
            "echo \"${{XDG_ACTIVATION_TOKEN-unset}}|${{DESKTOP_STARTUP_ID-unset}}\" > {}",
            out.display()
        ),
    );
    install::launch(&d, ID, Some("tok-1/x_TIME5")).unwrap();
    assert_eq!(
        fs::read_to_string(&out).unwrap().trim(),
        "tok-1/x_TIME5|tok-1/x_TIME5"
    );
    for bad in ["a b", "", "x\ny", "é", &"t".repeat(5000)] {
        install::launch(&d, ID, Some(bad)).unwrap();
        assert_eq!(
            fs::read_to_string(&out).unwrap().trim(),
            "unset|unset",
            "{bad:?}"
        );
    }
    install::launch(&d, ID, None).unwrap();
    assert_eq!(fs::read_to_string(&out).unwrap().trim(), "unset|unset");
}

#[test]
fn the_program_gets_no_descriptor_of_the_stores() {
    use std::os::fd::AsRawFd;
    let (d, root) = dirs("fds");
    let out = root.join("fds");
    // A descriptor the Store (or a library in it) forgot to mark close-on-exec.
    let f = fs::File::open("/dev/null").unwrap();
    // SAFETY: duplicating an open descriptor to a high number, without the flag.
    let fd = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_DUPFD, 201) };
    assert!(fd >= 201);
    script_app(
        &root,
        &d,
        &format!(
            "if [ -e /proc/$$/fd/{fd} ]; then echo leaked; else echo clean; fi > {}",
            out.display()
        ),
    );
    let r = install::launch(&d, ID, None);
    // SAFETY: closing the descriptor made above.
    unsafe { libc::close(fd) };
    r.unwrap();
    assert_eq!(fs::read_to_string(&out).unwrap().trim(), "clean");
}

#[test]
fn open_does_not_run_through_a_current_link_that_leaves_the_app() {
    let (d, root) = dirs("launch-link");
    let marker = root.join("ran");
    script_app(&root, &d, "exit 0");
    // `current` now names a folder of someone else's.
    let evil = root.join("evil/bin");
    fs::create_dir_all(&evil).unwrap();
    let p = evil.join("telamon-gates");
    fs::write(&p, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(d.app(ID).join("current")).unwrap();
    symlink(root.join("evil"), d.app(ID).join("current")).unwrap();
    assert!(install::launch(&d, ID, None).is_err());
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(!marker.exists());
}

// ---- (d) hostile archives ----

struct Tree {
    files: Vec<(String, Vec<u8>, bool)>,
    links: Vec<(String, String)>,
}

impl Tree {
    fn minimal() -> Tree {
        Tree {
            files: vec![("bin/app".into(), b"#!/bin/sh\nexit 0\n".to_vec(), true)],
            links: Vec::new(),
        }
    }

    fn manifest(&self) -> Vec<u8> {
        let m = Manifest {
            schema: 1,
            id: ID.into(),
            name: "Telamon Gates".into(),
            version: "1.0.0".into(),
            summary: "x".into(),
            homepage: String::new(),
            license: "MIT".into(),
            arch: "x86_64".into(),
            min_telamon_ui: "2.0.0".into(),
            min_os_version: "44".into(),
            files: self
                .files
                .iter()
                .map(|(p, b, x)| FileEntry {
                    path: p.clone(),
                    size: b.len() as u64,
                    sha256: hex(&Sha256::digest(b)),
                    executable: *x,
                })
                .collect(),
            links: self
                .links
                .iter()
                .map(|(p, t)| LinkEntry {
                    path: p.clone(),
                    target: t.clone(),
                })
                .collect(),
            archive: None,
        };
        serde_json::to_vec(&m).unwrap()
    }

    /// The tar of exactly this tree, with the end blocks left off.
    fn tar(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (p, b, x) in &self.files {
            out.extend(entry(
                p.as_bytes(),
                b'0',
                b,
                b"",
                if *x { 0o755 } else { 0o644 },
            ));
        }
        for (p, t) in &self.links {
            out.extend(entry(p.as_bytes(), b'2', b"", t.as_bytes(), 0o777));
        }
        out.extend(entry(
            b"telamon-bundle.json",
            b'0',
            &self.manifest(),
            b"",
            0o644,
        ));
        out
    }
}

fn pad(out: &mut Vec<u8>, n: usize) {
    out.extend(std::iter::repeat_n(0u8, (512 - n % 512) % 512));
}

/// One tar entry as given: a GNU header with `name` (at most 100 bytes) and the
/// data after it.
fn entry(name: &[u8], kind: u8, data: &[u8], link: &[u8], mode: u32) -> Vec<u8> {
    raw_entry(name, kind, data.len() as u64, data, link, mode)
}

fn raw_entry(name: &[u8], kind: u8, size: u64, data: &[u8], link: &[u8], mode: u32) -> Vec<u8> {
    let mut h = tar::Header::new_gnu();
    {
        let g = h.as_gnu_mut().unwrap();
        g.name[..name.len().min(100)].copy_from_slice(&name[..name.len().min(100)]);
        g.linkname[..link.len().min(100)].copy_from_slice(&link[..link.len().min(100)]);
    }
    h.set_size(size);
    h.set_mode(mode);
    h.set_mtime(0);
    h.as_mut_bytes()[156] = kind;
    h.set_cksum();
    let mut out = h.as_bytes().to_vec();
    out.extend_from_slice(data);
    pad(&mut out, data.len());
    out
}

fn pax(records: &[(&str, &str)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (k, v) in records {
        let rest = format!(" {k}={v}\n");
        let mut len = rest.len() + 1;
        while len.to_string().len() + rest.len() != len {
            len = len.to_string().len() + rest.len();
        }
        body.extend(format!("{len}{rest}").into_bytes());
    }
    entry(b"PaxHeader/x", b'x', &body, b"", 0o644)
}

fn long_name(name: &str) -> Vec<u8> {
    let mut data = name.as_bytes().to_vec();
    data.push(0);
    entry(b"././@LongLink", b'L', &data, b"", 0o644)
}

fn end() -> Vec<u8> {
    vec![0u8; 1024]
}

fn zst(tar: &[u8]) -> Vec<u8> {
    zstd::stream::encode_all(tar, 3).unwrap()
}

/// Unpacks `tar` (as `.tar.zst`) into a fresh folder; the folder's parent
/// holds nothing else, to see that nothing is written beside it.
fn unpack_raw(name: &str, compressed: &[u8]) -> (Result<Manifest, String>, PathBuf, PathBuf) {
    let root = scratch(name);
    let f = root.join("b.tar.zst");
    fs::write(&f, compressed).unwrap();
    let dest = root.join("dest");
    fs::create_dir(&dest).unwrap();
    let r = archive::unpack(&f, &dest, None).map_err(|e| e.0);
    (r, root, dest)
}

fn unpack_tar(name: &str, tar: &[u8]) -> Result<Manifest, String> {
    let (r, root, _) = unpack_raw(name, &zst(tar));
    let names: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        names.iter().all(|n| n == "b.tar.zst" || n == "dest"),
        "something was written beside the folder: {names:?}"
    );
    r
}

fn with_end(mut tar: Vec<u8>) -> Vec<u8> {
    tar.extend(end());
    tar
}

#[test]
fn the_plain_archive_the_tests_start_from_unpacks() {
    let t = Tree::minimal();
    let (r, _root, dest) = unpack_raw("plain", &zst(&with_end(t.tar())));
    r.unwrap();
    assert_eq!(
        fs::metadata(dest.join("bin/app")).unwrap().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(dest.join("bin")).unwrap().mode() & 0o777,
        0o700
    );
    // `./` prefixes and folder entries are fine.
    let mut tar = entry(b"./", b'5', b"", b"", 0o755);
    tar.extend(entry(b"./bin/", b'5', b"", b"", 0o755));
    tar.extend(entry(b"./bin/app", b'0', &t.files[0].1, b"", 0o755));
    tar.extend(entry(
        b"./telamon-bundle.json",
        b'0',
        &t.manifest(),
        b"",
        0o644,
    ));
    unpack_tar("prefix", &with_end(tar)).unwrap();
    // Block padding after the end is fine.
    let mut padded = with_end(t.tar());
    padded.extend(vec![0u8; 10240 - (padded.len() % 10240)]);
    unpack_tar("padded", &padded).unwrap();
}

#[test]
fn names_that_leave_or_repeat_are_refused() {
    let t = Tree::minimal();
    let body = t.files[0].1.clone();
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    // `././x` is a `.` part after the one `./` that is dropped.
    let mut tar = entry(b"././bin/app", b'0', &body, b"", 0o755);
    tar.extend(entry(
        b"telamon-bundle.json",
        b'0',
        &t.manifest(),
        b"",
        0o644,
    ));
    cases.push(("././", tar));
    // The same file as `bin/app` and `./bin/app`.
    let mut tar = t.tar();
    tar.extend(entry(b"./bin/app", b'0', &body, b"", 0o755));
    cases.push(("same name twice", tar));
    // A name that is not text.
    let mut tar = t.tar();
    tar.extend(entry(b"bin/\xff\xfe", b'0', b"x", b"", 0o644));
    cases.push(("not UTF-8", tar));
    // The manifest twice, as a folder, as a link.
    let mut tar = t.tar();
    tar.extend(entry(
        b"telamon-bundle.json",
        b'0',
        &t.manifest(),
        b"",
        0o644,
    ));
    cases.push(("manifest twice", tar));
    let mut tar = entry(b"telamon-bundle.json/", b'5', b"", b"", 0o755);
    tar.extend(entry(b"bin/app", b'0', &body, b"", 0o755));
    cases.push(("manifest as a folder", tar));
    let mut tar = entry(b"telamon-bundle.json", b'2', b"", b"bin/app", 0o777);
    tar.extend(entry(b"bin/app", b'0', &body, b"", 0o755));
    cases.push(("manifest as a link", tar));
    for (what, tar) in cases {
        assert!(unpack_tar("names", &with_end(tar)).is_err(), "{what}");
    }
}

#[test]
fn folders_and_files_with_the_same_name_are_refused() {
    let t = Tree::minimal();
    let body = t.files[0].1.clone();
    let m = entry(b"telamon-bundle.json", b'0', &t.manifest(), b"", 0o644);
    let cases: Vec<(&str, Vec<Vec<u8>>)> = vec![
        (
            "a folder over a file",
            vec![
                entry(b"bin/app", b'0', &body, b"", 0o755),
                entry(b"bin/app/", b'5', b"", b"", 0o755),
            ],
        ),
        (
            "a file over a folder",
            vec![
                entry(b"bin/app/", b'5', b"", b"", 0o755),
                entry(b"bin/app", b'0', &body, b"", 0o755),
            ],
        ),
        (
            "a file over the folder of another file",
            vec![
                entry(b"x/y", b'0', b"y", b"", 0o644),
                entry(b"x", b'0', b"x", b"", 0o644),
            ],
        ),
        (
            "a file under a file",
            vec![
                entry(b"x", b'0', b"x", b"", 0o644),
                entry(b"x/y", b'0', b"y", b"", 0o644),
            ],
        ),
        (
            "a file under a link",
            vec![
                entry(b"bin/app", b'0', &body, b"", 0o755),
                entry(b"l", b'2', b"", b"bin", 0o777),
                entry(b"l/y", b'0', b"y", b"", 0o644),
            ],
        ),
        (
            "a link under a link",
            vec![
                entry(b"bin/app", b'0', &body, b"", 0o755),
                entry(b"l", b'2', b"", b"bin", 0o777),
                entry(b"l/m", b'2', b"", b"../bin/app", 0o777),
            ],
        ),
        (
            "a link over a folder",
            vec![
                entry(b"bin/app", b'0', &body, b"", 0o755),
                entry(b"d/", b'5', b"", b"", 0o755),
                entry(b"d", b'2', b"", b"bin", 0o777),
            ],
        ),
    ];
    for (what, entries) in cases {
        let mut tar: Vec<u8> = entries.concat();
        tar.extend(&m);
        assert!(unpack_tar("clash", &with_end(tar)).is_err(), "{what}");
    }
}

#[test]
fn link_chains_are_followed_by_hand_and_must_end_inside() {
    let mk = |links: &[(&str, &str)]| {
        let mut t = Tree::minimal();
        t.links = links
            .iter()
            .map(|(p, l)| (p.to_string(), l.to_string()))
            .collect();
        t
    };
    // Fine: a link to a link to a file, and to a folder.
    let ok = mk(&[("a", "b"), ("b", "bin/app"), ("c", "bin")]);
    unpack_tar("chain-ok", &with_end(ok.tar())).unwrap();
    for (what, links) in [
        ("a loop", vec![("a", "b"), ("b", "a")]),
        ("a link to itself", vec![("a", "a")]),
        ("a link to nothing", vec![("a", "missing")]),
        ("up out by a chain", vec![("x/up", ".."), ("b", "x/up/..")]),
        ("up from the root", vec![("a", "..")]),
        ("a to b, b out", vec![("a", "b"), ("b", "../x")]),
        ("out deeper", vec![("d/l", "../..")]),
        ("absolute", vec![("a", "/etc/passwd")]),
        (
            "through a link to the parent of the root",
            vec![("d/up", ".."), ("e", "d/up/../..")],
        ),
    ] {
        let t = mk(&links);
        assert!(unpack_tar("chain", &with_end(t.tar())).is_err(), "{what}");
    }
}

#[test]
fn what_the_tar_library_resolves_is_checked_like_everything_else() {
    let t = Tree::minimal();
    let body = t.files[0].1.clone();
    let m = entry(b"telamon-bundle.json", b'0', &t.manifest(), b"", 0o644);
    let tar_of = |parts: Vec<Vec<u8>>| {
        let mut tar: Vec<u8> = parts.concat();
        tar.extend(&m);
        with_end(tar)
    };
    // A PAX path that leaves, on an innocent header name.
    let hostile = tar_of(vec![
        pax(&[("path", "../evil")]),
        entry(b"bin/app", b'0', &body, b"", 0o755),
    ]);
    assert!(unpack_tar("pax-path", &hostile).is_err());
    // A PAX link target that leaves.
    let hostile = tar_of(vec![
        entry(b"bin/app", b'0', &body, b"", 0o755),
        pax(&[("linkpath", "/etc/passwd")]),
        entry(b"l", b'2', b"", b"bin/app", 0o777),
    ]);
    assert!(unpack_tar("pax-link", &hostile).is_err());
    // A GNU long name that leaves, and a long link target.
    let hostile = tar_of(vec![
        long_name(&format!("share/{}/../../../evil", "a".repeat(120))),
        entry(b"placeholder", b'0', b"x", b"", 0o644),
        entry(b"bin/app", b'0', &body, b"", 0o755),
    ]);
    assert!(unpack_tar("gnu-long", &hostile).is_err());
    // Two long names for one entry.
    let hostile = tar_of(vec![
        long_name("share/a"),
        long_name("share/b"),
        entry(b"placeholder", b'0', b"x", b"", 0o644),
        entry(b"bin/app", b'0', &body, b"", 0o755),
    ]);
    assert!(unpack_tar("two-long", &hostile).is_err());
    // A long name for a file that is there on the manifest's word is fine.
    let deep = format!("share/{}/{}/file", "d".repeat(90), "e".repeat(90));
    let mut t2 = Tree::minimal();
    t2.files.push((deep.clone(), b"deep".to_vec(), false));
    let mut tar = entry(b"bin/app", b'0', &body, b"", 0o755);
    tar.extend(long_name(&deep));
    tar.extend(entry(b"placeholder", b'0', b"deep", b"", 0o644));
    tar.extend(entry(
        b"telamon-bundle.json",
        b'0',
        &t2.manifest(),
        b"",
        0o644,
    ));
    let (r, _root, dest) = unpack_raw("gnu-long-ok", &zst(&with_end(tar)));
    r.unwrap();
    assert_eq!(fs::read(dest.join(&deep)).unwrap(), b"deep");
}

#[test]
fn a_pax_size_that_disagrees_with_the_header_is_not_believed_either_way() {
    let t = Tree::minimal();
    let body = t.files[0].1.clone();
    let m = entry(b"telamon-bundle.json", b'0', &t.manifest(), b"", 0o644);
    let tar_of = |parts: Vec<Vec<u8>>| {
        let mut tar: Vec<u8> = parts.concat();
        tar.extend(&m);
        with_end(tar)
    };
    // Bigger than the data there is: the library would read on into the next
    // headers; the file's checksum and size then differ from the manifest's.
    let r = unpack_tar(
        "pax-big",
        &tar_of(vec![
            pax(&[("size", "100000")]),
            entry(b"bin/app", b'0', &body, b"", 0o755),
        ]),
    );
    assert!(r.is_err());
    // Smaller than the header says.
    let r = unpack_tar(
        "pax-small",
        &tar_of(vec![
            pax(&[("size", "3")]),
            entry(b"bin/app", b'0', &body, b"", 0o755),
        ]),
    );
    assert!(r.is_err());
    // Over the cap for one file, with a header that says 0.
    let r = unpack_tar(
        "pax-huge",
        &tar_of(vec![
            pax(&[("size", "99999999999")]),
            entry(b"bin/app", b'0', b"", b"", 0o755),
        ]),
    );
    let e = r.unwrap_err();
    assert!(e.contains("too large") || e.contains("damaged"), "{e}");
}

#[test]
fn headers_that_claim_more_than_can_be_held_are_stopped() {
    let t = Tree::minimal();
    let body = t.files[0].1.clone();
    let big = 3u64 << 30;
    // An extension header, a long name and a folder that claim gigabytes with
    // no data behind them: refused without being read into memory.
    for (what, first) in [
        (
            "long name",
            raw_entry(b"././@LongLink", b'L', big, b"", b"", 0o644),
        ),
        ("pax", raw_entry(b"PaxHeader/x", b'x', big, b"", b"", 0o644)),
        (
            "global pax",
            raw_entry(b"PaxHeader/g", b'g', big, b"", b"", 0o644),
        ),
        ("folder", raw_entry(b"d/", b'5', big, b"", b"", 0o755)),
        ("link", raw_entry(b"l", b'2', big, b"", b"bin/app", 0o777)),
    ] {
        let mut tar = first;
        tar.extend(entry(b"bin/app", b'0', &body, b"", 0o755));
        let started = std::time::Instant::now();
        let r = unpack_tar("claim", &with_end(tar));
        assert!(r.is_err(), "{what}");
        assert!(started.elapsed().as_secs() < 20, "{what}");
    }
    // A size that overflows the arithmetic.
    for size in [u64::MAX, u64::MAX - 100, 1 << 62, 1 << 41] {
        let mut tar = raw_entry(b"bin/app", b'0', size, b"x", b"", 0o755);
        tar.extend(entry(
            b"telamon-bundle.json",
            b'0',
            &t.manifest(),
            b"",
            0o644,
        ));
        assert!(unpack_tar("size", &with_end(tar)).is_err(), "{size}");
    }
}

#[test]
fn sparse_and_other_special_entries_are_refused() {
    let t = Tree::minimal();
    let body = t.files[0].1.clone();
    for (what, kind) in [
        ("GNU sparse", b'S'),
        ("hard link", b'1'),
        ("character device", b'3'),
        ("block device", b'4'),
        ("FIFO", b'6'),
        ("global PAX header", b'g'),
        ("an unknown type", b'Z'),
    ] {
        let mut tar = entry(b"bin/app", b'0', &body, b"", 0o755);
        tar.extend(entry(b"extra", kind, b"", b"bin/app", 0o644));
        tar.extend(entry(
            b"telamon-bundle.json",
            b'0',
            &t.manifest(),
            b"",
            0o644,
        ));
        assert!(unpack_tar("special", &with_end(tar)).is_err(), "{what}");
    }
    // The "contiguous file" type is a regular file.
    let mut t2 = Tree::minimal();
    t2.files.push(("extra".into(), b"c".to_vec(), false));
    let mut tar = entry(b"bin/app", b'0', &body, b"", 0o755);
    tar.extend(entry(b"extra", b'7', b"c", b"", 0o644));
    tar.extend(entry(
        b"telamon-bundle.json",
        b'0',
        &t2.manifest(),
        b"",
        0o644,
    ));
    unpack_tar("contiguous", &with_end(tar)).unwrap();
}

#[test]
fn too_many_entries_are_refused() {
    let t = Tree::minimal();
    let mut tar = Vec::new();
    for _ in 0..40_001 {
        tar.extend(entry(b"./", b'5', b"", b"", 0o755));
    }
    tar.extend(t.tar());
    let e = unpack_tar("many", &with_end(tar)).unwrap_err();
    assert!(e.contains("too many"), "{e}");
}

#[test]
fn the_decompressor_is_held_to_a_window_and_to_the_end_of_the_tar() {
    let t = Tree::minimal();
    let tar = with_end(t.tar());
    // A window the Store allows, and one it does not.
    let frame = |log: u32| {
        let mut enc = zstd::stream::Encoder::new(Vec::new(), 3).unwrap();
        enc.window_log(log).unwrap();
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap()
    };
    let (r, ..) = unpack_raw("window-27", &frame(27));
    r.unwrap();
    let (r, ..) = unpack_raw("window-28", &frame(28));
    assert!(r.is_err(), "a 256 MiB window was accepted");
    let (r, ..) = unpack_raw("window-30", &frame(30));
    assert!(r.is_err(), "a 1 GiB window was accepted");

    // Garbage after the end of the tar.
    let mut junk = tar.clone();
    junk.extend(b"garbage after the end");
    assert!(unpack_tar("garbage", &junk).is_err());
    let mut junk = tar.clone();
    junk.extend(vec![0u8; 100]);
    junk.push(1);
    assert!(unpack_tar("garbage-late", &junk).is_err());
    // A second tar behind the first one's end is not read as more entries.
    let mut two = tar.clone();
    two.extend(entry(b"evil", b'0', b"x", b"", 0o644));
    two.extend(end());
    assert!(unpack_tar("second-tar", &two).is_err());

    // A bomb after the end: far more zeros than padding may be. It is stopped
    // when the allowance is used up, not when the whole thing is expanded.
    let mut enc = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
    enc.write_all(&tar).unwrap();
    let chunk = vec![0u8; 1 << 20];
    for _ in 0..300 {
        enc.write_all(&chunk).unwrap();
    }
    let bomb = enc.finish().unwrap();
    assert!(bomb.len() < 200 * 1024, "{}", bomb.len());
    let started = std::time::Instant::now();
    let (r, ..) = unpack_raw("bomb", &bomb);
    assert!(r.is_err());
    assert!(started.elapsed().as_secs() < 20);
}

#[test]
fn a_bundle_that_is_cut_short_is_refused() {
    let t = Tree::minimal();
    let tar = with_end(t.tar());
    for cut in [100, 513, tar.len() / 2, tar.len() - 1030] {
        let r = unpack_tar("cut", &tar[..cut]);
        assert!(r.is_err(), "cut at {cut}");
    }
}

#[test]
fn the_archive_unpacks_in_a_folder_that_is_a_link_only_by_its_own_path() {
    // `unpack` opens its destination without following a link in the last part.
    let t = Tree::minimal();
    let root = scratch("dest-link");
    let o = outside(&root);
    let f = root.join("b.tar.zst");
    fs::write(&f, zst(&with_end(t.tar()))).unwrap();
    symlink(&o, root.join("dest")).unwrap();
    assert!(archive::unpack(&f, &root.join("dest"), None).is_err());
    untouched(&o);
}

// ---- the files a real Telamon app ships are not turned away ----

#[test]
fn the_files_telamon_gates_and_the_app_template_ship_are_accepted() {
    // Copies of Telamon Gates' desktop entry, metainfo and icon, and of the
    // framework template's notification file (renamed for the app).
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native/gates-files");
    let read = |n: &str| fs::read(dir.join(n)).unwrap();
    let (d, root) = dirs("real-files");
    let b = gates("0.1.0")
        .file(
            &format!("share/applications/{ID}.desktop"),
            &read("net.eterneon.telamon.gates.desktop"),
            false,
        )
        .file(
            &format!("share/metainfo/{ID}.metainfo.xml"),
            &read("net.eterneon.telamon.gates.metainfo.xml"),
            false,
        )
        .file(
            &format!("share/icons/hicolor/scalable/apps/{ID}.svg"),
            &read("net.eterneon.telamon.gates.svg"),
            false,
        )
        .file(
            "share/knotifications6/telamon-gates.notifyrc",
            &read("telamon-gates.notifyrc"),
            false,
        );
    install_local(&d, &root, &b.build()).unwrap();
    assert!(install::list(&d)[0].icon.is_some());
    assert!(
        d.data
            .join("knotifications6/telamon-gates.notifyrc")
            .is_file()
    );
}

// ---- depth of a bundle's tree ----

use telamon_store_core::native::dirfd::test_hooks::NO_RENAME_FLAGS;
use telamon_store_core::native::manifest::MAX_PATH_PARTS;

fn deep(parts: usize) -> String {
    vec!["d"; parts].join("/")
}

#[test]
fn a_tree_deeper_than_the_cap_is_refused_before_anything_is_made() {
    let body = b"x".to_vec();
    let m = Tree::minimal().tar();
    // A chain of folders, as long names (GNU `L`) and as short ones.
    for depth in [MAX_PATH_PARTS + 1, 40, 300] {
        let mut tar = Vec::new();
        let path = format!("{}/", deep(depth));
        tar.extend(long_name(&path));
        tar.extend(entry(b"placeholder", b'5', b"", b"", 0o755));
        tar.extend(&m);
        let (r, _root, dest) = unpack_raw("deep-dirs", &zst(&with_end(tar)));
        assert!(r.is_err(), "{depth} folders");
        assert!(
            files_under(&dest).is_empty(),
            "{depth}: {:?}",
            files_under(&dest)
        );
        // And a file at the bottom.
        let mut tar = Vec::new();
        tar.extend(long_name(&format!("{}/f", deep(depth))));
        tar.extend(entry(b"placeholder", b'0', &body, b"", 0o644));
        tar.extend(&m);
        let (r, _root, dest) = unpack_raw("deep-file", &zst(&with_end(tar)));
        assert!(r.is_err(), "{depth} deep file");
        assert!(files_under(&dest).is_empty());
    }
    // Names short enough for a plain header: 40 parts of `a/`.
    let mut tar = entry(
        format!("{}/", "a/".repeat(39) + "a").as_bytes(),
        b'5',
        b"",
        b"",
        0o755,
    );
    tar.extend(&m);
    assert!(unpack_tar("deep-short", &with_end(tar)).is_err());
    // A manifest that lists a path or a link over the cap.
    let mut t = Tree::minimal();
    t.files
        .push((format!("{}/f", deep(MAX_PATH_PARTS)), b"x".to_vec(), false));
    assert!(unpack_tar("deep-manifest", &with_end(t.tar())).is_err());
    let mut t = Tree::minimal();
    t.links
        .push(("l".into(), format!("bin/{}", deep(MAX_PATH_PARTS))));
    assert!(unpack_tar("deep-link", &with_end(t.tar())).is_err());
}

#[test]
fn a_tree_at_the_cap_installs_uninstalls_and_sweeps() {
    let (d, root) = dirs("deep-cap");
    let file = format!("share/{}/f", deep(MAX_PATH_PARTS - 2));
    assert_eq!(file.split('/').count(), MAX_PATH_PARTS);
    let built = gates("0.1.0").file(&file, b"bottom", false).build();
    install_local(&d, &root, &built).unwrap();
    assert_eq!(
        fs::read(d.app(ID).join("0.1.0").join(&file)).unwrap(),
        b"bottom"
    );
    install::uninstall(&d, ID).unwrap();
    assert!(!d.app(ID).exists());
    assert!(install::list(&d).is_empty());

    // A staging folder a killed install left, deeper than any bundle can be,
    // is swept by the next install.
    install_local(&d, &root, &built).unwrap();
    let left = d.apps().join(".staging-killed-1");
    fs::create_dir_all(left.join(deep(200))).unwrap();
    fs::write(left.join(deep(200)).join("f"), b"x").unwrap();
    install_local(
        &d,
        &root,
        &gates("0.2.0").file(&file, b"bottom", false).build(),
    )
    .unwrap();
    assert!(!left.exists(), "the staging folder was not swept");
}

#[test]
fn many_deep_links_are_resolved_quickly() {
    let mut t = Tree::minimal();
    let target = format!("{}/f", deep(MAX_PATH_PARTS - 2));
    t.files.push((target.clone(), b"bottom".to_vec(), false));
    for i in 0..4000 {
        t.links.push((format!("l{i}"), target.clone()));
    }
    // Links in the deepest folder, to its file.
    for i in 0..500 {
        t.links.push((
            format!("{}/m{i}", deep(MAX_PATH_PARTS - 2)),
            "f".to_string(),
        ));
    }
    let started = std::time::Instant::now();
    let (r, _root, dest) = unpack_raw("deep-links", &zst(&with_end(t.tar())));
    r.unwrap();
    assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
    assert_eq!(fs::read(dest.join("l3999")).unwrap(), b"bottom");
}

#[test]
fn a_removal_that_stops_half_way_leaves_the_app_listed_and_removable() {
    let (d, root) = dirs("uninstall-half");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    FAIL_AFTER.with(|f| f.set(Some("contents")));
    let e = install::uninstall(&d, ID).unwrap_err();
    FAIL_AFTER.with(|f| f.set(None));
    assert!(e.0.contains("contents"), "{e}");
    // The folder's contents are gone but the app is still known.
    assert_eq!(install::list(&d).len(), 1);
    assert!(install::read_record(&d, ID).is_some());
    assert!(d.app(ID).join("install.json").is_file());
    // And it can be removed, or installed again over.
    install::uninstall(&d, ID).unwrap();
    assert!(!d.app(ID).exists());
    assert!(install::list(&d).is_empty());
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    FAIL_AFTER.with(|f| f.set(Some("contents")));
    assert!(install::uninstall(&d, ID).is_err());
    FAIL_AFTER.with(|f| f.set(None));
    install_local(&d, &root, &gates("0.2.0").build()).unwrap();
    assert_eq!(install::read_record(&d, ID).unwrap().version, "0.2.0");
}

// ---- a FIFO in a system folder ----

#[test]
fn a_fifo_in_a_system_folder_does_not_hold_the_install() {
    let (mut d, root) = dirs("fifo");
    let sys = system_dir(&root);
    fs::create_dir_all(sys.join("dbus-1/services")).unwrap();
    let fifo = sys.join("dbus-1/services/evil.service");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    d.system = vec![sys];
    let (tx, rx) = std::sync::mpsc::channel();
    let (d2, root2) = (d.clone(), root.clone());
    std::thread::spawn(move || {
        let r = install_local(&d2, &root2, &gates("0.1.0").build());
        let _ = tx.send(r);
    });
    let got = rx.recv_timeout(std::time::Duration::from_secs(20));
    if got.is_err() {
        // Let the stuck read go, so the test can end.
        let _ = fs::OpenOptions::new().write(true).open(&fifo);
    }
    got.expect("the install is held by a FIFO").unwrap();
    assert_eq!(install::list(&d).len(), 1);
}

// ---- file systems without renameat2 flags ----

#[test]
fn an_install_update_and_removal_work_where_renameat2_flags_do_not() {
    NO_RENAME_FLAGS.with(|h| h.set(true));
    let (d, root) = dirs("noflags");
    install_local(
        &d,
        &root,
        &gates("0.1.0")
            .file(
                &format!("share/icons/hicolor/48x48/apps/{ID}.png"),
                &png(48, 48),
                false,
            )
            .build(),
    )
    .unwrap();
    // An update replaces the copies it wrote.
    install_local(&d, &root, &gates("0.2.0").build()).unwrap();
    assert!(
        !d.data
            .join(format!("icons/hicolor/48x48/apps/{ID}.png"))
            .exists()
    );
    let desktop = d.data.join(format!("applications/{ID}.desktop"));
    assert!(fs::read_to_string(&desktop).unwrap().contains("0.2.0"));
    // A file the user changed is still not replaced, nor removed.
    fs::write(
        &desktop,
        b"[Desktop Entry]\nType=Application\nName=Mine\nExec=true\n",
    )
    .unwrap();
    let e = install_local(&d, &root, &gates("0.3.0").build()).unwrap_err();
    assert!(e.contains("was changed since"), "{e}");
    assert_eq!(install::read_record(&d, ID).unwrap().version, "0.2.0");
    // A file someone else has is still not overwritten by a first install.
    let r = install::uninstall(&d, ID).unwrap();
    assert_eq!(r.left.len(), 1);
    assert!(desktop.is_file());
    let e = install_local(&d, &root, &gates("0.1.0").build()).unwrap_err();
    assert!(e.contains("didn't put it there"), "{e}");
    fs::remove_file(&desktop).unwrap();
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    // Rollback still works.
    FAIL_AFTER.with(|f| f.set(Some("record")));
    let before = files_under(&d.data);
    assert!(install_local(&d, &root, &gates("0.2.0").build()).is_err());
    FAIL_AFTER.with(|f| f.set(None));
    assert_eq!(files_under(&d.data), before);
    install::uninstall(&d, ID).unwrap();
    NO_RENAME_FLAGS.with(|h| h.set(false));
    assert!(install::list(&d).is_empty());
}

// ---- the data folder is not "the system" ----

#[test]
fn the_data_folder_in_the_system_list_is_not_the_system() {
    let svc = |b: BundleBuilder| {
        b.file(
            &format!("share/dbus-1/services/{ID}.Daemon.service"),
            &service(&format!("{ID}.Daemon")),
            false,
        )
    };
    let (mut d, root) = dirs("data-in-system");
    // Listed as it is, and through a link.
    symlink(&d.data, root.join("data-link")).unwrap();
    d.system = vec![d.data.clone(), root.join("data-link"), system_dir(&root)];
    install_local(&d, &root, &svc(gates("0.1.0")).build()).unwrap();
    install_local(&d, &root, &svc(gates("0.2.0")).build()).unwrap();
    assert_eq!(install::read_record(&d, ID).unwrap().version, "0.2.0");
    install::uninstall(&d, ID).unwrap();
    // A real system folder still counts.
    put(
        &d.system[2].clone(),
        "applications/net.eterneon.telamon.gates.desktop",
        b"x",
    );
    assert!(install_local(&d, &root, &gates("0.1.0").build()).is_err());
}

// ---- temporary files a killed install left ----

#[test]
fn temporary_files_of_a_killed_write_are_swept_only_when_they_are_the_stores() {
    let (d, root) = dirs("temps");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    let apps_dir = d.data.join("applications");
    let left = [
        apps_dir.join(format!(".{ID}.desktop.99.1.tmp")),
        d.app(ID).join(".install.json.99.2.tmp"),
    ];
    let stay = [
        apps_dir.join(".other.desktop.99.1.tmp"),
        apps_dir.join(format!(".{ID}.desktop.x.1.tmp")),
        apps_dir.join(format!("{ID}.desktop.orig-1")),
    ];
    let plant = || {
        for p in left.iter().chain(&stay) {
            fs::write(p, b"left").unwrap();
        }
        symlink("0.1.0", d.app(ID).join(".current.99-3")).unwrap();
        // The exact name, but a link and a folder: not the Store's files.
        symlink("x", apps_dir.join(format!(".{ID}.desktop.98.1.tmp"))).unwrap();
        fs::create_dir(apps_dir.join(format!(".{ID}.desktop.97.1.tmp"))).unwrap();
    };
    plant();
    install_local(&d, &root, &gates("0.2.0").build()).unwrap();
    for p in &left {
        assert!(!p.exists(), "{}", p.display());
    }
    assert!(std::fs::symlink_metadata(d.app(ID).join(".current.99-3")).is_err());
    for p in &stay {
        assert!(p.exists(), "{}", p.display());
    }
    assert!(std::fs::symlink_metadata(apps_dir.join(format!(".{ID}.desktop.98.1.tmp"))).is_ok());
    assert!(apps_dir.join(format!(".{ID}.desktop.97.1.tmp")).is_dir());
    // Uninstall sweeps the ones next to the files it removes.
    fs::write(&left[0], b"left").unwrap();
    install::uninstall(&d, ID).unwrap();
    assert!(!left[0].exists());
}

// ---- texts: translated marks, escaped actions, the allow-listed metainfo ----

#[test]
fn a_translated_name_with_right_to_left_marks_installs() {
    let (d, root) = dirs("rtl");
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName=Gates\nName[ar]=\u{200F}\u{0628}\u{0648}\u{0627}\u{0628}\u{0629}\u{200F}\nName[he]=\u{200E}\u{05E9}\u{05E2}\u{05E8}\u{200E}\nExec=telamon-gates\nIcon={ID}\n"
    );
    let b = gates("1.0.0").file(
        &format!("share/applications/{ID}.desktop"),
        desktop.as_bytes(),
        false,
    );
    install_local(&d, &root, &b.build()).unwrap();
    let out = fs::read_to_string(d.data.join(format!("applications/{ID}.desktop"))).unwrap();
    assert!(out.contains("Name[ar]=\u{200F}\u{0628}"), "{out}");
    // The same marks in an untranslated name are refused.
    let (d, root) = dirs("rtl-bad");
    let bad = desktop.replace("Name=Gates", "Name=\u{200F}Gates");
    let b = gates("1.0.0").file(
        &format!("share/applications/{ID}.desktop"),
        bad.as_bytes(),
        false,
    );
    assert!(install_local(&d, &root, &b.build()).is_err());
}

#[test]
fn an_escaped_execute_in_an_action_is_refused() {
    let (d, root) = dirs("kconfig-escape");
    let b = gates("1.0.0").file(
        "share/knotifications6/telamon-gates.notifyrc",
        b"[Event/m]\nName=x\nAction=Popup|Ex\\x65cute\nExecute=true\n",
        false,
    );
    assert!(install_local(&d, &root, &b.build()).is_err());
    let b = gates("1.0.0").file(
        "share/knotifications6/telamon-gates.notifyrc",
        b"[Event/m]\nName=x\nAction=Popup|Ex\\x65cute\n",
        false,
    );
    assert!(install_local(&d, &root, &b.build()).is_err());
}

#[test]
fn metainfo_files_as_the_appstream_specification_shows_them_install() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native/metainfo");
    for name in [
        "qt-app.metainfo.xml",
        "release-notes.metainfo.xml",
        "screenshots.metainfo.xml",
        "content-rating-requires.metainfo.xml",
    ] {
        let (d, root) = dirs("metainfo-fixture");
        let xml = fs::read(dir.join(name)).unwrap();
        let b = gates("1.0.0").file(&format!("share/metainfo/{ID}.metainfo.xml"), &xml, false);
        install_local(&d, &root, &b.build()).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // A package name or a bundle element is not copied.
    let (d, root) = dirs("metainfo-pkgname");
    let xml = format!("<component><id>{ID}</id><pkgname>coreutils</pkgname></component>");
    let b = gates("1.0.0").file(
        &format!("share/metainfo/{ID}.metainfo.xml"),
        xml.as_bytes(),
        false,
    );
    let e = install_local(&d, &root, &b.build()).unwrap_err();
    assert!(e.contains("pkgname"), "{e}");
}
