//! The `.gitignore` matcher, just enough for the column to not be noise.
//!
//! Without it, a Rust tree shows `target/` and its ten thousand files as
//! "untracked" and the column stops being useful for anything. It is
//! parsing text and matching globs — it does not touch git objects nor
//! pretend to be `git check-ignore`.
//!
//! What it covers: comments and empty lines, negation (`!`), anchoring to
//! the file's root (a leading `/` or a slash in the middle),
//! directories-only (trailing `/`), `*`, `?`, `[...]` classes and `**`. The
//! last matching rule wins, which is git's rule.

extern crate alloc;

use alloc::vec::Vec;

/// A rule from an ignore file.
#[derive(Debug, Clone)]
struct Rule {
    /// The pattern without `!` or a trailing slash.
    pattern: Vec<u8>,
    /// `true` if it starts with `!`: whatever matches STOPS being ignored.
    negated: bool,
    /// `true` if it ends in `/`: only matches directories.
    dir_only: bool,
    /// `true` if the pattern carries a slash (or starts with one): matches
    /// against the WHOLE path relative to the file, not against the bare
    /// name.
    anchored: bool,
    /// Where the ignore file lived, relative to the repository's root and
    /// without a trailing slash. Empty = the root.
    base: Vec<u8>,
}

/// The rules from one or more ignore files, in increasing precedence order
/// (a deeper `.gitignore`'s rules beat the ones above it).
#[derive(Debug, Default)]
pub struct Ignores {
    rules: Vec<Rule>,
}

impl Ignores {
    /// Adds the rules from an ignore file that lived at `base` (relative to
    /// the repository's root; empty = the root).
    pub fn add_file(&mut self, base: &[u8], content: &[u8]) {
        for line in content.split(|b| *b == b'\n') {
            let line = trim(line);
            if line.is_empty() || line[0] == b'#' {
                continue;
            }
            let (negated, rest) = match line.first() {
                Some(b'!') => (true, &line[1..]),
                _ => (false, line),
            };
            let dir_only = rest.last() == Some(&b'/');
            let rest = if dir_only {
                &rest[..rest.len() - 1]
            } else {
                rest
            };
            let anchored =
                rest.first() == Some(&b'/') || rest[..rest.len().saturating_sub(1)].contains(&b'/');
            let pattern = rest.strip_prefix(b"/").unwrap_or(rest).to_vec();
            if pattern.is_empty() {
                continue;
            }
            self.rules.push(Rule {
                pattern,
                negated,
                dir_only,
                anchored,
                base: base.to_vec(),
            });
        }
    }

    /// Is `path` (relative to the repository's root) ignored?
    ///
    /// `is_dir` matters: a `target/` rule ignores the directory and not a
    /// file with the same name.
    #[must_use]
    pub fn is_ignored(&self, path: &[u8], is_dir: bool) -> bool {
        let mut verdict = false;
        for rule in &self.rules {
            if rule.dir_only && !is_dir {
                continue;
            }
            let Some(rel) = strip_base(&rule.base, path) else {
                continue;
            };
            let matches = if rule.anchored {
                glob(&rule.pattern, rel)
            } else {
                // Unanchored, the rule matches against the name of ANY
                // component: `*.tmp` covers `a/b/c.tmp`, as in git.
                rel.split(|b| *b == b'/')
                    .any(|comp| glob(&rule.pattern, comp))
                    || glob(&rule.pattern, rel)
            };
            if matches {
                verdict = !rule.negated;
            }
        }
        verdict
    }

    /// `true` if no ignore file contributed any rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// `path` as seen from `base`, or `None` if it does not hang off it.
fn strip_base<'a>(base: &[u8], path: &'a [u8]) -> Option<&'a [u8]> {
    if base.is_empty() {
        return Some(path);
    }
    let rest = path.strip_prefix(base)?;
    rest.strip_prefix(b"/")
}

