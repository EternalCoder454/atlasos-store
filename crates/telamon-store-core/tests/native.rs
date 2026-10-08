//! Native Telamon apps: the catalog and a release checked, a bundle unpacked
//! with hostile contents refused, install, update, rollback and uninstall, in
//! scratch trees and against a fake GitHub (recorded-style answers). Nothing
//! here touches the network, the real home folder or the real Flatpak.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use telamon_store_core::native::check::{self, Status, TTL};
use telamon_store_core::native::fake::{Built, BundleBuilder, Fake, Raw};
use telamon_store_core::native::fetch::Cache;
use telamon_store_core::native::install::{self, Dirs, Options, Origin};
use telamon_store_core::native::manifest::{Host, Kind, Manifest};
use telamon_store_core::native::version::Version;
use telamon_store_core::native::{archive, catalog::Entry};
use telamon_store_core::net::NetError;

const ID: &str = "net.eterneon.telamon.gates";
const REPO: &str = "EternalCoder454/telamon-gates";

fn scratch(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "native-{name}-{}-{}",
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
    (Dirs { data, home }, root)
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

fn write_archive(root: &Path, built: &Built) -> PathBuf {
    let p = root.join(format!("bundle-{}.tar.zst", built.sha256.get(..8).unwrap()));
    fs::write(&p, &built.archive).unwrap();
    p
}

fn local(host: &Host) -> Options<'_> {
    Options {
        expect_id: None,
        outer: None,
        origin: Origin::local(),
        host,
    }
}

fn install_local(d: &Dirs, root: &Path, built: &Built) -> Result<install::Done, String> {
    let h = host();
    let f = write_archive(root, built);
    install::install_bundle(d, &f, &local(&h)).map_err(|e| e.0)
}

