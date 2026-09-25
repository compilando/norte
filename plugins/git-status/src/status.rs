//! From an index and a `stat` to a column cell.
//!
//! The order matters and it is git's: compare `stat` first —that answers
//! the vast majority of cases without opening anything—, and only when the
//! comparison is ambiguous (the "racy" case: the file has the SAME mtime as
//! the index, so it could have changed within the same second) read the
//! content and compare the object id.
//!
//! Cell vocabulary: empty = clean, `M` modified, `D` deleted, `?`
//! untracked, `!` ignored.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::ignore::Ignores;
use crate::index::GitIndex;

/// An entry's metadata as the host gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    /// `true` if it is a directory.
    pub is_dir: bool,
    /// Size in bytes.
    pub size: u64,
    /// mtime in seconds.
    pub mtime_sec: i64,
    /// mtime in nanoseconds.
    pub mtime_nsec: u32,
}

/// What the guest knows how to ask the location. A trait so the logic is
/// tested on the host without a wasm runtime in between: what is tested is
/// the decision, not the ABI.
pub trait Location {
    /// Bytes of a file under the root.
    ///
    /// # Errors
    /// Any string the host returns.
    fn read(&self, rel: &[u8]) -> Result<Vec<u8>, String>;

    /// An entry's metadata under the root.
    ///
    /// # Errors
    /// Any string the host returns.
    fn stat(&self, rel: &[u8]) -> Result<Meta, String>;
}

/// The state of ONE visible entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Tracked and unchanged.
    Clean,
    /// Tracked and different from what the index says.
    Modified,
    /// Tracked and no longer on disk.
    Deleted,
    /// Untracked.
    Untracked,
    /// Untracked and covered by a `.gitignore`.
    Ignored,
}

/// What each state is painted with (`[config] glyphs`, spec 2026-09-11 V4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Glyphs {
    /// `git status --short`'s letters: `M`, `D`, `?`, `!`.
    #[default]
    Letters,
    /// One-cell symbols: `●` modified, `✖` deleted, `+` new, `·` ignored.
    /// Read at a glance and not mistaken for a name.
    Symbols,
}

/// How the column is painted: the glyphs and whether ignored files are marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub glyphs: Glyphs,
    /// `false` = an ignored file paints as clean. With large `.gitignore`
    /// files the mark repeats across half the screen and stops saying
    /// anything.
    pub hide_ignored: bool,
}

impl Style {
    /// From the two `[config]` values as the host gives them.
    #[must_use]
    pub fn parse(glyphs: Option<&str>, ignored: Option<&str>) -> Self {
        Self {
            glyphs: match glyphs {
                Some("symbols") => Glyphs::Symbols,
                _ => Glyphs::Letters,
            },
            hide_ignored: matches!(ignored, Some("false")),
        }
    }
}

impl State {
    /// The cell the user sees, with the usual letters. `Clean` paints
    /// nothing: a column full of identical marks says nothing.
    #[must_use]
    pub fn cell(self) -> Option<String> {
        self.cell_with(Style::default())
    }

    /// The cell the user sees, with the configured style.
    #[must_use]
    pub fn cell_with(self, style: Style) -> Option<String> {
        let glyph = match (self, style.glyphs) {
            (Self::Clean, _) => return None,
            (Self::Ignored, _) if style.hide_ignored => return None,
            (Self::Modified, Glyphs::Letters) => "M",
            (Self::Deleted, Glyphs::Letters) => "D",
            (Self::Untracked, Glyphs::Letters) => "?",
            (Self::Ignored, Glyphs::Letters) => "!",
            (Self::Modified, Glyphs::Symbols) => "●",
            (Self::Deleted, Glyphs::Symbols) => "✖",
            (Self::Untracked, Glyphs::Symbols) => "+",
            (Self::Ignored, Glyphs::Symbols) => "·",
        };
        Some(glyph.to_string())
    }

