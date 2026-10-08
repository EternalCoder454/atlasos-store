//! What a bundle shows the rest of the desktop, and the rewrite that makes it
//! work from where the Store puts it.
//!
//! A bundle's tree is a prefix (`bin/`, `share/`...). The desktop only looks
//! in `~/.local/share/{applications,icons,metainfo,dbus-1/services,
//! knotifications6}`, so the Store copies a few files there. Which files, and
//! under which names, is fixed here; anything else under `share/` stays in the
//! app's own folder and is read by the app itself.
//!
//! | In the bundle | Copied to (`$XDG_DATA_HOME/`) | Name rule |
//! |---|---|---|
//! | `share/applications/<id>.desktop` | `applications/` | exactly this one file |
//! | `share/icons/hicolor/<size>/apps/<icon>` | `icons/hicolor/<size>/apps/` | `.png`/`.svg` named `<id>` or `<id>-...`/`<id>_...` |
//! | `share/metainfo/<id>.metainfo.xml` | `metainfo/` | `<id>.metainfo.xml` or `<id>.appdata.xml` |
//! | `share/dbus-1/services/<name>.service` | `dbus-1/services/` | `<name>` is `<id>` or `<id>.<more>` |
//! | `share/knotifications6/telamon-*.notifyrc` | `knotifications6/` | `telamon-<name>.notifyrc` |
//!
//! The names are the rule that keeps a bundle from replacing something that
//! is not its own (the user's folder comes before `/usr` in every search
//! path): a bundle can only ever write files that carry its app ID. The
//! desktop file and the D-Bus service are rewritten: the first word of every
//! `Exec` (a bare program name that must be a program in the bundle's `bin/`)
//! becomes the absolute path under `<app>/current/bin/`, `TryExec`, `Path` and
//! any `X-Telamon-Native-*` key the bundle brought are dropped, and the
//! Store's own `X-Telamon-Native-App` and `-Version` are added.

use std::collections::BTreeSet;
use std::path::Path;

use super::manifest::Manifest;
use super::{Error, err};
use crate::appimage::install::{escape_value, exec_arg};
use crate::keyfile::{KeyFile, Limits};

/// Largest file copied out of a bundle.
const MAX_EXPORT: usize = 1024 * 1024;
/// Most icons copied.
const MAX_ICONS: usize = 64;
/// The marker the Store writes into the desktop entry and the D-Bus service.
pub const MARKER: &str = "X-Telamon-Native-App";
pub const MARKER_VERSION: &str = "X-Telamon-Native-Version";

/// A file the Store copies out of the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// Where it was in the tree.
    pub from: String,
    /// Where it goes, relative to `$XDG_DATA_HOME`.
    pub to: String,
    /// Its content as it will be written (rewritten for the desktop entry and
    /// the D-Bus service).
    pub bytes: Vec<u8>,
}

/// What `plan` found out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub exports: Vec<Export>,
    /// The program Open runs: `bin/<name>`, the first word of the desktop
    /// entry's `Exec`.
    pub exe: String,
}

fn limits() -> Limits {
    Limits {
        max_bytes: 64 * 1024,
        max_lines: 1000,
        max_groups: 32,
        max_keys: 400,
        max_value: 8192,
    }
}

fn bare_name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 100
        && !n.starts_with('.')
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
}

/// The programs in `bin/` (directly below it): files the manifest marks
/// executable, and links there that lead to one of those.
pub fn programs(m: &Manifest) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = m
        .files
        .iter()
        .filter(|f| f.executable)
        .filter_map(|f| f.path.strip_prefix("bin/"))
        .filter(|n| !n.contains('/'))
        .map(str::to_string)
        .collect();
    for l in &m.links {
        let Some(name) = l.path.strip_prefix("bin/").filter(|n| !n.contains('/')) else {
            continue;
        };
        // The target, read from the link's folder, lexically.
        let mut parts: Vec<&str> = vec!["bin"];
        for part in l.target.split('/') {
            match part {
                ".." => {
                    parts.pop();
                }
                p => parts.push(p),
            }
        }
        let target = parts.join("/");
        if m.files.iter().any(|f| f.executable && f.path == target) {
            out.insert(name.to_string());
        }
    }
    out
}

