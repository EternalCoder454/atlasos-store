//! Invariants of the permissions model, on fixtures shaped like real Flathub
//! apps and on small hand-written metadata.

use std::collections::HashSet;
use telamon_store_core::permissions::{PermError, Permission, Permissions, Risk};

fn fixture(name: &str) -> Vec<u8> {
    let p = format!(
        "{}/tests/fixtures/permissions/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn app(name: &str) -> Permissions {
    Permissions::from_metadata(&fixture(name)).unwrap()
}

fn meta(context: &str) -> Permissions {
    Permissions::from_metadata(format!("[Application]\nname=org.example.App\n{context}").as_bytes())
        .unwrap()
}

fn codes(v: &[Permission]) -> Vec<&str> {
    v.iter().map(|p| p.code()).collect()
}

fn find<'a>(v: &'a [Permission], code: &str) -> &'a Permission {
    v.iter()
        .find(|p| p.code() == code)
        .unwrap_or_else(|| panic!("no {code} in {:?}", codes(v)))
}

const APPS: [&str; 4] = [
    "firefox.metadata",
    "obs.metadata",
    "steam.metadata",
    "gimp.metadata",
];

#[test]
fn risk_is_ordered() {
    assert!(Risk::Low < Risk::Medium && Risk::Medium < Risk::High);
}

#[test]
fn every_known_value_maps_to_a_known_item() {
    let all = meta(
        "[Context]\nshared=network;ipc;\n\
         sockets=x11;wayland;fallback-x11;pulseaudio;system-bus;session-bus;ssh-auth;pcsc;cups;gpg-agent;inherit-wayland-socket;\n\
         devices=dri;input;usb;kvm;shm;all;\n\
         features=devel;multiarch;bluetooth;canbus;per-app-dev-shm;\n\
         filesystems=host;host-os;host-etc;home;xdg-desktop;xdg-documents;xdg-download;xdg-music;xdg-pictures;xdg-public-share;xdg-videos;xdg-templates;xdg-config;xdg-cache;xdg-data;xdg-run;xdg-download/sub:ro;~/x:create;/opt/x:rw;\n\
         persistent=.mozilla;\nunset-environment=FOO;\n\
         [Session Bus Policy]\norg.example.A=talk\norg.example.B=own\norg.example.C=see\norg.example.*=talk\n\
         [System Bus Policy]\norg.example.D=talk\n",
    );
    let p = all.permissions();
    assert_eq!(p.len(), 50, "{:?}", codes(&p));
    for x in &p {
        assert!(!x.is_unknown(), "{} is unknown", x.code());
        assert!(!x.describe().is_empty());
        assert!(x.describe().starts_with(|c: char| c.is_uppercase()));
        assert!(!x.describe().ends_with('.'));
    }
}

#[test]
fn codes_are_unique_and_sorted_by_risk() {
    for name in APPS {
        let p = app(name).permissions();
        let set: HashSet<&str> = p.iter().map(|x| x.code()).collect();
        assert_eq!(set.len(), p.len(), "{name}");
        assert!(p.windows(2).all(|w| w[0].risk() >= w[1].risk()), "{name}");
        assert!(p.iter().all(|x| !x.is_unknown()), "{name}: {:?}", codes(&p));
    }
}

#[test]
fn nothing_added_since_itself() {
    for name in APPS {
        let a = app(name);
        assert!(a.added_since(&a).is_empty(), "{name}");
    }
    let r = app("runtime.metadata");
    assert!(r.added_since(&r).is_empty());
}

#[test]
fn real_app_shapes() {
    let ff = app("firefox.metadata").permissions();
    assert_eq!(find(&ff, "device:all").risk(), Risk::High);
    assert_eq!(find(&ff, "share:network").risk(), Risk::Medium);
    assert_eq!(find(&ff, "socket:wayland").risk(), Risk::Low);
    // Wayland is there, so the X11 fallback is only a fallback.
    assert_eq!(find(&ff, "socket:fallback-x11").risk(), Risk::Low);
    assert_eq!(
        find(&ff, "session-bus:talk:org.freedesktop.Notifications").risk(),
        Risk::Medium
    );
    assert_eq!(
        find(&ff, "filesystem:xdg-download:rw").describe(),
        "Can read and change files in your Downloads folder"
    );

    let obs = app("obs.metadata").permissions();
    let esc = find(&obs, "session-bus:talk:org.freedesktop.Flatpak");
    assert_eq!(esc.risk(), Risk::High);
    assert!(esc.describe().contains("break out of its sandbox"));
    assert_eq!(find(&obs, "filesystem:home:rw").risk(), Risk::High);
    assert_eq!(
        find(&obs, "filesystem:home:rw").describe(),
        "Can read and change all files in your home folder"
    );
    // x11 next to wayland: still a risk, but not the keylogging one.
    assert_eq!(find(&obs, "socket:x11").risk(), Risk::Medium);
    assert_eq!(
        find(&obs, "persistent:.config/obs-studio").risk(),
        Risk::Low
    );

    let steam = app("steam.metadata").permissions();
    assert_eq!(find(&steam, "filesystem:xdg-pictures:ro").risk(), Risk::Low);
    let d = find(
        &steam,
        "filesystem:xdg-run/app/com.discordapp.Discord:create",
    );
    // Writing there could replace the socket: High.
    assert_eq!(d.risk(), Risk::High);
    assert_eq!(
        find(&steam, "system-bus:talk:org.freedesktop.UDisks2").risk(),
        Risk::High
    );
    assert_eq!(find(&steam, "feature:bluetooth").risk(), Risk::Medium);
    assert_eq!(find(&steam, "env:LD_LIBRARY_PATH").risk(), Risk::Medium);

    let gimp = app("gimp.metadata").permissions();
    assert_eq!(find(&gimp, "device:dri").risk(), Risk::Low);
    assert_eq!(
        find(&gimp, "session-bus:talk:org.gtk.vfs.*").risk(),
        Risk::Medium
    );
    assert_eq!(
        find(&gimp, "session-bus:talk:org.freedesktop.portal.Fcitx").risk(),
        Risk::Low
    );
    assert_eq!(
        find(&gimp, "filesystem:xdg-config/gtk-3.0:ro").risk(),
        Risk::Low
    );
}

#[test]
fn x11_without_wayland_is_high() {
    let p = meta("[Context]\nsockets=x11;\n").permissions();
    assert_eq!(find(&p, "socket:x11").risk(), Risk::High);
    let p = meta("[Context]\nsockets=fallback-x11;\n").permissions();
    assert_eq!(find(&p, "socket:fallback-x11").risk(), Risk::High);
}

#[test]
fn flatpak_escape_names_are_high_even_through_wildcards() {
    for name in [
        "org.freedesktop.Flatpak",
        "org.freedesktop.*",
        "org.freedesktop.Flatpak.*",
        "org.*",
    ] {
        for level in ["talk", "own"] {
            let p = meta(&format!("[Session Bus Policy]\n{name}={level}\n")).permissions();
            assert_eq!(p[0].risk(), Risk::High, "{name} {level}");
        }
    }
    let p = meta("[Session Bus Policy]\norg.freedesktop.Flatpak=talk\n").permissions();
    assert!(p[0].describe().contains("break out of its sandbox"));
    // The portal is the safe way in: only the Flatpak service is the escape.
    for (name, level, risk) in [
        ("org.freedesktop.portal.Flatpak", "talk", Risk::Low),
        ("org.freedesktop.portal.Flatpak", "see", Risk::Low),
        ("org.freedesktop.portal.Desktop", "talk", Risk::Low),
        ("org.freedesktop.portal.*", "talk", Risk::Low),
        ("org.freedesktop.portal.Desktop", "own", Risk::Medium),
        ("org.freedesktop.FlatpakX", "talk", Risk::Medium),
        ("org.freedesktop.Flat.*", "talk", Risk::Medium),
        ("org.freedesktop.Flatpak", "see", Risk::Low),
    ] {
        let p = meta(&format!("[Session Bus Policy]\n{name}={level}\n")).permissions();
        assert_eq!(p[0].risk(), risk, "{name} {level}");
    }
    let p = meta("[Session Bus Policy]\norg.freedesktop.portal.Flatpak=talk\n").permissions();
    assert_eq!(
        p[0].describe(),
        "Can talk to the org.freedesktop.portal.Flatpak service"
    );
}