fn files_under(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            out.push(rel);
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

// ---- hostile archives ----

fn raw(name: &str, kind: tar::EntryType, data: &[u8], link: Option<&str>) -> Raw {
    Raw {
        name: name.as_bytes().to_vec(),
        kind,
        data: data.to_vec(),
        link: link.map(|l| l.as_bytes().to_vec()),
    }
}

#[test]
fn a_hostile_archive_is_refused_and_leaves_nothing() {
    use tar::EntryType::{Block, Char, Fifo, Link, Regular, Symlink};
    let cases: Vec<(&str, BundleBuilder)> = vec![
        (
            "parent dir",
            gates("1.0.0").raw(raw("../evil", Regular, b"x", None)),
        ),
        (
            "deep parent dir",
            gates("1.0.0").raw(raw("bin/../../evil", Regular, b"x", None)),
        ),
        (
            "absolute",
            gates("1.0.0").raw(raw("/etc/evil", Regular, b"x", None)),
        ),
        (
            "dot component",
            gates("1.0.0").raw(raw("share/./x", Regular, b"x", None)),
        ),
        (
            "empty component",
            gates("1.0.0").raw(raw("share//x", Regular, b"x", None)),
        ),
        (
            "duplicate",
            gates("1.0.0").raw(raw("bin/telamon-gates", Regular, b"x", None)),
        ),
        (
            "symlink to absolute",
            gates("1.0.0").link("share/passwd", "/etc/passwd"),
        ),
        (
            "symlink up out",
            gates("1.0.0").link("share/up", "../../.."),
        ),
        ("symlink up from root", gates("1.0.0").link("up", "..")),
        (
            "raw symlink to absolute",
            gates("1.0.0").raw(raw("share/l", Symlink, b"", Some("/etc"))),
        ),
        (
            "hard link",
            gates("1.0.0").raw(raw("share/h", Link, b"", Some("bin/telamon-gates"))),
        ),
        ("fifo", gates("1.0.0").raw(raw("share/f", Fifo, b"", None))),
        (
            "char device",
            gates("1.0.0").raw(raw("share/c", Char, b"", None)),
        ),
        (
            "block device",
            gates("1.0.0").raw(raw("share/b", Block, b"", None)),
        ),
        (
            "file under a file",
            gates("1.0.0").raw(raw("bin/telamon-gates/x", Regular, b"x", None)),
        ),
        (
            "not in the manifest",
            gates("1.0.0").raw(raw("share/extra", Regular, b"x", None)),
        ),
        ("no manifest", gates("1.0.0").no_manifest()),
        (
            "bad checksum",
            gates("1.0.0").edit_inner(|m| m.files[0].sha256 = "0".repeat(64)),
        ),
        (
            "bad size",
            gates("1.0.0").edit_inner(|m| m.files[0].size += 1),
        ),
        (
            "missing file",
            gates("1.0.0").edit_inner(|m| {
                m.files
                    .push(telamon_store_core::native::manifest::FileEntry {
                        path: "share/gone".into(),
                        size: 1,
                        sha256: "a".repeat(64),
                        executable: false,
                    })
            }),
        ),
        (
            "manifest names an unlisted link",
            gates("1.0.0").edit_inner(|m| {
                m.links
                    .push(telamon_store_core::native::manifest::LinkEntry {
                        path: "share/l".into(),
                        target: "x".into(),
                    })
            }),
        ),
    ];
    for (what, builder) in cases {
        let (d, root) = dirs("hostile");
        let built = builder.build();
        let r = install_local(&d, &root, &built);
        assert!(r.is_err(), "{what}: installed");
        assert!(
            !root.join("evil").exists() && !d.data.join("evil").exists(),
            "{what}"
        );
        // Nothing of the app is left: no folder, no copied file, no stray staging.
        let left: Vec<String> = files_under(&d.data);
        assert!(
            left.iter()
                .all(|p| p == "telamon-apps" || p == "telamon-apps/.lock"),
            "{what}: left {left:?}"
        );
        assert!(install::list(&d).is_empty(), "{what}");
    }
}

#[test]
fn a_link_chain_that_climbs_out_is_caught_on_the_real_folders() {
    // `a/up` is the root; `b` -> `a/up/..` reads as inside lexically but leaves.
    let (d, root) = dirs("chain");
    let built = gates("1.0.0")
        .link("a/up", "..")
        .link("b", "a/up/..")
        .build();
    let r = install_local(&d, &root, &built);
    assert!(r.is_err(), "installed");
    assert!(install::list(&d).is_empty());
}

#[test]
fn a_link_inside_the_folder_is_allowed() {
    let (d, root) = dirs("link-ok");
    let built = gates("1.0.0")
        .link("share/alias", "net.eterneon.telamon.gates")
        .build();
    install_local(&d, &root, &built).unwrap();
    let alias = d.app(ID).join("current/share/alias/data.txt");
    assert!(alias.is_file());
}

#[test]
fn an_oversize_declaration_is_refused() {
    let (d, root) = dirs("oversize");
    let built = gates("1.0.0")
        .edit_inner(|m| m.files[0].size = telamon_store_core::native::manifest::MAX_FILE + 1)
        .build();
    assert!(install_local(&d, &root, &built).is_err());
    // A tar header that claims more than the cap is stopped before any data.
    let mut big = tar::Header::new_gnu();
    big.set_size(telamon_store_core::native::manifest::MAX_FILE + 1);
    big.set_path("share/huge").unwrap();
    big.set_cksum();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(big.as_bytes());
    let archive_bytes = zstd::stream::encode_all(&bytes[..], 3).unwrap();
    let f = root.join("big.tar.zst");
    fs::write(&f, archive_bytes).unwrap();
    let h = host();
    let e = install::install_bundle(&d, &f, &local(&h)).unwrap_err();
    assert!(e.0.contains("too large") || e.0.contains("damaged"), "{e}");
}

#[test]
fn not_a_zstd_archive_is_refused() {
    let (d, root) = dirs("garbage");
    let f = root.join("junk.tar.zst");
    fs::write(&f, b"this is not an archive at all").unwrap();
    let h = host();
    assert!(install::install_bundle(&d, &f, &local(&h)).is_err());
    assert!(install::list(&d).is_empty());
}

#[test]
fn the_outer_manifest_must_match_the_one_inside() {
    let (d, root) = dirs("outer");
    let built = gates("1.0.0").build();
    let mut outer = built.outer.clone();
    outer.summary = "something else".into();
    let h = host();
    let f = write_archive(&root, &built);
    let e = install::install_bundle(
        &d,
        &f,
        &Options {
            expect_id: Some(ID),
            outer: Some(&outer),
            origin: Origin::local(),
            host: &h,
        },
    )
    .unwrap_err();
    assert!(e.0.contains("inside"), "{e}");
    // The right one installs.
    install::install_bundle(
        &d,
        &f,
        &Options {
            expect_id: Some(ID),
            outer: Some(&built.outer),
            origin: Origin::local(),
            host: &h,
        },
    )
    .unwrap();
    // An expected id that differs is refused.
    let (d2, root2) = dirs("outer-id");
    let f2 = write_archive(&root2, &built);
    let e = install::install_bundle(
        &d2,
        &f2,
        &Options {
            expect_id: Some("org.example.Other"),
            outer: None,
            origin: Origin::local(),
            host: &h,
        },
    )
    .unwrap_err();
    assert!(e.0.contains("different app"), "{e}");
}

#[test]
fn a_bundle_that_does_not_fit_this_system_is_refused() {
    let (d, root) = dirs("compat");
    let built = gates("1.0.0")
        .edit_inner(|m| m.min_telamon_ui = "9.0.0".into())
        .build();
    let e = install_local(&d, &root, &built).unwrap_err();
    assert!(e.contains("Telamon.Ui"), "{e}");
    let built = gates("1.0.0")
        .edit_inner(|m| m.min_os_version = "99".into())
        .build();
    assert!(
        install_local(&d, &root, &built)
            .unwrap_err()
            .contains("Fedora")
    );
}

// ---- install, update, rollback, uninstall ----

#[test]
fn an_install_puts_the_tree_and_the_desktop_files_in_place() {
    let (d, root) = dirs("install");
    let done = install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    assert_eq!(
        (done.id.as_str(), done.version.as_str(), done.replaced),
        (ID, "0.1.0", None)
    );
    let app = d.app(ID);
    assert_eq!(
        fs::read_link(app.join("current")).unwrap(),
        Path::new("0.1.0")
    );
    let exe = app.join("0.1.0/bin/telamon-gates");
    assert_eq!(fs::metadata(&exe).unwrap().mode() & 0o777, 0o755);
    assert_eq!(
        fs::metadata(app.join("0.1.0/share/net.eterneon.telamon.gates/data.txt"))
            .unwrap()
            .mode()
            & 0o777,
        0o644
    );
    // The desktop entry runs the program through `current`, by absolute path.
    let desktop = fs::read_to_string(d.data.join(format!("applications/{ID}.desktop"))).unwrap();
    let want = format!(
        "Exec={}/bin/telamon-gates %U",
        app.join("current").display()
    );
    assert!(desktop.contains(&want), "{desktop}");
    assert!(desktop.contains(&format!("X-Telamon-Native-App={ID}")));
    assert!(
        d.data
            .join(format!("icons/hicolor/scalable/apps/{ID}.svg"))
            .is_file()
    );
    assert!(d.data.join(format!("metainfo/{ID}.metainfo.xml")).is_file());
    // The listing and the record.
    let list = install::list(&d);
    assert_eq!(list.len(), 1);
    assert_eq!(
        (
            list[0].id.as_str(),
            list[0].version.as_str(),
            list[0].present
        ),
        (ID, "0.1.0", true)
    );
    assert!(list[0].icon.as_ref().is_some_and(|p| p.is_file()));
    // The link resolves to the program the desktop entry names.
    assert!(app.join("current/bin/telamon-gates").is_file());
}

#[test]
fn installing_the_same_version_again_says_so() {
    let (d, root) = dirs("again");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    let e = install_local(&d, &root, &gates("0.1.0").build()).unwrap_err();
    assert!(e.contains("already installed"), "{e}");
    assert_eq!(install::list(&d).len(), 1);
}

#[test]
fn an_update_goes_beside_the_old_version_then_switches_and_keeps_one_back() {
    let (d, root) = dirs("update");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    // A running app holds the old program open: replacing must not touch it.
    let old_exe = d.app(ID).join("0.1.0/bin/telamon-gates");
    let held = fs::File::open(&old_exe).unwrap();
    let done = install_local(&d, &root, &gates("0.2.0").build()).unwrap();
    assert_eq!(done.replaced.as_deref(), Some("0.1.0"));
    assert_eq!(
        fs::read_link(d.app(ID).join("current")).unwrap(),
        Path::new("0.2.0")
    );
    assert!(
        d.app(ID).join("0.1.0").is_dir(),
        "the old version stays until the next update"
    );
    drop(held);
    let rec = install::read_record(&d, ID).unwrap();
    assert_eq!(
        (rec.version.as_str(), rec.previous.as_deref()),
        ("0.2.0", Some("0.1.0"))
    );
    // The next update drops the one before.
    install_local(&d, &root, &gates("0.3.0").build()).unwrap();
    assert!(!d.app(ID).join("0.1.0").exists());
    assert!(d.app(ID).join("0.2.0").is_dir() && d.app(ID).join("0.3.0").is_dir());
    let desktop = fs::read_to_string(d.data.join(format!("applications/{ID}.desktop"))).unwrap();
    assert!(desktop.contains("X-Telamon-Native-Version=0.3.0"));
}

#[test]
fn an_update_that_drops_a_file_removes_its_copy() {
    let (d, root) = dirs("drop");
    install_local(
        &d,
        &root,
        &gates("0.1.0")
            .file(
                &format!("share/icons/hicolor/48x48/apps/{ID}.png"),
                b"png",
                false,
            )
            .build(),
    )
    .unwrap();
    let png = d.data.join(format!("icons/hicolor/48x48/apps/{ID}.png"));
    assert!(png.is_file());
    install_local(&d, &root, &gates("0.2.0").build()).unwrap();
    assert!(!png.exists());
}

#[test]
fn a_failed_update_leaves_the_old_version_working() {
    use telamon_store_core::native::install::test_hooks::FAIL_AFTER;
    let (d, root) = dirs("rollback");
    install_local(
        &d,
        &root,
        &gates("0.1.0")
            .file(
                &format!("share/icons/hicolor/48x48/apps/{ID}.png"),
                b"old icon",
                false,
            )
            .build(),
    )
    .unwrap();
    let desktop = d.data.join(format!("applications/{ID}.desktop"));
    let before_desktop = fs::read(&desktop).unwrap();
    let before_files = files_under(&d.data);
    let before_record = fs::read(d.app(ID).join("install.json")).unwrap();

    // 1. A damaged archive fails before anything changes.
    let bad = gates("0.2.0")
        .edit_inner(|m| m.files[0].sha256 = "0".repeat(64))
        .build();
    assert!(install_local(&d, &root, &bad).is_err());
    assert_eq!(files_under(&d.data), before_files);

    // 2. A failure at each step of the commit is undone: the copied files are
    // as they were (the new icon is gone, the old one back), `current` still
    // names the old version, the new folder is gone, the record is unchanged.
    for step in ["exports", "switch", "record"] {
        FAIL_AFTER.with(|f| f.set(Some(step)));
        let new = gates("0.2.0")
            .file(
                &format!("share/icons/hicolor/48x48/apps/{ID}.png"),
                b"new icon",
                false,
            )
            .file(
                &format!("share/icons/hicolor/64x64/apps/{ID}.png"),
                b"new 64",
                false,
            )
            .build();
        let e = install_local(&d, &root, &new).unwrap_err();
        FAIL_AFTER.with(|f| f.set(None));
        assert!(e.contains(&format!("after {step}")), "{step}: {e}");
        assert_eq!(
            fs::read_link(d.app(ID).join("current")).unwrap(),
            Path::new("0.1.0"),
            "{step}"
        );
        assert!(
            !d.app(ID).join("0.2.0").exists(),
            "{step}: the new folder was removed"
        );
        assert_eq!(fs::read(&desktop).unwrap(), before_desktop, "{step}");
        assert_eq!(
            fs::read(d.data.join(format!("icons/hicolor/48x48/apps/{ID}.png"))).unwrap(),
            b"old icon",
            "{step}"
        );
        assert!(
            !d.data
                .join(format!("icons/hicolor/64x64/apps/{ID}.png"))
                .exists(),
            "{step}"
        );
        assert_eq!(
            fs::read(d.app(ID).join("install.json")).unwrap(),
            before_record,
            "{step}"
        );
        assert_eq!(files_under(&d.data), before_files, "{step}");
        assert!(
            d.app(ID).join("current/bin/telamon-gates").is_file(),
            "{step}"
        );
    }

    // And the same update goes through afterwards.
    install_local(&d, &root, &gates("0.2.0").build()).unwrap();
    assert_eq!(install::read_record(&d, ID).unwrap().version, "0.2.0");
}

#[test]
fn a_failed_first_install_leaves_nothing() {
    use telamon_store_core::native::install::test_hooks::FAIL_AFTER;
    let (d, root) = dirs("rollback-first");
    for step in ["exports", "switch", "record"] {
        FAIL_AFTER.with(|f| f.set(Some(step)));
        let r = install_local(&d, &root, &gates("0.1.0").build());
        FAIL_AFTER.with(|f| f.set(None));
        assert!(r.is_err(), "{step}");
        let left = files_under(&d.data);
        assert!(
            left.iter().all(|p| p == "telamon-apps"
                || p == "telamon-apps/.lock"
                || p == "telamon-apps/net.eterneon.telamon.gates"
                || p.starts_with("applications")
                || p.starts_with("icons")
                || p.starts_with("metainfo")),
            "{step}: {left:?}"
        );
        assert!(
            !d.data.join(format!("applications/{ID}.desktop")).exists(),
            "{step}"
        );
        assert!(install::list(&d).is_empty(), "{step}");
    }
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
}

#[test]
fn an_install_never_replaces_a_file_that_is_not_its_own() {
    let (d, root) = dirs("conflict");
    fs::create_dir_all(d.data.join("applications")).unwrap();
    let theirs = d.data.join(format!("applications/{ID}.desktop"));
    fs::write(
        &theirs,
        b"[Desktop Entry]\nType=Application\nName=Mine\nExec=true\n",
    )
    .unwrap();
    let e = install_local(&d, &root, &gates("0.1.0").build()).unwrap_err();
    assert!(e.contains("didn't put it there"), "{e}");
    assert_eq!(
        fs::read(&theirs).unwrap(),
        b"[Desktop Entry]\nType=Application\nName=Mine\nExec=true\n"
    );
    assert!(!d.app(ID).exists() || files_under(&d.app(ID)).is_empty());
    // Not through a link either.
    fs::remove_file(&theirs).unwrap();
    let target = root.join("elsewhere");
    fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, &theirs).unwrap();
    assert!(install_local(&d, &root, &gates("0.1.0").build()).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"keep");
}