    /// Which of two states rules when aggregating a directory. The
    /// strongest wins: a directory with something modified inside is
    /// modified, no matter how many clean files come with it.
    fn rank(self) -> u8 {
        match self {
            Self::Clean => 0,
            Self::Ignored => 1,
            Self::Untracked => 2,
            Self::Deleted => 3,
            Self::Modified => 4,
        }
    }

    fn strongest(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// The state of every visible entry in a directory.
///
/// `prefix` is the visible directory's path relative to the repository's
/// root (empty = the root), and `names` are the page's names.
pub fn status_for(
    index: &GitIndex,
    ignores: &Ignores,
    loc: &dyn Location,
    prefix: &[u8],
    names: &[Vec<u8>],
    index_mtime_sec: i64,
) -> Vec<Option<String>> {
    status_for_with(
        index,
        ignores,
        loc,
        prefix,
        names,
        index_mtime_sec,
        Style::default(),
    )
}

/// [`status_for`] with the configured style (`[config] glyphs`/`ignored`).
pub fn status_for_with(
    index: &GitIndex,
    ignores: &Ignores,
    loc: &dyn Location,
    prefix: &[u8],
    names: &[Vec<u8>],
    index_mtime_sec: i64,
    style: Style,
) -> Vec<Option<String>> {
    names
        .iter()
        .map(|name| {
            let rel = join(prefix, name);
            state_of(index, ignores, loc, &rel, index_mtime_sec).cell_with(style)
        })
        .collect()
}

#[cfg(test)]
mod style_tests {
    use super::*;

    #[test]
    fn the_style_changes_the_glyph_and_can_silence_ignored_files() {
        assert_eq!(State::Modified.cell(), Some("M".to_string()));
        let symbols = Style::parse(Some("symbols"), Some("true"));
        assert_eq!(State::Modified.cell_with(symbols), Some("●".to_string()));
        assert_eq!(State::Deleted.cell_with(symbols), Some("✖".to_string()));
        assert_eq!(State::Untracked.cell_with(symbols), Some("+".to_string()));
        assert_eq!(State::Ignored.cell_with(symbols), Some("·".to_string()));
        assert_eq!(
            State::Clean.cell_with(symbols),
            None,
            "clean paints nothing"
        );
        let without_ignored = Style::parse(Some("letters"), Some("false"));
        assert_eq!(State::Ignored.cell_with(without_ignored), None);
        assert_eq!(
            State::Untracked.cell_with(without_ignored),
            Some("?".to_string())
        );
        assert_eq!(
            Style::parse(None, None),
            Style::default(),
            "no config, the usual"
        );
    }
}

fn join(prefix: &[u8], name: &[u8]) -> Vec<u8> {
    if prefix.is_empty() {
        return name.to_vec();
    }
    let mut out = prefix.to_vec();
    out.push(b'/');
    out.extend_from_slice(name);
    out
}

/// The state of ONE path relative to the repository's root.
fn state_of(
    index: &GitIndex,
    ignores: &Ignores,
    loc: &dyn Location,
    rel: &[u8],
    index_mtime_sec: i64,
) -> State {
    let meta = loc.stat(rel).ok();
    if let Some(entry) = index.get(rel) {
        let Some(meta) = meta else {
            return State::Deleted;
        };
        return compare(entry, &meta, loc, rel, index_mtime_sec);
    }
    let is_dir = meta.is_some_and(|m| m.is_dir);
    if is_dir {
        // A directory aggregates the strongest state underneath. The index
        // is SORTED, so the tracked entries under the prefix are a
        // contiguous span and there is no need to walk it whole.
        let mut dir_prefix = rel.to_vec();
        dir_prefix.push(b'/');
        let mut worst = State::Clean;
        let mut tracked = false;
        for entry in index.under_prefix(&dir_prefix) {
            tracked = true;
            let child = match loc.stat(&entry.path) {
                Ok(meta) => compare(entry, &meta, loc, &entry.path, index_mtime_sec),
                Err(_) => State::Deleted,
            };
            worst = worst.strongest(child);
            if worst == State::Modified {
                break; // nothing stronger left to find
            }
        }
        if tracked {
            return worst;
        }
    }
    if ignores.is_ignored(rel, is_dir) {
        State::Ignored
    } else {
        State::Untracked
    }
}

/// Compares an index entry with the current `stat`, and only reads the file
/// if that does not decide it.
fn compare(
    entry: &crate::index::IndexEntry,
    meta: &Meta,
    loc: &dyn Location,
    rel: &[u8],
    index_mtime_sec: i64,
) -> State {
    // A SUBMODULE is not a file (#225). Its index entry is a "gitlink" —mode
    // `0o160000`— whose path is the directory, so the exact lookup finds it
    // and the comparison below used to see a directory where the index said
    // file: it gave `D`, i.e. reporting a perfectly healthy submodule as
    // deleted.
    //
    // What is answered is NOTHING, and it is deliberate: knowing whether it
    // has changes requires opening the repository inside it —another
    // `.git`, another index, another object tree—, which is the same
    // boundary that leaves out the "staged" state. Staying silent is
    // honest; putting a mark would be asserting something that has not been
    // looked at.
    if entry.mode & 0o170_000 == 0o160_000 {
        return State::Clean;
    }
    if meta.is_dir {
        // It was a tracked file and now there is a directory: for git that
        // is the file deleted.
        return State::Deleted;
    }
    if u64::from(entry.size) != meta.size {
        return State::Modified;
    }
    let mtime_equal =
        i64::from(entry.mtime_sec) == meta.mtime_sec && entry.mtime_nsec == meta.mtime_nsec;
    if !mtime_equal {
        // Same size, different mtime: it could be a `touch` with no
        // changes, so the content decides, not the timestamp.
        return by_content(entry, loc, rel);
    }
    // The `stat` matches. It can still lie: if the entry was saved in the
    // SAME second the index was written —the "racy git" case—, a later
    // change within that second is indistinguishable. git resolves this
    // exactly this way, comparing against the index's OWN mtime, and that
    // is why `read` exists in the interface.
    if i64::from(entry.mtime_sec) >= index_mtime_sec {
        return by_content(entry, loc, rel);
    }
    State::Clean
}

/// The content tiebreaker: the object id git would give the file. If it
/// cannot be read —budget exhausted, permissions— the answer is clean,
/// never a made-up mark.
fn by_content(entry: &crate::index::IndexEntry, loc: &dyn Location, rel: &[u8]) -> State {
    let Ok(bytes) = loc.read(rel) else {
        return State::Clean;
    };
    if u64::from(entry.size) != bytes.len() as u64 {
        return State::Modified;
    }
    if entry.oid == [0u8; 20] {
        // The index carried no id (a test forge): with the size equal,
        // there is nothing else to compare.
        return State::Clean;
    }
    if crate::sha1::blob_oid(&bytes) == entry.oid {
        State::Clean
    } else {
        State::Modified
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use core::cell::RefCell;

    /// A fake location that COUNTS reads: the test that `stat` is enough is
    /// a test about how many times a file was opened.
    #[derive(Default)]
    struct FakeLocation {
        files: BTreeMap<Vec<u8>, (Vec<u8>, Meta)>,
        reads: RefCell<usize>,
    }

    impl FakeLocation {
        fn with(mut self, rel: &[u8], content: &[u8], meta: Meta) -> Self {
            self.files.insert(rel.to_vec(), (content.to_vec(), meta));
            self
        }

        fn with_dir(mut self, rel: &[u8]) -> Self {
            self.files.insert(
                rel.to_vec(),
                (
                    Vec::new(),
                    Meta {
                        is_dir: true,
                        size: 0,
                        mtime_sec: 0,
                        mtime_nsec: 0,
                    },
                ),
            );
            self
        }

        fn reads(&self) -> usize {
            *self.reads.borrow()
        }
    }

    impl Location for FakeLocation {
        fn read(&self, rel: &[u8]) -> Result<Vec<u8>, String> {
            *self.reads.borrow_mut() += 1;
            self.files
                .get(rel)
                .map(|(c, _)| c.clone())
                .ok_or_else(|| "does not exist".to_string())
        }

        fn stat(&self, rel: &[u8]) -> Result<Meta, String> {
            self.files
                .get(rel)
                .map(|(_, m)| *m)
                .ok_or_else(|| "does not exist".to_string())
        }
    }

    /// The index was written AFTER the entries: nothing is racy.
    const NEW_INDEX: i64 = 100;
    /// The index was written at the same time as the entry: the racy case.
    const OLD_INDEX: i64 = 0;

    fn meta(size: u64, mtime_sec: i64, mtime_nsec: u32) -> Meta {
        Meta {
            is_dir: false,
            size,
            mtime_sec,
            mtime_nsec,
        }
    }

    /// An index with the given entries: `(path, size, mtime, oid)`.
    fn index_with(entries: &[(&[u8], u32, u32, [u8; 20])]) -> GitIndex {
        GitIndex::parse(&crate::index::tests_support::forja(entries)).expect("forged index")
    }

    #[test]
    fn stat_equal_to_the_index_is_clean_and_does_not_read_the_file() {
        let idx = index_with(&[(b"a.txt", 4, 11, [0u8; 20])]);
        let fs = FakeLocation::default().with(b"a.txt", b"text", meta(4, 11, 0));
        let cells = status_for(
            &idx,
            &Ignores::default(),
            &fs,
            b"",
            &[b"a.txt".to_vec()],
            NEW_INDEX,
        );
        assert_eq!(cells, vec![None], "empty cell = clean");
        assert_eq!(fs.reads(), 0, "stat is enough: content is not read");
    }

    #[test]
    fn same_mtime_but_different_size_is_modified() {
        let idx = index_with(&[(b"a.txt", 4, 11, [0u8; 20])]);
        let fs = FakeLocation::default().with(b"a.txt", b"growth", meta(6, 11, 0));
        let cells = status_for(
            &idx,
            &Ignores::default(),
            &fs,
            b"",
            &[b"a.txt".to_vec()],
            NEW_INDEX,
        );
        assert_eq!(cells, vec![Some("M".to_string())]);
        assert_eq!(fs.reads(), 0, "the size already decided it");
    }

    /// The racy case: same mtime, same size and the index with no
    /// nanoseconds. The `stat` does NOT decide, so the object id is read
    /// and compared — exactly what git does, and that is why `read` exists
    /// in the interface.
    #[test]
    fn the_racy_case_reads_and_compares_the_oid() {
        let oid = crate::sha1::blob_oid(b"read");
        let idx = index_with(&[(b"a.txt", 4, 11, oid)]);
        let fs = FakeLocation::default().with(b"a.txt", b"seen", meta(4, 11, 0));
        // The index was written in the same second: the stat matches and
        // still decides nothing.
        let cells = status_for(
            &idx,
            &Ignores::default(),
            &fs,
            b"",
            &[b"a.txt".to_vec()],
            OLD_INDEX,
        );
        assert_eq!(cells, vec![Some("M".to_string())]);
        assert_eq!(fs.reads(), 1);

        let clean = FakeLocation::default().with(b"a.txt", b"read", meta(4, 11, 0));
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &clean,
                b"",
                &[b"a.txt".to_vec()],
                OLD_INDEX
            ),
            vec![None]
        );
    }

    #[test]
    fn a_tracked_entry_that_is_no_longer_there_is_deleted() {
        let idx = index_with(&[(b"a.txt", 4, 11, [0u8; 20])]);
        let fs = FakeLocation::default();
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"",
                &[b"a.txt".to_vec()],
                NEW_INDEX
            ),
            vec![Some("D".to_string())]
        );
    }

    #[test]
    fn the_untracked_is_a_question_mark_and_the_ignored_is_an_exclamation_mark() {
        let idx = index_with(&[(b"tracked.txt", 1, 11, [0u8; 20])]);
        let mut ign = Ignores::default();
        ign.add_file(b"", b"target/\n*.tmp\n");
        let fs = FakeLocation::default()
            .with_dir(b"target")
            .with(b"new.rs", b"", meta(0, 1, 1))
            .with(b"junk.tmp", b"", meta(0, 1, 1));
        let cells = status_for(
            &idx,
            &ign,
            &fs,
            b"",
            &[b"target".to_vec(), b"new.rs".to_vec(), b"junk.tmp".to_vec()],
            NEW_INDEX,
        );
        assert_eq!(
            cells,
            vec![
                Some("!".to_string()),
                Some("?".to_string()),
                Some("!".to_string())
            ]
        );
    }

    /// **A submodule is not deleted** (#225).
    ///
    /// Its index entry is a "gitlink" (mode `0o160000`) whose path is the
    /// DIRECTORY, so `index.get` finds it and `compare` used to see a
    /// directory where the index said file: `D`. I.e., the column reported
    /// a perfectly healthy submodule as deleted, which is a false alarm
    /// about the thing that scares people most.
    ///
    /// What it says now is NOTHING: without opening the repository inside
    /// it, there is no way to know whether it has changes, and staying
    /// silent is the honest thing. Saying "clean" with a mark would be
    /// asserting it.
    #[test]
    fn a_submodule_does_not_come_out_as_deleted() {
        let idx = GitIndex::parse(&crate::index::tests_support::forja_con_modo(&[(
            b"vendor/lib",
            0,
            11,
            [0u8; 20],
            0o160_000,
        )]))
        .expect("forged index");
        let fs = FakeLocation::default().with_dir(b"vendor/lib");
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"vendor",
                &[b"lib".to_vec()],
                NEW_INDEX
            ),
            vec![None],
            "a healthy submodule is neither \"deleted\" nor \"untracked\""
        );
    }

    #[test]
    fn a_directory_aggregates_the_strongest_state_underneath() {
        let idx = index_with(&[
            (b"src/deep/x.rs", 4, 11, [0u8; 20]),
            (b"src/clean.rs", 4, 11, [0u8; 20]),
        ]);
        let fs = FakeLocation::default()
            .with_dir(b"src")
            .with(b"src/deep/x.rs", b"other", meta(9, 11, 0))
            .with(b"src/clean.rs", b"text", meta(4, 11, 0));
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"",
                &[b"src".to_vec()],
                NEW_INDEX
            ),
            vec![Some("M".to_string())]
        );
    }

    #[test]
    fn a_directory_with_everything_clean_paints_nothing() {
        let idx = index_with(&[(b"src/a.rs", 4, 11, [0u8; 20])]);
        let fs =
            FakeLocation::default()
                .with_dir(b"src")
                .with(b"src/a.rs", b"text", meta(4, 11, 0));
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"",
                &[b"src".to_vec()],
                NEW_INDEX
            ),
            vec![None]
        );
    }

    /// The prefix is what makes this work outside the root: the opened root
    /// is the repository, and the panel can be three levels in.
    #[test]
    fn the_prefix_places_the_page_inside_the_repository() {
        let idx = index_with(&[(b"src/deep/x.rs", 4, 11, [0u8; 20])]);
        let fs = FakeLocation::default().with(b"src/deep/x.rs", b"OTHER", meta(9, 11, 0));
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"src/deep",
                &[b"x.rs".to_vec()],
                NEW_INDEX
            ),
            vec![Some("M".to_string())]
        );
    }

    #[test]
    fn without_an_index_every_cell_is_untracked() {
        let idx = GitIndex::default();
        let fs = FakeLocation::default().with(b"a", b"", meta(0, 1, 1));
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"",
                &[b"a".to_vec()],
                NEW_INDEX
            ),
            vec![Some("?".to_string())]
        );
    }
}
