//! Parser tests: the real Flathub sample, then hostile input. Nothing here
//! touches the network or the system's own data.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use atlas_store_core::appstream::{
    Block, Catalog, Component, Intensity, Kind, Limits, ParseError, ParseOptions, RatingScheme,
    ReleaseKind, Span, Style, UrlKind, parse, parse_gz_file,
};

const SAMPLE: &str = include_str!("fixtures/flathub-sample.xml");

fn opts(langs: &[&str]) -> ParseOptions {
    ParseOptions {
        origin: "flathub".into(),
        langs: langs.iter().map(|s| s.to_string()).collect(),
        limits: Limits::default(),
    }
}

fn p(xml: &str) -> Result<Catalog, ParseError> {
    parse(xml.as_bytes(), &opts(&[]))
}

fn sample(langs: &[&str]) -> Catalog {
    parse(SAMPLE.as_bytes(), &opts(langs)).expect("the sample parses")
}

fn get<'a>(c: &'a Catalog, id: &str) -> &'a Component {
    c.components
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("{id} is in the catalog"))
}

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("appstream-{}", std::process::id()));
    fs::create_dir_all(&d).expect("temp dir");
    d.join(name)
}

fn comp(id: &str, name: &str, extra: &str) -> String {
    format!(
        "<component type=\"desktop-application\"><id>{id}</id><name>{name}</name>{extra}\
         <bundle type=\"flatpak\">app/{id}/x86_64/stable</bundle></component>"
    )
}

fn wrap(inner: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><components version=\"0.8\" origin=\"flatpak\">{inner}</components>"
    )
}

fn plain(t: &str) -> Span {
    Span {
        text: t.into(),
        style: Style::Plain,
    }
}

// ---- the real sample ----

#[test]
fn sample_catalog() {
    let c = sample(&[]);
    assert_eq!(c.origin, "flathub");
    assert_eq!(c.skipped, 0);
    let ids: Vec<&str> = c.components.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "ai.jan.Jan",
            "app.ywallet.Ywallet",
            "chat.iamb.iamb",
            "com.github.nihui.waifu2x-ncnn-vulkan",
            "net.nymtech.NymVPN",
            "org.gnome.NautilusPreviewer",
            "ua.org.brezblock.q4wine",
            "org.freedesktop.Sdk.Extension.golang",
        ]
    );
}

#[test]
fn sample_jan() {
    let c = sample(&[]);
    let j = get(&c, "ai.jan.Jan");
    assert_eq!(j.kind, Kind::DesktopApp);
    assert_eq!(j.name, "Jan");
    assert_eq!(j.summary, "Private offline AI assistant");
    assert_eq!(j.developer, "Menlo Research Pte. Ltd.");
    assert_eq!(j.license, "Apache-2.0");
    assert_eq!(j.categories, ["Utility", "Education", "Chat", "Dictionary"]);
    assert_eq!(j.keywords, ["chatbot", "ai assistant", "offline ai"]);
    let icon = j.icon.as_ref().expect("icon");
    assert_eq!(icon.file, "ai.jan.Jan.png");
    assert_eq!(icon.sizes, [48, 64, 128]);
    assert_eq!(
        j.urls,
        [
            (UrlKind::Homepage, "https://jan.ai/".to_string()),
            (
                UrlKind::Bugtracker,
                "https://github.com/janhq/jan/issues".to_string()
            ),
            (
                UrlKind::VcsBrowser,
                "https://github.com/janhq/jan".to_string()
            ),
        ]
    );
    assert_eq!(j.description.len(), 4);
    assert_eq!(
        j.description[0],
        Block::Paragraph(vec![plain(
            "Jan is an open source alternative to ChatGPT that runs 100% offline on your computer."
        )])
    );
    match &j.description[2] {
        Block::List { ordered, items } => {
            assert!(!ordered);
            assert_eq!(items.len(), 6);
            assert_eq!(
                items[0],
                vec![plain(
                    "Local AI models: Run LLMs such as Llama, Gemma, Qwen and others on your device"
                )]
            );
        }
        other => panic!("not a list: {other:?}"),
    }
    assert_eq!(j.screenshots.len(), 6);
    assert!(!j.screenshots[0].default && j.screenshots[1].default);
    assert_eq!(j.screenshots[0].caption, "Blank Jan page with 0 threads");
    assert_eq!(j.screenshots[0].images.len(), 4);
    let img = &j.screenshots[0].images[0];
    assert!(!img.thumbnail);
    assert_eq!((img.width, img.height), (960, 743));
    assert!(img.url.ends_with("/screenshots/image-1_orig.png"));
    assert!(j.screenshots[0].images[1].thumbnail);
    let versions: Vec<_> = j
        .releases
        .iter()
        .map(|r| (r.version.as_str(), r.timestamp))
        .collect();
    assert_eq!(
        versions,
        [
            ("0.8.4", 1784764800),
            ("0.8.3", 1782259200),
            ("0.8.2", 1780272000),
            ("0.8.1", 1780012800)
        ]
    );
    assert_eq!(j.releases[0].kind, ReleaseKind::Stable);
    assert_eq!(j.releases[0].description.len(), 1);
    let r = j.content_rating.as_ref().expect("rating");
    assert_eq!(r.scheme, RatingScheme::Oars11);
    assert!(r.attrs.is_empty());
    let b = j.bundle.as_ref().expect("bundle");
    assert_eq!(b.reference, "app/ai.jan.Jan/x86_64/stable");
    assert_eq!(b.runtime.as_deref(), Some("org.gnome.Platform/x86_64/50"));
    assert_eq!(b.sdk.as_deref(), Some("org.gnome.Sdk/x86_64/50"));
    assert_eq!(j.launchable.as_deref(), Some("ai.jan.Jan.desktop"));
    assert!(j.extends.is_empty() && j.branding.is_none());
    let v = j.verification.as_ref().expect("verified");
    assert_eq!(v.method, "website");
    assert_eq!(v.website, "jan.ai");
    assert_eq!(v.timestamp, 1764081595);
    assert!(!v.organization);
}

