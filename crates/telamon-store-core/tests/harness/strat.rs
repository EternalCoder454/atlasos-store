//! Input strategies for the property tests. Structure-aware: valid inputs are
//! built from their parts and then broken one field at a time, and byte
//! inputs are fixtures mutated by flips, insertions, deletions, splices of
//! interesting tokens and truncation.
#![allow(dead_code)]

use proptest::prelude::*;
use proptest::sample::select;

/// One of a fixed list of strings.
fn pick(items: &'static [&'static str]) -> impl Strategy<Value = &'static str> {
    select(items)
}
use serde_json::{Value, json};

/// A bounded run: the case count comes from `PROPTEST_CASES` (default 256),
/// nothing is written to disk, shrinking is capped.
pub fn config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256);
    ProptestConfig {
        cases,
        failure_persistence: None,
        max_shrink_iters: 2048,
        max_global_rejects: 1_000_000,
        ..ProptestConfig::default()
    }
}

/// `common` nine times in ten, `odd` the rest: valid inputs with one thing wrong
/// reach deeper than inputs that are wrong everywhere.
pub fn rare<T: std::fmt::Debug + 'static>(
    common: impl Strategy<Value = T> + 'static,
    odd: impl Strategy<Value = T> + 'static,
) -> impl Strategy<Value = T> {
    prop_oneof![9 => common, 1 => odd]
}

// ---- byte mutation ----

#[derive(Debug, Clone)]
pub enum Op {
    Flip(usize, u8),
    Set(usize, u8),
    Insert(usize, Vec<u8>),
    Delete(usize, usize),
    Splice(usize, usize),
    Dup(usize, usize),
    Truncate(usize),
    /// Overwrite 1, 2, 4 or 8 bytes with an extreme value, little endian.
    Extreme(usize, u8, u8),
}

fn op(dict_len: usize) -> impl Strategy<Value = Op> {
    let pos = 0usize..1 << 20;
    prop_oneof![
        3 => (pos.clone(), 0u8..8).prop_map(|(p, b)| Op::Flip(p, b)),
        2 => (pos.clone(), any::<u8>()).prop_map(|(p, v)| Op::Set(p, v)),
        2 => (pos.clone(), prop::collection::vec(any::<u8>(), 1..16)).prop_map(|(p, v)| Op::Insert(p, v)),
        2 => (pos.clone(), 1usize..64).prop_map(|(p, n)| Op::Delete(p, n)),
        3 => (pos.clone(), 0..dict_len.max(1)).prop_map(|(p, d)| Op::Splice(p, d)),
        1 => (pos.clone(), 1usize..64).prop_map(|(p, n)| Op::Dup(p, n)),
        1 => pos.clone().prop_map(Op::Truncate),
        3 => (pos, 0u8..4, 0u8..6).prop_map(|(p, w, v)| Op::Extreme(p, w, v)),
    ]
}

pub fn apply(mut b: Vec<u8>, ops: &[Op], dict: &[&[u8]]) -> Vec<u8> {
    for o in ops {
        let n = b.len();
        match o {
            Op::Flip(p, bit) if n > 0 => b[p % n] ^= 1 << bit,
            Op::Set(p, v) if n > 0 => b[p % n] = *v,
            Op::Insert(p, v) => {
                let at = if n == 0 { 0 } else { p % (n + 1) };
                b.splice(at..at, v.iter().copied());
            }
            Op::Delete(p, len) if n > 0 => {
                let at = p % n;
                b.drain(at..(at + len).min(n));
            }
            Op::Splice(p, d) if !dict.is_empty() => {
                let at = if n == 0 { 0 } else { p % (n + 1) };
                b.splice(at..at, dict[d % dict.len()].iter().copied());
            }
            Op::Dup(p, len) if n > 0 => {
                let at = p % n;
                let end = (at + len).min(n);
                let piece = b[at..end].to_vec();
                b.splice(end..end, piece);
            }
            Op::Truncate(p) if n > 0 => b.truncate(p % n),
            Op::Extreme(p, w, v) if n > 0 => {
                let width = 1usize << w;
                let val: u64 = match v {
                    0 => 0,
                    1 => 1,
                    2 => u64::MAX,
                    3 => 0x7FFF_FFFF_FFFF_FFFF,
                    4 => 0x8000_0000,
                    _ => 0xFFFF_FFFF,
                };
                let at = p % n;
                for (i, byte) in val.to_le_bytes()[..width].iter().enumerate() {
                    if at + i < n {
                        b[at + i] = *byte;
                    }
                }
            }
            _ => {}
        }
        // Keep one input small enough to run in microseconds.
        b.truncate(1 << 20);
    }
    b
}

/// `base` with up to `max_ops` mutations; `dict` holds tokens worth splicing in.
pub fn mutated(
    base: Vec<u8>,
    dict: &'static [&'static [u8]],
    max_ops: usize,
) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(op(dict.len()), 0..=max_ops)
        .prop_map(move |ops| apply(base.clone(), &ops, dict))
}

/// Mutations of a text, kept as bytes (so invalid UTF-8 appears too).
pub fn mutated_text(
    base: &str,
    dict: &'static [&'static [u8]],
    max_ops: usize,
) -> impl Strategy<Value = Vec<u8>> {
    mutated(base.as_bytes().to_vec(), dict, max_ops)
}