/// Splits an `Exec` value into its first word and the rest (kept as written).
/// The first word must be a bare name (no quotes, no path).
fn first_word(value: &str) -> Result<(&str, &str), Error> {
    let end = value.find([' ', '\t']).unwrap_or(value.len());
    let (word, rest) = value.split_at(end);
    if !bare_name_ok(word) {
        return Err(err(
            "The bundle's launcher must start its program by its plain name from bin/.",
        ));
    }
    Ok((word, rest))
}

/// The new `Exec` value: the program at `<prefix>/bin/<word>`, quoted as the
/// Desktop Entry spec wants, then the original arguments.
fn exec_value(
    value: &str,
    prefix: &Path,
    programs: &BTreeSet<String>,
) -> Result<(String, String), Error> {
    let (word, rest) = first_word(value)?;
    if !programs.contains(word) {
        return Err(err(
            "The bundle's launcher starts a program the bundle does not contain.",
        ));
    }
    let path = prefix.join("bin").join(word);
    Ok((
        format!("{}{rest}", escape_value(&exec_arg(&path.to_string_lossy()))),
        word.to_string(),
    ))
}

/// Rewrites the bundle's desktop entry. Returns the text and the program the
/// first `Exec` of the main group starts.
pub fn rewrite_desktop(
    src: &[u8],
    id: &str,
    version: &str,
    prefix: &Path,
    programs: &BTreeSet<String>,
) -> Result<(Vec<u8>, String), Error> {
    let bad = |_| err("The bundle's desktop entry is not valid.");
    let kf = KeyFile::parse(src, &limits()).map_err(bad)?;
    if kf.groups().next() != Some("Desktop Entry") {
        return Err(err(
            "The bundle's desktop entry does not start with [Desktop Entry].",
        ));
    }
    if kf.raw("Desktop Entry", "Type") != Some("Application") {
        return Err(err("The bundle's desktop entry is not an application."));
    }
    let text =
        std::str::from_utf8(src).map_err(|_| err("The bundle's desktop entry is not text."))?;
    let mut out = String::new();
    let mut group = String::new();
    let mut main_done = false;
    let mut exe: Option<String> = None;
    let mut exe_main: Option<String> = None;
    let marker = |out: &mut String| {
        out.push_str(&format!("{MARKER}={id}\n{MARKER_VERSION}={version}\n"));
    };
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('[')
            && let Some(name) = rest.split(']').next()
        {
            if group == "Desktop Entry" && !main_done {
                marker(&mut out);
                main_done = true;
            }
            group = name.to_string();
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(trimmed);
            out.push('\n');
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(err("The bundle's desktop entry is not valid."));
        };
        let key = key.trim_end();
        let value = value.trim_start();
        if key.starts_with("Exec[") || key.starts_with("TryExec[") || key.starts_with("Path[") {
            return Err(err(
                "The bundle's desktop entry has a launcher key the Store won't copy.",
            ));
        }
        match key {
            "TryExec" | "Path" => {}
            k if k.starts_with("X-Telamon-Native-") => {}
            "Exec" => {
                let (new, word) = exec_value(value, prefix, programs)?;
                if group == "Desktop Entry" {
                    exe_main = Some(word.clone());
                }
                exe.get_or_insert(word);
                out.push_str(&format!("Exec={new}\n"));
            }
            _ => {
                out.push_str(&format!("{key}={value}\n"));
            }
        }
    }
    if group == "Desktop Entry" && !main_done {
        marker(&mut out);
    }
    let exe = exe_main
        .or(exe)
        .ok_or_else(|| err("The bundle's desktop entry has no Exec."))?;
    // What was written must read back as a valid entry with our marker.
    let check = KeyFile::parse(out.as_bytes(), &limits()).map_err(bad)?;
    debug_assert_eq!(check.raw("Desktop Entry", MARKER), Some(id));
    Ok((out.into_bytes(), exe))
}