#[test]
fn sample_unverified_and_branding() {
    let c = sample(&[]);
    assert!(get(&c, "app.ywallet.Ywallet").verification.is_none());
    let i = get(&c, "chat.iamb.iamb");
    assert_eq!(i.kind, Kind::ConsoleApp);
    let b = i.branding.as_ref().expect("branding");
    assert_eq!(b.light, Some([0xfa, 0xfa, 0xfa]));
    assert_eq!(b.dark, Some([0x28, 0x2d, 0x3f]));
    let r = i.content_rating.as_ref().expect("rating");
    assert_eq!(r.attrs, [("social-chat".to_string(), Intensity::Intense)]);
    assert!(i.verification.is_some());
    assert_eq!(i.releases.len(), 4);
    assert_eq!(i.releases[0].version, "0.0.12");
}

#[test]
fn sample_oars10_runtime_addon() {
    let c = sample(&[]);
    let w = get(&c, "com.github.nihui.waifu2x-ncnn-vulkan");
    assert_eq!(
        w.content_rating.as_ref().map(|r| r.scheme),
        Some(RatingScheme::Oars10)
    );
    assert_eq!(w.kind, Kind::ConsoleApp);
    assert_eq!(w.developer, "nihui");

    let rt = get(&c, "org.freedesktop.Sdk.Extension.golang");
    assert_eq!(rt.kind, Kind::Runtime);
    assert_eq!(
        rt.bundle.as_ref().map(|b| b.reference.as_str()),
        Some("runtime/org.freedesktop.Sdk.Extension.golang/x86_64/18.08")
    );
    assert!(rt.icon.is_none() && rt.description.is_empty());

    let a = get(&c, "org.gnome.NautilusPreviewer");
    assert_eq!(a.kind, Kind::Addon);
    assert_eq!(a.extends, ["org.gnome.Nautilus.desktop"]);
    assert_eq!(a.extends_ids().collect::<Vec<_>>(), ["org.gnome.Nautilus"]);
    assert_eq!(a.name, "Sushi");
    assert_eq!(a.license, "GPL-2.0+");
}

#[test]
fn sample_markup_becomes_spans() {
    let c = sample(&[]);
    let n = get(&c, "net.nymtech.NymVPN");
    let Block::List { items, .. } = &n.description[2] else {
        panic!("third block is a list");
    };
    let em = |t: &str| Span {
        text: t.into(),
        style: Style::Emphasis,
    };
    let code = |t: &str| Span {
        text: t.into(),
        style: Style::Code,
    };
    assert_eq!(
        items[0],
        vec![
            plain("5-hop "),
            em("Anonymous"),
            plain(" mode (using the Nym Mixnet)")
        ]
    );
    let Block::Paragraph(note) = &n.description[3] else {
        panic!("fourth block is a paragraph");
    };
    assert_eq!(note[1], em("client"));
    assert_eq!(note[3], code("nym-vpnd"));
    assert!(
        n.description
            .iter()
            .any(|b| matches!(b, Block::List { ordered: true, items } if items.len() >= 3))
    );
    // No markup survives anywhere in the text.
    for b in &n.description {
        let spans: Vec<&Span> = match b {
            Block::Paragraph(s) => s.iter().collect(),
            Block::List { items, .. } => items.iter().flatten().collect(),
        };
        assert!(
            spans
                .iter()
                .all(|s| !s.text.contains('<') && !s.text.contains('>'))
        );
    }
}