#[test]
fn sensitive_session_names_are_high() {
    for name in [
        "org.freedesktop.systemd1",
        "org.freedesktop.secrets",
        "org.kde.kwalletd5",
        "org.kde.kwalletd6",
        "org.gnome.keyring",
        "org.gnome.keyring.SystemPrompter",
        "ca.desrt.dconf",
        "org.kde.klauncher5",
        "org.kde.klauncher6",
        "org.kde.KWin",
        "org.kde.plasmashell",
        "org.kde.kded5",
        "org.kde.kded6",
        "org.gnome.Shell",
        "org.freedesktop.PackageKit",
        "org.freedesktop.impl.portal.PermissionStore",
        "org.freedesktop.impl.portal.*",
        "org.kde.*",
        "org.gnome.*",
        "org.freedesktop.impl.*",
        "org.gnome.keyring.*",
        "org.kde.KWin.*",
    ] {
        for level in ["talk", "own"] {
            let p = meta(&format!("[Session Bus Policy]\n{name}={level}\n")).permissions();
            assert_eq!(p[0].risk(), Risk::High, "{name} {level}");
            assert!(!p[0].is_unknown(), "{name}");
        }
        let p = meta(&format!("[Session Bus Policy]\n{name}=see\n")).permissions();
        assert_eq!(p[0].risk(), Risk::Low, "{name} see");
    }
    let p = meta("[Session Bus Policy]\norg.freedesktop.secrets=talk\n").permissions();
    assert_eq!(p[0].describe(), "Can read your saved passwords");
    // Ordinary names stay as they were.
    for name in [
        "org.gtk.vfs.*",
        "org.kde.StatusNotifierWatcher",
        "org.freedesktop.Notifications",
    ] {
        let p = meta(&format!("[Session Bus Policy]\n{name}=talk\n")).permissions();
        assert_eq!(p[0].risk(), Risk::Medium, "{name}");
    }
}

#[test]
fn system_bus_is_high_except_seeing() {
    let p = meta("[System Bus Policy]\na.b.C=talk\na.b.D=own\na.b.E=see\n").permissions();
    assert_eq!(find(&p, "system-bus:talk:a.b.C").risk(), Risk::High);
    assert_eq!(find(&p, "system-bus:own:a.b.D").risk(), Risk::High);
    assert_eq!(find(&p, "system-bus:see:a.b.E").risk(), Risk::Low);
}

#[test]
fn wildcard_bus_names() {
    let p = meta("[Session Bus Policy]\norg.gtk.vfs.*=talk\n").permissions();
    assert_eq!(p[0].code(), "session-bus:talk:org.gtk.vfs.*");
    assert_eq!(p[0].describe(), "Can talk to all org.gtk.vfs services");
    // Names are compared as written.
    let old = meta("[Session Bus Policy]\norg.kde.*=talk\n");
    let new = meta("[Session Bus Policy]\norg.kde.*=talk\norg.kde.Foo=talk\n");
    assert_eq!(
        codes(&new.added_since(&old)),
        ["session-bus:talk:org.kde.Foo"]
    );
    // A name that isn't a name stays visible as unknown, and High.
    let p = meta("[Session Bus Policy]\norg..kde=talk\n*=talk\n").permissions();
    assert_eq!(p.len(), 2);
    assert!(p.iter().all(|x| x.is_unknown() && x.risk() == Risk::High));
}

#[test]
fn unknown_values_are_kept_and_cleaned() {
    let p = meta(
        "[Context]\nsockets=teleport;\nshared=gossip\\n;\nfuture-key=a;\nfilesystems=relative/dir;home:reset;\n\
         [Session Bus Policy]\norg.example.A=shout\n[Policy Tracker3]\ndbus=org.x;\n",
    )
    .permissions();
    assert_eq!(p.len(), 7, "{:?}", codes(&p));
    assert!(p.iter().all(|x| x.is_unknown() && x.risk() == Risk::High));
    let t = find(&p, "unknown:sockets:teleport");
    assert!(t.describe().contains("teleport"));
    // The control character of `gossip\n` is gone from the wording.
    let g = p.iter().find(|x| x.describe().contains("gossip")).unwrap();
    assert!(
        !g.describe().chars().any(|c| c.is_control()),
        "{}",
        g.describe()
    );
}

#[test]
fn hostile_text_never_reaches_the_wording_raw() {
    let p =
        meta("[Context]\nsockets=a\u{202E}b;\n[Environment]\nX=\u{0007}y\u{202E}\n").permissions();
    for x in p {
        for s in [x.describe().to_string(), x.code().to_string()] {
            assert!(!s.contains('\u{202E}') && !s.contains('\u{0007}'), "{s:?}");
        }
    }
}

#[test]
fn metadata_needs_an_application_or_runtime_group() {
    let e = Permissions::from_metadata(b"[Context]\nshared=network;\n").unwrap_err();
    assert_eq!(e, PermError::NotMetadata);
    assert!(e.to_string().contains("no Application or Runtime"));
    assert!(matches!(
        Permissions::from_metadata(b"[Application]\n\xff"),
        Err(PermError::Unreadable(_))
    ));
    assert!(matches!(
        Permissions::from_metadata(b"not a key file"),
        Err(PermError::Unreadable(_))
    ));
    // No [Context] means no permissions.
    assert!(app("runtime.metadata").permissions().len() == 1);
    assert!(meta("").permissions().is_empty());
    assert_eq!(meta("").max_risk(), None);
}

#[test]
fn filesystem_modes_and_widening() {
    let ro = meta("[Context]\nfilesystems=home:ro;\n");
    let rw = meta("[Context]\nfilesystems=home;\n");
    let create = meta("[Context]\nfilesystems=home:create;\n");
    assert_eq!(codes(&ro.permissions()), ["filesystem:home:ro"]);
    assert_eq!(codes(&rw.permissions()), ["filesystem:home:rw"]);
    assert_eq!(codes(&create.permissions()), ["filesystem:home:create"]);
    assert_eq!(ro.permissions()[0].risk(), Risk::High);
    assert_eq!(rw.permissions()[0].risk(), Risk::High);
    assert_eq!(create.permissions()[0].risk(), Risk::High);
    assert_eq!(codes(&rw.added_since(&ro)), ["filesystem:home:rw"]);
    assert_eq!(codes(&create.added_since(&rw)), ["filesystem:home:create"]);
    assert_eq!(codes(&create.added_since(&ro)), ["filesystem:home:create"]);
    assert!(ro.added_since(&rw).is_empty());
    assert!(rw.added_since(&create).is_empty());
    // Explicit :rw is the default.
    assert_eq!(
        codes(&meta("[Context]\nfilesystems=home:rw;\n").permissions()),
        ["filesystem:home:rw"]
    );
    // Slashes don't make a new path.
    let a = meta("[Context]\nfilesystems=~/x/;\n");
    let b = meta("[Context]\nfilesystems=~//x;\n");
    assert!(a.added_since(&b).is_empty() && b.added_since(&a).is_empty());
}

#[test]
fn a_wider_grant_counts_as_added() {
    let dl = meta("[Context]\nfilesystems=xdg-download;\n");
    let home = meta("[Context]\nfilesystems=home;\n");
    assert_eq!(codes(&home.added_since(&dl)), ["filesystem:home:rw"]);
    // Reported the way Telamon Updater reports it, even though home covers it.
    assert_eq!(
        codes(&dl.added_since(&home)),
        ["filesystem:xdg-download:rw"]
    );

    let names = meta("[Session Bus Policy]\norg.example.A=talk\n");
    let sock = meta("[Context]\nsockets=session-bus;\n[Session Bus Policy]\norg.example.A=talk\n");
    assert_eq!(codes(&sock.added_since(&names)), ["socket:session-bus"]);
    assert!(names.added_since(&sock).is_empty());
}

#[test]
fn bus_levels_are_monotone() {
    let at = |l: &str| meta(&format!("[Session Bus Policy]\norg.example.A={l}\n"));
    let order = ["none", "see", "talk", "own"];
    for (i, a) in order.iter().enumerate() {
        for (j, b) in order.iter().enumerate() {
            let n = at(b).added_since(&at(a)).len();
            assert_eq!(n, usize::from(j > i && *b != "none"), "{a} -> {b}");
        }
    }
}

#[test]
fn taking_away_is_not_added_and_negation_removes() {
    let p = meta("[Context]\nsockets=x11;wayland;!x11;\nfilesystems=home;!home;xdg-music;\nshared=!network;\n")
        .permissions();
    assert_eq!(codes(&p), ["filesystem:xdg-music:rw", "socket:wayland"]);
    // Order matters: a later grant wins.
    let p = meta("[Context]\nsockets=!x11;x11;\n").permissions();
    assert_eq!(codes(&p), ["socket:x11"]);
    let old = meta("[Context]\nsockets=x11;wayland;\nshared=network;\n");
    let new = meta("[Context]\nsockets=wayland;\n");
    assert!(new.added_since(&old).is_empty());
    assert_eq!(
        codes(&old.added_since(&new)),
        ["share:network", "socket:x11"]
    );
}

#[test]
fn overrides_merge_like_flatpak() {
    let base = app("obs.metadata");
    let o = base.with_overrides(&fixture("override-user.ini")).unwrap();
    let p = o.permissions();
    let c = codes(&p);
    // Removed: home, x11, all devices, the Flatpak talk name.
    for gone in [
        "filesystem:home:rw",
        "socket:x11",
        "device:all",
        "session-bus:talk:org.freedesktop.Flatpak",
        "session-bus:talk:org.freedesktop.Notifications",
    ] {
        assert!(!c.contains(&gone), "{gone}");
    }
    // Added or kept.
    for kept in [
        "filesystem:xdg-music:ro",
        "filesystem:xdg-videos:rw",
        "device:dri",
        "socket:wayland",
        "share:network",
        "session-bus:talk:org.kde.StatusNotifierWatcher",
        "env:QT_QPA_PLATFORM",
    ] {
        assert!(c.contains(&kept), "{kept} in {c:?}");
    }
    // Removing things only narrows.
    assert!(o.max_risk().unwrap() <= base.max_risk().unwrap());
    // Only what the override grants is new, and nothing is high.
    let added = o.added_since(&base);
    assert!(
        added.iter().all(|a| a.risk() < Risk::High),
        "{:?}",
        codes(&added)
    );
}