#[test]
fn a_bundle_can_only_write_files_named_after_its_id() {
    for (what, builder) in [
        (
            "a desktop entry of another app",
            gates("1.0.0").file(
                "share/applications/org.kde.dolphin.desktop",
                b"[Desktop Entry]\nType=Application\nName=x\nExec=telamon-gates\n",
                false,
            ),
        ),
        (
            "an icon of the theme",
            gates("1.0.0").file(
                "share/icons/hicolor/48x48/apps/document-save.png",
                b"x",
                false,
            ),
        ),
        (
            "a metainfo of another app",
            gates("1.0.0").file(
                "share/metainfo/org.kde.dolphin.metainfo.xml",
                b"<c/>",
                false,
            ),
        ),
        (
            "a bus service of another name",
            gates("1.0.0").file(
                "share/dbus-1/services/org.freedesktop.Notifications.service",
                b"[D-BUS Service]\nName=org.freedesktop.Notifications\nExec=telamon-gates\n",
                false,
            ),
        ),
        (
            "a launcher that runs something else",
            gates("1.0.0").file(
                &format!("share/applications/{ID}.desktop"),
                b"[Desktop Entry]\nType=Application\nName=x\nExec=/usr/bin/konsole\n",
                false,
            ),
        ),
        (
            "a launcher for a program that is not there",
            gates("1.0.0").file(
                &format!("share/applications/{ID}.desktop"),
                b"[Desktop Entry]\nType=Application\nName=x\nExec=nothing\n",
                false,
            ),
        ),
        (
            "no desktop entry",
            gates("1.0.0").without(&format!("share/applications/{ID}.desktop")),
        ),
        (
            "a notifyrc of another app",
            gates("1.0.0").file("share/knotifications6/plasma.notifyrc", b"x", false),
        ),
    ] {
        let (d, root) = dirs("names");
        let r = install_local(&d, &root, &builder.build());
        assert!(r.is_err(), "{what}: installed");
        assert!(
            !d.data.join("applications/org.kde.dolphin.desktop").exists(),
            "{what}"
        );
        assert!(install::list(&d).is_empty(), "{what}");
    }
}

