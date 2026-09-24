//! The in-RAM tree of ONE `.rar`: entries validated as `VPath` segments,
//! skipped ones counted, and the question that decides whether a name can be
//! asked of the delegate without ambiguity.

use std::collections::{BTreeSet, HashMap};

use norte_proto::{Entry, EntryKind, Error, Segment, VPath};

use crate::RarLimits;
use crate::delegate::RarError;
use crate::listing::RawEntry;

/// Tree key: the inner path's components, in bytes.
pub(crate) type InnerPath = Vec<Vec<u8>>;

/// A node of the virtual tree.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub mtime_ms: Option<i64>,
    /// The entry is encrypted: it gets LISTED, but reading it would ask for a
    /// password nobody is going to type (the child has `stdin` set to null).
    pub encrypted: bool,
}

impl Node {
    fn dir(mtime_ms: Option<i64>) -> Self {
        Self {
            kind: EntryKind::Dir,
            size: None,
            mtime_ms,
            encrypted: false,
        }
    }
}

/// Full index of ONE archive, bound to a generation of the container.
pub struct ArchiveIndex {
    pub(crate) nodes: HashMap<InnerPath, Node>,
    pub(crate) children: HashMap<InnerPath, BTreeSet<Vec<u8>>>,
    skipped: u64,
    /// The FULL names exactly as they would be asked of the delegate. Kept
    /// separately because the ambiguity test is against them, not against
    /// the tree.
    full_names: Vec<Vec<u8>>,
    /// `(mtime_ms, size)` of the container at indexing time — the
    /// invalidation key.
    pub(crate) generation: (Option<i64>, Option<u64>),
}

impl ArchiveIndex {
    /// Builds the index from what the delegate printed, with the default
    /// limits. For the tests and for whoever configures nothing.
    #[must_use]
    pub fn from_raw(raw: Vec<RawEntry>) -> Self {
        Self::build(raw, &RarLimits::default(), (None, None), 0)
    }

    /// Like [`from_raw`](Self::from_raw), given limits, generation and how
    /// many entries the PARSER already discarded (those count too).
    #[must_use]
    pub(crate) fn build(
        raw: Vec<RawEntry>,
        limits: &RarLimits,
        generation: (Option<i64>, Option<u64>),
        skipped_by_parser: u64,
    ) -> Self {
        let mut idx = Self {
            nodes: HashMap::new(),
            children: HashMap::new(),
            skipped: skipped_by_parser,
            full_names: Vec::new(),
            generation,
        };
        for entry in raw {
            idx.insert(&entry, limits);
        }
        if idx.skipped > 0 {
            tracing::warn!(
                skipped = idx.skipped,
                indexed = idx.nodes.len(),
                "rar entries skipped for a non-representable name"
            );
        }
        idx
    }

    /// How many entries of the archive are NOT in the tree.
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// How many nodes the tree has (implicit dirs included).
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// `true` if the archive brought no representable entry.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Can this name be asked of the delegate without it pulling OUT
    /// something else?
    ///
    /// MEASURED: both delegates treat the entry's name as a **pattern**, and
    /// neither has a "this is literal" switch. An entry named
    /// `star?name.txt` also extracts `starXname.txt`, and the stream looks
    /// perfectly healthy. Since the whole index is already in memory, the
    /// ambiguity is decided HERE, against our own names, before starting any
    /// process.
    ///
    /// # Errors
    ///
    /// [`RarError::AmbiguousForDelegate`] if the name, treated as a pattern,
    /// reaches some other entry of the archive.
    pub fn addressable(&self, name: &[u8]) -> Result<(), RarError> {
        if !name.iter().any(|b| matches!(b, b'*' | b'?' | b'[' | b']')) {
            return Ok(()); // no metacharacters, nothing to confuse
        }
        let collisions = self
            .full_names
            .iter()
            .filter(|other| other.as_slice() != name && glob_matches(name, other))
            .count();
        if collisions == 0 {
            Ok(())
        } else {
            tracing::warn!(
                name = ?String::from_utf8_lossy(name),
                collisions,
                "unaddressable name: the delegate would treat it as a pattern"
            );
            Err(RarError::AmbiguousForDelegate)
        }
    }