/// Raw bytes, mostly short, some long.
pub fn bytes(max: usize) -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        8 => prop::collection::vec(any::<u8>(), 0..64.min(max)),
        2 => prop::collection::vec(any::<u8>(), 0..max),
    ]
}

// ---- text ----

pub const NASTY: &[&str] = &[
    "\u{202E}",
    "\u{202A}",
    "\u{2066}",
    "\u{2069}",
    "\u{200B}",
    "\u{200E}",
    "\u{200F}",
    "\u{200D}",
    "\u{FEFF}",
    "\u{061C}",
    "\u{00AD}",
    "\u{034F}",
    "\u{3164}",
    "\u{FFA0}",
    "\u{E0001}",
    "\u{E0020}",
    "\u{FE0F}",
    "\u{FFFE}",
    "\u{FDD0}",
    "\u{1FFFF}",
    "\u{0085}",
    "\u{2028}",
    "\u{2029}",
    "\u{00A0}",
    "\u{3000}",
    "\r\n",
    "\r",
    "\n",
    "\t",
    "\u{0B}",
    "\u{0C}",
    "\u{1B}[31m",
    "\u{7F}",
    "\0",
    "  ",
    " ",
    "é",
    "e\u{301}",
    "\u{1F600}",
    "\\",
    "\"",
    "'",
    "%",
    "$",
    "`",
    ";",
    "=",
    "[",
    "]",
    "#",
    "<",
    ">",
    "&",
    "..",
    "./",
    "/",
];

/// A string made of ordinary characters and the troublesome ones.
pub fn nasty_string() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            4 => any::<char>().prop_map(|c| c.to_string()),
            3 => pick(NASTY).prop_map(str::to_string),
            3 => "[a-zA-Z0-9 .:/_-]{0,8}",
        ],
        0..24,
    )
    .prop_map(|v| v.concat())
}

/// A string with no NUL and no newline (a path or a value).
pub fn plain_string() -> impl Strategy<Value = String> {
    nasty_string().prop_map(|s| s.replace(['\0', '\n'], ""))
}

// ---- URLs ----

pub fn url() -> impl Strategy<Value = String> {
    let scheme = rare(
        Just("https://"),
        pick(&[
            "https://",
            "HTTPS://",
            "Https://",
            "http://",
            "",
            "https:/",
            "https:",
            "https:\\\\",
            "ftp://",
            "javascript:",
            "https://https://",
            " https://",
            "https://\u{200B}",
        ]),
    );
    let userinfo = rare(
        Just(""),
        pick(&["u@", "u:p@", "@", ":@", "evil.com@", "a:b@c@"]),
    );
    let label = rare(
        "[a-z][a-z0-9]{1,8}",
        prop_oneof![
            2 => "[a-zA-Z0-9]{1,8}",
            2 => "[a-z]{2,6}-[a-z0-9]{1,4}",
            3 => pick(&[
                "localhost", "local", "internal", "0", "127", "1", "xn--p1ai", "é", "-a", "a-",
                "EXAMPLE", "x_y", "", "com", "org", "onion", "test", "lan", "255",
            ]).prop_map(str::to_string),
        ],
    );
    let host = rare(
        prop::collection::vec(label, 2..5).prop_map(|v| v.join(".")),
        pick(&[
            "127.0.0.1",
            "[::1]",
            "[2001:db8::1]",
            "1.2.3.4",
            "0x7f.1",
            "2130706433",
            "example.com.",
            "example..com",
            ".example.com",
            "a.b.c.d.e.f.g.h",
            "例え.jp",
            "EXAMPLE.COM",
            "ExAmPlE.cOm",
            "github.com",
            "api.github.com",
            "flathub.org",
        ])
        .prop_map(str::to_string),
    );
    let port = rare(
        Just(""),
        pick(&[":443", ":80", ":0", ":", ":99999", ":44a", ":00443"]),
    );
    let seg = rare(
        pick(&["a", "b", "x.y", "dir", "file.txt"]),
        pick(&[
            "", ".", "..", "%2e", "%2E", "%2e%2e", ".%2e", "%2E%2e", "%2f", "..%2f", "%252e",
            "...", ".;", "%00", "%", "a b", "é", "\\",
        ]),
    );
    let path = prop::collection::vec(seg, 0..5).prop_map(|v| format!("/{}", v.join("/")));
    let tail = rare(
        pick(&["", "?x=1", "#frag", "?a=b#c"]),
        pick(&[
            "?a/../b", "#/../", "?", "#", "/?", "/#", " ", "\n", "\"", "<", "`", "|",
        ]),
    );
    (scheme, userinfo, host, port, path, tail)
        .prop_map(|(s, u, h, p, path, t)| format!("{s}{u}{h}{p}{path}{t}"))
}

/// An https URL the policy accepts, mostly (a start for redirects).
pub fn good_url() -> impl Strategy<Value = String> {
    (
        prop::collection::vec("[a-z][a-z0-9]{0,6}", 1..3),
        prop::collection::vec("[a-z0-9._~-]{1,6}", 0..4),
    )
        .prop_map(|(hs, ps)| format!("https://{}.com/{}", hs.join("."), ps.join("/")))
}