fn trim(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    // Trailing whitespace IS trimmed (git does it unless escaped); so is a
    // `\r` from a file with Windows line endings.
    while let [rest @ .., last] = s {
        if last.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// gitignore glob: `*` does not cross `/`, `**` does, `?` is one byte,
/// `[...]` is a class.
fn glob(pattern: &[u8], candidate: &[u8]) -> bool {
    match pattern.first() {
        None => candidate.is_empty(),
        Some(b'*') if pattern.get(1) == Some(&b'*') => {
            let rest = pattern[2..].strip_prefix(b"/").unwrap_or(&pattern[2..]);
            (0..=candidate.len()).any(|skip| glob(rest, &candidate[skip..]))
        }
        Some(b'*') => (0..=candidate.len())
            .take_while(|skip| !candidate[..*skip].contains(&b'/'))
            .any(|skip| glob(&pattern[1..], &candidate[skip..])),
        Some(b'?') => {
            !candidate.is_empty() && candidate[0] != b'/' && glob(&pattern[1..], &candidate[1..])
        }
        Some(b'[') => match class_end(pattern) {
            Some(end) => {
                !candidate.is_empty()
                    && class_matches(&pattern[1..end], candidate[0])
                    && glob(&pattern[end + 1..], &candidate[1..])
            }
            None => literal(pattern, candidate),
        },
        Some(_) => literal(pattern, candidate),
    }
}

fn literal(pattern: &[u8], candidate: &[u8]) -> bool {
    match (pattern.first(), candidate.first()) {
        (Some(p), Some(c)) if p == c => glob(&pattern[1..], &candidate[1..]),
        _ => false,
    }
}

fn class_end(pattern: &[u8]) -> Option<usize> {
    let start = if pattern.get(1) == Some(&b'!') { 2 } else { 1 };
    let start = if pattern.get(start) == Some(&b']') {
        start + 1
    } else {
        start
    };
    pattern[start..]
        .iter()
        .position(|b| *b == b']')
        .map(|at| at + start)
}

fn class_matches(class: &[u8], byte: u8) -> bool {
    let (negate, body) = match class.first() {
        Some(b'!') => (true, &class[1..]),
        _ => (false, class),
    };
    let mut hit = false;
    let mut i = 0;
    while i < body.len() {
        if i + 2 < body.len() && body[i + 1] == b'-' {
            if (body[i]..=body[i + 2]).contains(&byte) {
                hit = true;
            }
            i += 3;
        } else {
            if body[i] == byte {
                hit = true;
            }
            i += 1;
        }
    }
    hit != negate
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignores(content: &[u8]) -> Ignores {
        let mut i = Ignores::default();
        i.add_file(b"", content);
        i
    }

    #[test]
    fn a_directory_with_a_trailing_slash_only_covers_directories() {
        let i = ignores(b"target/\n");
        assert!(i.is_ignored(b"target", true));
        assert!(
            !i.is_ignored(b"target", false),
            "a FILE with the same name, no"
        );
        assert!(i.is_ignored(b"a/target", true), "unanchored, at any level");
    }

    #[test]
    fn an_extension_covers_at_any_depth() {
        let i = ignores(b"*.tmp\n");
        assert!(i.is_ignored(b"junk.tmp", false));
        assert!(i.is_ignored(b"a/b/junk.tmp", false));
        assert!(!i.is_ignored(b"junk.txt", false));
    }

    #[test]
    fn a_leading_slash_anchors_to_the_root() {
        let i = ignores(b"/build\n");
        assert!(i.is_ignored(b"build", true));
        assert!(
            !i.is_ignored(b"sub/build", true),
            "anchored: only at the root"
        );
    }

    #[test]
    fn negation_wins_if_it_comes_after() {
        let i = ignores(b"*.log\n!important.log\n");
        assert!(i.is_ignored(b"noise.log", false));
        assert!(
            !i.is_ignored(b"important.log", false),
            "the last one that matches rules"
        );
    }

    #[test]
    fn comments_and_empty_lines_are_not_rules() {
        let i = ignores(b"# this is a comment\n\n   \n*.o\n");
        assert!(i.is_ignored(b"a.o", false));
        assert!(!i.is_ignored(b"# this is a comment", false));
    }

    #[test]
    fn a_deeper_gitignore_beats_the_one_above() {
        let mut i = Ignores::default();
        i.add_file(b"", b"*.log\n");
        i.add_file(b"sub", b"!kept.log\n");
        assert!(i.is_ignored(b"root.log", false));
        assert!(!i.is_ignored(b"sub/kept.log", false));
        assert!(i.is_ignored(b"other/kept.log", false), "only under `sub`");
    }

    #[test]
    fn a_double_asterisk_crosses_directories_and_a_single_one_does_not() {
        let i = ignores(b"docs/**/draft.md\n");
        assert!(i.is_ignored(b"docs/a/b/draft.md", false));
        assert!(i.is_ignored(b"docs/draft.md", false));
        let j = ignores(b"docs/*/draft.md\n");
        assert!(j.is_ignored(b"docs/a/draft.md", false));
        assert!(!j.is_ignored(b"docs/a/b/draft.md", false));
    }

    #[test]
    fn a_non_utf8_name_matches_by_bytes() {
        let i = ignores(b"cp437-\xa4\xa5.txt\n");
        assert!(i.is_ignored(b"cp437-\xa4\xa5.txt", false));
        assert!(!i.is_ignored(b"cp437-.txt", false));
    }
}
