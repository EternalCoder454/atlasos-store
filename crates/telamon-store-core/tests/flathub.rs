//! Flathub's curated lists: the answers are parsed and checked, cached,
//! expired, used stale, and matched against the local catalog. Nothing here
//! uses the network: the fetcher is a closure, the clock a number. The JSON
//! fixtures in `fixtures/flathub/` are real answers of
//! `flathub.org/api/v2` (recorded 2026-10-07), trimmed to a few hits and
//! fields.

use std::cell::{Cell, RefCell};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use telamon_store_core::appstream::{ParseOptions, parse as parse_appstream};
use telamon_store_core::catalog::{CatalogSource, Category, Library};
use telamon_store_core::flathub::{
    self, Cached, List, MAX_BODY, MAX_IDS, ParseError, RETRY_AFTER, RefreshError, RetryGate,
};
use telamon_store_core::flatpak::Scope;
use telamon_store_core::net::NetError;

const POPULAR: &str = include_str!("fixtures/flathub/popular.json");
const RECENT: &str = include_str!("fixtures/flathub/recently-updated.json");
const GAMES: &str = include_str!("fixtures/flathub/category-game.json");
const WEEK: &str = include_str!("fixtures/flathub/apps-of-the-week.json");
const ALPHA: &str = include_str!("fixtures/catalog-alpha.xml");

/// 2026-10-07 00:00:00 UTC.
const NOW: u64 = 1_791_331_200;
const DAY: u64 = 24 * 3600;