/// Rewrites the bundle's D-Bus service file: `Name` and `Exec` only.
pub fn rewrite_dbus(
    src: &[u8],
    id: &str,
    version: &str,
    prefix: &Path,
    programs: &BTreeSet<String>,
) -> Result<(Vec<u8>, String), Error> {
    let bad = |_| err("The bundle's D-Bus service file is not valid.");
    let kf = KeyFile::parse(src, &limits()).map_err(bad)?;
    let name = kf
        .raw("D-BUS Service", "Name")
        .ok_or_else(|| err("The bundle's D-Bus service file has no Name."))?;
    let exec = kf
        .raw("D-BUS Service", "Exec")
        .ok_or_else(|| err("The bundle's D-Bus service file has no Exec."))?;
    let owns = name == id || name.strip_prefix(id).is_some_and(|r| r.starts_with('.'));
    if !owns || !super::valid_app_id(name) {
        return Err(err(
            "The bundle's D-Bus service is not named after the app.",
        ));
    }
    let (value, _) = exec_value(exec, prefix, programs)?;
    let text = format!(
        "[D-BUS Service]\nName={name}\nExec={value}\n{MARKER}={id}\n{MARKER_VERSION}={version}\n"
    );
    Ok((text.into_bytes(), name.to_string()))
}

fn icon_name_ok(file: &str, id: &str) -> bool {
    let Some((stem, ext)) = file.rsplit_once('.') else {
        return false;
    };
    matches!(ext, "png" | "svg")
        && (stem == id
            || stem
                .strip_prefix(id)
                .is_some_and(|r| r.starts_with('-') || r.starts_with('_')))
}

fn icon_dir_ok(dir: &str) -> bool {
    dir == "scalable"
        || dir.split_once('x').is_some_and(|(w, h)| {
            let digits =
                |s: &str| !s.is_empty() && s.len() <= 4 && s.bytes().all(|b| b.is_ascii_digit());
            digits(w) && digits(h)
        })
}