    /// Validates and splits the raw name. `None` = not representable,
    /// already counted.
    fn split_name(&mut self, raw: &[u8], limits: &RarLimits) -> Option<(InnerPath, bool)> {
        let mut hostile = |why: &str| {
            tracing::debug!(name = ?String::from_utf8_lossy(raw), why, "entry skipped");
            self.skipped += 1;
            None
        };
        if raw.is_empty() {
            return hostile("empty name");
        }
        if raw.len() > limits.max_name_bytes {
            return hostile("name too long");
        }
        if raw.first() == Some(&b'/') {
            return hostile("absolute path");
        }
        // RAR stores `\` as a separator when the archive was made on
        // Windows; it is NOT translated here: a name with `\` is a name with
        // `\`, and converting it would invent a hierarchy the archive does
        // not declare.
        let is_dir = raw.last() == Some(&b'/');
        let body = if is_dir { &raw[..raw.len() - 1] } else { raw };
        let mut parts: InnerPath = Vec::new();
        for comp in body.split(|&b| b == b'/') {
            if comp.is_empty() || comp == b"." || comp == b".." {
                return hostile("`.`/`..`/empty component (traversal)");
            }
            if comp == b"!" {
                return hostile("`!` component (ADR 0018 marker, unaddressable)");
            }
            if Segment::new(comp.to_vec()).is_err() {
                return hostile("component invalid as a VPath segment");
            }
            parts.push(comp.to_vec());
        }
        if parts.is_empty() {
            return hostile("name with no components");
        }
        if parts.len() > limits.max_depth {
            return hostile("excessive depth");
        }
        Some((parts, is_dir))
    }

    fn add_child(&mut self, parent: InnerPath, name: Vec<u8>) {
        self.children.entry(parent).or_default().insert(name);
    }

    /// Materializes the ancestors as implicit dirs, with the same "the dir
    /// wins" criterion as ADR 0018: without this, an `a` file followed by
    /// `a/child` would leave the subtree invisible.
    fn ensure_parents(&mut self, path: &[Vec<u8>]) {
        for depth in 0..path.len().saturating_sub(1) {
            let dir: InnerPath = path[..=depth].to_vec();
            let node = self.nodes.entry(dir).or_insert_with(|| Node::dir(None));
            if node.kind != EntryKind::Dir {
                *node = Node::dir(node.mtime_ms);
                self.skipped += 1;
            }
            self.add_child(path[..depth].to_vec(), path[depth].clone());
        }
    }

    fn insert(&mut self, entry: &RawEntry, limits: &RarLimits) {
        if self.nodes.len() >= limits.max_entries {
            self.skipped += 1;
            return;
        }
        let Some((path, trailing_slash)) = self.split_name(&entry.name, limits) else {
            return;
        };
        let is_dir = entry.is_dir || trailing_slash;
        let node = Node {
            kind: if is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            size: if is_dir { None } else { Some(entry.size) },
            mtime_ms: entry.mtime.map(|s| s * 1_000),
            encrypted: entry.encrypted,
        };
        self.ensure_parents(&path);
        // A dir never downgrades to a file: the subtree would be lost.
        if self
            .nodes
            .get(&path)
            .is_some_and(|prev| prev.kind == EntryKind::Dir && node.kind != EntryKind::Dir)
        {
            self.skipped += 1;
            return;
        }
        // The name that will be asked of the delegate is the TREE's, not
        // the raw one: if the raw one carried a trailing `dir/`, asking with
        // the slash extracts nothing.
        let full = path.join(&b'/');
        if !self.full_names.contains(&full) {
            self.full_names.push(full);
        }
        self.nodes.insert(path.clone(), node);
        let (parent, name) = (
            path[..path.len() - 1].to_vec(),
            path.last().expect("non-empty path").clone(),
        );
        self.add_child(parent, name);
    }

    /// A node's `Entry`, or the container's synthetic root.
    pub(crate) fn entry_for(&self, at: &VPath, inner: &[Vec<u8>]) -> Result<Entry, Error> {
        if inner.is_empty() {
            return Ok(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: at.clone(),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: self.generation.0,
            });
        }
        let node = self.nodes.get(inner).ok_or(Error::NotFound)?;
        Ok(Entry {
            attrs: std::collections::BTreeMap::new(),
            path: at.clone(),
            kind: node.kind,
            size: node.size,
            mtime_ms: node.mtime_ms,
        })
    }

    pub(crate) fn node(&self, inner: &[Vec<u8>]) -> Option<&Node> {
        self.nodes.get(inner)
    }
}