pub fn location() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => url(),
        3 => prop::collection::vec(pick(&["", ".", "..", "a", "%2e", "b.c", "x?y", "#z"]), 0..4)
            .prop_map(|v| format!("/{}", v.join("/"))),
        1 => pick(&["//evil.com/x", "///x", "/\\evil.com", "/", "//", "", "x", "?q", "#f"]).prop_map(str::to_string),
        1 => plain_string(),
    ]
}

pub fn app_id() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[a-zA-Z_][a-zA-Z0-9_-]{0,6}(\\.[a-zA-Z0-9_-]{1,8}){2,4}",
        2 => "[a-zA-Z0-9_.-]{0,20}",
        1 => nasty_string(),
        1 => pick(&["org.kde", "a..b.c", ".a.b.c", "a.b.c.", "9a.b.c", "a.9b.c", "a.b.c\n"]).prop_map(str::to_string),
    ]
}

pub fn version() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[0-9]{1,10}(\\.[0-9]{1,10}){0,6}(-[A-Za-z0-9.-]{0,20})?",
        1 => pick(&["", "1", "v1", "1.0+x", "999999999.9.9", "1.2.3.4.5.6.7", "1..2", "1.", ".1", "-", "1-", "1.0-0", "1.0-a.0", "00001", "1.0.0-beta.1", "1.0.0-01"]).prop_map(str::to_string),
        1 => nasty_string(),
    ]
}

// ---- JSON ----

#[derive(Debug, Clone)]
pub enum JOp {
    Remove(usize),
    Replace(usize, u8),
    Dup(usize),
}

fn count_objs(v: &Value) -> usize {
    match v {
        Value::Object(m) => 1 + m.values().map(count_objs).sum::<usize>(),
        Value::Array(a) => a.iter().map(count_objs).sum(),
        _ => 0,
    }
}

fn nth_obj<'a>(v: &'a mut Value, n: &mut usize) -> Option<&'a mut serde_json::Map<String, Value>> {
    match v {
        Value::Object(m) => {
            if *n == 0 {
                return Some(m);
            }
            *n -= 1;
            for c in m.values_mut() {
                if let Some(r) = nth_obj(c, n) {
                    return Some(r);
                }
            }
            None
        }
        Value::Array(a) => {
            for c in a {
                if let Some(r) = nth_obj(c, n) {
                    return Some(r);
                }
            }
            None
        }
        _ => None,
    }
}

/// Edits `v` (the objects in it) by removing keys, copying them and putting
/// values of the wrong type or extreme values in their place.
pub fn jmutate(v: &mut Value, ops: &[JOp]) {
    for o in ops {
        let count = count_objs(v);
        if count == 0 {
            return;
        }
        let (i, what) = match o {
            JOp::Remove(i) | JOp::Dup(i) => (*i, 0),
            JOp::Replace(i, w) => (*i, *w),
        };
        let mut n = i % count;
        let Some(m) = nth_obj(v, &mut n) else { return };
        if m.is_empty() {
            continue;
        }
        let key = m.keys().nth((i / 7) % m.len()).cloned().unwrap();
        match o {
            JOp::Remove(_) => {
                m.remove(&key);
            }
            JOp::Dup(_) => {
                let val = m[&key].clone();
                m.insert(format!("{key}2"), val);
            }
            JOp::Replace(..) => {
                let new = match what % 12 {
                    0 => Value::Null,
                    1 => json!(0),
                    2 => json!(-1),
                    3 => json!(u64::MAX),
                    4 => json!(1.5),
                    5 => json!(""),
                    6 => json!([]),
                    7 => json!({}),
                    8 => json!("\u{202E}x"),
                    9 => json!(true),
                    10 => json!(i64::MIN),
                    _ => json!("a".repeat(2000)),
                };
                m.insert(key, new);
            }
        }
    }
}

pub fn jops() -> impl Strategy<Value = Vec<JOp>> {
    prop::collection::vec(
        prop_oneof![
            (0usize..10_000).prop_map(JOp::Remove),
            (0usize..10_000, any::<u8>()).prop_map(|(i, w)| JOp::Replace(i, w)),
            (0usize..10_000).prop_map(JOp::Dup),
        ],
        0..5,
    )
}

/// A JSON document `base` edited by `jops`, then maybe byte-mutated.
pub fn json_variant(base: Value, dict: &'static [&'static [u8]]) -> impl Strategy<Value = Vec<u8>> {
    (
        jops(),
        prop::collection::vec(op(dict.len()), 0..3),
        any::<bool>(),
    )
        .prop_map(move |(jo, bo, pretty)| {
            let mut v = base.clone();
            jmutate(&mut v, &jo);
            let bytes = if pretty {
                serde_json::to_vec_pretty(&v)
            } else {
                serde_json::to_vec(&v)
            }
            .unwrap();
            apply(bytes, &bo, dict)
        })
}