#[test]
fn uninstall_removes_exactly_what_the_store_installed() {
    let (d, root) = dirs("uninstall");
    // Files of the user's, next to ours.
    fs::create_dir_all(d.data.join("applications")).unwrap();
    fs::write(d.data.join("applications/other.desktop"), b"other").unwrap();
    let app_data = d.data.join("telamon-gates");
    fs::create_dir_all(&app_data).unwrap();
    fs::write(app_data.join("conversation.md"), b"mine").unwrap();
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    install_local(&d, &root, &gates("0.2.0").build()).unwrap();
    let r = install::uninstall(&d, ID).unwrap();
    assert!(r.left.is_empty(), "{:?}", r.left);
    assert!(!d.app(ID).exists());
    assert!(!d.data.join(format!("applications/{ID}.desktop")).exists());
    assert!(
        !d.data
            .join(format!("icons/hicolor/scalable/apps/{ID}.svg"))
            .exists()
    );
    assert!(!d.data.join(format!("metainfo/{ID}.metainfo.xml")).exists());
    // The user's files and the app's own data are untouched.
    assert_eq!(
        fs::read(d.data.join("applications/other.desktop")).unwrap(),
        b"other"
    );
    assert_eq!(fs::read(app_data.join("conversation.md")).unwrap(), b"mine");
    assert!(install::list(&d).is_empty());
    // Not installed: refused.
    assert!(install::uninstall(&d, ID).is_err());
}