#[test]
fn override_round_trips() {
    let base = app("gimp.metadata");
    // An empty override changes nothing.
    assert_eq!(base.with_overrides(b"").unwrap(), base);
    assert_eq!(base.with_overrides(b"[Context]\n").unwrap(), base);
    // Granting and then removing in two steps comes back.
    let granted = base
        .with_overrides(b"[Context]\nfilesystems=home;\nsockets=ssh-auth;\n")
        .unwrap();
    assert_eq!(
        codes(&granted.added_since(&base)),
        ["filesystem:home:rw", "socket:ssh-auth"]
    );
    let back = granted
        .with_overrides(b"[Context]\nfilesystems=!home;\nsockets=!ssh-auth;\n")
        .unwrap();
    assert_eq!(back, base);
    // Idempotent.
    let twice = granted
        .with_overrides(b"[Context]\nfilesystems=home;\nsockets=ssh-auth;\n")
        .unwrap();
    assert_eq!(twice, granted);
    // A policy replaces, `none` removes.
    let o = base
        .with_overrides(b"[Session Bus Policy]\norg.gtk.vfs.*=own\n")
        .unwrap();
    assert!(codes(&o.permissions()).contains(&"session-bus:own:org.gtk.vfs.*"));
    assert!(!codes(&o.permissions()).contains(&"session-bus:talk:org.gtk.vfs.*"));
    let o = o
        .with_overrides(b"[Session Bus Policy]\norg.gtk.vfs.*=none\n")
        .unwrap();
    assert!(
        !codes(&o.permissions())
            .iter()
            .any(|c| c.contains("org.gtk.vfs"))
    );
    // Runtime stays, and a bad override is an error, not a silent no-op.
    assert!(base.with_overrides(b"junk").is_err());
    assert!(base.with_overrides(b"[Context]\nsockets=!x11;\n").is_ok());
}

#[test]
fn a_new_environment_value_or_runtime_counts() {
    let old = meta("[Environment]\nA=1\n");
    assert!(old.added_since(&old).is_empty());
    assert_eq!(
        codes(&meta("[Environment]\nA=2\n").added_since(&old)),
        ["env:A"]
    );
    assert!(meta("").added_since(&old).is_empty());
    let a = Permissions::from_metadata(
        b"[Application]\nname=x.y\nruntime=org.kde.Platform/x86_64/6.9\n",
    )
    .unwrap();
    let b = Permissions::from_metadata(
        b"[Application]\nname=x.y\nruntime=org.kde.Platform/x86_64/6.10\n",
    )
    .unwrap();
    let c = Permissions::from_metadata(
        b"[Application]\nname=x.y\nruntime=org.gnome.Platform/x86_64/47\n",
    )
    .unwrap();
    assert!(b.added_since(&a).is_empty());
    assert_eq!(codes(&c.added_since(&a)), ["runtime:org.gnome.Platform"]);
    // The runtime isn't listed as a permission of the app.
    assert!(c.permissions().is_empty());
}

#[test]
fn what_cannot_be_read_always_counts() {
    let odd = meta("[Context]\nfilesystems=home;!home:ro;\n");
    let added = odd.added_since(&meta(""));
    assert_eq!(added.iter().filter(|x| x.is_unknown()).count(), 1);
    // The Updater reports what it can't read every time; so does the Store
    // (see the "odd-both-sides" pair). Only a runtime's is compared.
    let again = odd.added_since(&odd);
    assert_eq!(again.len(), 1);
    assert!(again[0].is_unknown());
    let other = meta("[Context]\nfilesystems=home;!home:rw;\n");
    assert_eq!(other.added_since(&odd).len(), 1);
}

#[test]
fn limits_hold() {
    let mut big = String::from("[Application]\nname=a.b\n[Context]\nfilesystems=");
    for i in 0..30_000 {
        big.push_str(&format!("~/d{i};"));
    }
    // Over the key file's value limit: refused, not a huge list.
    assert!(Permissions::from_metadata(big.as_bytes()).is_err());
}

/// What Telamon Updater's `new_permissions` reports for the same pairs
/// (telamon-framework-flatpak's tests) is the least this must report.
#[test]
fn reports_at_least_what_the_updater_does() {
    let text = String::from_utf8(fixture("framework-pairs.txt")).unwrap();
    let mut n = 0;
    for case in text.split("### ").skip(1) {
        let (head, body) = case.split_once('\n').unwrap();
        let (name, min) = head.rsplit_once(' ').unwrap();
        let min: usize = min.parse().unwrap();
        let (old, new) = body.split_once("@@@\n").unwrap();
        let (o, nw) = (
            Permissions::from_metadata(old.as_bytes()),
            Permissions::from_metadata(new.as_bytes()),
        );
        let (Ok(o), Ok(nw)) = (o, nw) else {
            panic!("{name}: a pair the Updater reads must parse here too");
        };
        let ours = nw.added_since(&o);
        assert!(ours.len() >= min, "{name}: {:?}", codes(&ours));
        if min == 0 {
            // Only a first extra-data download is news to us and not to the
            // Updater.
            assert!(
                ours.iter().all(|p| p.code().starts_with("extra-data:")),
                "{name}: {:?}",
                codes(&ours)
            );
        }
        n += 1;
    }
    assert_eq!(n, 27);
}

// ---- filesystem canonicalization and escapes ----------------------------

fn fs_one(item: &str) -> Permission {
    let p = meta(&format!("[Context]\nfilesystems={item};\n")).permissions();
    assert_eq!(p.len(), 1, "{item}: {:?}", codes(&p));
    p.into_iter().next().unwrap()
}

/// The ways one home-relative path can be written.
fn home_spellings(rel: &str) -> Vec<String> {
    let mut v = vec![
        format!("~/{rel}"),
        format!("/home/u/{rel}"),
        format!("/var/home/u/{rel}"),
        format!("/home/someone/./{rel}"),
        format!("~/x/../{rel}"),
        format!("~//{rel}/"),
    ];
    for (home, xdg) in [
        (".config", "xdg-config"),
        (".local/share", "xdg-data"),
        (".cache", "xdg-cache"),
    ] {
        if rel == home {
            v.push(xdg.to_string());
        } else if let Some(r) = rel.strip_prefix(&format!("{home}/")) {
            v.push(format!("{xdg}/{r}"));
        }
    }
    v
}

#[test]
fn home_spellings_are_one_item() {
    for s in [
        "home",
        "~",
        "~/",
        "/home/u",
        "/var/home/u",
        "/root",
        "/var/roothome",
        "/home/u/.",
        "/home/u/x/..",
        "~/a/..",
    ] {
        let p = fs_one(s);
        // Another user's home is its own item, named by its path.
        let want = if s.starts_with("/") && s != "/" && !s.starts_with("/home/u/..") {
            format!("filesystem:{s}:rw")
        } else {
            "filesystem:home:rw".to_string()
        };
        assert_eq!(p.code(), want, "{s}");
        assert_eq!(p.risk(), Risk::High, "{s}");
        assert_eq!(fs_one(&format!("{s}:ro")).risk(), Risk::High, "{s}:ro");
    }
    assert_eq!(fs_one("/root/.ssh").code(), "filesystem:/root/.ssh:rw");
    assert_eq!(fs_one("~/.config/x").code(), "filesystem:xdg-config/x:rw");
    assert_eq!(
        fs_one("~/.local/share/x").code(),
        "filesystem:xdg-data/x:rw"
    );
    assert_eq!(fs_one("~/.cache/x/y").code(), "filesystem:xdg-cache/x/y:rw");
    assert_eq!(
        fs_one("xdg-download/a/../b").code(),
        "filesystem:xdg-download/b:rw"
    );
    assert_eq!(fs_one("/opt//x/./y").code(), "filesystem:/opt/x/y:rw");
    // Flatpak follows $XDG_*: spellings are different grants for the Updater,
    // but one item on screen.
    let a = meta("[Context]\nfilesystems=~/.config/x;\n");
    let b = meta("[Context]\nfilesystems=xdg-config/x;\n");
    assert_eq!(codes(&a.added_since(&b)), ["filesystem:xdg-config/x:rw"]);
    assert_eq!(codes(&b.added_since(&a)), ["filesystem:xdg-config/x:rw"]);
    let both =
        meta("[Context]\nfilesystems=~/.config/x:ro;xdg-config/x;/home/u/.config/x:create;\n");
    assert_eq!(
        codes(&both.permissions()),
        [
            "filesystem:/home/u/.config/x:create",
            "filesystem:xdg-config/x:rw"
        ]
    );
}

