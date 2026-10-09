//! Property tests of everything the native Telamon apps read from GitHub or
//! from a bundle: the catalog, the release JSON, both manifests, and the
//! desktop entry and D-Bus service rewrite. Bounded runs; `fuzz/` runs the
//! same checks guided by coverage.

mod harness;

use std::path::PathBuf;

use harness::{checks, strat};
use proptest::prelude::*;
use serde_json::{Value, json};
use telamon_store_core::native::manifest::{Kind, Manifest};

const RELEASE: &[u8] = include_bytes!("fixtures/native/github-release-latest.json");

proptest! {
    #![proptest_config(strat::config())]

    #[test]
    fn generated_manifests(v in prop_oneof![strat::manifest_value(true), strat::manifest_value(false)]) {
        checks::native_manifest(&serde_json::to_vec(&v).unwrap());
    }

    #[test]
    fn broken_outer_manifests(data in strat::json_variant(strat::valid_manifest_value(true), strat::JSON_DICT)) {
        checks::native_manifest(&data);
    }

    #[test]
    fn broken_inner_manifests(data in strat::json_variant(strat::valid_manifest_value(false), strat::JSON_DICT)) {
        checks::native_manifest(&data);
    }

    #[test]
    fn manifests_from_noise(data in strat::bytes(200)) {
        checks::native_manifest(&data);
    }

    #[test]
    fn paths_and_link_targets(path in strat::rel_path(), target in strat::link_target()) {
        checks::native_paths(&path, &target);
    }

    #[test]
    fn generated_catalogs(v in strat::catalog_value()) {
        checks::native_catalog(&serde_json::to_vec(&v).unwrap());
    }

    #[test]
    fn broken_catalogs(data in strat::json_variant(json!({"schema":1,"apps":[{"id":"net.eterneon.telamon.gates","repo":"EternalCoder454/telamon-gates","channel":"releases"}]}), strat::JSON_DICT)) {
        checks::native_catalog(&data);
    }

    #[test]
    fn catalogs_from_noise(data in strat::bytes(200)) {
        checks::native_catalog(&data);
    }

    #[test]
    fn generated_releases(v in strat::release_value(), repo in prop::sample::select(vec![
        "EternalCoder454/telamon-gates", "eternalcoder454/telamon-gates", "EternalCoder454/other",
    ])) {
        checks::github_release(&serde_json::to_vec(&v).unwrap(), repo);
    }

    #[test]
    fn broken_recorded_release(data in {
        let v: Value = serde_json::from_slice(RELEASE).unwrap();
        strat::json_variant(v, strat::JSON_DICT)
    }) {
        checks::github_release(&data, "EternalCoder454/atlasos-store");
        checks::github_release(&data, "EternalCoder454/telamon-gates");
    }

    #[test]
    fn releases_from_noise(data in strat::bytes(200)) {
        checks::github_release(&data, checks::REPO);
    }

    #[test]
    fn desktop_entries_are_rewritten_to_absolute_programs(
        src in prop_oneof![
            3 => strat::desktop_text(),
            2 => strat::mutated(
                b"[Desktop Entry]\nType=Application\nName=Gates\nExec=telamon-gates %U\nTryExec=telamon-gates\nPath=/tmp\nIcon=x\n\n[Desktop Action new]\nName=New\nExec=telamon-gates --new\n".to_vec(),
                strat::DESKTOP_DICT, 8),
        ],
        prefix in strat::prefix(),
        programs in strat::programs(),
    ) {
        checks::native_desktop(&src, &prefix, &programs);
    }

    #[test]
    fn dbus_services_are_rewritten_to_absolute_programs(
        src in prop_oneof![
            3 => strat::dbus_text(),
            1 => strat::mutated(b"[D-BUS Service]\nName=net.eterneon.telamon.gates\nExec=telamon-gates --gapplication-service\n".to_vec(), strat::DESKTOP_DICT, 6),
        ],
        prefix in strat::prefix(),
        programs in strat::programs(),
    ) {
        checks::native_dbus(&src, &prefix, &programs);
    }

    #[test]
    fn a_plan_exports_only_files_that_carry_the_app_id(
        desktop in strat::desktop_text(),
        dbus in strat::dbus_text(),
        extra in prop::collection::vec((strat::rel_path(), strat::bytes(64), any::<bool>()), 0..4),
        keep in prop::collection::vec(any::<bool>(), 8),
        links in prop::collection::vec((strat::rel_path(), strat::link_target()), 0..3),
        prefix in strat::prefix(),
    ) {
        let id = checks::ID;
        let mut files: Vec<(String, Vec<u8>, bool)> = vec![
            ("bin/telamon-gates".into(), b"#!/bin/sh\n".to_vec(), true),
            ("bin/other".into(), b"x".to_vec(), true),
            (format!("share/applications/{id}.desktop"), desktop, false),
            (format!("share/icons/hicolor/scalable/apps/{id}.svg"), b"<svg/>".to_vec(), false),
            (format!("share/icons/hicolor/48x48/apps/{id}-big.png"), b"png".to_vec(), false),
            (format!("share/metainfo/{id}.metainfo.xml"), b"<component/>".to_vec(), false),
            (format!("share/dbus-1/services/{id}.service"), dbus, false),
            ("share/knotifications6/telamon-gates.notifyrc".into(), b"[Global]\n".to_vec(), false),
        ];
        let mut i = 0;
        files.retain(|_| { i += 1; keep[i - 1] || i <= 3 });
        files.extend(extra);
        // A duplicate path would be refused by the manifest, not by `plan`.
        let mut seen = std::collections::BTreeSet::new();
        files.retain(|(p, _, _)| seen.insert(p.clone()));
        let links: Vec<_> = links.into_iter().filter(|(p, _)| seen.insert(p.clone())).collect();
        checks::native_plan(&files, &links, &prefix);
    }
}