#[test]
fn uninstall_leaves_a_file_the_user_changed() {
    let (d, root) = dirs("uninstall-changed");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    let desktop = d.data.join(format!("applications/{ID}.desktop"));
    fs::write(
        &desktop,
        b"[Desktop Entry]\nType=Application\nName=Edited\nExec=true\n",
    )
    .unwrap();
    let r = install::uninstall(&d, ID).unwrap();
    assert_eq!(r.left.len(), 1);
    assert_eq!(r.left[0].0, desktop);
    assert!(desktop.is_file());
    assert!(!d.app(ID).exists());
}

#[test]
fn a_tampered_record_cannot_make_uninstall_remove_other_files() {
    let (d, root) = dirs("record");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    let victim = d.home.join("precious");
    fs::write(&victim, b"keep").unwrap();
    let rec_path = d.app(ID).join("install.json");
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&rec_path).unwrap()).unwrap();
    v["copied"] = serde_json::json!([
        {"to": "../../home/precious", "sha256": "0".repeat(64)},
        {"to": "/etc/hostname", "sha256": "0".repeat(64)}
    ]);
    fs::write(&rec_path, serde_json::to_vec(&v).unwrap()).unwrap();
    // A record that lists paths outside the allowed folders is not ours.
    assert!(install::read_record(&d, ID).is_none());
    assert!(install::uninstall(&d, ID).is_err());
    assert_eq!(fs::read(&victim).unwrap(), b"keep");
    // And a record for another id is not read as this one's.
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&rec_path).unwrap()).unwrap();
    v["copied"] = serde_json::json!([]);
    v["id"] = serde_json::json!("org.example.Other");
    fs::write(&rec_path, serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(install::read_record(&d, ID).is_none());
}

#[test]
fn a_folder_the_store_did_not_make_is_not_taken_over() {
    let (d, root) = dirs("foreign");
    fs::create_dir_all(d.app(ID).join("stuff")).unwrap();
    let e = install_local(&d, &root, &gates("0.1.0").build()).unwrap_err();
    assert!(e.contains("already exists"), "{e}");
    assert!(d.app(ID).join("stuff").is_dir());
}

#[test]
fn open_runs_the_current_program() {
    let (d, root) = dirs("open");
    let out = root.join("ran");
    let script = format!("#!/bin/sh\necho \"$0\" > {}\nexit 0\n", out.display());
    install_local(
        &d,
        &root,
        &gates("0.1.0")
            .file("bin/telamon-gates", script.as_bytes(), true)
            .build(),
    )
    .unwrap();
    install::launch(&d, ID, None).unwrap();
    let ran = fs::read_to_string(&out).unwrap();
    assert!(
        ran.trim().ends_with("telamon-gates") && ran.contains("current"),
        "{ran}"
    );
    assert!(install::launch(&d, "org.example.Nope", None).is_err());
}

#[test]
fn installed_apps_are_listed_by_name() {
    let (d, root) = dirs("list");
    install_local(
        &d,
        &root,
        &BundleBuilder::new("org.example.Zed", "Zed", "1.0.0").build(),
    )
    .unwrap();
    install_local(
        &d,
        &root,
        &BundleBuilder::new("org.example.alpha", "alpha", "1.0.0").build(),
    )
    .unwrap();
    let names: Vec<_> = install::list(&d).into_iter().map(|i| i.name).collect();
    assert_eq!(names, ["alpha", "Zed"]);
    // A folder that is not an app is skipped.
    fs::create_dir_all(d.apps().join("not-an-id")).unwrap();
    fs::create_dir_all(d.apps().join("org.example.empty")).unwrap();
    assert_eq!(install::list(&d).len(), 2);
}