#[test]
fn whole_system_and_ancestors_are_high_in_every_mode() {
    for s in [
        "/",
        "/.",
        "/./",
        "/..",
        "/home",
        "/var/home",
        "/home/u/..",
        "/var",
        "/etc",
        "/usr",
        "/usr/share/fonts",
        "/boot",
        "/sysroot",
        "/run",
        "/run/media/u",
        "/var/run/x",
        "/var/lib/flatpak",
        "/run/user",
        "/run/user/1000",
        "/run/user/1000/bus",
        "xdg-run",
        "xdg-run/pipewire-0",
        "xdg-run/../../x",
        "host",
        "host-os",
        "host-etc",
    ] {
        for mode in ["", ":ro", ":create"] {
            if s.contains("../../x") {
                continue;
            }
            let p = fs_one(&format!("{s}{mode}"));
            assert_eq!(p.risk(), Risk::High, "{s}{mode}");
        }
    }
    assert_eq!(fs_one("/").code(), "filesystem:/:rw");
    assert_eq!(fs_one("/./").code(), "filesystem:/:rw");
    assert_eq!(
        fs_one("/run/user/1000/bus").code(),
        "filesystem:/run/user/1000/bus:rw"
    );
    // Not system places.
    assert_eq!(fs_one("/data/x").risk(), Risk::Medium);
    assert_eq!(fs_one("/data/x:ro").risk(), Risk::Low);
    // Mounted disks, Nix and the like lead out of the sandbox.
    for s in [
        "/mnt",
        "/mnt/data",
        "/media/u/disk",
        "/opt/x",
        "/srv",
        "/lib32",
        "/libx32",
        "/nix/store",
    ] {
        for mode in ["", ":ro", ":create"] {
            assert_eq!(
                fs_one(&format!("{s}{mode}")).risk(),
                Risk::High,
                "{s}{mode}"
            );
        }
    }
}

#[test]
fn credential_folders_are_high_in_every_mode_and_spelling() {
    for rel in [
        ".ssh",
        ".gnupg",
        ".aws",
        ".mozilla",
        ".local/share/keyrings",
        ".local/share/kwalletd",
        ".ssh/id_ed25519",
        ".netrc",
        ".git-credentials",
        ".password-store",
        ".docker",
        ".kube",
        ".thunderbird",
        ".pki",
        ".config/gcloud",
        ".config/gh",
        ".config/rclone",
        ".config/chromium",
        ".config/google-chrome",
        ".config/BraveSoftware",
        ".config/Signal",
        ".config/discord",
        ".config/kdeconnect",
    ] {
        for s in home_spellings(rel) {
            for mode in ["", ":ro", ":create"] {
                let p = fs_one(&format!("{s}{mode}"));
                assert_eq!(p.risk(), Risk::High, "{s}{mode}");
                assert!(p.describe().contains("saved passwords and keys"), "{s}");
            }
        }
    }
}

#[test]
fn escape_locations_are_high_when_writable() {
    let paths = [
        ".config/autostart",
        ".config/autostart/x.desktop",
        ".config/systemd/user",
        ".config/environment.d",
        ".config/plasma-workspace/env",
        ".config/plasma-workspace/shutdown",
        ".config/fish",
        ".local/share/flatpak",
        ".local/share/applications",
        ".local/share/dbus-1/services",
        ".local/share/systemd",
        ".local/share/kservices5",
        ".local/share/kservices6",
        ".local/share/plasma",
        ".local/bin",
        "bin",
        ".bashrc",
        ".bash_profile",
        ".profile",
        ".zshrc",
        ".zprofile",
        ".bashrc.d",
        ".bash_logout",
        ".zlogin",
        ".zlogout",
        ".xprofile",
        ".xinitrc",
        ".xsession",
        ".xsessionrc",
        ".pam_environment",
        ".vimrc",
        ".vim",
        ".config/nvim",
        ".emacs.d",
        ".config/autostart-scripts",
        ".local/share/kwin",
        ".local/share/kio",
        ".config/kglobalshortcutsrc",
        ".config/khotkeysrc",
        ".config/konsolerc",
        ".local/share/konsole",
        ".config/kdeglobals",
        ".local/lib",
        ".gitconfig",
        ".config/git",
        ".cargo/bin",
        ".var/app",
        // The bare folders and what holds them.
        ".config",
        ".local/share",
        ".local",
    ];
    for rel in paths {
        for s in home_spellings(rel) {
            for mode in ["", ":rw", ":create"] {
                let p = fs_one(&format!("{s}{mode}"));
                assert_eq!(p.risk(), Risk::High, "{s}{mode}");
                // A folder that holds an escape location says so.
                assert!(
                    p.describe()
                        .starts_with("Can add programs that run outside its sandbox")
                        || p.describe().contains("everything in"),
                    "{s}: {}",
                    p.describe()
                );
            }
            // Read-only is no way out; the bare folders still show.
            let ro = fs_one(&format!("{s}:ro")).risk();
            // They hold credential folders, so even reading them is High.
            let holds =
                [".config", ".local/share", ".local"].contains(&rel) || rel.starts_with(".var");
            // An unlisted dot entry of the home folder is Medium to read.
            let first = rel.split('/').next().unwrap();
            let dot = first.starts_with('.') && ![".config", ".local", ".cache"].contains(&first);
            let want = if holds {
                Risk::High
            } else if dot {
                Risk::Medium
            } else {
                Risk::Low
            };
            assert_eq!(ro, want, "{s}:ro");
        }
    }
    // Other places under them are ordinary.
    for (s, rw, ro) in [
        ("xdg-config/GIMP", Risk::Medium, Risk::Low),
        ("~/.config/GIMP", Risk::Medium, Risk::Low),
        ("xdg-cache/thumbnails", Risk::Medium, Risk::Low),
        ("xdg-data/gimp", Risk::Medium, Risk::Low),
        ("~/Documents", Risk::Medium, Risk::Low),
        ("xdg-download", Risk::Medium, Risk::Low),
        ("~/.local/state", Risk::Medium, Risk::Low),
    ] {
        assert_eq!(fs_one(s).risk(), rw, "{s}");
        assert_eq!(fs_one(&format!("{s}:ro")).risk(), ro, "{s}:ro");
    }
}

#[test]
fn a_dotdot_that_leaves_its_base_is_unknown_and_high() {
    for s in [
        "~/..",
        "~/../x",
        "xdg-download/..",
        "xdg-download/../x",
        "xdg-config/../.ssh",
        "xdg-run/..",
        "~/a/../../b",
        "relative/dir",
        "xdg-bogus",
        "~user/x",
        "home:reset",
        "a\\:rw",
    ] {
        let p = fs_one(s);
        assert!(
            p.is_unknown() && p.risk() == Risk::High,
            "{s}: {}",
            p.code()
        );
    }
}

// ---- other items --------------------------------------------------------

#[test]
fn persistent_paths() {
    let p = meta("[Context]\npersistent=.mozilla;a/b;\n").permissions();
    assert_eq!(codes(&p), ["persistent:.mozilla", "persistent:a/b"]);
    assert_eq!(
        p[0].describe(),
        "Keeps its own copy of \".mozilla\" in the app's private data"
    );
    assert!(p.iter().all(|x| x.risk() == Risk::Low));
    for v in [
        "/", "/etc", "..", "a/../b", "../x", ".", "~", "My App", "a:b",
    ] {
        let p = meta(&format!("[Context]\npersistent={v};\n")).permissions();
        assert_eq!(p.len(), 1, "{v}");
        assert!(
            p[0].is_unknown() && p[0].risk() == Risk::High,
            "{v}: {}",
            p[0].code()
        );
    }
}

#[test]
fn localized_keys_always_count() {
    let m = meta(
        "[Context]\nshared[de]=network;\n[Session Bus Policy]\norg.x.A[de]=talk\n[Environment]\nA[fr]=b\n",
    );
    let p = m.permissions();
    assert_eq!(p.len(), 3, "{:?}", codes(&p));
    assert!(p.iter().all(|x| x.is_unknown() && x.risk() == Risk::High));
    // Counted every time, as the Updater does.
    assert_eq!(m.added_since(&meta("")).len(), 3);
    assert_eq!(m.added_since(&m).len(), 3);
}

#[test]
fn extra_data_hosts() {
    let with = |uri: &str| meta(&format!("[Extra Data]\nname=a.deb\nsize=1\nuri={uri}\n"));
    let a = with("https://Dl.Example.org:8443/x/2.deb");
    let p = a.permissions();
    assert_eq!(codes(&p), ["extra-data:dl.example.org"]);
    assert_eq!(p[0].risk(), Risk::Medium);
    assert_eq!(
        p[0].describe(),
        "Downloads extra files from dl.example.org when installed"
    );
    // A new release from the same host is not news; another host is.
    let b = with("https://dl.example.org/y/3.deb");
    assert!(b.added_since(&a).is_empty());
    let c = with("https://evil.example.net/3.deb");
    assert_eq!(codes(&c.added_since(&a)), ["extra-data:evil.example.net"]);
    assert!(meta("").added_since(&a).is_empty());
    // A URL without a plain host is unknown.
    for bad in [
        "not a url",
        "file:///x",
        "https://[::1]/x",
        "https://a b/x",
        "https://exa..mple/",
    ] {
        let p = with(bad).permissions();
        assert_eq!(p.len(), 1, "{bad}");
        assert!(p[0].is_unknown() && p[0].risk() == Risk::High, "{bad}");
    }
}