#[test]
fn sample_languages() {
    let none = sample(&[]);
    let de = sample(&["de_DE", "de"]);
    let pl = sample(&["pl"]);
    let ja = sample(&["ja"]);
    let q = "ua.org.brezblock.q4wine";
    assert_eq!(
        get(&none, q).summary,
        "Utility for Wine applications and prefixes management"
    );
    assert_eq!(
        get(&pl, q).summary,
        "Zarządzanie programami Wine i prefiksami"
    );
    assert_eq!(get(&ja, q).summary, get(&none, q).summary);
    let s = "org.gnome.NautilusPreviewer";
    assert_eq!(
        get(&de, s).summary,
        "Verschiedene Arten von Dateien schnell ansehen"
    );
    assert_eq!(
        get(&pl, s).summary,
        "Dodaje funkcję szybkiego podglądu różnych rodzajów plików"
    );
    assert!(
        matches!(&get(&pl, s).description[0], Block::Paragraph(sp) if sp[0].text.starts_with("Sushi to aplikacja"))
    );
    assert!(
        matches!(&get(&de, s).description[0], Block::Paragraph(sp) if sp[0].text.starts_with("Sushi ist"))
    );
    assert!(
        matches!(&get(&none, s).description[0], Block::Paragraph(sp) if sp[0].text.starts_with("Sushi is a file"))
    );
    // Jan has a Polish summary and caption but its keywords are unlocalized.
    let j = get(&pl, "ai.jan.Jan");
    assert_eq!(
        j.summary,
        "Prywatny asystent SI działający bez dostępu do sieci"
    );
    assert_eq!(
        j.screenshots[0].caption,
        "Pusta strona Jan bez żadnych wątków"
    );
    assert_eq!(j.keywords, ["chatbot", "ai assistant", "offline ai"]);
    // Only one language is kept: the count of components doesn't change.
    assert_eq!(pl.components.len(), none.components.len());
}

#[test]
fn language_matching_and_fallback() {
    let xml = wrap(&comp(
        "org.x.A",
        "plain",
        "<summary xml:lang=\"en\">english</summary><summary xml:lang=\"PT-br\">brasil</summary>\
         <summary xml:lang=\"pt\">portugal</summary><summary>unlocalized</summary>",
    ));
    let sum = |langs: &[&str]| {
        parse(xml.as_bytes(), &opts(langs)).unwrap().components[0]
            .summary
            .clone()
    };
    assert_eq!(sum(&["pt_BR", "pt"]), "brasil");
    assert_eq!(sum(&["pt"]), "portugal");
    assert_eq!(sum(&["fr"]), "unlocalized");
    // With no unlocalized element, `en` is the fallback.
    let xml2 = wrap(&comp(
        "org.x.A",
        "n",
        "<summary xml:lang=\"de\">de</summary><summary xml:lang=\"en\">en</summary>",
    ));
    let c = parse(xml2.as_bytes(), &opts(&["fr"])).unwrap();
    assert_eq!(c.components[0].summary, "en");
}

// ---- hostile input ----

#[test]
fn doctype_and_entities_are_refused() {
    let laughs = "<?xml version=\"1.0\"?><!DOCTYPE lolz [<!ENTITY lol \"lol\">\
        <!ENTITY lol2 \"&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;\">\
        <!ENTITY lol3 \"&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;\">]>\
        <components><component type=\"desktop-application\"><id>a.b</id><name>&lol3;</name></component></components>";
    assert_eq!(p(laughs), Err(ParseError::DocType));
    let xxe = "<!DOCTYPE x SYSTEM \"file:///etc/passwd\"><components/>";
    assert_eq!(p(xxe), Err(ParseError::DocType));
    // An entity with no DOCTYPE: in text, in an unknown element, in an attribute.
    for bad in [
        wrap(&comp("a.b", "&xxe;", "")),
        wrap("<unknown>&xxe;</unknown>"),
        wrap("<unknown a=\"&xxe;\"/>"),
        wrap(&comp("a.b", "n", "<summary>&nbsp;</summary>")),
    ] {
        assert!(
            matches!(p(&bad), Err(ParseError::Entity(_) | ParseError::Xml { .. })),
            "{bad}"
        );
    }
}

#[test]
fn predefined_and_numeric_references_work() {
    let c = p(&wrap(&comp(
        "a.b",
        "A &amp; B &lt;x&gt; &quot;q&quot; &apos;s&apos; &#x41;&#66;",
        "",
    )))
    .unwrap();
    assert_eq!(c.components[0].name, "A & B <x> \"q\" 's' AB");
    let c = p(&wrap(&comp("a.b", "<![CDATA[<raw> & text]]>", ""))).unwrap();
    assert_eq!(c.components[0].name, "<raw> & text");
}

