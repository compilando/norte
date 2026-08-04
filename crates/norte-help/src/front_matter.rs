//! A topic header: TOML between `+++` fences (ADR 0040, decision 2). TOML
//! and not YAML because the workspace already parses TOML everywhere, and
//! pulling in a YAML crate for six fields would not survive rule 8.

use serde::Deserialize;

/// The fence as a literal, so that both [`FENCE`] and [`CLOSE`] come from a
/// SINGLE definition: widening the fence cannot desynchronise the needle we
/// search for from the number of bytes we skip afterwards.
macro_rules! fence_lit {
    () => {
        "+++"
    };
}

/// Fence that opens and closes the header.
const FENCE: &str = fence_lit!();

/// What terminates the header: a line break immediately followed by the
/// fence. Searching for the line break together with the fence is also what
/// keeps every slice index below on a `char` boundary, since the needle is
/// pure ASCII.
const CLOSE: &str = concat!("\n", fence_lit!());

/// Header declared by a topic.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrontMatter {
    /// Unique topic id.
    pub id: String,
    /// Displayed title.
    pub title: String,
    /// Grouping tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Related topics.
    #[serde(default)]
    pub see_also: Vec<String>,
    /// Commands documented by the topic.
    #[serde(default)]
    pub commands: Vec<String>,
    /// UI contexts that open this topic with F1.
    #[serde(default)]
    pub context: Vec<String>,
}

/// Failures while reading the header.
///
/// `#[non_exhaustive]` on purpose: the hostile path (plugin `help.md`, task
/// 6) will keep growing reasons to reject a header, and adding one must not
/// break the `match`es of whoever reports them.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrontMatterError {
    /// The file does not start with `+++`.
    #[error("the topic does not start with the `+++` fence")]
    Missing,
    /// The closing fence is missing.
    #[error("the `+++` header is never closed")]
    Unterminated,
    /// The closing fence line carries something besides the fence itself.
    #[error("the closing `+++` fence line has trailing content")]
    TrailingContent,
    /// Invalid TOML or an unknown field.
    #[error("invalid TOML header: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Strips ONE line break, LF or CRLF, or reports that there was none.
///
/// CRLF matters twice over: a Windows checkout must not turn every topic
/// into a hard [`FrontMatterError::Missing`], and a `\r` left dangling at the
/// end of the header is not valid TOML.
fn strip_line_break(s: &str) -> Option<&str> {
    s.strip_prefix("\r\n").or_else(|| s.strip_prefix('\n'))
}