#[test]
fn runtime_context_comes_first() {
    let rt = Permissions::from_metadata(
        b"[Runtime]\nname=org.kde.Platform\n[Context]\nsockets=x11;pulseaudio;\nshared=network;\n[Environment]\nA=runtime\nB=runtime\n",
    )
    .unwrap();
    let a = meta(
        "[Context]\nsockets=!x11;wayland;\n[Environment]\nA=app\n[Session Bus Policy]\norg.x.A=talk\n",
    );
    let both = a.with_runtime(&rt);
    let c = codes(&both.permissions()).join(" ");
    assert!(!c.contains("socket:x11"), "{c}");
    for want in [
        "socket:pulseaudio",
        "share:network",
        "socket:wayland",
        "session-bus:talk:org.x.A",
        "env:B",
    ] {
        assert!(c.contains(want), "{want} in {c}");
    }
    let env = both
        .permissions()
        .into_iter()
        .find(|x| x.code() == "env:A")
        .unwrap();
    assert!(env.describe().contains("\"app\""), "{}", env.describe());
    // The runtime's grants are not the app's own: they show up as added
    // against the bare app.
    assert_eq!(
        codes(&both.added_since(&a)),
        ["share:network", "socket:pulseaudio", "env:B"]
    );
    // Overrides still apply on top.
    let o = both
        .with_overrides(b"[Context]\nshared=!network;\n")
        .unwrap();
    assert!(!codes(&o.permissions()).contains(&"share:network"));
}

#[test]
fn overrides_apply_in_flatpaks_order() {
    let base = app("gimp.metadata");
    let sys_global = b"[Context]\nfilesystems=home;\nsockets=ssh-auth;\n";
    let sys_app = b"[Context]\nfilesystems=!home;\n";
    let user_global = b"[Context]\nfilesystems=xdg-music;\n[Session Bus Policy]\norg.x.A=own\n";
    let user_app = b"[Context]\nfilesystems=!xdg-music;\nsockets=!ssh-auth;\n[Session Bus Policy]\norg.x.A=none\n";
    let mut cur = base.clone();
    let mut seen = Vec::new();
    for o in [&sys_global[..], sys_app, user_global, user_app] {
        cur = cur.with_overrides(o).unwrap();
        seen.push(codes(&cur.permissions()).join(" "));
    }
    assert!(seen[0].contains("filesystem:home:rw") && seen[0].contains("socket:ssh-auth"));
    assert!(!seen[1].contains("filesystem:home") && seen[1].contains("socket:ssh-auth"));
    assert!(
        seen[2].contains("filesystem:xdg-music:rw") && seen[2].contains("session-bus:own:org.x.A")
    );
    assert_eq!(cur, base);
    // The order matters: a later grant beats an earlier removal.
    let swapped = base
        .with_overrides(sys_app)
        .unwrap()
        .with_overrides(sys_global)
        .unwrap();
    assert!(codes(&swapped.permissions()).contains(&"filesystem:home:rw"));
}

#[test]
fn shown_values_are_quoted_safely_and_cut_with_an_ellipsis() {
    let p = meta("[Environment]\nX=say \"hi\" \"\n").permissions();
    let d = p[0].describe();
    assert_eq!(d, "Sets the environment variable X to \"say 'hi' '\"");
    let long = "a".repeat(300);
    let p = meta(&format!("[Context]\nfilesystems=~/{long};\n")).permissions();
    assert!(p[0].describe().contains("…\""), "{}", p[0].describe());
    assert!(p[0].describe().chars().count() < 200);
    // The code keeps the whole value, so two long values stay distinct.
    assert!(p[0].code().contains(&long));
    let other = format!("{}b", "a".repeat(300));
    let q = meta(&format!("[Context]\nfilesystems=~/{other};\n")).permissions();
    assert_ne!(p[0].code(), q[0].code());
    let l1 = format!("{}x", "p".repeat(200));
    let l2 = format!("{}y", "p".repeat(200));
    let r = meta(&format!("[Context]\npersistent={l1};{l2};\n")).permissions();
    assert_ne!(r[0].code(), r[1].code());
    // Unknown values too, with a hex form for what isn't plain.
    let u = meta("[Context]\nsockets=a b;\n").permissions();
    assert!(
        u[0].is_unknown() && u[0].code().contains("hex "),
        "{}",
        u[0].code()
    );
}

#[test]
fn new_wording() {
    let p = meta("[Context]\ndevices=input;\nsockets=session-bus;system-bus;\nshared=network;\n")
        .permissions();
    assert_eq!(
        find(&p, "device:input").describe(),
        "Can read all keyboard, mouse and controller input, including what you type in other apps"
    );
    assert_eq!(
        find(&p, "share:network").describe(),
        "Can access the internet, your local network and services running on this computer"
    );
    for c in ["socket:session-bus", "socket:system-bus"] {
        assert!(find(&p, c).describe().contains("break out of its sandbox"));
    }
}

#[test]
fn parents_of_credentials_are_high_even_read_only() {
    for s in [
        "xdg-config:ro",
        "xdg-data:ro",
        "~/.local/share:ro",
        "~/.local:ro",
        "~/.config:ro",
        "/home/u/.local:ro",
    ] {
        assert_eq!(fs_one(s).risk(), Risk::High, "{s}");
        assert!(
            fs_one(s).describe().contains("saved passwords and keys"),
            "{s}"
        );
    }
    // The cache holds none: read-only is Medium, writing is High but isn't
    // worded as an escape.
    assert_eq!(fs_one("xdg-cache:ro").risk(), Risk::Medium);
    let c = fs_one("xdg-cache");
    assert_eq!(c.risk(), Risk::High);
    assert!(
        !c.describe().contains("run outside its sandbox"),
        "{}",
        c.describe()
    );
    assert!(c.describe().contains("cache folder"));
}

#[test]
fn unlisted_dot_entries_in_home_are_high_when_writable() {
    for s in [
        "~/.unknownrc",
        "~/.tool/plugins",
        "/home/u/.mytool",
        "~/.fish_functions/x",
    ] {
        for mode in ["", ":create"] {
            let p = fs_one(&format!("{s}{mode}"));
            assert_eq!(p.risk(), Risk::High, "{s}{mode}");
            assert_eq!(p.describe(), "Can change settings that other programs run");
        }
        assert_eq!(fs_one(&format!("{s}:ro")).risk(), Risk::Medium, "{s}:ro");
    }
    // The short allowlist, plain folders, and the xdg folders stay ordinary.
    for s in [
        "~/.themes",
        "~/.icons",
        "~/.fonts",
        "~/Documents",
        "~/.config/GIMP",
        "~/.local/share/gimp",
        "~/.cache/x",
    ] {
        assert_eq!(fs_one(s).risk(), Risk::Medium, "{s}");
    }
}

#[test]
fn tmp_is_high_in_every_mode() {
    for s in [
        "/tmp",
        "/tmp/.X11-unix",
        "/tmp/x/y",
        "/tmp/../tmp",
        "/var/tmp/../../tmp/a",
    ] {
        for mode in ["", ":ro", ":create"] {
            let p = fs_one(&format!("{s}{mode}"));
            assert_eq!(p.risk(), Risk::High, "{s}{mode}");
            assert!(p.describe().contains("temporary folder"), "{s}");
        }
    }
    assert_eq!(fs_one("/tmp").code(), "filesystem:/tmp:rw");
    assert_eq!(fs_one("/tmpx").risk(), Risk::Medium);
}

#[test]
fn identity_is_the_literal_text() {
    let add = |old: &str, new: &str| {
        let o = meta(&format!("[Context]\nfilesystems={old};\n"));
        let n = meta(&format!("[Context]\nfilesystems={new};\n"));
        n.added_since(&o)
    };
    // Another user's home is not yours, nor are the other homes.
    for (new, who) in [
        ("/home/bob", "another user's home folder (bob)"),
        ("/var/home/x", "another user's home folder (x)"),
        ("/root", "root's home folder"),
        ("/var/roothome", "root's home folder"),
    ] {
        let a = add("home", new);
        assert_eq!(codes(&a), [format!("filesystem:{new}:rw")], "{new}");
        assert_eq!(a[0].risk(), Risk::High);
        assert!(a[0].describe().contains(who), "{new}: {}", a[0].describe());
        assert!(!a[0].describe().contains("your home folder"), "{new}");
    }
    let sub = fs_one("/home/bob/Documents");
    assert_eq!(
        sub.describe(),
        "Can read and change files in \"Documents\" in another user's home folder (bob)"
    );
    // $XDG_* can point anywhere.
    let a = add("xdg-run", "/run/user/1001");
    assert_eq!(codes(&a), ["filesystem:/run/user/1001:rw"]);
    assert!(
        a[0].describe().contains("a user's runtime folder"),
        "{}",
        a[0].describe()
    );
    assert!(
        fs_one("/run/user")
            .describe()
            .contains("every user's runtime folder")
    );
    assert!(
        fs_one("/run/user/7/x")
            .describe()
            .contains("a user's runtime folder")
    );
    assert!(
        fs_one("xdg-run/x")
            .describe()
            .contains("the runtime folder of your session")
    );
    assert_eq!(
        codes(&add("xdg-config", "~/.config")),
        ["filesystem:xdg-config:rw"]
    );
    assert_eq!(
        codes(&add("xdg-data", "~/.local/share")),
        ["filesystem:xdg-data:rw"]
    );
    assert_eq!(
        codes(&add("xdg-cache", "/home/u/.cache")),
        ["filesystem:/home/u/.cache:rw"]
    );
    // Only the same text is the same grant.
    assert!(add("~/x", "~//x/").is_empty());
    assert!(add("home", "home:ro").is_empty());
    assert_eq!(codes(&add("home:ro", "home")), ["filesystem:home:rw"]);
    // A removal needs the same literal.
    for rm in [
        "!/home/zach",
        "!~/x/..",
        "!/root",
        "!~",
        "!xdg-download/..",
        "!/home/u",
    ] {
        let p = meta(&format!("[Context]\nfilesystems=home;{rm};\n")).permissions();
        assert!(
            p.iter().any(|x| x.code() == "filesystem:home:rw"),
            "home gone after {rm}: {:?}",
            codes(&p)
        );
    }
    let p = meta("[Context]\nfilesystems=home;!home;\n").permissions();
    assert!(p.is_empty());
    let p = meta("[Context]\nfilesystems=~/x;!~//x/;\n").permissions();
    assert!(p.is_empty());
    // Several spellings of one place show once, at the highest access.
    let p = meta("[Context]\nfilesystems=~/.config/x:ro;xdg-config/x;\n").permissions();
    assert_eq!(codes(&p), ["filesystem:xdg-config/x:rw"]);
    // Spellings that say something else stay apart, each with its own words.
    let p = meta("[Context]\nfilesystems=home;/home/bob;/root;\n").permissions();
    assert_eq!(p.len(), 3, "{:?}", codes(&p));
    let words: HashSet<String> = p.iter().map(|x| x.describe().to_string()).collect();
    assert_eq!(words.len(), 3, "{words:?}");
    let ids: HashSet<&str> = p.iter().map(|x| x.code()).collect();
    assert_eq!(ids.len(), 3);
    let p = meta("[Context]\nfilesystems=xdg-run;/run/user/1001;\n").permissions();
    assert_eq!(p.len(), 2, "{:?}", codes(&p));
    assert_ne!(p[0].describe(), p[1].describe());
    // The aliases of one place all count in an update, as in the Updater's.
    let o = meta("[Context]\nfilesystems=home;\n");
    let n = meta("[Context]\nfilesystems=home;/home/bob;\n");
    assert_eq!(codes(&n.added_since(&o)), ["filesystem:/home/bob:rw"]);
    assert!(o.added_since(&n).is_empty());
}