/// Decides what is copied out of the bundle at `tree`, and reads and rewrites
/// it. `prefix` is the app's folder as the desktop will run it
/// (`<data>/telamon-apps/<id>/current`). Errors name the first thing that is
/// not allowed.
pub fn plan(tree: &Path, m: &Manifest, prefix: &Path) -> Result<Plan, Error> {
    let id = &m.id;
    let progs = programs(m);
    let mut exports = Vec::new();
    let mut exe = None;
    let mut icons = 0usize;
    let mut desktop = false;
    let read = |rel: &str, max: usize| -> Result<Vec<u8>, Error> {
        let file = tree.join(rel);
        match crate::appimage::fsutil::read_private(&file, max as u64) {
            Ok(Some(b)) => Ok(b),
            _ => Err(err("A file the bundle shows the desktop cannot be read.")),
        }
    };
    let listed = m
        .files
        .iter()
        .map(|f| f.path.as_str())
        .chain(m.links.iter().map(|l| l.path.as_str()));
    for path in listed {
        let Some(rest) = path.strip_prefix("share/") else {
            continue;
        };
        let parts: Vec<&str> = rest.split('/').collect();
        let is_link = m.links.iter().any(|l| l.path == path);
        match parts.as_slice() {
            ["applications", file] => {
                if *file != format!("{id}.desktop") || is_link {
                    return Err(err(
                        "The bundle's share/applications may hold only <app ID>.desktop.",
                    ));
                }
                let (bytes, e) =
                    rewrite_desktop(&read(path, 64 * 1024)?, id, &m.version, prefix, &progs)?;
                exe = Some(e);
                desktop = true;
                exports.push(Export {
                    from: path.into(),
                    to: format!("applications/{file}"),
                    bytes,
                });
            }
            ["icons", "hicolor", dir, "apps", file]
                if icon_dir_ok(dir) && icon_name_ok(file, id) =>
            {
                if is_link {
                    return Err(err("The bundle's icons must be files, not links."));
                }
                icons += 1;
                if icons > MAX_ICONS {
                    return Err(err("The bundle has too many icons."));
                }
                exports.push(Export {
                    from: path.into(),
                    to: format!("icons/hicolor/{dir}/apps/{file}"),
                    bytes: read(path, MAX_EXPORT)?,
                });
            }
            ["icons", ..] => {
                return Err(err(
                    "The bundle's share/icons may hold only icons in hicolor/<size>/apps named after the app ID.",
                ));
            }
            ["metainfo", file] => {
                if (*file != format!("{id}.metainfo.xml") && *file != format!("{id}.appdata.xml"))
                    || is_link
                {
                    return Err(err(
                        "The bundle's share/metainfo may hold only <app ID>.metainfo.xml.",
                    ));
                }
                exports.push(Export {
                    from: path.into(),
                    to: format!("metainfo/{file}"),
                    bytes: read(path, MAX_EXPORT)?,
                });
            }
            ["dbus-1", "services", file] => {
                let Some(name) = file.strip_suffix(".service") else {
                    return Err(err(
                        "The bundle's share/dbus-1/services holds a file that is not a .service.",
                    ));
                };
                if is_link {
                    return Err(err("The bundle's D-Bus services must be files, not links."));
                }
                let (bytes, bus) =
                    rewrite_dbus(&read(path, 64 * 1024)?, id, &m.version, prefix, &progs)?;
                if bus != name {
                    return Err(err(
                        "The bundle's D-Bus service file is not named after its bus name.",
                    ));
                }
                exports.push(Export {
                    from: path.into(),
                    to: format!("dbus-1/services/{file}"),
                    bytes,
                });
            }
            ["dbus-1", ..] => {
                return Err(err(
                    "The bundle's share/dbus-1 may hold only session services.",
                ));
            }
            ["knotifications6", file] => {
                let ok = file
                    .strip_suffix(".notifyrc")
                    .is_some_and(|s| s.starts_with("telamon-") && bare_name_ok(s));
                if !ok || is_link {
                    return Err(err(
                        "The bundle's share/knotifications6 may hold only telamon-<name>.notifyrc files.",
                    ));
                }
                exports.push(Export {
                    from: path.into(),
                    to: format!("knotifications6/{file}"),
                    bytes: read(path, MAX_EXPORT)?,
                });
            }
            _ => {}
        }
    }
    if !desktop {
        return Err(err(
            "The bundle has no desktop entry (share/applications/<app ID>.desktop).",
        ));
    }
    exports.sort_by(|a, b| a.to.cmp(&b.to));
    Ok(Plan {
        exports,
        exe: format!("bin/{}", exe.expect("set with the desktop entry")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "net.eterneon.telamon.gates";

    fn progs() -> BTreeSet<String> {
        ["telamon-gates".to_string()].into()
    }

    fn rewrite(text: &str) -> Result<(String, String), Error> {
        rewrite_desktop(
            text.as_bytes(),
            ID,
            "0.2.0",
            Path::new("/home/u/.local/share/telamon-apps/net.eterneon.telamon.gates/current"),
            &progs(),
        )
        .map(|(b, e)| (String::from_utf8(b).unwrap(), e))
    }

    const GOOD: &str = "[Desktop Entry]\nType=Application\nName=Telamon Gates\nName[de]=Tor\nExec=telamon-gates %U\nTryExec=telamon-gates\nIcon=net.eterneon.telamon.gates\nTerminal=false\n\n[Desktop Action new]\nName=New Chat\nExec=telamon-gates --new\n";

    #[test]
    fn exec_becomes_absolute_and_marked() {
        let (text, exe) = rewrite(GOOD).unwrap();
        assert_eq!(exe, "telamon-gates");
        assert!(text.contains("Exec=/home/u/.local/share/telamon-apps/net.eterneon.telamon.gates/current/bin/telamon-gates %U\n"), "{text}");
        assert!(text.contains("Exec=/home/u/.local/share/telamon-apps/net.eterneon.telamon.gates/current/bin/telamon-gates --new\n"));
        assert!(!text.contains("TryExec"));
        assert!(text.contains("Name[de]=Tor"));
        assert!(text.contains(&format!("{MARKER}={ID}\n{MARKER_VERSION}=0.2.0\n")));
        // The marker sits in the main group, before the action.
        let marker = text.find(MARKER).unwrap();
        assert!(marker < text.find("[Desktop Action new]").unwrap());
        let kf = KeyFile::parse(text.as_bytes(), &limits()).unwrap();
        assert_eq!(kf.raw("Desktop Entry", MARKER), Some(ID));
    }

    #[test]
    fn a_home_with_a_space_is_quoted() {
        let (text, _) = rewrite_desktop(
            GOOD.as_bytes(),
            ID,
            "1",
            Path::new("/home/my user/apps/x/current"),
            &progs(),
        )
        .map(|(b, e)| (String::from_utf8(b).unwrap(), e))
        .unwrap();
        assert!(
            text.contains("Exec=\"/home/my user/apps/x/current/bin/telamon-gates\" %U"),
            "{text}"
        );
        // And it reads back as one argument.
        let line = text.lines().find(|l| l.starts_with("Exec=")).unwrap();
        let args = crate::appimage::install::split_exec(&line[5..].replace("\\\\", "\\")).unwrap();
        assert_eq!(args[0], "/home/my user/apps/x/current/bin/telamon-gates");
    }

    #[test]
    fn a_main_group_only_file_gets_its_marker() {
        let (text, _) =
            rewrite("[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates").unwrap();
        assert!(text.contains(MARKER), "{text}");
    }

    #[test]
    fn the_bundle_cannot_bring_its_own_marker_or_launcher_tricks() {
        let (text, _) = rewrite("[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nX-Telamon-Native-App=org.evil.app\nPath=/etc\n").unwrap();
        assert_eq!(text.matches(MARKER).count(), 1);
        assert!(text.contains(&format!("{MARKER}={ID}")));
        assert!(!text.contains("Path="));
    }

    #[test]
    fn launchers_that_leave_the_bundle_are_refused() {
        for exec in [
            "/usr/bin/konsole",
            "../../bin/sh",
            "\"telamon-gates\"",
            "env FOO=1 telamon-gates",
            "sh -c telamon-gates",
            "bin/telamon-gates",
            "other-program",
            "",
            ".hidden",
        ] {
            let text = format!("[Desktop Entry]\nType=Application\nName=X\nExec={exec}\n");
            assert!(rewrite(&text).is_err(), "{exec:?}");
        }
        for text in [
            "[Desktop Entry]\nType=Application\nName=X\n",
            "[Desktop Entry]\nType=Link\nName=X\nExec=telamon-gates\n",
            "Name=X\n[Desktop Entry]\nType=Application\nExec=telamon-gates\n",
            "[Other]\nType=Application\n[Desktop Entry]\nExec=telamon-gates\n",
            "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nExec[de]=sh\n",
        ] {
            assert!(rewrite(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn dbus_services_are_rewritten_to_name_and_exec() {
        let src = "[D-BUS Service]\nName=net.eterneon.telamon.gates\nExec=telamon-gates --gapplication-service\nUser=root\nSystemdService=evil.service\n";
        let (bytes, name) =
            rewrite_dbus(src.as_bytes(), ID, "1", Path::new("/p/current"), &progs()).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(name, ID);
        assert!(text.contains("Exec=/p/current/bin/telamon-gates --gapplication-service"));
        assert!(!text.contains("root") && !text.contains("Systemd"));
        for bad in [
            "[D-BUS Service]\nName=org.freedesktop.Notifications\nExec=telamon-gates\n",
            "[D-BUS Service]\nName=net.eterneon.telamon.gatesX\nExec=telamon-gates\n",
            "[D-BUS Service]\nName=net.eterneon.telamon.gates\nExec=/usr/bin/sh\n",
            "[D-BUS Service]\nExec=telamon-gates\n",
        ] {
            assert!(
                rewrite_dbus(bad.as_bytes(), ID, "1", Path::new("/p"), &progs()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_link_in_bin_to_a_program_is_a_program() {
        use crate::native::manifest::{FileEntry, LinkEntry};
        let mut m = crate::native::manifest::tests_sample();
        m.files = vec![FileEntry {
            path: "bin/real".into(),
            size: 1,
            sha256: "a".repeat(64),
            executable: true,
        }];
        m.links = vec![
            LinkEntry {
                path: "bin/alias".into(),
                target: "real".into(),
            },
            LinkEntry {
                path: "bin/other".into(),
                target: "../share/x".into(),
            },
        ];
        let p = programs(&m);
        assert!(
            p.contains("real") && p.contains("alias") && !p.contains("other"),
            "{p:?}"
        );
    }

    #[test]
    fn icons_must_carry_the_app_id() {
        assert!(icon_name_ok("net.eterneon.telamon.gates.svg", ID));
        assert!(icon_name_ok("net.eterneon.telamon.gates-symbolic.svg", ID));
        assert!(!icon_name_ok("document-save.svg", ID));
        assert!(!icon_name_ok("net.eterneon.telamon.gatesX.svg", ID));
        assert!(!icon_name_ok("net.eterneon.telamon.gates.xpm", ID));
        assert!(
            icon_dir_ok("48x48")
                && icon_dir_ok("scalable")
                && !icon_dir_ok("../x")
                && !icon_dir_ok("48")
        );
    }
}