pub const JSON_DICT: &[&[u8]] = &[
    b"\"..\"",
    b"\"/\"",
    b"null",
    b"\"../../etc/passwd\"",
    b"\"\\u202e\"",
    b"\\u0000",
    b"18446744073709551616",
    b"-0",
    b"1e400",
    b"{",
    b"}",
    b"[",
    b"]",
    b",",
    b"\"schema\":1",
    b"\"files\":[]",
    b"\"links\":[]",
    b"\"archive\":null",
    b"\"sha256\":\"",
    b"\"size\":",
    b"\"path\":\"",
    b"\"target\":\"",
    b"\"x86_64\"",
    b"\"id\":\"",
    b"\"tag_name\":\"",
    b"\"assets\":",
    b"\"browser_download_url\":\"",
    b"\"digest\":\"sha256:",
    b"\"state\":",
    b"\"draft\":true",
    b"\"prerelease\":true",
    b"\"repo\":\"",
    b"\"channel\":\"releases\"",
    b"\xEF\xBB\xBF",
    b"\xFF",
    b"\xC0\x80",
    b"\xED\xA0\x80",
];

// ---- native manifest ----

const HEX: &str = "0123456789abcdef";

pub fn sha() -> impl Strategy<Value = String> {
    prop_oneof![
        28 => prop::collection::vec(0usize..16, 64).prop_map(|v| v.iter().map(|i| &HEX[*i..*i + 1]).collect::<String>()),
        1 => "[0-9A-F]{64}",
        1 => "[0-9a-f]{0,70}",
    ]
}

pub fn rel_path() -> impl Strategy<Value = String> {
    prop_oneof![
        17 => prop::collection::vec(prop_oneof![
            4 => "[a-z][a-z0-9._-]{0,8}",
            1 => pick(&["bin", "share", "lib", "applications", "icons", "hicolor", "apps", "metainfo", "dbus-1", "services"]).prop_map(str::to_string),
        ], 1..5).prop_map(|v| v.join("/")),
        2 => pick(&[
            "..", "../x", "a/../b", "/abs", "a//b", ".", "a/.", "a/./b", "telamon-bundle.json",
            "bin/", "", "a/b/", "bin/\u{202E}x", "x\ny", "é/ü", "\u{200B}", "a\\b", "a b",
        ]).prop_map(str::to_string),
        1 => plain_string(),
    ]
}

pub fn link_target() -> impl Strategy<Value = String> {
    prop_oneof![
        14 => prop::collection::vec(pick(&["..", "a", "b", "bin", "lib", "x.so"]), 1..5).prop_map(|v| v.join("/")),
        2 => pick(&["", ".", "/", "/etc/passwd", "../../..", "a//b", "a/./b", "./a", "..", "../..", "a/..", "a/../..", "a/../../b"]).prop_map(str::to_string),
        1 => plain_string(),
    ]
}

/// A manifest as JSON: valid most of the time, with one or more fields wrong.
pub fn manifest_value(outer: bool) -> impl Strategy<Value = Value> {
    let id = prop_oneof![
        20 => Just("net.eterneon.telamon.gates".to_string()),
        6 => "[a-zA-Z][a-zA-Z0-9_-]{0,6}(\\.[a-zA-Z0-9_-]{1,6}){2,3}",
        1 => app_id(),
    ];
    let files = prop::collection::vec(
        (
            rel_path(),
            prop_oneof![30 => 0u64..100_000, 1 => any::<u64>(), 1 => Just(512u64 << 20), 1 => Just((512u64 << 20) + 1)],
            sha(),
            any::<bool>(),
        ),
        1..8,
    );
    let links = prop::collection::vec((rel_path(), link_target()), 0..4);
    (
        id,
        prop_oneof![15 => "[0-9]{1,3}(\\.[0-9]{1,3}){0,3}(-[a-z]{1,4}\\.[0-9])?", 1 => version()],
        (
            files,
            links,
            prop_oneof![30 => Just("x86_64".to_string()), 1 => "[a-z0-9_]{0,8}"],
            prop_oneof![20 => Just("2.0.2".to_string()), 1 => version()],
            prop_oneof![20 => Just("44".to_string()), 1 => "[0-9]{0,6}", 1 => nasty_string()],
        ),
        (
            prop_oneof![8 => "[A-Za-z][A-Za-z0-9 ]{0,20}", 2 => nasty_string()],
            prop_oneof![8 => "[A-Za-z0-9 .,]{0,60}", 2 => nasty_string()],
            prop_oneof![6 => Just("https://github.com/EternalCoder454/telamon-gates".to_string()), 2 => Just(String::new()), 1 => url()],
            prop_oneof![8 => Just("MIT".to_string()), 2 => nasty_string()],
        ),
        (sha(), 0u64..300_000_000, any::<bool>()),
    )
        .prop_map(move |(id, version, (files, links, arch, ui, os), (name, summary, home, license), (asha, asize, good_name))| {
            let mut v = json!({
                "schema": 1,
                "id": id,
                "name": name,
                "version": version,
                "summary": summary,
                "homepage": home,
                "license": license,
                "arch": arch,
                "min_telamon_ui": ui,
                "min_os_version": os,
                "files": files.iter().map(|(p, s, h, x)| json!({"path": p, "size": s, "sha256": h, "executable": x})).collect::<Vec<_>>(),
                "links": links.iter().map(|(p, t)| json!({"path": p, "target": t})).collect::<Vec<_>>(),
            });
            if outer {
                let name = if good_name || asize % 7 != 0 {
                    format!("{}-{}-x86_64.tar.zst", v["id"].as_str().unwrap(), v["version"].as_str().unwrap())
                } else {
                    "x.tar.zst".to_string()
                };
                v["archive"] = json!({"name": name, "sha256": asha, "size": asize});
            }
            v
        })
}

