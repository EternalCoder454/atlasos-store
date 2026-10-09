//! Property tests of the key file reader and of the `.flatpakref` and
//! `.flatpakrepo` parsers on top of it: never a panic, the limits hold, what
//! is accepted is fully checked, and writing it out and reading it again
//! gives the same value. Bounded runs; `fuzz/` runs the same checks guided by
//! coverage.

mod harness;

use harness::{checks, strat};
use proptest::prelude::*;
use telamon_store_core::flatpakref;
use telamon_store_core::keyfile::Limits;

const DESKTOP: &[u8] = b"[Desktop Entry]\nType=Application\nName=Gates\nName[de]=Tor\nExec=telamon-gates %U\nTryExec=telamon-gates\nIcon=x\nCategories=Utility;\n\n[Desktop Action new]\nName=New\nExec=telamon-gates --new\n";
const METADATA: &[u8] = include_bytes!("fixtures/permissions/firefox.metadata");

proptest! {
    #![proptest_config(strat::config())]

    #[test]
    fn generated_key_files(data in strat::keyfile_text(), limits in strat::keyfile_limits()) {
        checks::keyfile(&data, &limits);
    }

    #[test]
    fn mutated_key_files(data in prop_oneof![
        strat::mutated(DESKTOP.to_vec(), strat::KEYFILE_DICT, 8),
        strat::mutated(METADATA.to_vec(), strat::KEYFILE_DICT, 8),
        strat::mutated(strat::FLATPAKREF.to_vec(), strat::KEYFILE_DICT, 8),
    ], limits in strat::keyfile_limits()) {
        checks::keyfile(&data, &limits);
    }

    #[test]
    fn key_files_from_noise(data in strat::bytes(300)) {
        checks::keyfile(&data, &Limits::default());
    }

    #[test]
    fn generated_flatpakrefs(data in prop_oneof![strat::flatpak_file(false), strat::flatpak_file(true)]) {
        checks::flatpakref(&data);
    }

    #[test]
    fn mutated_flatpakrefs(data in prop_oneof![
        strat::mutated(strat::FLATPAKREF.to_vec(), strat::FLATPAK_DICT, 8),
        strat::mutated(strat::FLATPAKREPO.to_vec(), strat::FLATPAK_DICT, 8),
    ]) {
        checks::flatpakref(&data);
    }

    #[test]
    fn flatpakrefs_from_noise(data in strat::bytes(400)) {
        checks::flatpakref(&data);
    }
}

#[test]
fn the_fixtures_are_accepted_and_round_trip() {
    let r = flatpakref::parse_flatpakref(strat::FLATPAKREF).expect("the fixture is a good ref");
    checks::flatpakref(strat::FLATPAKREF);
    assert_eq!(r.name, "org.test.Hello");
    flatpakref::parse_flatpakrepo(strat::FLATPAKREPO).expect("the fixture is a good repo");
    checks::flatpakref(strat::FLATPAKREPO);
}

#[test]
fn a_file_over_the_cap_is_refused_whatever_it_holds() {
    let mut big = strat::FLATPAKREF.to_vec();
    big.extend(std::iter::repeat_n(b'\n', flatpakref::MAX_FILE_BYTES));
    assert!(flatpakref::parse_flatpakref(&big).is_err());
    assert!(flatpakref::parse_flatpakrepo(&big).is_err());
    checks::flatpakref(&big);
}
