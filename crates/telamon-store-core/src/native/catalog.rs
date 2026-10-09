//! The list of connected apps: `catalog/native-apps.json` in the Store's
//! repository, fetched from its main branch (so connecting an app is one line
//! in a pull request, not a Store release) and cached.
//!
//! ```json
//! { "schema": 1,
//!   "apps": [ { "id": "net.eterneon.telamon.gates",
//!               "repo": "EternalCoder454/telamon-gates",
//!               "channel": "releases",
//!               "signers": [ { "type": "minisign", "key": "RWT..." } ] } ] }
//! ```
//!
//! An entry names the app's ID, the one GitHub repository whose releases may
//! provide it, and 1 to 4 `signers`: the keys that may sign its releases (see
//! [`super::sign`]). A release is used only when one of them verifies its
//! manifest, so an entry with no usable signer is skipped, never installed
//! unchecked. The only `type` is `minisign`, whose `key` is the `RW...`
//! string of the public key; a `type` the Store does not know is ignored (a
//! later kind can be added beside it, and an older Store keeps using the
//! kinds it knows), but a `minisign` signer with a key that cannot be read
//! skips the entry, so a typo is loud and never leaves the entry with fewer
//! keys than its owner listed. Entries for owners the Store does not know
//! ([`ALLOWED_OWNERS`]), with another channel, or malformed are skipped and
//! reported, never half-used. `schema` stays 1: `signers` is an addition an
//! older Store does not read (it also cannot verify, and has no signatures
//! to ask for, so it keeps its own behavior until it is updated).

use serde::Deserialize;

use super::sign::{self, MAX_SIGNERS, Signer};
use super::{ALLOWED_OWNERS, Error, err, valid_app_id};

/// Largest catalog read.
pub const MAX_CATALOG: u64 = 256 * 1024;
/// Most entries kept.
pub const MAX_ENTRIES: usize = 200;

/// One connected app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    /// `Owner/name`, as GitHub spells it.
    pub repo: String,
    /// Only `releases` (the latest published, non-prerelease release).
    pub channel: String,
    /// The keys that may sign this app's releases; never empty.
    pub signers: Vec<Signer>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub apps: Vec<Entry>,
    /// Why entries were left out, in plain words (for the log).
    pub skipped: Vec<String>,
}

#[derive(Deserialize)]
struct Raw {
    schema: u32,
    #[serde(default)]
    apps: Vec<RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    channel: String,
    #[serde(default)]
    signers: Vec<RawSigner>,
}

#[derive(Deserialize)]
struct RawSigner {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    key: String,
}

/// The usable signers of an entry, or why there are none. Unknown types are
/// ignored; a key listed twice counts once.
fn read_signers(raw: &[RawSigner]) -> Result<Vec<Signer>, &'static str> {
    if raw.len() > MAX_SIGNERS {
        return Err("too many signers");
    }
    let mut out: Vec<Signer> = Vec::new();
    for s in raw {
        if s.kind != sign::MINISIGN {
            continue;
        }
        let Some(signer) = Signer::minisign(&s.key) else {
            return Err("signer key not usable");
        };
        if !out.iter().any(|o| o.key_id == signer.key_id) {
            out.push(signer);
        }
    }
    if out.is_empty() {
        return Err("no signer");
    }
    Ok(out)
}

/// `Owner/name` with an owner the Store knows: GitHub's characters only, so
/// it can be put in an address, and compared without regard to case.
pub fn valid_repo(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else {
        return false;
    };
    let name_ok = !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && !name.ends_with(".git")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    name_ok
        && ALLOWED_OWNERS
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(owner))
}