#[test]
fn the_recorded_release_and_a_good_manifest_are_accepted() {
    let r = telamon_store_core::native::github::Release::parse(
        RELEASE,
        "EternalCoder454/atlasos-store",
    );
    assert!(r.is_ok(), "{r:?}");
    checks::github_release(RELEASE, "EternalCoder454/atlasos-store");
    for outer in [true, false] {
        let v = strat::valid_manifest_value(outer);
        let kind = if outer { Kind::Outer } else { Kind::Inner };
        let m = Manifest::parse(&serde_json::to_vec(&v).unwrap(), kind).expect("a good manifest");
        checks::check_manifest(&m, kind);
    }
}

#[test]
fn a_prefix_with_spaces_and_quotes_is_one_argument() {
    let prefix = PathBuf::from("/home/my user/it's \"a\" $HOME/50% off/.local/share/x/current");
    let programs = ["telamon-gates".to_string()].into();
    checks::native_desktop(
        b"[Desktop Entry]\nType=Application\nName=G\nExec=telamon-gates %U\n",
        &prefix,
        &programs,
    );
    checks::native_dbus(
        b"[D-BUS Service]\nName=net.eterneon.telamon.gates\nExec=telamon-gates\n",
        &prefix,
        &programs,
    );
}

/// FINDING (low): the prefix is the user's data folder, not the bundle's, but
/// a folder name with a control character that `exec_arg` leaves unquoted (CR,
/// VT, FF, ...) is written as a space by `escape_value`, which splits the
/// program's path into two `Exec` arguments. Expected: the rewrite refuses
/// such a prefix (an error), or quotes it, so the first word is the path.
/// Ignored until the fix (in `native/desktop.rs` `exec_value`, or in
/// `appimage/install.rs` `exec_arg`) is merged; run with `--ignored`.
#[test]
#[ignore = "known gap: a control character in the prefix splits the Exec path"]
fn a_prefix_with_a_control_character_is_refused_or_kept_whole() {
    let programs: std::collections::BTreeSet<String> = ["telamon-gates".to_string()].into();
    for c in ['\r', '\x0b', '\x0c', '\x7f', '\u{85}'] {
        let prefix = PathBuf::from(format!("/home/u{c}x/.local/share/telamon-apps/x/current"));
        let r = telamon_store_core::native::desktop::rewrite_desktop(
            b"[Desktop Entry]\nType=Application\nName=G\nExec=telamon-gates\n",
            checks::ID,
            "1.0.0",
            &prefix,
            &programs,
        );
        if let Ok((out, _)) = r {
            let kf =
                telamon_store_core::keyfile::KeyFile::parse(&out, &Default::default()).unwrap();
            let value = kf.string("Desktop Entry", "Exec").unwrap().unwrap();
            let words = telamon_store_core::appimage::install::split_exec(&value).unwrap();
            assert_eq!(
                words[0],
                prefix.join("bin/telamon-gates").to_string_lossy(),
                "{c:?} changed the program's path"
            );
        }
    }
}