#[test]
fn deep_nesting_fails_fast() {
    let deep = format!(
        "<components><component type=\"desktop-application\"><id>a.b</id><name>n</name>{}{}</component></components>",
        "<a>".repeat(10_000),
        "</a>".repeat(10_000)
    );
    assert_eq!(p(&deep), Err(ParseError::TooDeep));
    // Unknown subtrees within the limit are skipped, with their text.
    let ok = wrap(&comp(
        "a.b",
        "n",
        &format!("{}secret{}", "<z>".repeat(20), "</z>".repeat(20)),
    ));
    assert_eq!(p(&ok).unwrap().components.len(), 1);
    let at_limit = format!("{}{}", "<a>".repeat(33), "</a>".repeat(33));
    assert_eq!(p(&at_limit), Err(ParseError::NotCatalog));
    let exact = format!(
        "<components>{}{}</components>",
        "<a>".repeat(31),
        "</a>".repeat(31)
    );
    assert!(p(&exact).is_ok());
    let over = format!(
        "<components>{}{}</components>",
        "<a>".repeat(32),
        "</a>".repeat(32)
    );
    assert_eq!(p(&over), Err(ParseError::TooDeep));
}

#[test]
fn huge_text_node() {
    let big = "a".repeat(5 << 20);
    let xml = wrap(&comp(
        "a.b",
        "n",
        &format!("<summary>{big}</summary><description><p>{big}</p></description>"),
    ));
    let c = p(&xml).unwrap();
    let k = &c.components[0];
    assert_eq!(k.summary.chars().count(), 400);
    let Block::Paragraph(s) = &k.description[0] else {
        panic!("paragraph")
    };
    assert_eq!(s[0].text.chars().count(), 4096);
    // One node beyond the node cap is an error, not memory.
    let huge = wrap(&comp(
        "a.b",
        "n",
        &format!("<summary>{}</summary>", "a".repeat(17 << 20)),
    ));
    assert_eq!(p(&huge), Err(ParseError::Limit("one text node or tag")));
}

#[test]
fn bidi_and_control_characters() {
    let xml = wrap(&comp(
        "a.b",
        "Evil\u{202E}App\u{2066}\u{200E}&#7;&#x1B;x\u{FEFF}",
        "<summary>  Pay\u{202A}\u{202B}\u{202C}\u{202D}\u{2067}\u{2068}\u{2069} \t\n  now\u{85}\u{9F}\u{FFFE}  </summary>\
         <developer_name>\u{200D}Dev\u{200C}</developer_name>",
    ));
    let c = p(&xml).unwrap();
    let k = &c.components[0];
    assert_eq!(k.name, "EvilApp\u{200E}x");
    assert_eq!(k.summary, "Pay now");
    assert_eq!(k.developer, "\u{200D}Dev\u{200C}");
}

#[test]
fn field_caps() {
    let long = "é".repeat(1000);
    let kws: String = (0..100)
        .map(|i| format!("<keyword>k{i}</keyword>"))
        .collect();
    let cats: String = (0..40)
        .map(|i| format!("<category>c{i}</category>"))
        .collect();
    let xml = wrap(&comp(
        "a.b",
        &long,
        &format!(
            "<summary>{long}</summary><project_license>{long}</project_license><developer_name>{long}</developer_name>\
             <keywords>{kws}<keyword>{long}</keyword></keywords><categories>{cats}</categories>"
        ),
    ));
    let k = &p(&xml).unwrap().components[0];
    assert_eq!(k.name.chars().count(), 200);
    assert_eq!(k.summary.chars().count(), 400);
    assert_eq!(k.license.chars().count(), 300);
    assert_eq!(k.developer.chars().count(), 200);
    assert_eq!(k.keywords.len(), 64);
    assert_eq!(k.categories.len(), 16);
    let k2 = &p(&wrap(&comp(
        "a.b",
        "n",
        &format!("<keywords><keyword>{long}</keyword></keywords>"),
    )))
    .unwrap()
    .components[0];
    assert_eq!(k2.keywords[0].chars().count(), 64);
}

#[test]
fn hostile_urls() {
    let urls = [
        ("homepage", "https://example.org/"),
        ("homepage", "javascript:alert(1)"),
        ("bugtracker", "file:///etc/passwd"),
        ("help", "data:text/html;base64,AAAA"),
        ("donation", "https://user:pass@example.org/"),
        ("faq", "https://@example.org/"),
        ("contact", "https://exa mple.org/"),
        ("translate", "https:///nohost"),
        ("contribute", "http://example.org/plain"),
        ("vcs-browser", "ftp://example.org/"),
        ("unknown-kind", "https://example.org/unknown"),
        ("homepage", "  https://example.org/trimmed \n"),
        ("help", "https://example.org/\u{202E}rtl"),
    ];
    let long = format!("https://example.org/{}", "a".repeat(2100));
    let mut extra: String = urls
        .iter()
        .map(|(t, u)| format!("<url type=\"{t}\">{u}</url>"))
        .collect();
    extra += &format!("<url type=\"faq\">{long}</url>");
    let k = &p(&wrap(&comp("a.b", "n", &extra))).unwrap().components[0];
    assert_eq!(
        k.urls,
        [
            (UrlKind::Homepage, "https://example.org/".to_string()),
            (UrlKind::Contribute, "http://example.org/plain".to_string()),
            (UrlKind::Homepage, "https://example.org/trimmed".to_string()),
        ]
    );
    // Image URLs are https only.
    let shots = "<screenshots><screenshot><image type=\"source\">http://example.org/a.png</image>\
        <image type=\"thumbnail\" width=\"10\" height=\"x\">https://example.org/b.png</image>\
        <image>javascript:x</image></screenshot>\
        <screenshot><image>data:image/png;base64,AA</image></screenshot></screenshots>";
    let k = &p(&wrap(&comp("a.b", "n", shots))).unwrap().components[0];
    assert_eq!(k.screenshots.len(), 1);
    assert_eq!(k.screenshots[0].images.len(), 1);
    let i = &k.screenshots[0].images[0];
    assert!(i.thumbnail && i.width == 10 && i.height == 0 && i.url == "https://example.org/b.png");
    // At most 16 links.
    let many: String = (0..40)
        .map(|i| format!("<url type=\"homepage\">https://e.org/{i}</url>"))
        .collect();
    assert_eq!(
        p(&wrap(&comp("a.b", "n", &many))).unwrap().components[0]
            .urls
            .len(),
        16
    );
}

