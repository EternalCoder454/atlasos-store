//! The list of connected apps: `catalog/native-apps.json` in the Store's
//! repository, fetched from its main branch (so connecting an app is one line
//! in a pull request, not a Store release) and cached.
//!
//! ```json
//! { "schema": 1,
//!   "apps": [ { "id": "net.eterneon.telamon.gates",
//!               "repo": "EternalCoder454/telamon-gates",
//!               "channel": "releases" } ] }
//! ```
//!
//! An entry names the app's ID and the one GitHub repository whose releases
//! may provide it. Entries for owners the Store does not know
//! ([`ALLOWED_OWNERS`]), with another channel, or malformed are skipped and
//! reported, never half-used.

use serde::Deserialize;

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
            match why {
                Some(why) => out.skipped.push(format!("entry {}: {why}", i + 1)),
                None => out.apps.push(Entry {
                    id: e.id,
                    repo: e.repo,
                    channel: e.channel,
                }),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"schema":1,"apps":[
        {"id":"net.eterneon.telamon.gates","repo":"EternalCoder454/telamon-gates","channel":"releases"}]}"#;

    #[test]
    fn a_connected_app_is_read() {
        let c = Catalog::parse(GOOD.as_bytes()).unwrap();
        assert_eq!(c.apps.len(), 1);
        assert_eq!(c.apps[0].id, "net.eterneon.telamon.gates");
        assert_eq!(c.apps[0].repo, "EternalCoder454/telamon-gates");
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
        let text = r#"{"schema":1,"apps":[
          {"id":"a.b","repo":"EvilCorp/a","channel":"releases"},
          {"id":"a.c","repo":"EternalCoder454/a/../../b","channel":"releases"},
          {"id":"a.d","repo":"EternalCoder454/a","channel":"nightly"},
          {"id":"bad","repo":"EternalCoder454/a","channel":"releases"},
          {"id":"a.e","repo":"eternalcoder454/ok.app","channel":"releases"},
          {"id":"a.e","repo":"EternalCoder454/again","channel":"releases"},
          {"id":"a.f","repo":"EternalCoder454/x.git","channel":"releases"},
          {"id":"a.g","repo":"EternalCoder454/..","channel":"releases"},
          {"repo":"EternalCoder454/y"}]}"#;
        let c = Catalog::parse(text.as_bytes()).unwrap();
        assert_eq!(c.apps.len(), 1, "{c:?}");
        assert_eq!(c.apps[0].id, "a.e");
        assert_eq!(c.skipped.len(), 8);
    }

    #[test]
    fn a_broken_or_future_list_is_an_error() {
        assert!(Catalog::parse(b"nope").is_err());
        assert!(Catalog::parse(br#"{"schema":2,"apps":[]}"#).is_err());
        assert!(Catalog::parse(&vec![b' '; 300 * 1024]).is_err());
    }

    #[test]
    fn at_most_so_many_entries() {
        let entries: Vec<String> = (0..300)
            .map(|i| {
                format!(r#"{{"id":"a.app{i}","repo":"EternalCoder454/r{i}","channel":"releases"}}"#)
            })
            .collect();
        let text = format!(r#"{{"schema":1,"apps":[{}]}}"#, entries.join(","));
        let c = Catalog::parse(text.as_bytes()).unwrap();
        assert_eq!(c.apps.len(), MAX_ENTRIES);
    }
}
