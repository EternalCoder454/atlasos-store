//! SPDX licence expressions: just enough of the grammar to tell free from not
//! free. The input is untrusted catalog text, so it is capped in length and
//! nesting, and nothing here panics.

/// Longest expression looked at, in bytes. Longer is not free.
const MAX_LEN: usize = 1024;
/// Deepest parenthesis nesting accepted.
const MAX_DEPTH: usize = 16;

/// Free licence IDs, lowercase, without `+`, `-only` or `-or-later`: the common
/// OSI- and FSF-approved licences, free documentation and font licences, and
/// public-domain dedications.
const FREE: &[&str] = &[
    "0bsd",
    "afl-3.0",
    "agpl-1.0",
    "agpl-3.0",
    "apache-1.0",
    "apache-1.1",
    "apache-2.0",
    "artistic-1.0-perl",
    "artistic-2.0",
    "blueoak-1.0.0",
    "bsd-1-clause",
    "bsd-2-clause",
    "bsd-2-clause-patent",
    "bsd-3-clause",
    "bsd-3-clause-clear",
    "bsd-4-clause",
    "bsl-1.0",
    "cc-by-2.0",
    "cc-by-2.5",
    "cc-by-3.0",
    "cc-by-4.0",
    "cc-by-sa-2.0",
    "cc-by-sa-2.5",
    "cc-by-sa-3.0",
    "cc-by-sa-4.0",
    "cc0-1.0",
    "cddl-1.0",
    "cddl-1.1",
    "cecill-2.1",
    "ecl-2.0",
    "epl-1.0",
    "epl-2.0",
    "eupl-1.1",
    "eupl-1.2",
    "fsfap",
    "ftl",
    "gfdl-1.1",
    "gfdl-1.2",
    "gfdl-1.3",
    "gpl-1.0",
    "gpl-2.0",
    "gpl-3.0",
    "hpnd",
    "ijg",
    "imlib2",
    "isc",
    "lgpl-2.0",
    "lgpl-2.1",
    "lgpl-3.0",
    "libpng",
    "libpng-2.0",
    "lppl-1.3c",
    "mit",
    "mit-0",
    "mit-cmu",
    "mpl-1.0",
    "mpl-1.1",
    "mpl-2.0",
    "ms-pl",
    "ms-rl",
    "mulanpsl-2.0",
    "ncsa",
    "ofl-1.0",
    "ofl-1.1",
    "openssl",
    "osl-3.0",
    "postgresql",
    "psf-2.0",
    "python-2.0",
    "qhull",
    "ruby",
    "sissl",
    "sleepycat",
    "tcl",
    "unicode-3.0",
    "unicode-dfs-2016",
    "unlicense",
    "upl-1.0",
    "vim",
    "w3c",
    "wtfpl",
    "x11",
    "zlib",
    "zpl-2.1",
];

/// Licence families whose IDs carry `-only` and `-or-later`.
const SUFFIXED: &[&str] = &["gpl-", "lgpl-", "agpl-", "gfdl-"];

/// See [`super::is_free_license`].
pub(super) fn is_free(expr: &str) -> bool {
    if expr.len() > MAX_LEN {
        return false;
    }
    let mut p = Parser {
        toks: tokens(expr),
        pos: 0,
    };
    matches!(p.expr(0), Some(free) if p.pos == p.toks.len() && free)
}

#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Open,
    Close,
    Word(&'a str),
}

fn tokens(s: &str) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() || c == '(' || c == ')' {
            if let Some(st) = start.take() {
                out.push(Tok::Word(&s[st..i]));
            }
            match c {
                '(' => out.push(Tok::Open),
                ')' => out.push(Tok::Close),
                _ => {}
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        out.push(Tok::Word(&s[st..]));
    }
    out
}

struct Parser<'a> {
    toks: Vec<Tok<'a>>,
    pos: usize,
}

impl Parser<'_> {
    fn peek_keyword(&self, kw: &str) -> bool {
        matches!(self.toks.get(self.pos), Some(Tok::Word(w)) if w.eq_ignore_ascii_case(kw))
    }

    /// `and-expr (OR and-expr)*`. `None` is a syntax error; `Some` says whether
    /// the expression is free.
    fn expr(&mut self, depth: usize) -> Option<bool> {
        let mut free = self.and(depth)?;
        while self.peek_keyword("or") {
            self.pos += 1;
            let rhs = self.and(depth)?;
            free |= rhs;
        }
        Some(free)
    }

    fn and(&mut self, depth: usize) -> Option<bool> {
        let mut free = self.term(depth)?;
        while self.peek_keyword("and") {
            self.pos += 1;
            let rhs = self.term(depth)?;
            free &= rhs;
        }
        Some(free)
    }

    fn term(&mut self, depth: usize) -> Option<bool> {
        match self.toks.get(self.pos)? {
            Tok::Open => {
                if depth >= MAX_DEPTH {
                    return None;
                }
                self.pos += 1;
                let free = self.expr(depth + 1)?;
                if self.toks.get(self.pos) != Some(&Tok::Close) {
                    return None;
                }
                self.pos += 1;
                Some(free)
            }
            Tok::Close => None,
            Tok::Word(w) => {
                if is_operator(w) {
                    return None;
                }
                self.pos += 1;
                let free = id_is_free(w);
                if self.peek_keyword("with") {
                    self.pos += 1;
                    // The exception must be one plain ID; it does not change
                    // whether the licence is free.
                    match self.toks.get(self.pos)? {
                        Tok::Word(e) if !is_operator(e) => self.pos += 1,
                        _ => return None,
                    }
                }
                Some(free)
            }
        }
    }
}

fn is_operator(w: &str) -> bool {
    ["and", "or", "with"]
        .iter()
        .any(|k| w.eq_ignore_ascii_case(k))
}

/// Whether one licence ID (with an optional `+`) is on the list.
fn id_is_free(id: &str) -> bool {
    if id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
    {
        return false;
    }
    let lower = id.to_ascii_lowercase();
    let mut base = lower.strip_suffix('+').unwrap_or(&lower);
    if SUFFIXED.iter().any(|f| base.starts_with(f)) {
        base = base
            .strip_suffix("-only")
            .or_else(|| base.strip_suffix("-or-later"))
            .unwrap_or(base);
    }
    FREE.binary_search(&base).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_sorted_for_binary_search() {
        assert!(
            FREE.windows(2).all(|w| w[0] < w[1]),
            "FREE must be sorted and unique"
        );
        assert!(
            FREE.iter()
                .all(|s| s.bytes().all(|b| !b.is_ascii_uppercase()))
        );
    }
}