/// Does `candidate` match `pattern` understood as the glob the delegate
/// would apply?
///
/// Deliberately GENEROUS: `*` crosses slashes and a badly closed `[...]`
/// class is treated as literal. Erring toward more here produces a refusal
/// ("I cannot give you that entry without ambiguity"); erring toward less
/// produces ANOTHER file's content.
fn glob_matches(pattern: &[u8], candidate: &[u8]) -> bool {
    match pattern.first() {
        None => candidate.is_empty(),
        Some(b'*') => {
            (0..=candidate.len()).any(|skip| glob_matches(&pattern[1..], &candidate[skip..]))
        }
        Some(b'?') => !candidate.is_empty() && glob_matches(&pattern[1..], &candidate[1..]),
        Some(b'[') => match class_end(pattern) {
            Some(end) => {
                !candidate.is_empty()
                    && class_matches(&pattern[1..end], candidate[0])
                    && glob_matches(&pattern[end + 1..], &candidate[1..])
            }
            // Unclosed class: literal, as a shell does.
            None => literal_head(pattern, candidate),
        },
        Some(_) => literal_head(pattern, candidate),
    }
}

fn literal_head(pattern: &[u8], candidate: &[u8]) -> bool {
    match (pattern.first(), candidate.first()) {
        (Some(p), Some(c)) if p == c => glob_matches(&pattern[1..], &candidate[1..]),
        _ => false,
    }
}

fn class_end(pattern: &[u8]) -> Option<usize> {
    // `[]abc]` is a class that contains `]`: the first `]` glued to the
    // bracket does not close it.
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

    fn raw(name: &[u8], size: u64) -> RawEntry {
        RawEntry {
            name: name.to_vec(),
            size,
            is_dir: false,
            mtime: None,
            encrypted: false,
            solid: false,
        }
    }

    /// MEASURED: `star?name.txt` pulls out TWO entries from both delegates.
    #[test]
    fn a_name_that_is_another_ones_glob_is_rejected() {
        let idx = ArchiveIndex::from_raw(vec![raw(b"star?name.txt", 7), raw(b"starXname.txt", 7)]);
        assert!(matches!(
            idx.addressable(b"star?name.txt"),
            Err(RarError::AmbiguousForDelegate)
        ));
        // The literal twin is NOT ambiguous: it contains no metacharacters.
        assert!(idx.addressable(b"starXname.txt").is_ok());
    }

    /// A pattern that only reaches itself CAN be asked for: refusing it
    /// would hide a file that can be served fine.
    #[test]
    fn a_glob_that_only_reaches_itself_is_allowed() {
        let idx = ArchiveIndex::from_raw(vec![raw(b"solo*.txt", 1), raw(b"otro.bin", 1)]);
        assert!(idx.addressable(b"solo*.txt").is_ok());
    }

    #[test]
    fn a_bracket_class_also_counts_as_a_pattern() {
        let idx = ArchiveIndex::from_raw(vec![raw(b"a[bc]d.txt", 1), raw(b"abd.txt", 1)]);
        assert!(matches!(
            idx.addressable(b"a[bc]d.txt"),
            Err(RarError::AmbiguousForDelegate)
        ));
    }

    #[test]
    fn an_unsafe_name_is_skipped_and_counted() {
        let idx = ArchiveIndex::from_raw(vec![
            raw(b"ok.txt", 1),
            raw(b"../outside.txt", 1),
            raw(b"/abs.txt", 1),
            raw(b"con\0nul", 1),
        ]);
        assert_eq!(idx.len(), 1);
        assert_eq!(
            idx.skipped(),
            3,
            "ADR 0018: skipped and counted, never fatal"
        );
    }

    #[test]
    fn intermediate_directories_are_materialized() {
        let idx = ArchiveIndex::from_raw(vec![raw(b"docs/sub/leaf.txt", 3)]);
        assert_eq!(idx.len(), 3, "docs, docs/sub and the leaf");
        assert_eq!(idx.children[&vec![]].len(), 1);
    }

    #[test]
    fn the_archive_marker_is_not_addressable_and_is_skipped() {
        let idx = ArchiveIndex::from_raw(vec![raw(b"a/!/b.txt", 1)]);
        assert!(idx.is_empty());
        assert_eq!(idx.skipped(), 1);
    }

    #[test]
    fn the_entry_cap_does_not_blow_up_the_whole_index() {
        let limits = RarLimits {
            max_entries: 2,
            ..RarLimits::default()
        };
        let idx = ArchiveIndex::build(
            (0..5)
                .map(|i| raw(format!("f{i}.txt").as_bytes(), 1))
                .collect(),
            &limits,
            (None, None),
            0,
        );
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.skipped(), 3, "the ones that don't fit are counted");
    }

    #[test]
    fn the_star_glob_crosses_slashes() {
        assert!(glob_matches(b"a*z", b"a/b/z"));
        assert!(!glob_matches(b"a?z", b"a/bz"));
        assert!(glob_matches(b"a[!x]z", b"abz"));
        assert!(!glob_matches(b"a[!x]z", b"axz"));
        assert!(glob_matches(b"a[b-d]z", b"acz"));
    }
}