// ---- the catalog, releases and downloads, against a fake GitHub ----

fn entry() -> Entry {
    Entry {
        id: ID.into(),
        repo: REPO.into(),
        channel: "releases".into(),
    }
}

fn world(name: &str) -> (Dirs, PathBuf, Cache, Fake) {
    let (d, root) = dirs(name);
    let cache = Cache::new(root.join("cache"));
    let fake = Fake::new();
    fake.catalog(&[(ID, REPO)]);
    (d, root, cache, fake)
}

const NOW: u64 = 1_800_000_000;

#[test]
fn a_connected_app_with_a_release_is_available() {
    let (d, _root, cache, fake) = world("avail");
    fake.publish(REPO, "v0.1.0", &gates("0.1.0").build());
    let r = check::check(&fake, &cache, &d, &host(), NOW, false);
    assert!(r.problems.is_empty(), "{:?}", r.problems);
    assert_eq!(r.apps.len(), 1);
    assert_eq!(r.apps[0].status, Status::Available);
    assert_eq!(r.apps[0].name, "Telamon Gates");
    assert_eq!(r.apps[0].candidate.as_ref().unwrap().tag, "v0.1.0");
}

#[test]
fn an_app_without_a_release_is_simply_not_listed() {
    let (d, _root, cache, fake) = world("norelease");
    let r = check::check(&fake, &cache, &d, &host(), NOW, false);
    assert!(r.apps.is_empty() && r.problems.is_empty(), "{r:?}");
    // A release without a bundle is the same.
    fake.set(
        &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        br#"{"tag_name":"v1","assets":[]}"#.to_vec(),
    );
    let r = check::check(&fake, &cache, &d, &host(), NOW, true);
    assert!(r.apps.is_empty() && r.problems.is_empty(), "{r:?}");
}

#[test]
fn a_newer_release_is_an_update_and_installs() {
    let (d, root, cache, fake) = world("updates");
    fake.publish(REPO, "v0.1.0", &gates("0.1.0").build());
    let h = host();
    let work = root.join("work");
    let r = check::check(&fake, &cache, &d, &h, NOW, false);
    let cand = r.apps[0].candidate.clone().unwrap();
    let mut seen = Vec::new();
    check::install_candidate(&fake, &d, &h, &cand, &work, &mut |a, b| seen.push((a, b))).unwrap();
    assert!(seen.last().is_some_and(|(a, b)| a == b && *a > 0));
    let rec = install::read_record(&d, ID).unwrap();
    assert_eq!(rec.origin, Origin::release(REPO, "v0.1.0"));
    assert!(
        fs::read_dir(&work).map(|r| r.count() == 0).unwrap_or(true),
        "the download was removed"
    );

    // Up to date.
    let r = check::check(&fake, &cache, &d, &h, NOW + 10, true);
    assert_eq!(r.apps[0].status, Status::UpToDate);

    // A new release.
    fake.publish(REPO, "v0.2.0", &gates("0.2.0").build());
    let r = check::check(&fake, &cache, &d, &h, NOW + 20, true);
    assert_eq!(r.apps[0].status, Status::Update);
    let cand = r.apps[0].candidate.clone().unwrap();
    let done = check::install_candidate(&fake, &d, &h, &cand, &work, &mut |_, _| {}).unwrap();
    assert_eq!(done.replaced.as_deref(), Some("0.1.0"));
    assert_eq!(
        fs::read_link(d.app(ID).join("current")).unwrap(),
        Path::new("0.2.0")
    );
    let r = check::check(&fake, &cache, &d, &h, NOW + 30, true);
    assert_eq!(r.apps[0].status, Status::UpToDate);
}

#[test]
fn the_cache_spares_github_until_it_expires_or_the_user_asks() {
    let (d, _root, cache, fake) = world("cache");
    fake.publish(REPO, "v0.1.0", &gates("0.1.0").build());
    check::check(&fake, &cache, &d, &host(), NOW, false);
    let first = fake.requests().len();
    assert_eq!(
        first,
        3,
        "catalog, release, manifest: {:?}",
        fake.requests()
    );
    check::check(&fake, &cache, &d, &host(), NOW + 60, false);
    assert_eq!(
        fake.requests().len(),
        first,
        "nothing asked within the cache time"
    );
    check::check(&fake, &cache, &d, &host(), NOW + TTL + 1, false);
    assert_eq!(fake.requests().len(), first + 3, "asked again once expired");
    check::check(&fake, &cache, &d, &host(), NOW + TTL + 2, true);
    assert_eq!(fake.requests().len(), first + 6, "force asks again");
}