/// Splits `(header, body)`. The body is returned verbatim, uninterpreted:
/// the module that parses it is `parse` (task 5).
///
/// The closing fence must OWN its line. A line such as `+++ ` or `++++` is
/// rejected instead of being accepted with an empty body, because an empty
/// body is invisible: the reader would just see a blank topic, and the
/// corpus checks of task 8 cannot flag what `split` reported as a success.
///
/// # Errors
/// - [`FrontMatterError::Missing`] if the file does not open with the fence.
/// - [`FrontMatterError::Unterminated`] if the fence is never closed.
/// - [`FrontMatterError::TrailingContent`] if the closing fence line carries
///   anything besides the fence.
/// - [`FrontMatterError::Toml`] if the header is not valid TOML or declares
///   an unknown field.
// Narrowest possible suppression: the module is private and nothing outside
// its own tests calls `split` yet — `parse` will, in task 5 of this phase.
// It sits on `split` ALONE because rustc treats a lint-allowed item as a live
// root, so everything `split` reaches (the fences, `FrontMatter`,
// `FrontMatterError`, `strip_line_break`) is analysed honestly and would be
// reported if it really went unused. `expect` and not `allow` so that the day
// task 5 calls `split` the expectation goes unfulfilled and the compiler
// forces this line out; task 5 deletes it.
#[cfg_attr(not(test), expect(dead_code))]
pub fn split(source: &str) -> Result<(FrontMatter, &str), FrontMatterError> {
    let rest = source
        .strip_prefix(FENCE)
        .and_then(strip_line_break)
        .ok_or(FrontMatterError::Missing)?;
    let end = rest.find(CLOSE).ok_or(FrontMatterError::Unterminated)?;
    // `CLOSE` starts at the `\n`, so under CRLF the header keeps a dangling
    // `\r`, which TOML rejects as a stray control character. That `\r`
    // belongs to the line break, not to the header.
    let header = rest[..end].strip_suffix('\r').unwrap_or(&rest[..end]);
    // The same constant is both the needle and the skip: they cannot drift.
    let after = &rest[end + CLOSE.len()..];
    let body = if after.is_empty() {
        // The fence is the last thing in the file, with no trailing newline.
        ""
    } else {
        strip_line_break(after).ok_or(FrontMatterError::TrailingContent)?
    };
    let fm: FrontMatter = toml::from_str(header)?;
    Ok((fm, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "+++\n\
id = \"copying\"\n\
title = \"Copying across backends\"\n\
tags = [\"doing\"]\n\
see_also = [\"selection\"]\n\
commands = [\"fs.copy\"]\n\
+++\n\
Body starts here.\n";

    #[test]
    fn splits_header_from_body() {
        let (fm, body) = split(SRC).expect("valid front matter");
        assert_eq!(fm.id, "copying");
        assert_eq!(fm.title, "Copying across backends");
        assert_eq!(fm.tags, vec!["doing".to_owned()]);
        assert_eq!(fm.see_also, vec!["selection".to_owned()]);
        assert_eq!(fm.commands, vec!["fs.copy".to_owned()]);
        assert!(fm.context.is_empty(), "optional field, empty by default");
        assert_eq!(body, "Body starts here.\n");
    }

    #[test]
    fn without_an_opening_fence_it_is_an_error() {
        let err = split("id = \"x\"\nbody").unwrap_err();
        assert!(matches!(err, FrontMatterError::Missing));
    }

    #[test]
    fn an_unclosed_fence_is_an_error() {
        let err = split("+++\nid = \"x\"\nbody\n").unwrap_err();
        assert!(matches!(err, FrontMatterError::Unterminated));
    }

    #[test]
    fn an_unknown_field_is_an_error() {
        let src = "+++\nid = \"x\"\ntitle = \"X\"\nbogus = 1\n+++\nbody\n";
        assert!(matches!(split(src).unwrap_err(), FrontMatterError::Toml(_)));
    }

    // --- The closing fence owns its line. Every case below used to parse as
    // a success with an EMPTY body, which is the worst possible outcome: the
    // topic renders blank and nothing downstream can see the problem.

    #[test]
    fn a_trailing_space_after_the_closing_fence_is_an_error() {
        let src = "+++\nid = \"x\"\ntitle = \"X\"\n+++ \nbody\n";
        assert!(matches!(
            split(src).unwrap_err(),
            FrontMatterError::TrailingContent
        ));
    }

    #[test]
    fn a_longer_fence_line_is_an_error() {
        let src = "+++\nid = \"x\"\ntitle = \"X\"\n++++\nbody\n";
        assert!(matches!(
            split(src).unwrap_err(),
            FrontMatterError::TrailingContent
        ));
    }

    #[test]
    fn a_crlf_closing_fence_is_accepted() {
        let src = "+++\nid = \"x\"\ntitle = \"X\"\n+++\r\nbody\n";
        let (fm, body) = split(src).expect("CRLF is a line break, not content");
        assert_eq!(fm.id, "x");
        assert_eq!(body, "body\n");
    }

    #[test]
    fn a_whole_crlf_file_parses() {
        // A Windows checkout: every line break is CRLF, the one that closes
        // the header included.
        let src = "+++\r\nid = \"x\"\r\ntitle = \"X\"\r\n+++\r\nbody\r\n";
        let (fm, body) = split(src).expect("a Windows checkout is not a broken topic");
        assert_eq!(fm.id, "x");
        assert_eq!(fm.title, "X");
        assert_eq!(
            body, "body\r\n",
            "the body is returned verbatim, CRLF included"
        );
    }

    // --- Hardening: this module sits on the path of third-party plugin help
    // (task 6), so hostile input must DEGRADE into a typed error, never panic
    // and never cut a multi-byte character in half.

    #[test]
    fn a_truncated_fence_without_a_newline_is_an_error() {
        assert!(matches!(
            split(FENCE).unwrap_err(),
            FrontMatterError::Missing
        ));
    }

    #[test]
    fn an_opening_fence_alone_is_an_error() {
        assert!(matches!(
            split("+++\n").unwrap_err(),
            FrontMatterError::Unterminated
        ));
    }

    #[test]
    fn a_closing_fence_at_end_of_file_leaves_an_empty_body() {
        let src = "+++\nid = \"x\"\ntitle = \"X\"\n+++";
        let (fm, body) = split(src).expect("closed header, even without a final newline");
        assert_eq!(fm.id, "x");
        assert_eq!(body, "", "nothing after the closing fence");
    }

    #[test]
    fn the_first_closing_fence_wins() {
        let src = "+++\nid = \"x\"\ntitle = \"X\"\n+++\nbefore\n+++\nafter\n";
        let (fm, body) = split(src).expect("valid front matter");
        assert_eq!(fm.title, "X");
        assert_eq!(
            body, "before\n+++\nafter\n",
            "the second fence is body, not header"
        );
    }

    #[test]
    fn a_multibyte_title_survives_byte_for_byte() {
        let src = "+++\nid = \"copying\"\ntitle = \"Copiar entre backends — ñ\"\n\
tags = [\"日本語\"]\n+++\nbody — ñ\n";
        let (fm, body) = split(src).expect("valid front matter");
        assert_eq!(fm.title.as_bytes(), "Copiar entre backends — ñ".as_bytes());
        assert_eq!(fm.tags, vec!["日本語".to_owned()]);
        assert_eq!(body.as_bytes(), "body — ñ\n".as_bytes());
    }

    #[test]
    fn a_multibyte_char_right_before_the_closing_fence_is_not_cut() {
        // The header's last line is a TOML comment ending in a 3-byte `—`,
        // so the multi-byte character ABUTS the `\n` that opens the closing
        // fence, and the body starts with another one. An off-by-one in the
        // header slice or in the skip would panic here, or show up in the
        // asserts below.
        let src = "+++\nid = \"x\"\ntitle = \"X\"\n# note —\n+++\n— body\n";
        let (fm, body) = split(src).expect("valid front matter");
        assert_eq!(fm.id, "x");
        assert_eq!(
            body, "— body\n",
            "the body starts exactly after the fence line"
        );
    }

    #[test]
    fn no_truncation_of_the_source_panics() {
        let src = "+++\nid = \"x\"\ntitle = \"ñ — 日本語\"\n+++\nbody — ñ\n";
        for (i, _) in src.char_indices().chain(std::iter::once((src.len(), ' '))) {
            // Each prefix is a plausible truncated file: it either parses or
            // returns a typed error, but it must never slice mid-character.
            let _ = split(&src[..i]);
        }
    }
}