/// A manifest that passes the Store's checks, for starting from.
pub fn valid_manifest_value(outer: bool) -> Value {
    let mut v = json!({
        "schema": 1,
        "id": "net.eterneon.telamon.gates",
        "name": "Telamon Gates",
        "version": "0.2.0",
        "summary": "Chat with a local AI model",
        "homepage": "https://github.com/EternalCoder454/telamon-gates",
        "license": "MIT",
        "arch": "x86_64",
        "min_telamon_ui": "2.0.2",
        "min_os_version": "44",
        "files": [
            {"path": "bin/telamon-gates", "size": 10, "sha256": "a".repeat(64), "executable": true},
            {"path": "share/applications/net.eterneon.telamon.gates.desktop", "size": 40, "sha256": "b".repeat(64)},
        ],
        "links": [{"path": "bin/gates", "target": "telamon-gates"}],
    });
    if outer {
        v["archive"] = json!({"name": "net.eterneon.telamon.gates-0.2.0-x86_64.tar.zst", "sha256": "c".repeat(64), "size": 1000});
    }
    v
}

pub fn catalog_value() -> impl Strategy<Value = Value> {
    let entry = (
        prop_oneof![3 => Just("net.eterneon.telamon.gates".to_string()), 2 => app_id()],
        prop_oneof![
            3 => Just("EternalCoder454/telamon-gates".to_string()),
            2 => "[A-Za-z0-9_.-]{0,12}/[A-Za-z0-9_.-]{0,12}",
            1 => pick(&["eternalcoder454/x", "EternalCoder454/../x", "EternalCoder454/a b", "EternalCoder454/x.git", "EternalCoder454/", "/x", "EternalCoder454", "EternalCoder454/x/y", "Eternalcoder4540/x"]).prop_map(str::to_string),
        ],
        prop_oneof![4 => Just("releases".to_string()), 1 => "[a-z]{0,8}"],
    );
    (prop::collection::vec(entry, 0..12), prop_oneof![5 => Just(1u64), 1 => any::<u64>()]).prop_map(
        |(es, schema)| {
            json!({"schema": schema, "apps": es.iter().map(|(i, r, c)| json!({"id": i, "repo": r, "channel": c})).collect::<Vec<_>>()})
        },
    )
}

pub fn release_value() -> impl Strategy<Value = Value> {
    let tag = prop_oneof![
        4 => "[a-z]{0,2}[0-9]{1,2}(\\.[0-9]{1,2}){0,3}",
        1 => pick(&["", "v1/../x", "v1 2", "é", "v1.0.0-rc.1", "v1\n"]).prop_map(str::to_string),
        1 => Just("a".repeat(65)),
    ];
    let name = prop_oneof![
        4 => "[a-z][a-z0-9._-]{0,12}",
        1 => pick(&["telamon-bundle.json", "a b", "a/b", "../x", "", "é.tar.zst"]).prop_map(str::to_string),
    ];
    let asset = (name, 0u64..1_000_000, any::<bool>(), 0u8..6, 0u8..4, 0u8..4);
    (
        tag,
        prop::collection::vec(asset, 0..8),
        prop_oneof![9 => Just(false), 1 => Just(true)],
        prop_oneof![9 => Just(false), 1 => Just(true)],
    )
        .prop_map(|(tag, assets, draft, pre)| {
            let repo = "EternalCoder454/telamon-gates";
            let list: Vec<Value> = assets
                .iter()
                .map(|(name, size, right_prefix, digest, state, case)| {
                    let base = match *case {
                        0 | 1 => format!("https://github.com/{repo}/releases/download/{tag}/"),
                        2 => format!(
                            "https://github.com/{}/releases/download/{tag}/",
                            repo.to_lowercase()
                        ),
                        _ => format!("https://github.com/{repo}/releases/download/other/"),
                    };
                    let url = if *right_prefix {
                        format!("{base}{name}")
                    } else {
                        format!("https://evil.example/{name}")
                    };
                    let mut a = json!({"name": name, "size": size, "browser_download_url": url});
                    match *digest {
                        0 => {}
                        1 => a["digest"] = json!(format!("sha256:{}", "ab".repeat(32))),
                        2 => a["digest"] = json!(format!("sha256:{}", "AB".repeat(32))),
                        3 => a["digest"] = json!("md5:abc"),
                        4 => a["digest"] = json!(null),
                        _ => a["digest"] = json!("sha256:"),
                    }
                    match *state {
                        0 => {}
                        1 => a["state"] = json!("uploaded"),
                        2 => a["state"] = json!("open"),
                        _ => a["state"] = json!(null),
                    }
                    a
                })
                .collect();
            json!({"tag_name": tag, "draft": draft, "prerelease": pre, "assets": list})
        })
}

// ---- key files ----

