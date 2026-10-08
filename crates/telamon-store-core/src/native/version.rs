//! Versions of native apps: dotted numbers with an optional `-prerelease`
//! (`0.2.0`, `1.0.0-beta.1`), ordered the way semver orders them: numbers
//! compare as numbers, a missing number is zero, a prerelease sorts before
//! its release.

use std::cmp::Ordering;

/// A parsed version. Equality and order ignore trailing zero numbers
/// (`1.0` == `1.0.0`).
#[derive(Debug, Clone)]
pub struct Version {
    text: String,
    nums: Vec<u64>,
    pre: Vec<Ident>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ident {
    Num(u64),
    Text(String),
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            (Ident::Num(_), Ident::Text(_)) => Ordering::Less,
            (Ident::Text(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Text(a), Ident::Text(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Version {
    /// `1`, `1.2`, `1.2.3.4` and any of them with `-pre.release`: up to 6
    /// numbers of at most 9 digits, a prerelease of dot-separated ASCII
    /// letters, digits and `-`, 64 characters in all. No `v` prefix, no build
    /// metadata.
    pub fn parse(s: &str) -> Option<Version> {
        if s.is_empty() || s.len() > 64 {
            return None;
        }
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut nums = Vec::new();
        for part in core.split('.') {
            nums.push(number(part)?);
        }
        if nums.len() > 6 {
            return None;
        }
        let mut idents = Vec::new();
        if let Some(pre) = pre {
            for part in pre.split('.') {
                if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    return None;
                }
                idents.push(match number(part) {
                    Some(n) => Ident::Num(n),
                    None => Ident::Text(part.to_string()),
                });
            }
        }
        while nums.len() > 1 && nums.last() == Some(&0) {
            nums.pop();
        }
        Some(Version {
            text: s.to_string(),
            nums,
            pre: idents,
        })
    }

    /// The version as it was written.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

/// A run of digits, no leading zero (except `0`), at most 9 digits.
fn number(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None;
    }
    s.parse().ok()
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Version {}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.nums
            .cmp(&other.nums)
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn numbers_compare_as_numbers() {
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.1") > v("1.0"));
        assert_eq!(v("1.0"), v("1.0.0"));
        assert!(v("2") > v("1.99.99"));
    }

    #[test]
    fn prereleases_sort_before_their_release() {
        assert!(v("1.0.0-beta.1") < v("1.0.0"));
        assert!(v("1.0.0-beta.2") > v("1.0.0-beta.1"));
        assert!(v("1.0.0-beta.10") > v("1.0.0-beta.2"));
        assert!(v("1.0.0-rc.1") > v("1.0.0-beta.9"));
        assert!(v("1.0.0-1") < v("1.0.0-alpha"));
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        assert!(v("1.0.1-beta") > v("1.0.0"));
    }

    #[test]
    fn only_plain_versions_parse() {
        for bad in [
            "",
            "v1.0",
            "1..0",
            "1.0.",
            ".1",
            "1.a",
            "01.0",
            "1.0+build",
            "1.0-",
            "1.0-a..b",
            "1.0.0.0.0.0.1",
            "1234567890.0",
            "1 .0",
            "1.0\n",
            "-1",
            "1.0-é",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
        assert_eq!(v("1.2.3-rc.1").as_str(), "1.2.3-rc.1");
        assert!(v("1.2.3-rc.1").is_prerelease());
    }
}