#[test]
fn with_no_network_the_cache_is_used_and_it_is_said() {
    let (d, _root, cache, fake) = world("offline");
    fake.publish(REPO, "v0.1.0", &gates("0.1.0").build());
    check::check(&fake, &cache, &d, &host(), NOW, false);
    fake.fail(telamon_store_core::native::CATALOG_URL, NetError::TimedOut);
    fake.fail(
        &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        NetError::TimedOut,
    );
    let r = check::check(&fake, &cache, &d, &host(), NOW + 10 * TTL, false);
    assert!(r.stale);
    assert_eq!(r.apps.len(), 1);
    assert_eq!(r.apps[0].status, Status::Available);
    assert_eq!(r.checked_at, NOW);
    // With no cache and no network: no apps, one problem, no panic.
    let (d2, _r2, cache2, fake2) = world("offline-cold");
    fake2.fail(telamon_store_core::native::CATALOG_URL, NetError::TimedOut);
    let r = check::check(&fake2, &cache2, &d2, &host(), NOW, false);
    assert!(r.apps.is_empty() && r.problems.len() == 1 && r.stale);
}

#[test]
fn an_installed_app_stays_listed_when_github_is_unreachable() {
    let (d, root, cache, fake) = world("installed-offline");
    install_local(&d, &root, &gates("0.1.0").build()).unwrap();
    fake.fail(
        telamon_store_core::native::CATALOG_URL,
        NetError::Failed("no route".into()),
    );
    let r = check::check(&fake, &cache, &d, &host(), NOW, false);
    assert_eq!(r.apps.len(), 1);
    assert_eq!(r.apps[0].status, Status::UpToDate);
    assert!(r.apps[0].installed.is_some());
}

#[test]
fn a_release_for_another_app_or_repo_is_refused() {
    // The manifest names another app than the catalog entry.
    let (d, _root, cache, fake) = world("id-mismatch");
    let other = BundleBuilder::new("org.example.Other", "Other", "0.1.0").build();
    fake.publish(REPO, "v0.1.0", &other);
    let r = check::check(&fake, &cache, &d, &host(), NOW, false);
    assert!(r.apps.is_empty());
    assert_eq!(r.problems.len(), 1, "{:?}", r.problems);
    assert!(
        r.problems[0].text.contains("different app"),
        "{:?}",
        r.problems
    );

    // The API names assets of another repository.
    let (d, _root, cache, fake) = world("wrong-repo");
    fake.publish("Someone/else", "v0.1.0", &gates("0.1.0").build());
    let api = fake
        .get(
            "https://api.github.com/repos/Someone/else/releases/latest",
            "x",
            1 << 20,
        )
        .unwrap();
    use telamon_store_core::native::fetch::Fetcher;
    fake.set(
        &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        api,
    );
    let r = check::check(&fake, &cache, &d, &host(), NOW, false);
    assert!(r.apps.is_empty(), "{:?}", r.apps);

    // The catalog entry itself must name a known owner.
    let (d, _root, cache, fake) = world("stranger");
    fake.catalog(&[(ID, "Stranger/telamon-gates")]);
    fake.publish("Stranger/telamon-gates", "v0.1.0", &gates("0.1.0").build());
    let r = check::check(&fake, &cache, &d, &host(), NOW, false);
    assert!(r.apps.is_empty());
    assert!(
        fake.requests().iter().all(|u| !u.contains("Stranger")),
        "{:?}",
        fake.requests()
    );
}

#[test]
fn a_download_that_is_not_what_the_manifest_says_installs_nothing() {
    let h = host();
    // The archive served differs from the manifest's hash.
    let (d, root, cache, fake) = world("badsha");
    let good = gates("0.1.0").build();
    fake.publish(REPO, "v0.1.0", &good);
    let tampered = gates("0.1.0")
        .file(
            "share/net.eterneon.telamon.gates/data.txt",
            b"tampered",
            false,
        )
        .build();
    fake.set(
        &format!(
            "https://github.com/{REPO}/releases/download/v0.1.0/{}",
            good.outer.archive.as_ref().unwrap().name
        ),
        tampered.archive.clone(),
    );
    let r = check::check(&fake, &cache, &d, &h, NOW, false);
    let cand = r.apps[0].candidate.clone().unwrap();
    let e = check::install_candidate(&fake, &d, &h, &cand, &root.join("work"), &mut |_, _| {})
        .unwrap_err();
    assert!(e.0.contains("not what the release says"), "{e}");
    assert!(install::list(&d).is_empty());

    // More bytes than the manifest declared stop the download.
    let (d, root, cache, fake) = world("toolong");
    fake.publish(REPO, "v0.1.0", &good);
    let mut longer = good.archive.clone();
    longer.extend_from_slice(&[0u8; 100]);
    fake.set(
        &format!(
            "https://github.com/{REPO}/releases/download/v0.1.0/{}",
            good.outer.archive.as_ref().unwrap().name
        ),
        longer,
    );
    let r = check::check(&fake, &cache, &d, &h, NOW, false);
    let cand = r.apps[0].candidate.clone().unwrap();
    assert!(
        check::install_candidate(&fake, &d, &h, &cand, &root.join("work"), &mut |_, _| {}).is_err()
    );
    assert!(install::list(&d).is_empty());
}

#[test]
fn a_tag_that_does_not_match_the_bundle_version_is_refused() {
    let (d, _root, cache, fake) = world("tag");
    fake.publish(REPO, "v0.0.9", &gates("0.1.0").build());
    let r = check::check(&fake, &cache, &d, &host(), NOW, false);
    assert!(r.apps.is_empty());
    assert_eq!(r.problems.len(), 1);
}

