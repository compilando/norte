//! Which TWO files `pane.compare-files` compares (#312).
//!
//! Comparing two trees already exists (`pane.compare-dirs`, with its
//! differences panel). What was missing is the pair, which is what Total
//! Commander and Krusader have, and which here is delegated to an external
//! program — issue's step 1.
//!
//! The rule for WHICH two lives here, in the shared crate, because it is
//! the same decision in the terminal and in the window, and a decision
//! duplicated between frontends drifts apart silently (ADR 0077).
//!
//! **Two files or nothing.** It is not guessed: with three marked, with
//! only one, or with a folder in the mix, the command SAYS SO. Comparing
//! "whatever there happens to be" is the kind of convenience that ends up
//! showing the diff between two files the reader never chose.

use norte_proto::{Entry, EntryKind, VPath};

/// Why there is no pair to compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    /// Not exactly two: neither two marked, nor one in each panel.
    NotTwo,
    /// There are two, and one of them is not a file (a folder is
    /// `compare-dirs`).
    NotFiles,
}

impl PairError {
    /// The Fluent key it is said with.
    #[must_use]
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::NotTwo => "msg-compare-files-need-two",
            Self::NotFiles => "msg-compare-files-not-files",
        }
    }
}

/// The pair to compare, in the order it is shown: first the focused
/// panel's (or the first marked), then the other.
///
/// Two sources, in this order:
///
/// 1. **What is marked in the focused panel**, if there are marks — the
///    usual operand. There have to be exactly two.
/// 2. **One file in each panel**, which is how the two reference managers
///    compare: this one against the one across from it.
///
/// ```
/// use norte_frontend::diffpair::{pair, PairError};
/// # use norte_proto::{Entry, EntryKind, VPath, Segment};
/// # fn f(n: &str) -> Entry {
/// #     Entry {
/// #         attrs: std::collections::BTreeMap::new(),
/// #         path: VPath::parse("file:///d").unwrap()
/// #             .join(Segment::new(n.as_bytes().to_vec()).unwrap()),
/// #         kind: EntryKind::File, size: Some(1), mtime_ms: None,
/// #     }
/// # }
/// let (a, b) = (f("a"), f("b"));
/// assert!(pair(&[&a, &b], None, None).is_ok());
/// assert_eq!(pair(&[&a], Some(&a), None), Err(PairError::NotTwo));
/// ```
///
/// The comparator when `[ui] diff` says nothing: `diff -u` with the TWO
/// paths. POSIX guarantees it exists and its output is text that stays on
/// screen — the honest equivalent of what `xdg-open` does for
/// `pane.open`: something that works without having written any config.
/// Whoever wants `meld`, `delta` or `vimdiff` says so in `[ui] diff`.
///
/// Here and not in each frontend (ADR 0077): the terminal runs it waiting
/// for a keypress and the window runs it capturing the output, but WHAT is
/// run is the same decision.
pub const DEFAULT_ARGV: [&str; 3] = ["diff", "-u", "%F"];

/// # Errors
///
/// [`PairError`] when there are not exactly two, or when one of them is
/// not a file.
pub fn pair(
    marked: &[&Entry],
    here: Option<&Entry>,
    there: Option<&Entry>,
) -> Result<(VPath, VPath), PairError> {
    let (a, b) = if marked.is_empty() {
        // With no marks, this one against the one across from it. If there
        // are not two panels — a single-listing layout — there is no pair
        // either, and that is an honest `NotTwo`: there is nothing to
        // compare against.
        match (here, there) {
            (Some(a), Some(b)) => (a, b),
            _ => return Err(PairError::NotTwo),
        }
    } else if let [a, b] = marked {
        (*a, *b)
    } else {
        return Err(PairError::NotTwo);
    };
    // A link is fine: what is on the other side is a file, and whoever
    // marked it knows what they marked. A directory is not, and there
    // `pane.compare-dirs` is the answer — saying so is more useful than
    // comparing two listings by hand.
    if a.kind == EntryKind::Dir || b.kind == EntryKind::Dir {
        return Err(PairError::NotFiles);
    }
    // The same path twice is not a comparison: it is an empty diff that
    // reads as "they're the same" when what actually happened is that only
    // one thing got marked.
    if a.path == b.path {
        return Err(PairError::NotTwo);
    }
    Ok((a.path.clone(), b.path.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse("file:///d")
                .expect("wire")
                .join(norte_proto::Segment::new(name.as_bytes().to_vec()).expect("segment")),
            kind,
            size: Some(1),
            mtime_ms: None,
        }
    }

    #[test]
    fn two_marked_ones_are_the_pair() {
        let (a, b) = (entry("a", EntryKind::File), entry("b", EntryKind::File));
        let (x, y) = pair(&[&a, &b], None, None).expect("two marked files");
        assert_eq!((x, y), (a.path, b.path));
    }

    #[test]
    fn with_no_marks_it_is_one_from_each_pane() {
        let (a, b) = (entry("a", EntryKind::File), entry("b", EntryKind::File));
        assert!(pair(&[], Some(&a), Some(&b)).is_ok());
    }

    /// Three marked are not a pair, and guessing which two would be
    /// showing the diff between two files nobody chose.
    #[test]
    fn neither_one_nor_three() {
        let (a, b, c) = (
            entry("a", EntryKind::File),
            entry("b", EntryKind::File),
            entry("c", EntryKind::File),
        );
        assert_eq!(pair(&[&a, &b, &c], None, None), Err(PairError::NotTwo));
        assert_eq!(pair(&[&a], Some(&a), Some(&b)), Err(PairError::NotTwo));
        assert_eq!(pair(&[], Some(&a), None), Err(PairError::NotTwo));
    }

    #[test]
    fn a_folder_sends_to_compare_directories() {
        let (a, d) = (entry("a", EntryKind::File), entry("d", EntryKind::Dir));
        assert_eq!(pair(&[&a, &d], None, None), Err(PairError::NotFiles));
    }

    /// The same file in both panels: the diff would come out empty and
    /// read as "they're the same", which is a wrong answer to a question
    /// nobody asked.
    #[test]
    fn the_same_file_twice_is_not_a_comparison() {
        let a = entry("a", EntryKind::File);
        assert_eq!(pair(&[], Some(&a), Some(&a)), Err(PairError::NotTwo));
    }
}