#[test]
fn hostile_icons() {
    let icon = |t: &str, f: &str| format!("<icon type=\"{t}\" width=\"64\">{f}</icon>");
    for bad in [
        icon("cached", "../../etc/passwd.png"),
        icon("cached", "/etc/passwd"),
        icon("cached", "/etc/passwd.png"),
        icon("cached", ".hidden.png"),
        icon("cached", "sub/dir.png"),
        icon("cached", "back\\slash.png"),
        icon("cached", "icon.jpg"),
        icon("cached", "icon.png.exe"),
        icon("cached", ""),
        icon("remote", "https://dl.flathub.org/x.png"),
        icon("stock", "org.x.A"),
        "<icon type=\"cached\" scale=\"2\" width=\"128\">scaled.png</icon>".to_string(),
    ] {
        let k = &p(&wrap(&comp("a.b", "n", &bad))).unwrap().components[0];
        assert!(k.icon.is_none(), "{bad}");
    }
    let good = format!(
        "{}{}{}{}",
        icon("cached", "a.b.png"),
        "<icon type=\"cached\" width=\"128\">a.b.png</icon>",
        "<icon type=\"cached\" scale=\"2\" width=\"256\">a.b.png</icon>",
        icon("cached", "other.png"),
    );
    let k = &p(&wrap(&comp("a.b", "n", &good))).unwrap().components[0];
    let i = k.icon.as_ref().unwrap();
    assert_eq!(
        (i.file.as_str(), i.sizes.as_slice()),
        ("a.b.png", &[64u16, 128][..])
    );
}

#[test]
fn bad_and_duplicate_ids_and_skips() {
    let mut x = String::new();
    for id in [
        "nodots", "a..b", ".a.b", "a.b.", "a.b c", "../x.y", "a/b.c", "a.b;c", "",
    ] {
        x += &comp(&id.replace('&', "&amp;"), "bad", "");
    }
    x += &comp("good.one", "first", "");
    x += &comp("good.one", "second", "");
    x += &comp("good.two.desktop", "desktop suffix", "");
    // No name, a whitespace-only name, no bundle, an invalid bundle.
    x += "<component type=\"desktop-application\"><id>no.name</id><bundle type=\"flatpak\">app/no.name/x86_64/stable</bundle></component>";
    x += &comp("blank.name", " \t\u{7} ", "");
    x += "<component type=\"desktop-application\"><id>no.bundle</id><name>n</name></component>";
    x += "<component type=\"runtime\"><id>no.bundle2</id><name>n</name></component>";
    x += "<component type=\"addon\"><id>bad.ref</id><name>n</name><bundle type=\"flatpak\">app/../../x/y</bundle></component>";
    x += "<component type=\"addon\"><id>wrong.type</id><name>n</name><bundle type=\"tarball\">app/wrong.type/x86_64/stable</bundle></component>";
    // A generic component needs no bundle.
    x += "<component><id>generic.thing</id><name>generic</name></component>";
    x += "<component type=\"generic\"><id>generic.two</id><name>g2</name></component>";
    x += &comp(&"a.".repeat(200), "too long", "");
    let c = p(&wrap(&x)).unwrap();
    let ids: Vec<(&str, &str)> = c
        .components
        .iter()
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();
    assert_eq!(
        ids,
        [
            ("good.one", "first"),
            ("good.two.desktop", "desktop suffix"),
            ("generic.thing", "generic"),
            ("generic.two", "g2"),
        ]
    );
    assert_eq!(c.components[2].kind, Kind::Other);
    // 9 bad ids, 1 duplicate, 2 without a name, 2 without a bundle, 2 with a bad one, 1 too long.
    assert_eq!(c.skipped, 17);
}

