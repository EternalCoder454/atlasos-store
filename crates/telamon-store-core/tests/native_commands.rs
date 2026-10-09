//! A native app's commands on `PATH` (`~/.local/bin/<name>`): made on install,
//! kept through an update, removed on uninstall, and never over anything that
//! is not the Store's link for that app. Scratch trees only: nothing here
//! touches the real home folder.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use telamon_store_core::native::fake::{Built, BundleBuilder};
use telamon_store_core::native::install::{self, Dirs, Options, Origin};
use telamon_store_core::native::manifest::Host;
use telamon_store_core::native::version::Version;

const ID: &str = "net.eterneon.telamon.gates";
const CMD: &str = "telamon-gates";

fn scratch(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "native-cmd-{name}-{}-{}",
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
    // A stand-in for /usr/bin: what is there must not be shadowed.
    let sys = root.join("usr-bin");
    fs::create_dir_all(&sys).unwrap();
    fs::write(sys.join("sudo"), b"#!/bin/sh\n").unwrap();
    (
        Dirs {
            data,
            home,
            system: Vec::new(),
            path: vec![sys],
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
    BundleBuilder::new(ID, "Telamon Gates", version).exe(CMD)
}

/// Gates with more programs in `bin/`, all listed as commands.
fn gates_with(version: &str, commands: &[&'static str]) -> BundleBuilder {
    let mut b = gates(version);
    for c in commands {
        if *c != CMD {
            b = b.file(
                &format!("bin/{c}"),
                format!("#!/bin/sh\n# {c} {version}\n").as_bytes(),
                true,
            );
        }
    }
    let list: Vec<String> = commands.iter().map(|c| c.to_string()).collect();
    b.edit_inner(move |m| m.commands = Some(list.clone()))
}

fn install(d: &Dirs, root: &Path, built: &Built) -> install::Done {
    try_install(d, root, built).unwrap()
}

fn try_install(d: &Dirs, root: &Path, built: &Built) -> Result<install::Done, String> {
    let p = root.join(format!("bundle-{}.tar.zst", &built.sha256[..8]));
    fs::write(&p, &built.archive).unwrap();
    let h = host();
    install::install_bundle(
        d,
        &p,
        &Options {
            expect_id: None,
            outer: None,
            origin: Origin::local(),
            host: &h,
        },
    )
    .map_err(|e| e.0)
}

fn link_of(d: &Dirs, name: &str) -> Option<PathBuf> {
    fs::read_link(d.bin().join(name)).ok()
}

fn ours(d: &Dirs, name: &str) -> PathBuf {
    d.app(ID).join("current/bin").join(name)
}

#[test]
fn an_install_puts_the_program_on_path() {
    let (d, root) = dirs("install");
    assert!(!d.bin().exists());
    let done = install(&d, &root, &gates("0.1.0").build());
    assert_eq!(done.commands, vec![CMD.to_string()]);
    // `~/.local/bin` is made, 0755, and the link goes through `current`.
    let mode = fs::metadata(d.bin()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
    assert_eq!(link_of(&d, CMD), Some(ours(&d, CMD)));
    assert!(
        String::from_utf8(fs::read(d.bin().join(CMD)).unwrap())
            .unwrap()
            .contains("0.1.0")
    );
    let rec = install::read_record(&d, ID).unwrap();
    assert_eq!(rec.commands, Some(vec![CMD.to_string()]));
}

#[test]
fn an_update_keeps_the_link_and_drops_what_the_new_version_does_not_have() {
    let (d, root) = dirs("update");
    let done = install(
        &d,
        &root,
        &gates_with("0.1.0", &[CMD, "telamon-gates-cli"]).build(),
    );
    assert_eq!(done.commands.len(), 2);
    assert_eq!(
        link_of(&d, "telamon-gates-cli"),
        Some(ours(&d, "telamon-gates-cli"))
    );
    let done = install(&d, &root, &gates_with("0.2.0", &[CMD]).build());
    assert_eq!(done.commands, vec![CMD.to_string()]);
    // The same link, now leading to the new version.
    assert_eq!(link_of(&d, CMD), Some(ours(&d, CMD)));
    assert!(
        String::from_utf8(fs::read(d.bin().join(CMD)).unwrap())
            .unwrap()
            .contains("0.2.0")
    );
    // The command the new version dropped is gone.
    assert!(
        d.bin()
            .join("telamon-gates-cli")
            .symlink_metadata()
            .is_err()
    );
}

#[test]
fn uninstall_removes_only_the_stores_links() {
    let (d, root) = dirs("uninstall");
    install(
        &d,
        &root,
        &gates_with("0.1.0", &[CMD, "telamon-gates-cli"]).build(),
    );
    // The user put their own link in place of one of ours, and has files of
    // their own in the folder.
    fs::remove_file(d.bin().join("telamon-gates-cli")).unwrap();
    symlink("/usr/bin/true", d.bin().join("telamon-gates-cli")).unwrap();
    fs::write(d.bin().join("mine"), b"mine").unwrap();
    install::uninstall(&d, ID).unwrap();
    assert!(d.bin().join(CMD).symlink_metadata().is_err());
    assert_eq!(
        link_of(&d, "telamon-gates-cli"),
        Some(PathBuf::from("/usr/bin/true"))
    );
    assert_eq!(fs::read(d.bin().join("mine")).unwrap(), b"mine");
}

#[test]
fn a_file_or_link_that_is_not_ours_is_never_replaced() {
    let (d, root) = dirs("collision");
    fs::create_dir_all(d.bin()).unwrap();
    fs::write(d.bin().join(CMD), b"the user's own").unwrap();
    // A link to another app's place is that app's.
    let other = d
        .data
        .join("telamon-apps/net.eterneon.telamon.other/current/bin/telamon-gates-cli");
    symlink(&other, d.bin().join("telamon-gates-cli")).unwrap();
    let done = install(
        &d,
        &root,
        &gates_with("0.1.0", &[CMD, "telamon-gates-cli"]).build(),
    );
    // The install goes through; the commands are skipped.
    assert!(done.commands.is_empty(), "{:?}", done.commands);
    assert_eq!(fs::read(d.bin().join(CMD)).unwrap(), b"the user's own");
    assert_eq!(link_of(&d, "telamon-gates-cli"), Some(other.clone()));
    // An update and an uninstall leave them alone too.
    install(
        &d,
        &root,
        &gates_with("0.2.0", &[CMD, "telamon-gates-cli"]).build(),
    );
    install::uninstall(&d, ID).unwrap();
    assert_eq!(fs::read(d.bin().join(CMD)).unwrap(), b"the user's own");
    assert_eq!(link_of(&d, "telamon-gates-cli"), Some(other));
}

#[test]
fn a_link_of_ours_from_a_moved_data_folder_is_pointed_at_the_new_place() {
    let (d, root) = dirs("moved");
    fs::create_dir_all(d.bin()).unwrap();
    let old = format!("/old/data/telamon-apps/{ID}/current/bin/{CMD}");
    symlink(&old, d.bin().join(CMD)).unwrap();
    let done = install(&d, &root, &gates("0.1.0").build());
    assert_eq!(done.commands, vec![CMD.to_string()]);
    assert_eq!(link_of(&d, CMD), Some(ours(&d, CMD)));
}

#[test]
fn a_command_never_shadows_the_system_or_takes_a_name_not_its_own() {
    let (mut d, root) = dirs("shadow");
    // `telamon-gates` is already a command elsewhere on PATH.
    let other = root.join("other-bin");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join(CMD), b"#!/bin/sh\n").unwrap();
    d.path.push(other);
    let done = install(&d, &root, &gates("0.1.0").build());
    assert!(done.commands.is_empty(), "{:?}", done.commands);
    assert!(d.bin().join(CMD).symlink_metadata().is_err());
    // Recorded all the same: it is the app's name, only taken here.
    let rec = install::read_record(&d, ID).unwrap();
    assert_eq!(rec.commands, Some(vec![CMD.to_string()]));

    // A desktop program that is not named after the app is not linked...
    let (d, root) = dirs("not-its-own");
    let done = install(
        &d,
        &root,
        &BundleBuilder::new(ID, "Telamon Gates", "0.1.0")
            .exe("sudo")
            .build(),
    );
    assert!(done.commands.is_empty());
    assert!(d.bin().join("sudo").symlink_metadata().is_err());
    assert_eq!(install::read_record(&d, ID).unwrap().commands, Some(vec![]));
    // ...and a manifest that lists one is refused.
    let built = gates("0.1.0")
        .file("bin/sudo", b"#!/bin/sh\n", true)
        .edit_inner(|m| m.commands = Some(vec![CMD.into(), "sudo".into()]))
        .build();
    let (d, root) = dirs("listed");
    assert!(try_install(&d, &root, &built).is_err());
    assert!(!d.bin().exists());
}

#[test]
fn with_local_bin_on_path_an_update_keeps_the_link() {
    let (mut d, root) = dirs("on-path");
    // As in a real session: `~/.local/bin` is one of PATH's folders.
    fs::create_dir_all(d.bin()).unwrap();
    d.path.insert(0, d.bin());
    install(&d, &root, &gates("0.1.0").build());
    let done = install(&d, &root, &gates("0.2.0").build());
    assert_eq!(done.commands, vec![CMD.to_string()]);
    assert_eq!(link_of(&d, CMD), Some(ours(&d, CMD)));
}

#[test]
fn a_record_with_a_command_not_its_own_is_not_believed() {
    let (d, root) = dirs("tampered");
    install(&d, &root, &gates("0.1.0").build());
    let rec_path = d.app(ID).join("install.json");
    let good: serde_json::Value = serde_json::from_slice(&fs::read(&rec_path).unwrap()).unwrap();
    fs::write(d.bin().join("sudo"), b"the user's").unwrap();
    for bad in [
        serde_json::json!(["sudo"]),
        serde_json::json!(["../x"]),
        serde_json::json!(vec!["gates"; 17]),
    ] {
        let mut v = good.clone();
        v["commands"] = bad;
        fs::write(&rec_path, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(install::read_record(&d, ID).is_none());
        assert!(install::uninstall(&d, ID).is_err());
    }
    assert_eq!(fs::read(d.bin().join("sudo")).unwrap(), b"the user's");
}

#[test]
fn an_update_from_an_older_record_to_no_commands_removes_the_link() {
    let (d, root) = dirs("older-update");
    install(&d, &root, &gates("0.1.0").build());
    let rec_path = d.app(ID).join("install.json");
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&rec_path).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("commands");
    fs::write(&rec_path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let done = install(&d, &root, &gates_with("0.2.0", &[]).build());
    assert!(done.commands.is_empty());
    assert!(d.bin().join(CMD).symlink_metadata().is_err());
}

#[test]
fn an_empty_list_puts_nothing_on_path() {
    let (d, root) = dirs("none");
    let done = install(&d, &root, &gates_with("0.1.0", &[]).build());
    assert!(done.commands.is_empty());
    assert!(d.bin().join(CMD).symlink_metadata().is_err());
}

#[test]
fn an_app_installed_before_commands_gets_its_program_on_path_once() {
    let (d, root) = dirs("older");
    install(&d, &root, &gates("0.1.0").build());
    // As a Store before this one left it: no link, no `commands`.
    fs::remove_file(d.bin().join(CMD)).unwrap();
    let rec_path = d.app(ID).join("install.json");
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&rec_path).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("commands");
    fs::write(&rec_path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    assert_eq!(install::read_record(&d, ID).unwrap().commands, None);

    install::link_older_installs(&d);
    assert_eq!(link_of(&d, CMD), Some(ours(&d, CMD)));
    assert_eq!(
        install::read_record(&d, ID).unwrap().commands,
        Some(vec![CMD.to_string()])
    );
    // Once: a link the user removes afterwards stays removed.
    fs::remove_file(d.bin().join(CMD)).unwrap();
    install::link_older_installs(&d);
    assert!(d.bin().join(CMD).symlink_metadata().is_err());
}

#[test]
fn an_older_install_is_tried_again_while_local_bin_cannot_be_used() {
    let (d, root) = dirs("older-blocked");
    install(&d, &root, &gates("0.1.0").build());
    let rec_path = d.app(ID).join("install.json");
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&rec_path).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("commands");
    fs::write(&rec_path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    // `~/.local/bin` is a file: nothing can be linked, nothing is marked.
    fs::remove_dir_all(d.bin()).unwrap();
    fs::write(d.bin(), b"not a folder").unwrap();
    install::link_older_installs(&d);
    assert_eq!(install::read_record(&d, ID).unwrap().commands, None);
    // Once it can be used, the program is linked.
    fs::remove_file(d.bin()).unwrap();
    install::link_older_installs(&d);
    assert_eq!(link_of(&d, CMD), Some(ours(&d, CMD)));
    assert_eq!(
        install::read_record(&d, ID).unwrap().commands,
        Some(vec![CMD.to_string()])
    );
}