#[test]
fn an_incompatible_release_is_listed_but_cannot_be_installed() {
    let (d, _root, cache, fake) = world("incompat");
    fake.publish(
        REPO,
        "v0.1.0",
        &gates("0.1.0")
            .edit_inner(|m| m.min_telamon_ui = "9.0.0".into())
            .edit_outer(|m| m.min_telamon_ui = "9.0.0".into())
            .build(),
    );
    let r = check::check(&fake, &cache, &d, &host(), NOW, false);
    assert!(
        matches!(&r.apps[0].status, Status::Incompatible(why) if why.contains("Telamon.Ui")),
        "{:?}",
        r.apps[0].status
    );
}

#[test]
fn a_local_bundle_is_looked_at_without_installing() {
    let (d, root) = dirs("local");
    let built = gates("0.1.0").build();
    let f = write_archive(&root, &built);
    let (m, sha, size) = check::inspect_local(&f, &root.join("work")).unwrap();
    assert_eq!((m.id.as_str(), m.version.as_str()), (ID, "0.1.0"));
    assert_eq!(sha, built.sha256);
    assert_eq!(size, built.archive.len() as u64);
    assert!(install::list(&d).is_empty());
    assert!(
        !root
            .join("work")
            .join(format!("inspect-{}", std::process::id()))
            .exists()
    );
    let _ = (Kind::Inner, entry());
}

#[test]
fn the_unpacker_sets_modes_from_the_manifest_not_the_archive() {
    let (d, root) = dirs("modes");
    // An archive that marks everything 0777/setuid in the tar.
    let built = gates("0.1.0").build();
    let f = write_archive(&root, &built);
    let stage = root.join("stage");
    fs::create_dir(&stage).unwrap();
    let inner = archive::unpack(&f, &stage, Some(&built.outer)).unwrap();
    for file in &inner.files {
        let mode = fs::metadata(stage.join(&file.path))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            mode,
            if file.executable { 0o755 } else { 0o644 },
            "{}",
            file.path
        );
    }
    let _: Manifest = inner;
    let _ = d;
}

/// The bundles the framework's tool made from real apps (Telamon Gates,
/// Telamon Text Editor): installed, listed, opened-checked and removed. Only
/// when TELAMON_STORE_TEST_BUNDLES names a folder holding `*.tar.zst` files.
#[test]
fn real_bundles_from_the_framework_tool_install() {
    let Some(dir) = std::env::var_os("TELAMON_STORE_TEST_BUNDLES") else {
        eprintln!("skipped: TELAMON_STORE_TEST_BUNDLES is not set");
        return;
    };
    let mut found = 0;
    for e in fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if !p.to_string_lossy().ends_with(".tar.zst") {
            continue;
        }
        found += 1;
        let (d, _root) = dirs("real");
        let h = Host {
            os_version: Some(44),
            telamon_ui: None,
            arch: "x86_64".into(),
        };
        let (m, _, _) = check::inspect_local(&p, &_root.join("work")).unwrap();
        // The release's own manifest, when it was kept beside the archive:
        // the archive must match it (hash, size) and say the same.
        let outer_path = PathBuf::from(p.to_string_lossy().replace(".tar.zst", ".manifest.json"));
        let outer = fs::read(&outer_path)
            .ok()
            .map(|b| Manifest::parse(&b, Kind::Outer).unwrap());
        if let Some(o) = &outer {
            let (sha, size) = archive::sha256_file(&p).unwrap();
            let a = o.archive.as_ref().unwrap();
            assert_eq!((a.sha256.as_str(), a.size), (sha.as_str(), size));
        }
        let done = install::install_bundle(
            &d,
            &p,
            &Options {
                expect_id: Some(&m.id),
                outer: outer.as_ref(),
                origin: Origin::local(),
                host: &h,
            },
        )
        .unwrap();
        let list = install::list(&d);
        assert_eq!(list.len(), 1);
        assert!(list[0].present, "{}", m.id);
        let desktop =
            fs::read_to_string(d.data.join(format!("applications/{}.desktop", m.id))).unwrap();
        assert!(desktop.contains("/current/bin/"), "{desktop}");
        eprintln!("installed {} {}", done.name, done.version);
        assert!(install::uninstall(&d, &m.id).unwrap().left.is_empty());
        assert!(
            files_under(&d.data)
                .iter()
                .all(|p| p == "telamon-apps" || p == "telamon-apps/.lock" || !p.contains(&m.id)),
            "{:?}",
            files_under(&d.data)
        );
    }
    assert!(found > 0, "no bundles in the folder");
}

/// The file a pull request edits to connect an app: every line must be one
/// the Store accepts, so a typo fails here and not silently on users' computers.
#[test]
fn the_repositorys_own_catalog_is_valid() {
    let text = include_str!("../../../catalog/native-apps.json");
    let c = telamon_store_core::native::catalog::Catalog::parse(text.as_bytes()).unwrap();
    assert!(
        c.skipped.is_empty(),
        "entries the Store would skip: {:?}",
        c.skipped
    );
    let mut ids: Vec<_> = c.apps.iter().map(|a| a.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), c.apps.len(), "an ID is listed twice");
    // The file is also what it should look like: the tooling reads it as JSON.
    let v: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["schema"], 1);
}