#[test]
fn truncated_and_broken_xml_fail_the_whole_parse() {
    let full = wrap(&comp("a.b", "n", ""));
    for cut in [10, 40, 80, full.len() - 20, full.len() - 1] {
        let cut = (0..=cut).rev().find(|&i| full.is_char_boundary(i)).unwrap();
        let r = p(&full[..cut]);
        assert!(
            matches!(r, Err(ParseError::Xml { .. }) | Err(ParseError::NotCatalog)),
            "cut at {cut}: {r:?}"
        );
    }
    // Truncated mid-sample: whatever was complete is not returned.
    let r = p(&SAMPLE[..SAMPLE.len() / 2]);
    assert!(matches!(r, Err(ParseError::Xml { .. })), "{r:?}");
    let ParseError::Xml { position, message } = r.unwrap_err() else {
        unreachable!()
    };
    assert!(position > 0 && !message.is_empty());
    for bad in [
        "<components><component></components>",
        "<components></component></components>",
        "<components><a></b></components>",
        "<components><a b=c></a></components>",
        "<components><a b=\"1\" b=\"2\"/></components>",
        "<components>&#xD800;</components>",
        "<components>&#99999999;</components>",
        "<components>& </components>",
    ] {
        assert!(
            matches!(p(bad), Err(ParseError::Xml { .. })),
            "{bad}: {:?}",
            p(bad)
        );
    }
    // Invalid UTF-8.
    let mut b = wrap(&comp("a.b", "n", "")).into_bytes();
    let at = b.windows(3).position(|w| w == b"<id").unwrap();
    b[at + 5] = 0xFF;
    assert!(parse(&b[..], &opts(&[])).is_err());
}

#[test]
fn not_a_catalog() {
    assert_eq!(p(""), Err(ParseError::NotCatalog));
    assert_eq!(p("   "), Err(ParseError::NotCatalog));
    assert_eq!(p("<html></html>"), Err(ParseError::NotCatalog));
    assert_eq!(p("<components/><components/>"), Err(ParseError::NotCatalog));
    let c = p("<components/>").unwrap();
    assert!(c.components.is_empty());
}

#[test]
fn many_screenshots_and_releases_are_capped() {
    let mut shots = String::from("<screenshots>");
    for i in 0..10_000 {
        shots += &format!(
            "<screenshot><caption>s{i}</caption>{}</screenshot>",
            (0..12).map(|j| format!("<image type=\"source\" width=\"{j}\" height=\"1\">https://e.org/{i}/{j}.png</image>")).collect::<String>()
        );
    }
    shots += "</screenshots><releases>";
    for i in 0..10_000 {
        shots += &format!("<release version=\"{i}\" timestamp=\"{}\"/>", 1_000_000 + i);
    }
    shots += "<release version=\"dated\" date=\"2030-01-02\" type=\"development\"/>";
    shots += "<release version=\"snap\" timestamp=\"5\" type=\"snapshot\"/><release timestamp=\"99999999999\"/>";
    shots += "</releases>";
    let k = &p(&wrap(&comp("a.b", "n", &shots))).unwrap().components[0];
    assert_eq!(k.screenshots.len(), 16);
    assert_eq!(k.screenshots[15].caption, "s15");
    assert!(k.screenshots.iter().all(|s| s.images.len() == 8));
    assert_eq!(k.releases.len(), 10);
    assert_eq!(k.releases[0].version, "dated");
    assert_eq!(k.releases[0].timestamp, 1_893_542_400);
    assert_eq!(k.releases[0].kind, ReleaseKind::Development);
    let rest: Vec<&str> = k.releases[1..].iter().map(|r| r.version.as_str()).collect();
    assert_eq!(
        rest,
        [
            "9999", "9998", "9997", "9996", "9995", "9994", "9993", "9992", "9991"
        ]
    );
    assert!(
        k.releases
            .windows(2)
            .all(|w| w[0].timestamp >= w[1].timestamp)
    );
}

#[test]
fn description_caps() {
    let para = "word ".repeat(1500);
    let many: String = (0..100).map(|i| format!("<p>p{i}</p>")).collect();
    let k = &p(&wrap(&comp(
        "a.b",
        "n",
        &format!("<description>{many}</description>"),
    )))
    .unwrap()
    .components[0];
    assert_eq!(k.description.len(), 64);
    let big: String = (0..40).map(|_| format!("<p>{para}</p>")).collect();
    let k = &p(&wrap(&comp(
        "a.b",
        "n",
        &format!("<description>{big}</description>"),
    )))
    .unwrap()
    .components[0];
    let bytes: usize = k
        .description
        .iter()
        .map(|b| match b {
            Block::Paragraph(s) => s.iter().map(|s| s.text.len()).sum(),
            Block::List { .. } => 0,
        })
        .sum();
    assert!(bytes <= 32 << 10, "{bytes}");
    assert!(bytes > 28 << 10, "{bytes}");
    let items: String = (0..400).map(|i| format!("<li>i{i}</li>")).collect();
    let k = &p(&wrap(&comp(
        "a.b",
        "n",
        &format!("<description><ul>{items}</ul><ol></ol><p> </p></description>"),
    )))
    .unwrap()
    .components[0];
    assert_eq!(k.description.len(), 1);
    assert!(matches!(&k.description[0], Block::List { items, .. } if items.len() == 256));
    // Release notes: 8 KiB.
    let rel = format!(
        "<releases><release version=\"1\" timestamp=\"1\"><description>{big}</description></release></releases>"
    );
    let k = &p(&wrap(&comp("a.b", "n", &rel))).unwrap().components[0];
    let bytes: usize = k.releases[0]
        .description
        .iter()
        .map(|b| match b {
            Block::Paragraph(s) => s.iter().map(|s| s.text.len()).sum(),
            Block::List { .. } => 0,
        })
        .sum();
    assert!(bytes <= 8 << 10 && bytes > 7 << 10, "{bytes}");
}

