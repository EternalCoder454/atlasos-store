//! The launch arguments of the AppImage feature: `--appimage-install`, the
//! files the file manager hands over, and the two early options.
use std::path::{Path, PathBuf};

use telamon_store_core::launch::{FileKind, Request, internal_path, parse, parse_with};

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn cwd() -> &'static Path {
    Path::new("/home/u")
}

#[test]
fn appimage_install_takes_a_plain_file_path() {
    let l = parse(
        &args(&["--appimage-install", "/home/u/Downloads/App.AppImage"]),
        cwd(),
    );
    assert_eq!(
        l.requests,
        vec![Request::File(
            FileKind::AppImage,
            PathBuf::from("/home/u/Downloads/App.AppImage")
        )]
    );
    assert!(l.refused.is_empty());
    // Inline value, any name (the file is checked by its bytes later).
    let l = parse(&args(&["--appimage-install=/tmp/download"]), cwd());
    assert_eq!(
        l.requests,
        vec![Request::File(
            FileKind::AppImage,
            PathBuf::from("/tmp/download")
        )]
    );
    // A relative path is read against the folder given.
    let l = parse(
        &args(&["--appimage-install", "Downloads/App.AppImage"]),
        cwd(),
    );
    assert_eq!(
        l.requests,
        vec![Request::File(
            FileKind::AppImage,
            PathBuf::from("/home/u/Downloads/App.AppImage")
        )]
    );
}

#[test]
fn appimage_install_has_the_same_refusals_as_other_files() {
    for (arg, reason) in [
        ("/home/u/../etc/x.AppImage", "has \"..\" in it"),
        ("/home/u//x.AppImage", "not a plain path"),
        ("/home/u/./x.AppImage", "not a plain path"),
        ("/home/u/dir/", "is a folder"),
        (
            "/home/u/x\u{202e}.AppImage",
            "has hidden or control characters",
        ),
        ("/home/u/x\n.AppImage", "has hidden or control characters"),
    ] {
        let l = parse(&args(&["--appimage-install", arg]), cwd());
        assert!(l.requests.is_empty(), "{arg:?}");
        assert_eq!(l.refused.len(), 1, "{arg:?}");
        assert_eq!(l.refused[0].reason, reason, "{arg:?}");
    }
    let l = parse(&args(&["--appimage-install"]), cwd());
    assert_eq!(l.refused[0].reason, "needs a value");
    // With no folder to read a relative path against.
    let l = parse(&args(&["--appimage-install", "x.AppImage"]), Path::new(""));
    assert_eq!(
        l.refused[0].reason,
        "relative path with no folder to find it in"
    );
    // After `--` it is a file, not an option.
    let l = parse(&args(&["--", "--appimage-install"]), cwd());
    assert!(l.requests.is_empty());
}

#[test]
fn appimage_files_are_taken_by_name_or_by_their_bytes() {
    let l = parse(
        &args(&[
            "/home/u/a.AppImage",
            "/home/u/B.APPIMAGE",
            "file:///home/u/c%20d.appimage",
        ]),
        cwd(),
    );
    assert_eq!(l.requests.len(), 3, "{l:?}");
    assert!(
        l.requests
            .iter()
            .all(|r| matches!(r, Request::File(FileKind::AppImage, _)))
    );
    // An unknown name is refused, unless the caller says it starts like one.
    let l = parse(&args(&["/home/u/download"]), cwd());
    assert_eq!(l.refused[0].reason, "not a file the Store opens");
    let l = parse_with(
        &args(&["/home/u/download", "/home/u/other.txt"]),
        cwd(),
        &|p| p.ends_with("download"),
    );
    assert_eq!(
        l.requests,
        vec![Request::File(
            FileKind::AppImage,
            PathBuf::from("/home/u/download")
        )]
    );
    assert_eq!(l.refused.len(), 1);
    // The caller is not asked about paths that are refused anyway.
    let l = parse_with(&args(&["/home/u/../download"]), cwd(), &|_| panic!("asked"));
    assert_eq!(l.refused[0].reason, "has \"..\" in it");
}

#[test]
fn unknown_options_are_still_refused_and_the_early_ones_are_not_launch_options() {
    for opt in [
        "--appimage-check",
        "--appimage-inspect",
        "--appimage-extract",
        "--appimage-run",
    ] {
        let l = parse(&args(&[opt, "/home/u/Downloads"]), cwd());
        assert!(l.requests.is_empty(), "{opt}");
        assert_eq!(l.refused[0].reason, "unknown option", "{opt}");
    }
}

#[test]
fn the_early_options_take_exactly_one_plain_path() {
    let ok = internal_path(
        "--appimage-check",
        &args(&["--appimage-check", "/home/u/Downloads"]),
    );
    assert_eq!(ok, Some(Ok(PathBuf::from("/home/u/Downloads"))));
    let ok = internal_path(
        "--appimage-check",
        &args(&["--appimage-check=/home/u/Downloads"]),
    );
    assert_eq!(ok, Some(Ok(PathBuf::from("/home/u/Downloads"))));
    // Not these options: a normal launch.
    assert_eq!(internal_path("--appimage-check", &args(&[])), None);
    assert_eq!(
        internal_path("--appimage-check", &args(&["--app", "org.x.Y"])),
        None
    );
    assert_eq!(
        internal_path("--appimage-check", &args(&["--appimage-inspect", "/x/y"])),
        None
    );
    assert_eq!(
        internal_path("--appimage-check", &args(&["--appimage-checked", "/x/y"])),
        None
    );
    // These options, with a bad path or extra arguments: refused.
    for bad in [
        args(&["--appimage-check"]),
        args(&["--appimage-check", "/a", "/b"]),
        args(&["--appimage-check", "relative/dir"]),
        args(&["--appimage-check", "/a/../b"]),
        args(&["--appimage-check", "/a/"]),
        args(&["--appimage-check", "/a\u{202e}b"]),
        args(&["--appimage-check=/a", "--app"]),
    ] {
        assert!(
            matches!(internal_path("--appimage-check", &bad), Some(Err(_))),
            "{bad:?}"
        );
    }
}
