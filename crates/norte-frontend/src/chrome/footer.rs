//! A listing's footer (spec 2026-09-10): how many directories and files
//! there are and how much they weigh, what is marked, and the volume's
//! free space.
//!
//! One single wording for both frontends, like the header's notes
//! (`notes`): the TUI puts it on the pane's bottom border and the window
//! in a row below the listing, and two wordings of the same fact is where
//! half a parity audit came from (ADR 0077).

use norte_i18n::{Lang, ta_in};
use norte_proto::{Entry, EntryKind};

/// What is in a listing, counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Directories (symlinks to a directory count as a directory).
    pub dirs: usize,
    /// Everything else.
    pub files: usize,
    /// Bytes of what declares a size. A provider that does not bring it (the
    /// local one, for directories) does not add: the count is of what is
    /// known.
    pub bytes: u64,
}

/// Counts `entries`, skipping the `..` row if `parent_row` puts it at the
/// head: it is synthetic and is not in the directory.
#[must_use]
pub fn counts(entries: &[Entry], parent_row: bool) -> Counts {
    let mut c = Counts::default();
    for e in entries.iter().skip(usize::from(parent_row)) {
        match e.kind {
            EntryKind::Dir => c.dirs += 1,
            _ => c.files += 1,
        }
        c.bytes = c.bytes.saturating_add(e.size.unwrap_or(0));
    }
    c
}

/// What is marked, raw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Marked {
    /// How many entries.
    pub n: usize,
    /// How much the ones that declare a size weigh.
    pub bytes: u64,
    /// How many of them are directories.
    pub dirs: usize,
}

/// The footer, worded: `12 dirs · 84 files · 1.3 GiB`, plus `2 marked, 4.0
/// MiB` if there are marks and `120 GiB free` if it is known.
///
/// ```
/// use norte_frontend::footer::{Counts, Marked, pane_footer};
/// use norte_i18n::Lang;
///
/// let c = Counts { dirs: 2, files: 3, bytes: 2048 };
/// let s = pane_footer(c, Marked::default(), None, Lang::En);
/// assert!(s.contains('2') && s.contains('3') && s.contains("KiB"), "{s}");
/// assert!(!s.contains("marked") && !s.contains("free"), "{s}");
/// let s = pane_footer(c, Marked { n: 1, bytes: 10, dirs: 0 }, Some(1 << 30), Lang::En);
/// assert!(s.contains("marked") && s.contains("free"), "{s}");
/// ```
#[must_use]
pub fn pane_footer(counts: Counts, marked: Marked, free: Option<u64>, lang: Lang) -> String {
    join(&segments(counts, marked, free, lang))
}

/// The footer's segments with their PRIORITY (higher = more important): what
/// is marked is what the reader just did, the counts say what is there, and
/// the free space is the first thing to give way when it does not fit. They
/// go in screen order; the priority only decides what falls off.
#[must_use]
pub fn segments(
    counts: Counts,
    marked: Marked,
    free: Option<u64>,
    lang: Lang,
) -> Vec<(u8, String)> {
    let mut out = vec![(
        1,
        ta_in(
            lang,
            "pane-footer-counts",
            &[
                ("dirs", &counts.dirs.to_string()),
                ("files", &counts.files.to_string()),
                ("size", &crate::human_bytes(counts.bytes)),
            ],
        ),
    )];
    let marked_text = crate::notes::marked(marked.n, marked.bytes, marked.dirs, lang);
    if !marked_text.is_empty() {
        out.push((2, marked_text));
    }
    if let Some(free) = free {
        out.push((
            0,
            ta_in(
                lang,
                "pane-footer-free",
                &[("free", &crate::human_bytes(free))],
            ),
        ));
    }
    out
}

fn join(segments: &[(u8, String)]) -> String {
    segments
        .iter()
        .map(|(_, s)| s.as_str())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The footer that FITS in `width` cells: the lowest-priority segments get
/// dropped until it fits, and if not even the last one fits, it gets
/// truncated. A footer that says "2 marked" whole is worth more than one
/// that says "…ked" and the free space.
#[must_use]
pub fn fit(mut segments: Vec<(u8, String)>, width: usize) -> String {
    loop {
        let text = join(&segments);
        if crate::display::cells(&text) <= width || segments.len() <= 1 {
            return crate::middle_ellipsis(&text, width);
        }
        let Some((i, _)) = segments.iter().enumerate().min_by_key(|(_, (p, _))| *p) else {
            return String::new();
        };
        segments.remove(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{Segment, VPath};

    fn entry(name: &str, kind: EntryKind, size: Option<u64>) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse("file:///d")
                .unwrap()
                .join(Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind,
            size,
            mtime_ms: None,
        }
    }

    /// The `..` row does not count, a directory with no size does not add,
    /// and the symlink counts as a file.
    #[test]
    fn counts_without_the_parent_row_and_only_what_declares_size() {
        // The parent row is SYNTHETIC (a `..` is not a valid segment); here
        // any first entry stands in for it.
        let entries = [
            entry("parent", EntryKind::Dir, None),
            entry("a", EntryKind::Dir, None),
            entry("b", EntryKind::File, Some(100)),
            entry("c", EntryKind::Symlink, Some(5)),
        ];
        assert_eq!(
            counts(&entries, true),
            Counts {
                dirs: 1,
                files: 2,
                bytes: 105
            }
        );
        assert_eq!(
            counts(&entries, false).dirs,
            2,
            "without a parent row, `..` is one more dir"
        );
        assert_eq!(
            counts(&[], true),
            Counts::default(),
            "empty with a parent row: nothing"
        );
    }

    /// With no room, the free space falls first and then the counts; what
    /// is marked is the last thing lost, and whole.
    #[test]
    fn the_footer_gives_way_by_priority_and_not_in_the_middle() {
        let c = Counts {
            dirs: 1,
            files: 2,
            bytes: 0,
        };
        let m = Marked {
            n: 2,
            bytes: 4096,
            dirs: 0,
        };
        let s = segments(c, m, Some(120 << 30), Lang::En);
        let whole = fit(s.clone(), 200);
        assert!(
            whole.contains("free") && whole.contains("marked"),
            "{whole}"
        );
        let without_free = fit(s.clone(), crate::display::cells(&whole) - 1);
        assert!(
            !without_free.contains("free") && without_free.contains("marked"),
            "{without_free}"
        );
        let marked_only = fit(s, 20);
        assert!(marked_only.starts_with("2 marked"), "{marked_only}");
    }

    /// Both languages word it, and the footer is short: it fits on one edge.
    #[test]
    fn the_footer_words_in_both_languages() {
        let c = Counts {
            dirs: 12,
            files: 84,
            bytes: 1 << 30,
        };
        for lang in [Lang::En, Lang::Es] {
            let s = pane_footer(c, Marked::default(), Some(120 << 30), lang);
            assert!(
                s.contains("12") && s.contains("84") && s.contains("120"),
                "{s}"
            );
            assert!(!s.contains("pane-footer"), "untranslated key: {s}");
            assert!(crate::display::cells(&s) <= 56, "{s}");
        }
    }
}