#[test]
fn more_session_names_are_high() {
    for name in [
        "org.kde.krunner",
        "org.kde.konsole",
        "org.kde.konsole-12345",
        "org.kde.yakuake",
        "org.kde.kglobalaccel",
        "org.gnome.SettingsDaemon.Power",
        "org.gnome.SettingsDaemon.*",
        "org.gnome.Mutter.ScreenCast",
        "org.gnome.Mutter.*",
    ] {
        for level in ["talk", "own"] {
            let p = meta(&format!("[Session Bus Policy]\n{name}={level}\n")).permissions();
            assert_eq!(p[0].risk(), Risk::High, "{name} {level}");
            assert!(!p[0].is_unknown(), "{name}");
        }
        let p = meta(&format!("[Session Bus Policy]\n{name}=see\n")).permissions();
        assert_eq!(p[0].risk(), Risk::Low, "{name} see");
    }
}

#[test]
fn accessibility_bus_is_high() {
    for group in ["Accessibility Bus Policy", "A11y Bus Policy"] {
        for (level, risk) in [
            ("talk", Risk::High),
            ("own", Risk::High),
            ("see", Risk::Low),
        ] {
            let p = meta(&format!("[{group}]\norg.a11y.atspi.Registry={level}\n")).permissions();
            assert_eq!(p[0].risk(), risk, "{group} {level}");
        }
        let p = meta(&format!("[{group}]\norg.a11y.Bus=talk\n")).permissions();
        assert_eq!(p[0].describe(), "Can read every window and control input");
    }
}

#[test]
fn game_and_launcher_folders_run_code_on_the_host() {
    for rel in [
        ".steam",
        ".wine",
        ".minecraft",
        ".local/share/Steam",
        ".local/share/lutris",
        ".wine/drive_c/x",
    ] {
        for sp in home_spellings(rel) {
            for mode in ["", ":create"] {
                let p = fs_one(&format!("{sp}{mode}"));
                assert_eq!(p.risk(), Risk::High, "{sp}{mode}");
                assert!(
                    p.describe()
                        .starts_with("Can add programs that run outside its sandbox"),
                    "{sp}: {}",
                    p.describe()
                );
            }
        }
    }
    assert!(fs_one("~/.steam").describe().contains("(Steam)"));
    assert!(fs_one("~/.wine").describe().contains("(Wine)"));
    assert!(fs_one("~/.minecraft").describe().contains("(Minecraft)"));
    assert_eq!(fs_one("xdg-data/Steam").risk(), Risk::High);
    for s in ["~/.themes", "~/.icons", "~/.fonts"] {
        assert_eq!(fs_one(s).risk(), Risk::Medium, "{s}");
    }
}

#[test]
fn other_apps_data_and_more_credentials_are_high_to_read() {
    for s in [
        "~/.var/app:ro",
        "~/.var:ro",
        "~/.var/app/org.x.Y:ro",
        "/home/u/.var/app/org.x.Y:ro",
    ] {
        assert_eq!(fs_one(s).risk(), Risk::High, "{s}");
    }
    for s in [
        "~/.npmrc",
        "~/.pypirc",
        "~/.azure",
        "~/.bash_history",
        "~/.oci",
        "~/.boto",
    ] {
        assert!(fs_one(&format!("{s}:ro")).risk() >= Risk::Medium, "{s}:ro");
    }
    for rel in [
        ".config/mozilla",
        ".config/vivaldi",
        ".config/microsoft-edge",
        ".config/opera",
        ".config/containers",
        ".config/Element",
        ".config/1Password",
        ".config/Bitwarden",
        ".config/keepassxc",
    ] {
        for sp in home_spellings(rel) {
            assert_eq!(fs_one(&format!("{sp}:ro")).risk(), Risk::High, "{sp}:ro");
        }
    }
}

#[test]
fn more_escape_locations() {
    for rel in [
        ".local/share/bash-completion",
        ".local/share/fish",
        ".local/share/nautilus-python",
        ".local/share/nautilus/scripts",
        "go/bin",
        ".config/mimeapps.list",
        ".config/plasma-org.kde.plasma.desktop-appletsrc",
    ] {
        for sp in home_spellings(rel) {
            let p = fs_one(&sp);
            assert_eq!(p.risk(), Risk::High, "{sp}");
            assert!(
                p.describe()
                    .starts_with("Can add programs that run outside its sandbox"),
                "{sp}: {}",
                p.describe()
            );
        }
    }
}

#[test]
fn a_parent_names_the_whole_folder() {
    let p = fs_one("xdg-config:ro");
    assert_eq!(p.risk(), Risk::High);
    assert!(
        p.describe()
            .starts_with("Can read everything in your settings folder (~/.config), including "),
        "{}",
        p.describe()
    );
    let p = fs_one("~/.local:ro");
    assert!(p.describe().contains("everything in"), "{}", p.describe());
    let p = fs_one("xdg-data");
    assert!(
        p.describe()
            .starts_with("Can read and change everything in your data folder"),
        "{}",
        p.describe()
    );
    // Another user's home is never "your" folder.
    for s in [
        "/home/bob/.config:ro",
        "/home/bob/.ssh:ro",
        "/home/bob/.config",
        "/root/.local:ro",
        "/home/bob/.mozilla",
    ] {
        let d = fs_one(s).describe().to_string();
        assert!(!d.contains("your"), "{s}: {d}");
        assert!(d.contains("home folder"), "{s}: {d}");
    }
}

#[test]
fn an_unknown_bus_policy_group_is_unknown_high() {
    let p = meta("[Foo Bus Policy]\norg.x.A=talk\norg.x.B=see\n").permissions();
    assert_eq!(p.len(), 2);
    assert!(
        p.iter().all(|x| x.is_unknown() && x.risk() == Risk::High),
        "{:?}",
        codes(&p)
    );
    // Known groups are unchanged.
    assert!(!meta("[Session Bus Policy]\norg.x.A=talk\n").permissions()[0].is_unknown());
}

#[test]
fn kdeconnect_and_ksmserver_are_high() {
    for name in [
        "org.kde.kdeconnect",
        "org.kde.kdeconnect.daemon",
        "org.kde.ksmserver",
        "org.kde.kdeconnect.*",
    ] {
        for level in ["talk", "own"] {
            let p = meta(&format!("[Session Bus Policy]\n{name}={level}\n")).permissions();
            assert_eq!(p[0].risk(), Risk::High, "{name} {level}");
        }
    }
}

#[test]
fn a_riskier_grant_counts_as_added() {
    for sock in ["x11", "fallback-x11"] {
        let old = meta(&format!("[Context]\nsockets=wayland;{sock};\n"));
        let new = meta(&format!("[Context]\nsockets={sock};\n"));
        let before = find(&old.permissions(), &format!("socket:{sock}")).risk();
        let added = new.added_since(&old);
        assert_eq!(codes(&added), [format!("socket:{sock}")], "{sock}");
        assert!(added[0].risk() > before, "{sock}");
        // The other way it gets safer, which is not news.
        assert!(
            !codes(&old.added_since(&new)).contains(&format!("socket:{sock}").as_str()),
            "{sock}"
        );
    }
}