fn scratch(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "flathub-{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A cache folder as the Store makes it: `<scratch>/telamon-store/flathub`.
fn cache(name: &str) -> PathBuf {
    flathub::cache_dir(&scratch(name).join("telamon-store"))
}

fn library() -> Library {
    let opts = ParseOptions {
        origin: "alpha".into(),
        ..ParseOptions::default()
    };
    let catalog = parse_appstream(ALPHA.as_bytes(), &opts).expect("the fixture parses");
    let source = CatalogSource {
        scope: Scope::User,
        remote: "alpha".into(),
        title: "alpha".into(),
        priority: 0,
        dir: None,
        commit: None,
        updated: None,
    };
    Library::new(vec![(source, catalog)])
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

fn hits(app_ids: &[&str]) -> Vec<u8> {
    let list: Vec<String> = app_ids
        .iter()
        .map(|id| format!(r#"{{"app_id":{}}}"#, serde_json::to_string(id).unwrap()))
        .collect();
    format!(r#"{{"hits":[{}]}}"#, list.join(",")).into_bytes()
}

// ---- requests

#[test]
fn urls_name_a_list_and_nothing_about_the_user() {
    let all = [
        List::Popular,
        List::RecentlyUpdated,
        List::Picks,
        List::Category(Category::Games),
    ];
    for list in all {
        let url = list.url(NOW);
        assert!(url.starts_with("https://flathub.org/api/v2/"), "{url}");
        assert!(
            telamon_store_core::launch::https_url(&url).is_some(),
            "{url}"
        );
        assert!(!url.contains('@') && !url.contains('#'), "{url}");
        // The only query is the page size.
        if let Some((_, query)) = url.split_once('?') {
            assert!(
                query
                    .split('&')
                    .all(|kv| kv.starts_with("page=") || kv.starts_with("per_page=")),
                "{url}"
            );
        }
    }
    assert_eq!(
        List::Popular.url(NOW),
        "https://flathub.org/api/v2/collection/popular?page=1&per_page=48"
    );
    assert_eq!(
        List::RecentlyUpdated.url(NOW),
        "https://flathub.org/api/v2/collection/recently-updated?page=1&per_page=24"
    );
    assert_eq!(
        List::Picks.url(NOW),
        "https://flathub.org/api/v2/app-picks/apps-of-the-week/2026-10-07"
    );
    assert_eq!(
        List::Category(Category::Games).url(NOW),
        "https://flathub.org/api/v2/collection/category/game?page=1&per_page=40"
    );
}

#[test]
fn dates_are_utc_calendar_days() {
    for (secs, date) in [
        (0, "1970-01-01"),
        (86_399, "1970-01-01"),
        (86_400, "1970-01-02"),
        (951_782_400, "2000-02-29"),
        (1_709_164_800, "2024-02-29"),
        (NOW, "2026-10-07"),
        (NOW - 1, "2026-10-06"),
        (4_102_444_799, "2099-12-31"),
    ] {
        assert_eq!(flathub::utc_date(secs), date, "{secs}");
    }
}

#[test]
fn every_category_maps_to_a_name_the_api_knows() {
    // `MainCategory` of the API's OpenAPI document (2026-10-07).
    const API: [&str; 11] = [
        "audiovideo",
        "development",
        "education",
        "healthfitness",
        "game",
        "graphics",
        "network",
        "office",
        "science",
        "system",
        "utility",
    ];
    let mut seen = Vec::new();
    for c in Category::ALL {
        let name = flathub::api_category(c);
        assert!(API.contains(&name), "{c:?} -> {name}");
        assert!(!seen.contains(&name), "{name} twice");
        seen.push(name);
        // A category page asks for its own list.
        assert!(List::Category(c).url(NOW).contains(&format!("/{name}?")));
    }
    // Every list has its own cache file.
    let mut files: Vec<String> = Category::ALL
        .iter()
        .map(|c| List::Category(*c).name())
        .collect();
    files.extend([List::Popular, List::RecentlyUpdated, List::Picks].map(List::name));
    let count = files.len();
    files.sort();
    files.dedup();
    assert_eq!(files.len(), count);
    assert!(
        files
            .iter()
            .all(|f| f.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'))
    );
}

#[test]
fn lists_expire_after_their_time() {
    let fetched = Cached {
        ids: vec![],
        fetched_at: NOW,
    };
    // Popular, picks and categories 24 h; recently updated 6 h.
    for list in [List::Popular, List::Picks, List::Category(Category::Office)] {
        assert!(fetched.is_fresh(list, NOW), "{list:?}");
        assert!(fetched.is_fresh(list, NOW + DAY - 1), "{list:?}");
        assert!(!fetched.is_fresh(list, NOW + DAY), "{list:?}");
    }
    assert!(fetched.is_fresh(List::RecentlyUpdated, NOW + 6 * 3600 - 1));
    assert!(!fetched.is_fresh(List::RecentlyUpdated, NOW + 6 * 3600));
    // A time far ahead of the clock (it was set back) is not fresh; a few
    // minutes of difference between clocks is.
    assert!(fetched.is_fresh(List::Popular, NOW - 60));
    assert!(!fetched.is_fresh(List::Popular, NOW - 3600));
}

// ---- parsing

#[test]
fn the_recorded_answers_parse() {
    let popular = flathub::parse(List::Popular, POPULAR.as_bytes()).unwrap();
    assert_eq!(popular.len(), 10);
    // The dotted app_id, not the underscored `id`.
    assert_eq!(popular[0], "org.mozilla.firefox");
    assert!(popular.iter().all(|id| id.contains('.')));

    let recent = flathub::parse(List::RecentlyUpdated, RECENT.as_bytes()).unwrap();
    assert_eq!(recent.len(), 10);

    let games = flathub::parse(List::Category(Category::Games), GAMES.as_bytes()).unwrap();
    assert_eq!(games.len(), 10);
    assert_eq!(games[0], "org.vinegarhq.Sober");

    let week = flathub::parse(List::Picks, WEEK.as_bytes()).unwrap();
    assert_eq!(week.len(), 5);
    assert_eq!(week[0], "page.codeberg.foolish.Flipbook");
}

#[test]
fn the_editors_picks_keep_the_editors_order() {
    let body = br#"{"apps":[
        {"app_id":"org.c.Third","position":3,"isFullscreen":false},
        {"app_id":"org.a.First","position":1,"isFullscreen":false},
        {"app_id":"org.b.Second","position":2,"isFullscreen":true}]}"#;
    assert_eq!(
        flathub::parse(List::Picks, body).unwrap(),
        ids(&["org.a.First", "org.b.Second", "org.c.Third"])
    );
}

#[test]
fn an_empty_list_is_not_an_error() {
    assert_eq!(flathub::parse(List::Popular, br#"{"hits":[]}"#), Ok(vec![]));
    assert_eq!(flathub::parse(List::Picks, br#"{"apps":[]}"#), Ok(vec![]));
}

#[test]
fn answers_of_the_wrong_shape_are_refused() {
    for (list, body) in [
        (List::Popular, &b""[..]),
        (List::Popular, b"not json"),
        (List::Popular, b"[]"),
        (List::Popular, b"{}"),
        (List::Popular, br#"{"hits":null}"#),
        (List::Popular, br#"{"hits":{"app_id":"a.b"}}"#),
        // A hit without an app_id, or with one that is not text.
        (List::Popular, br#"{"hits":[{"id":"a_b"}]}"#),
        (List::Popular, br#"{"hits":[{"app_id":5}]}"#),
        (List::Popular, br#"{"hits":[{"app_id":null}]}"#),
        (List::Popular, br#"{"hits":[{"app_id":["a.b"]}]}"#),
        (List::Popular, br#"{"hits":["a.b"]}"#),
        // The picks need their position, as a number.
        (List::Picks, br#"{"apps":[{"app_id":"a.b"}]}"#),
        (
            List::Picks,
            br#"{"apps":[{"app_id":"a.b","position":"1"}]}"#,
        ),
        (List::Picks, br#"{"hits":[]}"#),
        (List::Picks, br#"{"apps":{}}"#),
    ] {
        let got = flathub::parse(list, body);
        assert!(
            matches!(got, Err(ParseError::Schema(_))),
            "{}: {got:?}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn unknown_fields_are_ignored() {
    let body = br#"{"hits":[{"app_id":"org.a.B","name":"x","extra":{"deep":[1,2,3]}}],
        "totalHits":1,"future":true}"#;
    assert_eq!(
        flathub::parse(List::Popular, body).unwrap(),
        ids(&["org.a.B"])
    );
}

#[test]
fn an_oversize_answer_is_refused_before_it_is_read() {
    // Valid JSON up to the cap and beyond: the size decides, not the content.
    let mut body = hits(&["org.a.B"]);
    let pad = MAX_BODY as usize + 1 - body.len();
    body.extend(std::iter::repeat_n(b' ', pad));
    assert_eq!(body.len() as u64, MAX_BODY + 1);
    assert_eq!(
        flathub::parse(List::Popular, &body),
        Err(ParseError::TooLarge)
    );
    body.pop();
    assert_eq!(
        flathub::parse(List::Popular, &body).unwrap(),
        ids(&["org.a.B"])
    );
}

#[test]
fn hostile_ids_are_dropped_one_by_one() {
    let long = format!("org.example.{}", "a".repeat(300));
    let answer = hits(&[
        "org.good.One",
        "../../etc/passwd",
        "org.evil/../x",
        "org bad.App",
        "org.evil.\u{202e}App",
        "org.evil.A\u{0}pp",
        "org.evil.A\npp",
        "<script>alert(1)</script>.app",
        "org.evil.<b>App</b>",
        "org.evil.&lt;App",
        "org.evil.App;rm -rf",
        "single",
        ".leading.dot",
        "trailing.dot.",
        "double..dot",
        "",
        "-",
        "https://evil.example/x",
        "file:///etc/passwd",
        long.as_str(),
        "org.good.Two",
        "org.good.One",
    ]);
    assert_eq!(
        flathub::parse(List::Popular, &answer).unwrap(),
        ids(&["org.good.One", "org.good.Two"])
    );
}

#[test]
fn html_in_names_is_never_read() {
    // The names, summaries and icons of an answer are not taken, so markup in
    // them cannot reach the screen; the app's id is all that is kept.
    let body = br#"{"hits":[{"app_id":"org.a.B","name":"<b>Bold</b>&lt;i&gt;",
        "summary":"<script>x</script>","icon":"javascript:alert(1)"}]}"#;
    assert_eq!(
        flathub::parse(List::Popular, body).unwrap(),
        ids(&["org.a.B"])
    );
}

#[test]
fn only_so_many_ids_are_kept() {
    let many: Vec<String> = (0..MAX_IDS * 3)
        .map(|i| format!("org.many.App{i}"))
        .collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let got = flathub::parse(List::Popular, &hits(&refs)).unwrap();
    assert_eq!(got.len(), MAX_IDS);
    assert_eq!(got[0], "org.many.App0");
}

// ---- the cache

#[test]
fn a_list_written_is_a_list_read() {
    let dir = cache("roundtrip");
    let list = List::Category(Category::Office);
    assert_eq!(flathub::read_cache(&dir, list), None);
    flathub::write_cache(&dir, list, &ids(&["org.a.B", "org.c.D"]), NOW).unwrap();
    let got = flathub::read_cache(&dir, list).unwrap();
    assert_eq!(got.ids, ids(&["org.a.B", "org.c.D"]));
    assert_eq!(got.fetched_at, NOW);
    // Another list is not that list.
    assert_eq!(flathub::read_cache(&dir, List::Popular), None);
    // Written again, it is replaced.
    flathub::write_cache(&dir, list, &ids(&["org.e.F"]), NOW + 5).unwrap();
    let got = flathub::read_cache(&dir, list).unwrap();
    assert_eq!((got.ids, got.fetched_at), (ids(&["org.e.F"]), NOW + 5));
}

#[test]
fn the_cache_is_private_and_leaves_no_temp_files() {
    let dir = cache("private");
    flathub::write_cache(&dir, List::Popular, &ids(&["org.a.B"]), NOW).unwrap();
    let mode = |p: &std::path::Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(dir.parent().unwrap()), 0o700);
    assert_eq!(mode(&dir.join("popular.json")), 0o600);
    let names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["popular.json"]);
}

#[test]
fn a_folder_that_group_can_write_to_is_set_back_when_written_and_not_read() {
    let dir = cache("mode");
    flathub::write_cache(&dir, List::Popular, &ids(&["org.a.B"]), NOW).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o775)).unwrap();
    // A read never changes the folder, and does not trust it.
    assert_eq!(flathub::read_cache(&dir, List::Popular), None);
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o775
    );
    // A write repairs it.
    flathub::write_cache(&dir, List::Popular, &ids(&["org.a.B"]), NOW).unwrap();
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(flathub::read_cache(&dir, List::Popular).is_some());
}

#[test]
fn a_cache_folder_that_is_a_link_is_refused() {
    let base = scratch("dirlink");
    let elsewhere = base.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::create_dir_all(base.join("telamon-store")).unwrap();
    let dir = base.join("telamon-store/flathub");
    symlink(&elsewhere, &dir).unwrap();
    assert!(flathub::write_cache(&dir, List::Popular, &ids(&["org.a.B"]), NOW).is_err());
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    // A list placed there by hand is not read either.
    fs::write(
        elsewhere.join("popular.json"),
        br#"{"v":1,"list":"popular","fetched_at":1,"ids":["org.a.B"]}"#,
    )
    .unwrap();
    assert_eq!(flathub::read_cache(&dir, List::Popular), None);
}

#[test]
fn a_cache_file_that_is_a_link_is_not_read_and_not_written_through() {
    let dir = cache("filelink");
    flathub::write_cache(&dir, List::Picks, &ids(&["org.x.Y"]), NOW).unwrap();
    let target = scratch("filelink-target").join("secret.json");
    fs::write(
        &target,
        br#"{"v":1,"list":"popular","fetched_at":1,"ids":["org.a.B"]}"#,
    )
    .unwrap();
    let file = dir.join("popular.json");
    symlink(&target, &file).unwrap();
    assert_eq!(flathub::read_cache(&dir, List::Popular), None);
    // Writing puts a file of its own in place of the link; the target stays.
    let before = fs::read(&target).unwrap();
    flathub::write_cache(&dir, List::Popular, &ids(&["org.c.D"]), NOW).unwrap();
    assert_eq!(fs::read(&target).unwrap(), before);
    assert!(
        !fs::symlink_metadata(&file)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        flathub::read_cache(&dir, List::Popular).unwrap().ids,
        ids(&["org.c.D"])
    );
}

#[test]
fn a_bad_cache_file_is_ignored() {
    let dir = cache("corrupt");
    flathub::write_cache(&dir, List::Popular, &ids(&["org.a.B"]), NOW).unwrap();
    let file = dir.join("popular.json");
    let good = r#"{"v":1,"list":"popular","fetched_at":5,"ids":["org.a.B"]}"#;
    fs::write(&file, good).unwrap();
    assert!(flathub::read_cache(&dir, List::Popular).is_some());

    let too_many = format!(
        r#"{{"v":1,"list":"popular","fetched_at":5,"ids":[{}]}}"#,
        (0..=MAX_IDS)
            .map(|i| format!(r#""org.a.App{i}""#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let huge = format!(
        r#"{{"v":1,"list":"popular","fetched_at":5,"ids":[],"pad":"{}"}}"#,
        "x".repeat(70 * 1024)
    );
    for bad in [
        "",
        "garbage",
        "[]",
        "{}",
        r#"{"v":2,"list":"popular","fetched_at":5,"ids":["org.a.B"]}"#,
        r#"{"v":1,"list":"picks","fetched_at":5,"ids":["org.a.B"]}"#,
        r#"{"v":1,"list":"popular","fetched_at":-5,"ids":["org.a.B"]}"#,
        r#"{"v":1,"list":"popular","fetched_at":"5","ids":["org.a.B"]}"#,
        r#"{"v":1,"list":"popular","fetched_at":5,"ids":"org.a.B"}"#,
        r#"{"v":1,"list":"popular","fetched_at":5,"ids":[5]}"#,
        r#"{"v":1,"list":"popular","fetched_at":5,"ids":["../x"]}"#,
        r#"{"v":1,"list":"popular","fetched_at":5,"ids":["org.a.B","org.a.B"]}"#,
        r#"{"v":1,"list":"popular","fetched_at":5,"ids":["org.a.<b>"]}"#,
        too_many.as_str(),
        huge.as_str(),
    ] {
        fs::write(&file, bad).unwrap();
        assert_eq!(
            flathub::read_cache(&dir, List::Popular),
            None,
            "{}",
            &bad[..bad.len().min(80)]
        );
    }
    // A directory or a missing file in its place: nothing to read.
    fs::remove_file(&file).unwrap();
    assert_eq!(flathub::read_cache(&dir, List::Popular), None);
    fs::create_dir(&file).unwrap();
    assert_eq!(flathub::read_cache(&dir, List::Popular), None);
}

// ---- fetching, expiry and the offline case

/// A fake network: answers with `result`, counts the calls, remembers the URLs.
struct Fake {
    result: RefCell<Result<Vec<u8>, NetError>>,
    calls: Cell<u32>,
    urls: RefCell<Vec<String>>,
}

impl Fake {
    fn new(result: Result<Vec<u8>, NetError>) -> Fake {
        Fake {
            result: RefCell::new(result),
            calls: Cell::new(0),
            urls: RefCell::new(Vec::new()),
        }
    }
    fn fetch(&self, url: &str) -> Result<Vec<u8>, NetError> {
        self.calls.set(self.calls.get() + 1);
        self.urls.borrow_mut().push(url.to_string());
        self.result.borrow().clone()
    }
}

#[test]
fn a_fetched_list_is_checked_and_cached() {
    let dir = cache("refresh");
    let fake = Fake::new(Ok(hits(&["org.a.B", "../bad", "org.c.D"])));
    let got = flathub::refresh(&dir, List::Popular, NOW, &|u| fake.fetch(u)).unwrap();
    assert_eq!(got, ids(&["org.a.B", "org.c.D"]));
    assert_eq!(*fake.urls.borrow(), [List::Popular.url(NOW)]);
    let cached = flathub::read_cache(&dir, List::Popular).unwrap();
    assert_eq!(cached.ids, got);
    assert_eq!(cached.fetched_at, NOW);
}

#[test]
fn a_bad_answer_is_neither_used_nor_cached() {
    let dir = cache("badanswer");
    for body in [b"<html>captive portal</html>".to_vec(), b"{}".to_vec()] {
        let fake = Fake::new(Ok(body));
        let got = flathub::refresh(&dir, List::Popular, NOW, &|u| fake.fetch(u));
        assert!(matches!(got, Err(RefreshError::Bad(_))), "{got:?}");
    }
    let fake = Fake::new(Ok(vec![b' '; MAX_BODY as usize + 1]));
    assert_eq!(
        flathub::refresh(&dir, List::Popular, NOW, &|u| fake.fetch(u)),
        Err(RefreshError::Bad(ParseError::TooLarge))
    );
    assert_eq!(flathub::read_cache(&dir, List::Popular), None);
}

#[test]
fn an_expired_list_is_still_there_when_the_network_fails() {
    let dir = cache("offline");
    flathub::write_cache(&dir, List::Popular, &ids(&["org.a.B"]), NOW).unwrap();
    let later = NOW + 3 * DAY;

    // Expired, so a page would ask for a new one...
    let cached = flathub::read_cache(&dir, List::Popular).unwrap();
    assert!(!cached.is_fresh(List::Popular, later));

    // ... the network is down ...
    let fake = Fake::new(Err(NetError::Failed("no route to host".into())));
    let err = flathub::refresh(&dir, List::Popular, later, &|u| fake.fetch(u)).unwrap_err();
    assert!(err.is_network());
    assert_eq!(fake.calls.get(), 1);

    // ... and the old list is still what the page shows.
    let again = flathub::read_cache(&dir, List::Popular).unwrap();
    assert_eq!(again, cached);

    // A server error, a redirect elsewhere and a timeout leave it too.
    for e in [
        NetError::Status(503),
        NetError::TimedOut,
        NetError::Redirect,
        NetError::TooLarge,
        NetError::NotPublic,
    ] {
        let fake = Fake::new(Err(e));
        assert!(flathub::refresh(&dir, List::Popular, later, &|u| fake.fetch(u)).is_err());
        assert_eq!(flathub::read_cache(&dir, List::Popular).unwrap(), cached);
    }
}

#[test]
fn a_cache_that_cannot_be_written_does_not_fail_the_fetch() {
    let base = scratch("unwritable");
    let blocker = base.join("telamon-store");
    fs::write(&blocker, b"a file where the folder should be").unwrap();
    let dir = flathub::cache_dir(&blocker);
    let fake = Fake::new(Ok(hits(&["org.a.B"])));
    let got = flathub::refresh(&dir, List::Popular, NOW, &|u| fake.fetch(u)).unwrap();
    assert_eq!(got, ids(&["org.a.B"]));
    assert_eq!(flathub::read_cache(&dir, List::Popular), None);
}

#[test]
fn the_picks_are_asked_for_by_the_days_date() {
    let dir = cache("picks");
    let fake = Fake::new(Ok(WEEK.as_bytes().to_vec()));
    let got =
        flathub::refresh(&dir, List::Picks, NOW + 7 * DAY + 3600, &|u| fake.fetch(u)).unwrap();
    assert_eq!(got.len(), 5);
    assert_eq!(
        *fake.urls.borrow(),
        ["https://flathub.org/api/v2/app-picks/apps-of-the-week/2026-10-14"]
    );
}

#[test]
fn a_failed_fetch_is_not_retried_for_ten_minutes() {
    let retry = RETRY_AFTER.as_secs();
    assert_eq!(retry, 600);
    let mut gate = RetryGate::default();
    assert!(gate.allowed(List::Popular, NOW));

    // One list that is wrong waits alone.
    gate.failed(
        List::Picks,
        &RefreshError::Fetch(NetError::Status(404)),
        NOW,
    );
    assert!(!gate.allowed(List::Picks, NOW));
    assert!(!gate.allowed(List::Picks, NOW + retry - 1));
    assert!(gate.allowed(List::Picks, NOW + retry));
    assert!(gate.allowed(List::Popular, NOW));

    // A network that fails holds every list back.
    let down = RefreshError::Fetch(NetError::Failed("no route".into()));
    gate.failed(List::Popular, &down, NOW + 1000);
    for list in [
        List::Popular,
        List::RecentlyUpdated,
        List::Category(Category::Games),
    ] {
        assert!(!gate.allowed(list, NOW + 1000 + retry - 1), "{list:?}");
        assert!(gate.allowed(list, NOW + 1000 + retry), "{list:?}");
    }
    let timeout = RefreshError::Fetch(NetError::TimedOut);
    assert!(timeout.is_network());
    assert!(!RefreshError::Bad(ParseError::TooLarge).is_network());

    // A good answer lifts it.
    gate.succeeded(List::Popular);
    assert!(gate.allowed(List::Popular, NOW + 1001));
    assert!(gate.allowed(List::RecentlyUpdated, NOW + 1001));

    // A failure ahead of the clock (it was set back) does not hold forever.
    let mut gate = RetryGate::default();
    gate.failed(List::Popular, &down, NOW + 10 * DAY);
    assert!(gate.allowed(List::Popular, NOW));
}

// ---- matching the local catalog

#[test]
fn only_apps_the_catalog_has_are_listed() {
    let lib = library();
    let found = flathub::resolve(
        &lib,
        &ids(&[
            "org.not.Here",
            "org.example.Browser",
            // An add-on, a runtime and an app without a bundle are no apps.
            "org.example.Addon",
            "org.example.Runtime",
            "org.example.NoBundle",
            "org.example.Browser",
            "org.example.Suffix",
            "org.example.Suffix.desktop",
            "org.example.Pictures",
        ]),
        None,
        10,
    );
    let names: Vec<&str> = found.iter().map(|&e| lib.component(e).id_bare()).collect();
    assert_eq!(
        names,
        [
            "org.example.Browser",
            "org.example.Suffix",
            "org.example.Pictures"
        ]
    );
    // The answer's order is kept, and the limit cuts it.
    assert_eq!(
        flathub::resolve(
            &lib,
            &ids(&["org.example.Pictures", "org.example.Browser"]),
            None,
            1
        )
        .len(),
        1
    );
    assert!(flathub::resolve(&lib, &ids(&["org.example.Browser"]), None, 0).is_empty());
    assert!(flathub::resolve(&lib, &[], None, 10).is_empty());
    assert!(flathub::resolve(&lib, &ids(&["org.not.Here"]), None, 10).is_empty());
}

#[test]
fn a_category_shelf_holds_only_apps_of_that_category() {
    let lib = library();
    // As Flathub lists the category: by installs, some of it not in this
    // catalog and some in it under other categories.
    let answer = ids(&[
        "org.not.Here",
        "org.example.Pictures",
        "org.example.Browser",
        "org.example.Tabs",
        "org.example.Anon",
        "org.example.GameOne",
    ]);
    let names = |found: Vec<_>| -> Vec<String> {
        found
            .into_iter()
            .map(|e| lib.component(e).id_bare().to_string())
            .collect()
    };
    assert_eq!(
        names(flathub::resolve(&lib, &answer, Some(Category::Network), 10)),
        [
            "org.example.Browser",
            "org.example.Tabs",
            "org.example.Anon"
        ]
    );
    assert_eq!(
        names(flathub::resolve(&lib, &answer, Some(Category::Network), 2)),
        ["org.example.Browser", "org.example.Tabs"]
    );
    assert_eq!(
        names(flathub::resolve(&lib, &answer, Some(Category::Games), 10)),
        ["org.example.GameOne"]
    );
    assert!(flathub::resolve(&lib, &answer, Some(Category::Science), 10).is_empty());
}

#[test]
fn nothing_matches_an_empty_library() {
    let empty = Library::new(Vec::new());
    let popular = flathub::parse(List::Popular, POPULAR.as_bytes()).unwrap();
    assert!(flathub::resolve(&empty, &popular, None, 12).is_empty());
}