pub const KEYFILE_DICT: &[&[u8]] = &[
    b"[",
    b"]",
    b"[Desktop Entry]\n",
    b"[Flatpak Ref]\n",
    b"[Flatpak Repo]\n",
    b"\n",
    b"\r\n",
    b"\r",
    b"=",
    b" = ",
    b"\\n",
    b"\\s",
    b"\\;",
    b"\\",
    b"\\x",
    b"#",
    b"; ",
    b"[de]",
    b"Exec=",
    b"TryExec=",
    b"Path=",
    b"Exec[de]=",
    b"Name=",
    b"Type=Application\n",
    b"Url=https://",
    b"GPGKey=",
    b"Filter=",
    b"Authenticator",
    b"X-Telamon-Native-App=x\n",
    b"\0",
    b"\xC3\x28",
    b"\xEF\xBB\xBF",
    b"\xC2\xA0",
    b"\x0B",
    b"\x0C",
    b"%f",
    b"%U",
    b"\"",
    b"%%",
];

pub fn keyfile_text() -> impl Strategy<Value = Vec<u8>> {
    let group = prop_oneof![
        30 => pick(&["Desktop Entry", "Flatpak Ref", "Flatpak Repo", "Desktop Action new", "D-BUS Service", "Context", "Application"]).prop_map(str::to_string),
        1 => plain_string(),
    ];
    let key = prop_oneof![
        40 => pick(&["Type", "Name", "Exec", "TryExec", "Path", "Icon", "Comment", "Url", "Title", "Branch", "IsRuntime", "GPGKey", "Filter", "AuthenticatorName", "X-Telamon-Native-App", "Name[de]", "Exec[de]", "Categories", "Terminal", "Version", "Homepage", "CollectionID"]).prop_map(str::to_string),
        10 => "[A-Za-z][A-Za-z0-9-]{0,8}(\\[[a-z_]{1,5}\\])?",
        1 => plain_string().prop_filter("a key", |k| !k.is_empty()),
    ];
    let value = prop_oneof![
        3 => pick(&["Application", "true", "false", "1", "0", "yes", "stable", "org.test.Hello", "https://example.org/repo", "https://dl.example.org/x.flatpakrepo", "telamon-gates %U", "telamon-gates", "/bin/sh -c id", "a\\;b;c", "a;b;c;", "\\s lead", "trail\\", "x\\ny", "", " ", "\t"]).prop_map(str::to_string),
        2 => plain_string(),
    ];
    let line = prop_oneof![
        40 => prop_oneof![
            2 => group.prop_map(|g| format!("[{g}]")),
            8 => (key, value).prop_map(|(k, v)| format!("{k}={v}")),
            1 => plain_string().prop_map(|c| format!("#{c}")),
            1 => Just(String::new()),
        ],
        1 => plain_string(),
    ];
    let eol = pick(&["\n", "\n", "\n", "\r\n", "\r", "\n\n"]);
    (prop::collection::vec((line, eol), 0..40), any::<bool>()).prop_map(|(ls, last)| {
        let mut s: String = ls.iter().map(|(l, e)| format!("{l}{e}")).collect();
        if !last {
            // No newline after the last line.
            while s.ends_with(['\n', '\r']) {
                s.pop();
            }
        }
        s.into_bytes()
    })
}

pub fn keyfile_limits() -> impl Strategy<Value = telamon_store_core::keyfile::Limits> {
    prop_oneof![
        2 => Just(telamon_store_core::keyfile::Limits::default()),
        3 => (1usize..600, 1usize..30, 1usize..6, 1usize..30, 1usize..80).prop_map(|(b, l, g, k, v)| telamon_store_core::keyfile::Limits {
            max_bytes: b, max_lines: l, max_groups: g, max_keys: k, max_value: v,
        }),
    ]
}

pub const FLATPAKREF: &[u8] = include_bytes!("../fixtures/flatpakref/hello.flatpakref");
pub const FLATPAKREPO: &[u8] = include_bytes!("../fixtures/flatpakref/test.flatpakrepo");
pub const ED_KEY: &str = include_str!("../fixtures/flatpakref/ed.b64");
pub const RSA_KEY: &str = include_str!("../fixtures/flatpakref/rsa.b64");

pub const FLATPAK_DICT: &[&[u8]] = &[
    b"\n",
    b"\r\n",
    b"Name=",
    b"Branch=",
    b"Url=",
    b"Url=http://",
    b"Url=https://x.org:8443/",
    b"Url=https://user@x.org/",
    b"Url=https://localhost/",
    b"Url=https://127.0.0.1/",
    b"IsRuntime=",
    b"true",
    b"false",
    b"GPGKey=",
    b"Filter=/etc/passwd\n",
    b"AuthenticatorName=x\n",
    b"SuggestRemoteName=",
    b"RuntimeRepo=https://",
    b"CollectionID=",
    b"DeployCollectionID=",
    b"Title=",
    b"Comment=",
    b"Description=",
    b"Icon=",
    b"Homepage=",
    b"DefaultBranch=",
    b"Unknown=1\n",
    b"[Flatpak Ref]\n",
    b"[Flatpak Repo]\n",
    b"[Other]\n",
    b"\\n",
    b"\\",
    b"\0",
    b"\xFF",
    b"=",
    b"\xE2\x80\xAE",
    b"AAAA",
    b"====",
    b"A",
    b" ",
];

