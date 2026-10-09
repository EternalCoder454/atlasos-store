//! Property tests of the text, URL and ID checks every untrusted string goes
//! through: `text::clean`, `launch::{https_url, app_id, internal_path,
//! parse}`, `net::redirect_target`, `native::version::Version`. Bounded runs
//! (`PROPTEST_CASES`, default 256); the same invariants are fuzzed by `fuzz/`.

mod harness;

use harness::{checks, strat};
use proptest::prelude::*;

proptest! {
    #![proptest_config(strat::config())]

    #[test]
    fn clean_removes_what_hides_and_is_idempotent(s in strat::nasty_string(), max in 0usize..80) {
        checks::text_clean(&s, max);
    }

    #[test]
    fn clean_any_string(s in any::<String>(), max in 0usize..400) {
        checks::text_clean(&s, max);
    }

    #[test]
    fn validators_hold_their_promises(s in prop_oneof![strat::nasty_string(), strat::url(), strat::app_id()]) {
        checks::text_validators(&s);
    }

    #[test]
    fn https_urls_are_one_plain_form(s in strat::url()) {
        checks::https_url(&s);
    }

    #[test]
    fn https_urls_from_noise(s in "\\PC{0,60}") {
        checks::https_url(&s);
    }

    #[test]
    fn mutated_urls(s in strat::mutated_text(
        "https://dl.example.org/a/b/../c%2e%2e/d?x=1#f",
        &[b"/..", b"%2e", b"%2E%2e", b"@", b":443", b"\\", b"HTTPS", b"\xE2\x80\xAE", b"/./", b"//"],
        6,
    )) {
        if let Ok(s) = String::from_utf8(s) {
            checks::https_url(&s);
        }
    }

    #[test]
    fn redirects_stay_on_https(base in strat::good_url(), loc in strat::location()) {
        checks::redirect(&base, &loc);
    }

    #[test]
    fn redirects_from_any_base(base in strat::url(), loc in strat::location()) {
        checks::redirect(&base, &loc);
    }

    #[test]
    fn app_ids(s in strat::app_id()) {
        checks::app_id(&s);
    }

    #[test]
    fn internal_paths(s in prop_oneof![
        3 => "/[a-z./]{0,24}",
        2 => strat::plain_string(),
        1 => "(/[a-z]{1,4}){1,4}(\\.AppImage)?",
        1 => "/[a-z]{1,3}/(\\.|\\.\\.)/[a-z]{1,3}",
    ]) {
        checks::internal_path(&s);
    }

    #[test]
    fn search_text(s in strat::nasty_string()) {
        checks::search_text(&s);
    }

    #[test]
    fn launch_command_lines(args in prop::collection::vec(prop_oneof![
        3 => strat::url(),
        2 => strat::app_id(),
        2 => strat::plain_string(),
        2 => prop::sample::select(vec![
            "--", "--page=home", "--page", "installed", "--app", "--remove", "--search", "--install-bundle",
            "--appimage-install", "--appimage-check", "--url", "flatpak+https://dl.example.org/x.flatpakref",
            "flatpak:org.test.Hello", "appstream:org.test.Hello", "appstream://org.test.Hello",
            "/home/u/x.flatpakref", "/home/u/../x.flatpakref", "x.flatpakrepo", "/a/b.AppImage",
            "/a/b.tar.zst", "/a/b.rpm", "/a/b.flatpak", "file:///a/b.flatpakref", "--page=\u{202E}",
        ]).prop_map(str::to_string),
    ], 0..8)) {
        checks::launch_args(&args);
    }

    #[test]
    fn versions_order_and_print(a in strat::version(), b in strat::version()) {
        checks::version(&a, &b);
    }
}

/// A base the fetcher never has (it only redirects from a URL it was given
/// and `https_url` accepted), but a short or odd one must refuse, not panic.
#[test]
fn a_redirect_from_a_malformed_base_is_refused_not_a_panic() {
    for base in [
        "",
        "x",
        "https:",
        "https://",
        "http://a",
        "é",
        "https://é",
        "\u{1F600}\u{1F600}",
    ] {
        for location in ["/x", "/", "https://example.org/", "x", "//evil.com/"] {
            let r = telamon_store_core::net::redirect_target(base, location);
            if let Some(t) = r {
                // Only an absolute https location can pass from such a base.
                assert!(t.starts_with("https://"), "{base:?} {location:?} -> {t:?}");
                assert!(!location.starts_with('/'), "{base:?} {location:?} -> {t:?}");
            }
        }
    }
}

/// Found by the `urls` fuzz target: the cut at 200 characters could fall on
/// the space between two words and leave it at the end of the text.
#[test]
fn search_text_never_ends_in_a_space_after_the_cut() {
    let text = "a ".repeat(150);
    let found = telamon_store_core::launch::search_text(&text).unwrap();
    assert!(found.chars().count() <= 200);
    assert_eq!(found.trim_end(), found, "{found:?}");
    assert_eq!(telamon_store_core::launch::search_text("  \t  "), None);
}