#[test]
fn paths_with_spaces_and_letters_are_known() {
    for (item, shown) in [
        ("~/My Games", "My Games"),
        ("xdg-documents/Música", "Música"),
        ("~/Документы", "Документы"),
        ("~/Dokumente/Über uns:ro", "Über uns"),
        ("/data/Ünï cödé", "Ünï cödé"),
    ] {
        let p = fs_one(item);
        assert!(!p.is_unknown(), "{item}");
        assert!(p.describe().contains(shown), "{item}: {}", p.describe());
        assert!(p.code().is_ascii(), "{item}: {}", p.code());
    }
    assert_eq!(fs_one("~/My Games").risk(), Risk::Medium);
    assert_eq!(fs_one("~/My Games:ro").risk(), Risk::Low);
    let a = meta("[Context]\nfilesystems=~/Música;\n");
    assert!(a.added_since(&a).is_empty());
    assert_eq!(
        meta("[Context]\nfilesystems=~/Musica;\n")
            .added_since(&a)
            .len(),
        1
    );
    assert_eq!(fs_one("~/Games").code(), "filesystem:~/Games:rw");
    // What could hide the real path is refused: unknown, High, ASCII code.
    for bad in [
        "~/a\u{202E}b",
        "~/a\u{200B}b",
        "~/a\u{200D}b",
        "~/a\u{FEFF}b",
        "~/a\u{2028}b",
        "~/a\u{0085}b",
        "~/a\u{0007}b",
        "~/a\\b",
        "\u{00A0}~/x",
        "~/x\u{00A0}",
    ] {
        let p = fs_one(bad);
        assert!(
            p.is_unknown() && p.risk() == Risk::High,
            "{bad:?}: {}",
            p.code()
        );
        assert!(p.code().is_ascii(), "{bad:?}");
    }
}

#[test]
fn a_runtimes_odd_items_do_not_flag_every_update() {
    let rt = Permissions::from_metadata(
        b"[Runtime]\nname=org.x.Platform\n[Context]\nshared[de]=network;\n",
    )
    .unwrap();
    let old = meta("[Context]\nsockets=wayland;\n").with_runtime(&rt);
    let new = meta("[Context]\nsockets=wayland;\nshared=ipc;\n").with_runtime(&rt);
    assert_eq!(new.permissions().len(), 3);
    assert_eq!(codes(&new.added_since(&old)), ["share:ipc"]);
    // A runtime that gets one is news.
    let plain = Permissions::from_metadata(b"[Runtime]\nname=org.x.Platform\n").unwrap();
    let a = meta("[Context]\nsockets=wayland;\n");
    let added = a.with_runtime(&rt).added_since(&a.with_runtime(&plain));
    assert_eq!(added.len(), 1);
    assert!(added[0].is_unknown());
}

#[test]
fn xdg_run_wordings() {
    let p = fs_one("xdg-run/pipewire-0");
    assert_eq!(p.risk(), Risk::High);
    assert_eq!(
        p.describe(),
        "Can use PipeWire directly, which can record audio and the screen without asking"
    );
    // Read-only these are Medium; writing could replace the socket.
    for (s, text) in [
        ("xdg-run/speech-dispatcher", "Can use the speech service"),
        (
            "xdg-run/gvfs",
            "Can use the file manager's network and device mounts",
        ),
        (
            "xdg-run/app/org.x.Y",
            "Shares files with org.x.Y while it runs",
        ),
    ] {
        let p = fs_one(&format!("{s}:ro"));
        assert_eq!((p.risk(), p.describe()), (Risk::Medium, text), "{s}:ro");
        for mode in ["", ":create"] {
            assert_eq!(
                fs_one(&format!("{s}{mode}")).risk(),
                Risk::High,
                "{s}{mode}"
            );
        }
    }
    for mode in ["", ":ro", ":create"] {
        let p = fs_one(&format!("xdg-run/gvfsd{mode}"));
        assert_eq!(p.risk(), Risk::High, "gvfsd{mode}");
        let verb = match mode {
            ":ro" => "read",
            ":create" => "read, change and create",
            _ => "read and change",
        };
        assert_eq!(
            p.describe(),
            format!("Can {verb} files on your network shares and cloud drives, and mount new ones")
        );
    }
    for id in [
        "org.keepassxc.KeePassXC",
        "com.bitwarden.desktop",
        "im.riot.Riot",
        "org.signal.Signal",
        "com.onepassword.OnePassword",
        "ORG.SIGNAL.SIGNAL",
    ] {
        for mode in ["", ":ro", ":create"] {
            let p = fs_one(&format!("xdg-run/app/{id}{mode}"));
            assert_eq!(p.risk(), Risk::High, "{id}{mode}");
            assert!(p.describe().contains("holds your passwords"), "{id}");
        }
    }
    // An ID that isn't one is High even read-only.
    for s in [
        "xdg-run/app/not-an-id:ro",
        "xdg-run/app/a b.c:ro",
        "xdg-run/app/x..y:ro",
    ] {
        assert_eq!(fs_one(s).risk(), Risk::High, "{s}");
    }
    for s in [
        "xdg-run",
        "xdg-run/bus",
        "xdg-run/app",
        "xdg-run/x/pipewire-0",
        "/run/user/1000/gvfsd",
        "/run/user/1000/pipewire-0",
    ] {
        assert_eq!(fs_one(s).risk(), Risk::High, "{s}");
    }
}

#[test]
fn environment_and_unset_environment_share_one_map() {
    let p = meta("[Context]\nunset-environment=A;\n[Environment]\nA=1\n").permissions();
    assert_eq!(codes(&p), ["env:A"]);
    let base = meta("[Environment]\nA=1\nB=2\n");
    let o = base
        .with_overrides(b"[Context]\nunset-environment=A;\n")
        .unwrap();
    assert_eq!(codes(&o.permissions()), ["env:B", "unset-env:A"]);
    let o = o.with_overrides(b"[Environment]\nA=3\n").unwrap();
    assert_eq!(codes(&o.permissions()), ["env:A", "env:B"]);
    let o = o
        .with_overrides(b"[Context]\nunset-environment=!A;\n")
        .unwrap();
    assert_eq!(codes(&o.permissions()), ["env:A", "env:B"]);
}

#[test]
fn bus_levels_are_not_trimmed() {
    let p = meta("[Session Bus Policy]\norg.x.A=talk x\norg.x.B=Talk\n").permissions();
    assert!(
        p.iter().all(|x| x.is_unknown() && x.risk() == Risk::High),
        "{:?}",
        codes(&p)
    );
    let a = meta("[Session Bus Policy]\norg.x.A=shout\n");
    let b = meta("[Session Bus Policy]\norg.x.A=scream\n");
    assert_eq!(b.added_since(&a).len(), 1);
    assert!(a.added_since(&a).is_empty());
}

#[test]
fn a_runtime_or_sdk_that_cannot_be_read_is_odd() {
    let m = Permissions::from_metadata(
        b"[Application]\nname=a.b\nruntime=org.x.Platform\\q/x86_64/1\nsdk=org.x.Sdk\\;/x86_64/1\n",
    )
    .unwrap();
    let p = m.permissions();
    assert_eq!(p.len(), 2, "{:?}", codes(&p));
    assert!(p.iter().all(|x| x.is_unknown() && x.risk() == Risk::High));
}

#[test]
fn runtime_and_overrides_replay_in_both_orders() {
    let rt = Permissions::from_metadata(
        b"[Runtime]\nname=org.x.Platform\n[Context]\nshared=network;\nsockets=x11;\n[Environment]\nA=rt\n",
    )
    .unwrap();
    let app = meta("[Context]\nsockets=wayland;\n[Environment]\nB=app\n");
    let o1 = b"[Context]\nshared=!network;\n";
    let o2 = b"[Environment]\nA=user\n[Context]\nsockets=!x11;\n";
    let a = app
        .with_runtime(&rt)
        .with_overrides(o1)
        .unwrap()
        .with_overrides(o2)
        .unwrap();
    let b = app
        .with_overrides(o1)
        .unwrap()
        .with_overrides(o2)
        .unwrap()
        .with_runtime(&rt);
    assert_eq!(a, b);
    assert_eq!(codes(&a.permissions()), codes(&b.permissions()));
    let c = codes(&a.permissions()).join(" ");
    assert!(
        !c.contains("share:network") && !c.contains("socket:x11"),
        "{c}"
    );
    assert!(
        c.contains("env:A") && c.contains("env:B") && c.contains("socket:wayland"),
        "{c}"
    );
    let env = a
        .permissions()
        .into_iter()
        .find(|x| x.code() == "env:A")
        .unwrap();
    assert!(env.describe().contains("\"user\""));
}

#[test]
fn unset_environment_is_kept_as_written_for_updates() {
    // Hidden from the list when `[Environment]` overrides it...
    let both = meta("[Context]\nunset-environment=A;\n[Environment]\nA=1\n");
    assert_eq!(codes(&both.permissions()), ["env:A"]);
    // ...but an update that adds it beside an existing variable is news.
    let old = meta("[Environment]\nA=1\n");
    assert_eq!(codes(&both.added_since(&old)), ["unset-env:A"]);
    // And one that comes after the variable shows.
    let later = old
        .with_overrides(b"[Context]\nunset-environment=A;\n")
        .unwrap();
    assert_eq!(codes(&later.permissions()), ["unset-env:A"]);
}