/// A `.flatpakref` / `.flatpakrepo` built key by key, with keys repeated,
/// dropped and wrong, and a real key now and then.
pub fn flatpak_file(repo: bool) -> impl Strategy<Value = Vec<u8>> {
    let keys: &'static [&'static str] = if repo {
        &[
            "Url",
            "Title",
            "Comment",
            "Description",
            "Icon",
            "Homepage",
            "DefaultBranch",
            "GPGKey",
            "CollectionID",
            "DeployCollectionID",
            "Filter",
            "AuthenticatorName",
            "Unknown",
        ]
    } else {
        &[
            "Name",
            "Branch",
            "Url",
            "Title",
            "Comment",
            "Description",
            "Icon",
            "Homepage",
            "IsRuntime",
            "GPGKey",
            "RuntimeRepo",
            "SuggestRemoteName",
            "CollectionID",
            "DeployCollectionID",
            "Filter",
            "Unknown",
        ]
    };
    let pools: Vec<Vec<String>> = keys
        .iter()
        .map(|k| match *k {
            "Name" => vec![
                "org.test.Hello".into(),
                "org.test.Platform".into(),
                "a".into(),
                "a/b.c".into(),
                "-x.y".into(),
                "org.test.Hello\n".into(),
            ],
            "Branch" | "DefaultBranch" => vec![
                "stable".into(),
                "master".into(),
                "x/y".into(),
                "-1".into(),
                "".into(),
                ".z".into(),
            ],
            "Url" | "RuntimeRepo" => vec![
                "https://dl.example.org/test/repo/".into(),
                "https://dl.example.org/test.flatpakrepo".into(),
                "http://dl.example.org/x".into(),
                "https://127.0.0.1/x".into(),
                "https://u@dl.example.org/".into(),
                "https://dl.example.org/../x".into(),
                "file:///etc".into(),
                "".into(),
            ],
            "IsRuntime" => vec![
                "true".into(),
                "false".into(),
                "1".into(),
                "0".into(),
                "yes".into(),
                "True".into(),
                "".into(),
            ],
            "GPGKey" => vec![
                ED_KEY.trim().into(),
                RSA_KEY.trim().into(),
                "AAAA".into(),
                "!!!".into(),
                "".into(),
                format!("{}A", ED_KEY.trim()),
            ],
            "SuggestRemoteName" => vec![
                "x-origin".into(),
                "-x".into(),
                "a b".into(),
                "a/b".into(),
                "".into(),
                "é".repeat(5),
            ],
            "CollectionID" | "DeployCollectionID" => vec![
                "org.test.Collection".into(),
                "x".into(),
                "".into(),
                "org.t est.x".into(),
            ],
            "Filter" | "AuthenticatorName" => vec!["/etc/passwd".into(), "x".into()],
            "Icon" | "Homepage" => vec![
                "https://example.org/i.png".into(),
                "http://example.org".into(),
                "javascript:x".into(),
                "".into(),
            ],
            _ => vec![
                "Hello".into(),
                "A\\nB".into(),
                "\u{202E}evil".into(),
                "x".repeat(400),
                "".into(),
            ],
        })
        .collect();
    let n = keys.len();
    let line = (0..n).prop_flat_map(move |i| {
        let pool = pools[i].clone();
        let (first, others) = (pool[0].clone(), pool[1..].to_vec());
        (
            Just(keys[i]),
            prop_oneof![
                30 => Just(first),
                3 => select(others),
                1 => plain_string(),
            ],
        )
    });
    let header = if repo {
        "[Flatpak Repo]"
    } else {
        "[Flatpak Ref]"
    };
    let required: Vec<&'static str> = if repo {
        vec!["Url"]
    } else {
        vec!["Name", "Url"]
    };
    let first_values: Vec<String> = required
        .iter()
        .map(|k| {
            let at = keys.iter().position(|x| x == k).unwrap();
            match *k {
                "Name" => "org.test.Hello".to_string(),
                _ => {
                    let _ = at;
                    "https://dl.example.org/test/repo/".to_string()
                }
            }
        })
        .collect();
    (
        prop::collection::vec(line, 0..10),
        prop_oneof![40 => Just(header.to_string()), 1 => Just("[Flatpak Ref]".to_string()), 1 => Just("[x]".to_string())],
        any::<bool>(),
        prop::collection::vec(prop_oneof![30 => Just(true), 1 => Just(false)], required.len()),
    )
        .prop_map(move |(ls, h, crlf, have)| {
            let eol = if crlf { "\r\n" } else { "\n" };
            let mut s = format!("{h}{eol}");
            for ((k, v), have) in required.iter().zip(&first_values).zip(&have) {
                if *have {
                    s.push_str(&format!("{k}={v}{eol}"));
                }
            }
            for (k, v) in ls {
                s.push_str(&format!("{k}={v}{eol}"));
            }
            s.into_bytes()
        })
}

// ---- desktop entries ----

pub const DESKTOP_DICT: &[&[u8]] = &[
    b"Exec=",
    b"TryExec=",
    b"Path=",
    b"Exec[de]=",
    b"X-Telamon-Native-App=evil\n",
    b"[Desktop Entry]\n",
    b"[Desktop Action x]\n",
    b"\n",
    b"\r\n",
    b"\r",
    b" ",
    b"\t",
    b"\\s",
    b"\\\\",
    b"\\n",
    b"\"",
    b"%f",
    b"%U",
    b"%%",
    b"telamon-gates",
    b"../",
    b"/",
    b"\xC2\xA0",
    b"\xE2\x80\xA8",
    b"\x0B",
    b"\x0C",
    b"\0",
    b"Type=Application\n",
    b"D-BUS Service",
    b"Name=",
];