#[test]
fn unknown_inline_markup_keeps_text_as_plain() {
    let d = "<description><p>a <b>bold <em>deep</em></b> <a href=\"javascript:x\">link</a> <script>alert(1)</script>z</p>\
             <div><p>skipped</p></div><unknown>skipped too</unknown>loose text</description>";
    let k = &p(&wrap(&comp("a.b", "n", d))).unwrap().components[0];
    assert_eq!(k.description.len(), 1);
    let Block::Paragraph(s) = &k.description[0] else {
        panic!()
    };
    let text: String = s.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(text, "a bold deep link alert(1)z");
    assert!(
        s.iter()
            .any(|s| s.style == Style::Emphasis && s.text == "deep")
    );
}

#[test]
fn verification_needs_verified_true() {
    let v = |val: &str| {
        format!(
            "<custom><value key=\"flathub::verification::verified\">{val}</value>\
             <value key=\"flathub::verification::method\">login_provider</value>\
             <value key=\"flathub::verification::login_name\">someone</value>\
             <value key=\"flathub::verification::login_provider\">github</value>\
             <value key=\"flathub::verification::login_is_organization\">true</value>\
             <value key=\"flathub::verification::timestamp\">12</value>\
             <value key=\"other::key\">x</value></custom>"
        )
    };
    let k = &p(&wrap(&comp("a.b", "n", &v("true")))).unwrap().components[0];
    let ver = k.verification.as_ref().unwrap();
    assert_eq!(
        (
            ver.method.as_str(),
            ver.login_name.as_str(),
            ver.login_provider.as_str()
        ),
        ("login_provider", "someone", "github")
    );
    assert!(ver.organization && ver.timestamp == 12);
    for no in ["false", "TRUE", "1", ""] {
        assert!(
            p(&wrap(&comp("a.b", "n", &v(no)))).unwrap().components[0]
                .verification
                .is_none(),
            "{no}"
        );
    }
}

#[test]
fn rating_branding_bundle_details() {
    let extra = "<content_rating type=\"oars-1.0\"><content_attribute id=\"violence-bloodshed\">mild</content_attribute>\
        <content_attribute id=\"bad id!\">mild</content_attribute><content_attribute id=\"drugs-alcohol\">weird</content_attribute>\
        <content_attribute id=\"language-profanity\">none</content_attribute></content_rating>\
        <content_rating type=\"x\"/>\
        <branding><color type=\"primary\" scheme_preference=\"dark\">#00ff7F</color>\
        <color type=\"primary\" scheme_preference=\"light\">red</color>\
        <color type=\"text\" scheme_preference=\"light\">#111111</color></branding>";
    let k = &p(&wrap(&comp("a.b", "n", extra))).unwrap().components[0];
    let r = k.content_rating.as_ref().unwrap();
    assert_eq!(r.scheme, RatingScheme::Oars10);
    assert_eq!(
        r.attrs,
        [
            ("violence-bloodshed".to_string(), Intensity::Mild),
            ("language-profanity".to_string(), Intensity::None)
        ]
    );
    let b = k.branding.as_ref().unwrap();
    assert_eq!((b.light, b.dark), (None, Some([0, 255, 0x7f])));
    let both = "<branding><color type=\"primary\">#010203</color></branding>";
    let k = &p(&wrap(&comp("a.b", "n", both))).unwrap().components[0];
    assert_eq!(
        k.branding.as_ref().map(|b| (b.light, b.dark)),
        Some((Some([1, 2, 3]), Some([1, 2, 3])))
    );
    let k = &p(&wrap(&comp(
        "a.b",
        "n",
        "<branding><color type=\"primary\">#12</color></branding>",
    )))
    .unwrap()
    .components[0];
    assert!(k.branding.is_none());
}

#[test]
fn long_attribute_values_are_ignored() {
    let v = "x".repeat(3000);
    let xml = wrap(&format!(
        "<component type=\"desktop-application\"><id>a.b</id><name xml:lang=\"{v}\">no</name><name>yes</name>\
         <bundle type=\"flatpak\" runtime=\"{v}\">app/a.b/x86_64/stable</bundle></component>"
    ));
    let k = &p(&xml).unwrap().components[0];
    // The over-long xml:lang counts as absent, so that name is the first unlocalized one.
    assert_eq!(k.name, "no");
    assert_eq!(k.bundle.as_ref().unwrap().runtime, None);
}