impl Catalog {
    pub fn parse(bytes: &[u8]) -> Result<Catalog, Error> {
        if bytes.len() as u64 > MAX_CATALOG {
            return Err(err(
                "The list of Telamon apps is larger than the Store accepts.",
            ));
        }
        let raw: Raw = serde_json::from_slice(bytes)
            .map_err(|_| err("The list of Telamon apps is not valid."))?;
        if raw.schema != 1 {
            return Err(err(
                "The list of Telamon apps is in a newer format. Update the Store.",
            ));
        }
        let mut out = Catalog::default();
        for (i, e) in raw.apps.into_iter().enumerate() {
            if out.apps.len() >= MAX_ENTRIES {
                out.skipped.push("too many entries".into());
                break;
            }
            let signers = read_signers(&e.signers);
            let why = if !valid_app_id(&e.id) {
                Some("not an app ID")
            } else if e.channel != "releases" {
                Some("unknown channel")
            } else if !valid_repo(&e.repo) {
                Some("repository not allowed")
            } else if out.apps.iter().any(|a| a.id == e.id) {
                Some("listed twice")
            } else {
                None
            };
            match (why, signers) {
                (Some(why), _) | (None, Err(why)) => {
                    out.skipped.push(format!("entry {}: {why}", i + 1));
                }
                (None, Ok(signers)) => out.apps.push(Entry {
                    id: e.id,
                    repo: e.repo,
                    channel: e.channel,
                    signers,
                }),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUB_A: &str = include_str!("../../tests/fixtures/native/signed/a.pub");
    const PUB_B: &str = include_str!("../../tests/fixtures/native/signed/b.pub");

    fn key(pub_file: &str) -> &str {
        pub_file.lines().nth(1).unwrap()
    }

    fn signers_json(keys: &[&str]) -> String {
        let list: Vec<String> = keys
            .iter()
            .map(|k| format!(r#"{{"type":"minisign","key":"{k}"}}"#))
            .collect();
        format!("[{}]", list.join(","))
    }

    fn good() -> String {
        format!(
            r#"{{"schema":1,"apps":[
        {{"id":"net.eterneon.telamon.gates","repo":"EternalCoder454/telamon-gates","channel":"releases","signers":{}}}]}}"#,
            signers_json(&[key(PUB_A)])
        )
    }

    #[test]
    fn a_connected_app_is_read() {
        let c = Catalog::parse(good().as_bytes()).unwrap();
        assert_eq!(c.apps.len(), 1);
        assert_eq!(c.apps[0].id, "net.eterneon.telamon.gates");
        assert_eq!(c.apps[0].repo, "EternalCoder454/telamon-gates");
        assert_eq!(c.apps[0].signers.len(), 1);
        assert_eq!(c.apps[0].signers[0].key, key(PUB_A));
        assert_eq!(c.apps[0].signers[0].key_id, "3AA68B0BC652CAF0");
        assert!(c.skipped.is_empty());
    }

    #[test]
    fn an_empty_list_is_fine() {
        let c = Catalog::parse(br#"{"schema":1,"apps":[]}"#).unwrap();
        assert!(c.apps.is_empty());
        let c = Catalog::parse(br#"{"schema":1}"#).unwrap();
        assert!(c.apps.is_empty());
    }

    #[test]
    fn entries_for_strangers_are_skipped_not_used() {
        let s = signers_json(&[key(PUB_A)]);
        let text = format!(
            r#"{{"schema":1,"apps":[
          {{"id":"a.x.b","repo":"EvilCorp/a","channel":"releases","signers":{s}}},
          {{"id":"a.x.c","repo":"EternalCoder454/a/../../b","channel":"releases","signers":{s}}},
          {{"id":"a.x.d","repo":"EternalCoder454/a","channel":"nightly","signers":{s}}},
          {{"id":"bad","repo":"EternalCoder454/a","channel":"releases","signers":{s}}},
          {{"id":"a.x.e","repo":"eternalcoder454/ok.app","channel":"releases","signers":{s}}},
          {{"id":"a.x.e","repo":"EternalCoder454/again","channel":"releases","signers":{s}}},
          {{"id":"a.x.f","repo":"EternalCoder454/x.git","channel":"releases","signers":{s}}},
          {{"id":"a.x.g","repo":"EternalCoder454/..","channel":"releases","signers":{s}}},
          {{"repo":"EternalCoder454/y"}}]}}"#
        );
        let c = Catalog::parse(text.as_bytes()).unwrap();
        assert_eq!(c.apps.len(), 1, "{c:?}");
        assert_eq!(c.apps[0].id, "a.x.e");
        assert_eq!(c.skipped.len(), 8);
    }

    #[test]
    fn an_entry_without_a_usable_signer_is_skipped() {
        let entry = |signers: &str| {
            format!(
                r#"{{"schema":1,"apps":[{{"id":"a.x.e","repo":"EternalCoder454/ok","channel":"releases"{signers}}}]}}"#
            )
        };
        let a = key(PUB_A);
        for (name, text) in [
            ("no field", entry("")),
            ("empty list", entry(r#","signers":[]"#)),
            ("not a list", entry(r#","signers":{"type":"minisign"}"#)),
            (
                "only an unknown type",
                entry(&format!(
                    r#","signers":[{{"type":"sigstore","key":"{a}"}}]"#
                )),
            ),
            (
                "no type",
                entry(&format!(r#","signers":[{{"key":"{a}"}}]"#)),
            ),
            (
                "a key that is not a key",
                entry(r#","signers":[{"type":"minisign","key":"RWnope"}]"#),
            ),
            (
                "one good key and one bad key",
                entry(&format!(
                    r#","signers":[{{"type":"minisign","key":"{a}"}},{{"type":"minisign","key":"RWnope"}}]"#
                )),
            ),
            (
                "a truncated key",
                entry(r#","signers":[{"type":"minisign","key":"RWRTY0Iy"}]"#),
            ),
            (
                "five signers",
                entry(&format!(r#","signers":{}"#, signers_json(&[a; 5]))),
            ),
        ] {
            let parsed = Catalog::parse(text.as_bytes());
            // "not a list" is a broken file as a whole; the others skip the entry.
            match parsed {
                Ok(c) => {
                    assert!(c.apps.is_empty(), "{name}: {c:?}");
                    assert_eq!(c.skipped.len(), 1, "{name}");
                }
                Err(_) => assert_eq!(name, "not a list"),
            }
        }
        // The reasons are told.
        let c = Catalog::parse(entry("").as_bytes()).unwrap();
        assert!(c.skipped[0].contains("no signer"), "{:?}", c.skipped);
    }

    #[test]
    fn unknown_signer_types_are_ignored_beside_a_known_one() {
        let (a, b) = (key(PUB_A), key(PUB_B));
        let text = format!(
            r#"{{"schema":1,"apps":[{{"id":"a.x.e","repo":"EternalCoder454/ok","channel":"releases","signers":[
              {{"type":"sigstore","issuer":"https://token.actions.githubusercontent.com","identity":"x"}},
              {{"type":"minisign","key":"{a}"}},
              {{"type":"minisign","key":"{a}"}},
              {{"type":"minisign","key":"{b}"}}]}}]}}"#
        );
        let c = Catalog::parse(text.as_bytes()).unwrap();
        assert!(c.skipped.is_empty(), "{:?}", c.skipped);
        let ids: Vec<_> = c.apps[0].signers.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(ids, [a, b], "a key listed twice counts once");
    }

    #[test]
    fn a_broken_or_future_list_is_an_error() {
        assert!(Catalog::parse(b"nope").is_err());
        assert!(Catalog::parse(br#"{"schema":2,"apps":[]}"#).is_err());
        assert!(Catalog::parse(&vec![b' '; 300 * 1024]).is_err());
    }

    #[test]
    fn at_most_so_many_entries() {
        let s = signers_json(&[key(PUB_A)]);
        let entries: Vec<String> = (0..300)
            .map(|i| {
                format!(
                    r#"{{"id":"a.x.app{i}","repo":"EternalCoder454/r{i}","channel":"releases","signers":{s}}}"#
                )
            })
            .collect();
        let text = format!(r#"{{"schema":1,"apps":[{}]}}"#, entries.join(","));
        let c = Catalog::parse(text.as_bytes()).unwrap();
        assert_eq!(c.apps.len(), MAX_ENTRIES);
    }
}