pub fn desktop_text() -> impl Strategy<Value = Vec<u8>> {
    let exec = prop_oneof![
        24 => pick(&["telamon-gates %U", "telamon-gates", "telamon-gates --new", "telamon-gates\t%f", "telamon-gates %%", "telamon-gates \"a b\" 'c'", "telamon-gates \\s", "other -x"]).prop_map(str::to_string),
        3 => pick(&["/bin/sh -c id", "\"telamon-gates\"", "env x=y telamon-gates", "../telamon-gates", "bin/telamon-gates", ".hidden", "", " telamon-gates", "other", "telamon-gates\u{a0}x", "telamon-gates\\"]).prop_map(str::to_string),
        1 => plain_string(),
    ];
    let key = prop_oneof![
        4 => pick(&["Type", "Name", "Comment", "Icon", "Terminal", "Categories", "MimeType", "Actions", "StartupWMClass", "Version"]).prop_map(str::to_string),
        3 => pick(&["Exec", "Exec", "TryExec", "Path", "X-Telamon-Native-App", "X-Telamon-Native-Version", "X-Telamon-Native-Foo", "Exec[de]", "TryExec[de]", "Path[de]", "\u{a0}Exec", "Exec\u{a0}", "exec"]).prop_map(str::to_string),
        1 => plain_string(),
    ];
    let group = prop_oneof![
        4 => Just("Desktop Entry".to_string()),
        2 => pick(&["Desktop Action new", "Desktop Action x", "Other"]).prop_map(str::to_string),
        1 => plain_string(),
    ];
    let kv = (
        key,
        prop_oneof![
            3 => exec.clone(),
            2 => pick(&["Application", "Link", "Gates", "x", "", "true"]).prop_map(str::to_string),
            1 => plain_string(),
        ],
    )
        .prop_map(|(k, v)| {
            let v = if k.starts_with("Exec") || k.contains("Exec") {
                v
            } else {
                v.replace("telamon-gates", "x")
            };
            format!("{k}={v}\n")
        });
    let block = (
        group,
        prop::collection::vec(kv, 0..6),
        pick(&["", "\n", "# c\n", "\r\n"]),
    )
        .prop_map(|(g, kvs, sep)| format!("{sep}[{g}]\n{}", kvs.concat()));
    let main = prop::collection::vec(exec, 1..3).prop_map(|execs| {
        format!(
            "[Desktop Entry]\nType=Application\nName=Gates\n{}Icon=x\n",
            execs
                .iter()
                .map(|e| format!("Exec={e}\n"))
                .collect::<String>()
        )
    });
    (
        prop_oneof![5 => main, 1 => Just(String::new())],
        prop::collection::vec(block, 0..4),
    )
        .prop_map(|(m, bs)| format!("{m}{}", bs.concat()).into_bytes())
}

pub fn dbus_text() -> impl Strategy<Value = Vec<u8>> {
    let name = prop_oneof![
        3 => Just("net.eterneon.telamon.gates".to_string()),
        2 => Just("net.eterneon.telamon.gates.Service".to_string()),
        2 => pick(&["net.eterneon.telamon.gatesx", "org.freedesktop.Notifications", "net.eterneon.telamon", "", "net.eterneon.telamon.gates."]).prop_map(str::to_string),
    ];
    let exec = pick(&[
        "telamon-gates --gapplication-service",
        "telamon-gates",
        "/usr/bin/sh -c x",
        "other x",
        "",
        "telamon-gates \"a b\"",
    ]);
    (
        name,
        exec,
        pick(&["[D-BUS Service]\n", "[D-BUS Service]\r\n", "[x]\n"]),
        any::<bool>(),
    )
        .prop_map(|(n, e, h, extra)| {
            format!(
                "{h}Name={n}\nExec={e}\n{}",
                if extra {
                    "User=root\nSystemdService=x\n"
                } else {
                    ""
                }
            )
            .into_bytes()
        })
}

pub fn prefix() -> impl Strategy<Value = std::path::PathBuf> {
    prop_oneof![
        3 => Just("/home/u/.local/share/telamon-apps/net.eterneon.telamon.gates/current".to_string()),
        4 => prop::collection::vec(pick(&["home", "my user", "a\"b", "it's", "$HOME", "`id`", "100%", "50%%", "back\\slash", "tab\there", "semi;colon", "(paren)", "a&b", "é", "~", "*", "?", "#hash", "<x>", "|", "x y z", "'", "\"", "\\", "% f", "%f"]), 1..5)
            .prop_map(|v| format!("/{}", v.join("/"))),
        1 => plain_string().prop_map(|s| format!("/{s}")),
        1 => nasty_string().prop_map(|s| format!("/{s}").replace('\0', "")),
    ]
    .prop_map(std::path::PathBuf::from)
}

pub fn programs() -> impl Strategy<Value = std::collections::BTreeSet<String>> {
    prop::collection::btree_set(
        prop_oneof![3 => Just("telamon-gates".to_string()), 1 => Just("other".to_string()), 1 => "[a-z][a-z0-9._+-]{0,10}"],
        0..3,
    )
}