// ---- size caps, gzip ----

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

#[test]
fn gzip_file_roundtrip_and_bomb() {
    let path = tmp("sample.xml.gz");
    fs::write(&path, gz(SAMPLE.as_bytes())).unwrap();
    let c = parse_gz_file(&path, &opts(&[])).unwrap();
    assert_eq!(c.components.len(), 8);
    assert_eq!(c, sample(&[]));

    // A tiny file that expands past the cap: it fails, it is not cut short.
    let mut bomb = b"<components>".to_vec();
    bomb.extend(std::iter::repeat_n(b' ', 8 << 20));
    bomb.extend_from_slice(b"</components>");
    let bomb_gz = gz(&bomb);
    assert!(bomb_gz.len() < 64 << 10, "{}", bomb_gz.len());
    let path = tmp("bomb.xml.gz");
    fs::write(&path, &bomb_gz).unwrap();
    let mut o = opts(&[]);
    o.limits.max_decompressed = 1 << 20;
    assert_eq!(
        parse_gz_file(&path, &o),
        Err(ParseError::Limit("decompressed size"))
    );
    // With the default cap it is only a lot of blanks.
    assert!(
        parse_gz_file(&path, &opts(&[]))
            .unwrap()
            .components
            .is_empty()
    );

    // The compressed cap is checked before reading.
    let mut o = opts(&[]);
    o.limits.max_compressed = 100;
    let path = tmp("big.xml.gz");
    fs::write(&path, gz(SAMPLE.as_bytes())).unwrap();
    assert_eq!(
        parse_gz_file(&path, &o),
        Err(ParseError::Limit("compressed size"))
    );
    assert_eq!(Limits::default().max_decompressed, 512 << 20);
    assert_eq!(Limits::default().max_compressed, 64 << 20);
}

#[test]
fn gzip_file_refusals() {
    let path = tmp("plain.gz");
    fs::write(&path, SAMPLE).unwrap();
    assert!(matches!(
        parse_gz_file(&path, &opts(&[])),
        Err(ParseError::Io(_))
    ));
    let truncated = tmp("cut.gz");
    let g = gz(SAMPLE.as_bytes());
    fs::write(&truncated, &g[..g.len() / 2]).unwrap();
    assert!(parse_gz_file(&truncated, &opts(&[])).is_err());
    let link = tmp("link.gz");
    let target = tmp("target.gz");
    fs::write(&target, gz(SAMPLE.as_bytes())).unwrap();
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(matches!(
        parse_gz_file(&link, &opts(&[])),
        Err(ParseError::Io(_))
    ));
    assert!(matches!(
        parse_gz_file(&tmp("missing.gz"), &opts(&[])),
        Err(ParseError::Io(_))
    ));
    let dir = tmp("adir");
    fs::create_dir_all(&dir).unwrap();
    assert!(matches!(
        parse_gz_file(&dir, &opts(&[])),
        Err(ParseError::Io(_))
    ));
}

#[test]
fn component_count_cap() {
    let x = wrap(
        &(0..20)
            .map(|i| comp(&format!("a.b{i}"), "n", ""))
            .collect::<String>(),
    );
    let mut o = opts(&[]);
    o.limits.max_components = 10;
    assert_eq!(
        parse(x.as_bytes(), &o),
        Err(ParseError::Limit("components"))
    );
    o.limits.max_components = 20;
    assert_eq!(parse(x.as_bytes(), &o).unwrap().components.len(), 20);
}

// ---- mutation test ----

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

#[test]
fn random_mutations_never_panic() {
    let base = SAMPLE.as_bytes();
    let mut rng = Xorshift(0x9E37_79B9_7F4A_7C15);
    let o = opts(&["pl"]);
    let (mut ok, mut err) = (0, 0);
    for _ in 0..2000 {
        let mut b = base.to_vec();
        for _ in 0..1 + rng.below(8) {
            let at = rng.below(b.len());
            match rng.below(4) {
                0 => {
                    let n = (1 + rng.below(200)).min(b.len() - at);
                    b.drain(at..at + n);
                }
                1 => {
                    let n = (1 + rng.below(300)).min(b.len() - at);
                    let chunk = b[at..at + n].to_vec();
                    let to = rng.below(b.len());
                    b.splice(to..to, chunk);
                }
                2 => b[at] ^= 1 << rng.below(8),
                _ => {
                    let junk = [b'<', b'>', b'&', b'"', b'/', b';', 0, 0xFF];
                    b[at] = junk[rng.below(junk.len())];
                }
            }
        }
        match parse(&b[..], &o) {
            Ok(c) => {
                ok += 1;
                assert!(c.components.len() <= 100);
            }
            Err(_) => err += 1,
        }
    }
    assert_eq!(ok + err, 2000);
    assert!(err > 100, "mutations should break the XML often: {err}");
}