#[test]
fn equality_ignores_empty_leftovers_and_history() {
    let e = meta("");
    assert_eq!(e.with_overrides(b"[Context]\nsockets=!x11;\n").unwrap(), e);
    assert_eq!(
        e.with_overrides(b"[Session Bus Policy]\norg.x.A=none\n")
            .unwrap(),
        e
    );
    let x = meta("[Context]\nsockets=x11;\n");
    assert_eq!(x.with_overrides(b"[Context]\nsockets=!x11;\n").unwrap(), e);
    let b = meta("[Session Bus Policy]\norg.x.A=talk\n");
    assert_eq!(
        b.with_overrides(b"[Session Bus Policy]\norg.x.A=none\n")
            .unwrap(),
        e
    );
    // An unset that `[Environment]` then overrides is the same as no unset.
    let u = e
        .with_overrides(b"[Context]\nunset-environment=A;\n")
        .unwrap()
        .with_overrides(b"[Environment]\nA=3\n")
        .unwrap();
    assert_eq!(u, meta("[Environment]\nA=3\n"));
    assert_ne!(u, meta("[Environment]\nA=4\n"));
}

#[test]
fn an_empty_runtime_or_sdk_id_is_odd() {
    for v in ["runtime=\n", "runtime=/x86_64/1\n", "sdk=\n", "sdk=/x/1\n"] {
        let m =
            Permissions::from_metadata(format!("[Application]\nname=a.b\n{v}").as_bytes()).unwrap();
        let p = m.permissions();
        assert_eq!(p.len(), 1, "{v}: {:?}", codes(&p));
        assert!(p[0].is_unknown() && p[0].risk() == Risk::High, "{v}");
    }
}

#[test]
fn odd_codes_say_what_was_odd() {
    // An odd list item, an odd value and an odd key never share a code.
    let m = meta("[Context]\nsockets=a b;\nshared=x\\q\n[Environment]\nA[de]=1\n");
    let p = m.permissions();
    assert_eq!(p.len(), 3, "{:?}", codes(&p));
    let c = codes(&p);
    assert!(c.iter().any(|x| x.starts_with("unknown:item:")), "{c:?}");
    assert!(c.iter().any(|x| x.starts_with("unknown:value:")), "{c:?}");
    assert!(c.iter().any(|x| x.starts_with("unknown:key:")), "{c:?}");
    // The same raw text in two scopes is two items.
    let a = meta("[Context]\nsockets=a b;\n").permissions();
    let b = meta("[Context]\nsockets=a\\q\n").permissions();
    assert_ne!(a[0].code(), b[0].code());
}

#[test]
fn display_spoofing_in_paths_is_refused() {
    for bad in [
        "~/ /etc",
        "~/\u{3000}/etc",
        "~/a/ b",
        "~/a /b",
        "~/\u{0301}x",
        "~/a\u{0301}\u{0302}\u{0303}b",
        "~/x/\u{0301}",
        "~/\u{2800}",
        "~/a\u{13430}b",
        "~/a\u{1343F}b",
    ] {
        let p = fs_one(bad);
        assert!(
            p.is_unknown() && p.risk() == Risk::High,
            "{bad:?}: {}",
            p.code()
        );
    }
    // One or two marks after a letter are fine (decomposed accents).
    assert!(!fs_one("~/Mu\u{0301}sica").is_unknown());
    assert!(!fs_one("~/a\u{0301}\u{0302}b").is_unknown());
    // Right-to-left letters are isolated in the sentence.
    let p = fs_one("~/\u{05D0}\u{05D1}");
    assert!(
        p.describe().contains("\u{2068}\u{05D0}\u{05D1}\u{2069}"),
        "{}",
        p.describe()
    );
    assert!(!fs_one("~/Музыка").describe().contains('\u{2068}'));
    // A read-only path with unusual characters is at least Medium and says so.
    for item in [
        "~/Музыка:ro",
        "/data/Ünï:ro",
        "~/Mu\u{0301}sica:ro",
        "xdg-documents/é:ro",
    ] {
        let p = fs_one(item);
        assert!(p.risk() >= Risk::Medium, "{item}");
        assert!(
            p.describe().ends_with("(contains unusual characters)"),
            "{item}: {}",
            p.describe()
        );
    }
    assert_eq!(fs_one("~/Games:ro").risk(), Risk::Low);
    assert!(!fs_one("~/Games:ro").describe().contains("unusual"));
}

#[test]
fn credential_and_escape_names_ignore_case() {
    for s in [
        "~/.SSH:ro",
        "~/.Gnupg:ro",
        "~/.Mozilla:ro",
        "~/.Config/Chromium:ro",
        "~/.VAR/app:ro",
        "~/.AWS/credentials:ro",
    ] {
        assert_eq!(fs_one(s).risk(), Risk::High, "{s}");
    }
    for s in [
        "~/.BASHRC",
        "~/.Config/AutoStart",
        "~/.Local/Bin",
        "~/.Steam",
    ] {
        let p = fs_one(s);
        assert_eq!(p.risk(), Risk::High, "{s}");
    }
}

#[test]
fn a_runtimes_odd_item_is_compared_but_the_apps_always_counts() {
    let rt = Permissions::from_metadata(
        b"[Runtime]\nname=org.x.Platform\n[Context]\nshared[de]=network;\n",
    )
    .unwrap();
    let app_odd = meta("[Context]\nshared[de]=network;\n");
    // The same odd item in the runtime and in the app: the app's own, so it
    // counts every time.
    let merged = app_odd.with_runtime(&rt);
    assert_eq!(merged.added_since(&merged).len(), 1);
    // Only the runtime's: not counted again.
    let plain = meta("");
    let merged = plain.with_runtime(&rt);
    assert!(merged.added_since(&merged).is_empty());
    assert_eq!(merged.added_since(&plain).len(), 1);
}

#[test]
fn collapse_gives_one_item_for_one_wording() {
    let p = meta("[Context]\nfilesystems=~/.config/x;xdg-config/x;\n").permissions();
    assert_eq!(p.len(), 1);
    // The Updater lists both literals; the places they cover are the same.
    let o = meta("");
    let n = meta("[Context]\nfilesystems=~/.config/x;xdg-config/x;\n");
    assert_eq!(n.added_since(&o).len(), 1);
}

#[test]
fn unparsable_extra_data_hosts_count_only_when_new() {
    let md = |uri: &str| {
        Permissions::from_metadata(
            format!("[Application]\nname=a.b\n[Extra Data]\nuri=http://[::1]/{uri}\n").as_bytes(),
        )
        .unwrap()
    };
    let a = md("x");
    let p = a.permissions();
    assert_eq!(p.len(), 1);
    assert!(p[0].is_unknown() && p[0].risk() == Risk::High);
    // Same one before: not news. A different one, or none before: news.
    assert!(a.added_since(&a).is_empty());
    assert_eq!(a.added_since(&md("y")).len(), 1);
    assert_eq!(a.added_since(&meta("")).len(), 1);
    let under = Permissions::from_metadata(
        b"[Application]\nname=a.b\n[Extra Data]\nuri=http://a_b.example/x\n",
    )
    .unwrap();
    assert!(under.permissions()[0].is_unknown());
    assert!(under.added_since(&under).is_empty());
}

#[test]
fn non_ascii_paths_use_an_allow_list() {
    for ok in [
        "~/\u{926}\u{938}\u{94D}\u{924}\u{93E}\u{935}\u{947}\u{91C}\u{93C}",
        "~/\u{E40}\u{E2D}\u{E01}\u{E2A}\u{E32}\u{E23}",
        "~/\u{E01}\u{E34}\u{E19}\u{E21}\u{E49}\u{E32}",
        "~/Caf\u{E9}\u{2013}\u{201C}x\u{201D}\u{AB}y\u{BB}a\u{B7}b",
        "~/a\u{A0}b",
        "~/a\u{0301}\u{0302}b",
    ] {
        assert!(!fs_one(ok).is_unknown(), "{ok:?}");
    }
    for bad in [
        "~/a\u{A0}\u{338}b",
        "~/a \u{338}b",
        "~/a.\u{338}b",
        "~/a\u{0301}\u{0302}\u{0303}b",
        "~/a\u{2215}b",
        "~/a\u{2603}b",
        "~/a\u{3000}b",
        "~/\u{A0}b",
    ] {
        let p = fs_one(bad);
        assert!(p.is_unknown() && p.risk() == Risk::High, "{bad:?}");
    }
}

#[test]
fn rtl_user_names_are_isolated() {
    let p = fs_one("/home/\u{5D0}\u{5D1}/x");
    assert!(
        p.describe().contains("\u{2068}\u{5D0}\u{5D1}\u{2069}"),
        "{}",
        p.describe()
    );
}

#[test]
fn newly_listed_sensitive_apps_are_high() {
    for id in [
        "org.telegram.desktop",
        "io.element.Element",
        "org.mozilla.Thunderbird",
        "org.gnome.seahorse.Application",
    ] {
        assert_eq!(
            fs_one(&format!("xdg-run/app/{id}:ro")).risk(),
            Risk::High,
            "{id}"
        );
    }
}
