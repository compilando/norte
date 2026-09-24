//! Listing order (presentation), shared by the frontends.

use norte_proto::{Entry, EntryKind};

/// Chosen order for the listing (#108 L7): column + direction + dir
/// grouping. The default reproduces EXACTLY the historical order (name/asc/
/// dirs-first), so nothing changes until the user picks something else.
/// Not `Copy` because its column no longer is ([`SortColumn::Attr`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SortSpec {
    /// The column sorted by.
    pub column: SortColumn,
    /// Direction: reverses ONLY the column comparison — never the dir
    /// grouping nor the name tie-break (total, stable order).
    pub dir: SortDir,
    /// Directories first (separate group, always ascending).
    pub dirs_first: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            column: SortColumn::Name,
            dir: SortDir::Asc,
            dirs_first: true,
        }
    }
}

impl SortSpec {
    /// The result of a click on `col`'s header (#108 b6): the active column
    /// reverses its direction; a new column sorts by it ASCENDING.
    /// `dirs_first` never changes on a click — it is a preference, not a
    /// column criterion. Shared: the GUI's header today, both frontends'
    /// pickers in block 7.
    #[must_use]
    pub fn after_click(self, col: SortColumn) -> Self {
        if self.column == col {
            let dir = match self.dir {
                SortDir::Asc => SortDir::Desc,
                SortDir::Desc => SortDir::Asc,
            };
            Self { dir, ..self }
        } else {
            Self {
                column: col,
                dir: SortDir::Asc,
                ..self
            }
        }
    }
}

/// Sort column (#108): the built-ins and, since ADR 0144, any provider
/// ATTRIBUTE (`attr:<id>`). Plugin ones are still not sortable: their values
/// do not live in the `Entry`.
///
/// Not `Copy` since it carries an attribute's id: that id is emitted by the
/// provider at runtime, so it is a `String`. Keeping `Copy` asked for
/// interning the ids with a deliberate leak, and a provider that emits a
/// hundred attributes turns it into a real one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortColumn {
    /// Name (NFC form as the key, raw bytes to break ties) — the usual
    /// order.
    Name,
    /// `Entry.size`. A missing value (dirs, lazy providers #52) goes AT THE
    /// END in both directions.
    Size,
    /// `Entry.mtime_ms` (negative pre-1970 values are valid). Missing = at
    /// the end.
    Mtime,
    /// The name's extension: what follows the LAST dot (#138).
    ///
    /// A directory has no extension, and neither does a name that starts
    /// with a dot —`.bashrc` is a name, not an extension—: both go AT THE
    /// END in both directions, like any missing value.
    Extension,
    /// A provider attribute by its id (`posix.mode`, `posix.uid`…), the same
    /// one that names its `attr:<id>` column (ADR 0144).
    ///
    /// Sorted by the attribute's VALUE, not by how it is painted: `rwxr-xr-x`
    /// and `0o755` are the same number, and it is the number that groups. A
    /// missing value —the provider does not know it, or does not emit it for
    /// that entry— goes at the END in both directions, like an unknown size.
    Attr(String),
}

/// A sort column that a COMMAND or the config can name: the built-ins, which
/// are the only ones with a key and a `[ui] sort` config key.
///
/// The conversion lives here and not in every place that needs it: it used
/// to be written inside config reading and the window had to repeat it.
impl From<norte_config::SortColumnKey> for SortColumn {
    fn from(k: norte_config::SortColumnKey) -> Self {
        match k {
            norte_config::SortColumnKey::Name => Self::Name,
            norte_config::SortColumnKey::Size => Self::Size,
            norte_config::SortColumnKey::Mtime => Self::Mtime,
            norte_config::SortColumnKey::Extension => Self::Extension,
        }
    }
}

/// The column's sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    /// Ascending.
    Asc,
    /// Descending (column only; see [`SortSpec::dir`]).
    Desc,
}

/// Listing order (presentation): directories first; within each group, by
/// the name's NFC form (spec §6.1: `unicode_compare = nfc` by default — ONLY
/// as the sort key, the bytes are never mutated) with a raw-byte tie-break.
/// Non-UTF-8 names: bytes as they are.
pub fn sort_entries(entries: &mut [Entry]) {
    sort_entries_with(entries, &SortSpec::default());
}

/// [`sort_entries`] under an explicit [`SortSpec`] (#108 L7). Stable; same
/// order as `sort_with_keys_spec`/`merge_keyed_spec` with the same spec.
pub fn sort_entries_with(entries: &mut [Entry], spec: &SortSpec) {
    let keys: Vec<SortKey> = entries.iter().map(sort_key).collect();
    // `sort_by` over indices would be more alloc-frugal, but only tests/CLI
    // use this path; panes go through `sort_with_keys` (#54).
    let mut pairs: Vec<(SortKey, Entry)> = keys.into_iter().zip(entries.iter().cloned()).collect();
    pairs.sort_by(|a, b| cmp_keyed_with((&a.0, &a.1), (&b.0, &b.1), spec));
    for (slot, (_, e)) in entries.iter_mut().zip(pairs) {
        *slot = e;
    }
}

/// The name's NFC form as the sort key, or `None` when it matches the raw
/// name byte for byte (ASCII, already-NFC, or non-UTF-8) — the overwhelmingly
/// common case, which this way materializes nothing (#94).
fn nfc_key(name: &[u8]) -> Option<Vec<u8>> {
    use unicode_normalization::{UnicodeNormalization, is_nfc};
    let s = std::str::from_utf8(name).ok()?;
    if is_nfc(s) {
        return None;
    }
    Some(s.nfc().collect::<String>().into_bytes())
}

fn name_bytes(e: &Entry) -> &[u8] {
    e.path.file_name().map_or(b"", |n| n.as_bytes())
}

/// PERSISTABLE sort key of an entry (#54): group (dirs first) + the name's
/// NFC form. The raw-byte tie-break is NOT materialized — it is read from the
/// `Entry` itself when comparing (one less alloc per entry). WATCH OUT: the
/// derived `PartialEq`/`Eq` are REPRESENTATIONAL, not semantic (#94): an
/// already-NFC name (`nfc: None`) and its NFD twin (`nfc: Some(..)`) have the
/// SAME effective key but are `!=` as structs. Nothing compares `SortKey`s for
/// equality for logic — only `cmp_keyed_with` defines the order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SortKey {
    not_dir: bool,
    /// `None` = the NFC key is the name's raw bytes (common case: ASCII,
    /// already-NFC, or non-UTF-8) — read from the `Entry` itself when
    /// comparing, without materializing ~one alloc per entry (#94).
    nfc: Option<Vec<u8>>,
}

pub(crate) fn sort_key(e: &Entry) -> SortKey {
    SortKey {
        not_dir: e.kind != EntryKind::Dir,
        nfc: nfc_key(name_bytes(e)),
    }
}

/// Total comparison (key, entry): key and, on a tie, the name's raw bytes —
/// EXACTLY the same order as [`sort_entries`].
///
/// INVARIANT: each key MUST have been computed from ITS paired entry
/// ([`sort_key`]). Since #94 the pairing is load-bearing for the PRIMARY key
/// (a `None` is resolved by reading the entry) — a mismatched key no longer
/// just corrupts the tie-break, it silently corrupts the order.
/// `cmp_keyed_with` under a [`SortSpec`] (#108 L7). Order, with every rule
/// load-bearing:
/// 1. dir grouping (if `dirs_first`) — ALWAYS ascending;
/// 2. the column, reversed if `Desc`; a MISSING value (a dir with no size,
///    an unknown mtime) goes at the end in BOTH directions — desc does not
///    fill the pane's header with blanks;
/// 3. tie-break: the usual name order, ALWAYS ascending — total, stable and
///    deterministic.
pub(crate) fn cmp_keyed_with(
    a: (&SortKey, &Entry),
    b: (&SortKey, &Entry),
    spec: &SortSpec,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    debug_assert!(
        a.0.not_dir == (a.1.kind != EntryKind::Dir),
        "key↔entry mismatched"
    );
    debug_assert!(
        b.0.not_dir == (b.1.kind != EntryKind::Dir),
        "key↔entry mismatched"
    );
    if spec.dirs_first {
        let group_order = a.0.not_dir.cmp(&b.0.not_dir);
        if group_order != Ordering::Equal {
            return group_order;
        }
    }
    let col = match &spec.column {
        SortColumn::Name => cmp_name(a, b),
        SortColumn::Size => cmp_missing_last(a.1.size, b.1.size, spec.dir),
        SortColumn::Mtime => cmp_missing_last(a.1.mtime_ms, b.1.mtime_ms, spec.dir),
        SortColumn::Extension => cmp_ext(a.1, b.1, spec.dir),
        SortColumn::Attr(id) => cmp_attr(a.1.attrs.get(id), b.1.attrs.get(id), spec.dir),
    };
    let col = match (&spec.column, spec.dir) {
        // Name carries its tie-break built in and its reversal is over the
        // whole block (there is no "missing" to anchor at the end).
        (SortColumn::Name, SortDir::Desc) => col.reverse(),
        _ => col,
    };
    col.then_with(|| cmp_name(a, b))
}

/// An entry's extension: the bytes after the LAST dot, or `None`.
///
/// `None` for a directory, for a name with no dot, for one that STARTS with a
/// dot (`.bashrc` is a whole name) and for one that ends in a dot (there is
/// nothing after it). Returns BYTES, not text: a name need not be UTF-8 (hard
/// rule 1).
fn ext_bytes(e: &Entry) -> Option<&[u8]> {
    if e.kind == EntryKind::Dir {
        return None;
    }
    let name = name_bytes(e);
    let dot = name.iter().rposition(|b| *b == b'.')?;
    if dot == 0 || dot + 1 == name.len() {
        return None;
    }
    Some(&name[dot + 1..])
}

/// Compares two extensions WITHOUT distinguishing case.
///
/// `.TXT` and `.txt` are the same extension for whoever is sorting —grouping
/// them is the whole point of sorting by extension— even though they remain
/// different names for everything else: identity never folds (ADR 0051),
/// ORDER does.
///
/// The common path (ASCII extension) allocates nothing; the rare one
/// delegates to the same `fold` the quick search uses, so as not to invent a
/// second folding vocabulary.
fn cmp_ext_bytes(one: &[u8], other: &[u8]) -> std::cmp::Ordering {
    if one.is_ascii() && other.is_ascii() {
        return one
            .iter()
            .map(u8::to_ascii_lowercase)
            .cmp(other.iter().map(u8::to_ascii_lowercase));
    }
    crate::nav::fold(one).cmp(&crate::nav::fold(other))
}

/// The EXTENSION column: missing goes at the end in both directions, same as
/// an unknown size or date.
fn cmp_ext(left: &Entry, right: &Entry, dir: SortDir) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (ext_bytes(left), ext_bytes(right)) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(one), Some(other)) => {
            let order = cmp_ext_bytes(one, other);
            match dir {
                SortDir::Asc => order,
                SortDir::Desc => order.reverse(),
            }
        }
    }
}

/// The rank of a value's shape, so the attribute order is TOTAL.
///
/// Two entries in the same listing almost always carry the same type for the
/// same attribute, but nothing guarantees it —a third-party provider can send
/// `Uint` on one and `Text` on another—, and a comparator that does not order
/// mixed types leaves `sort` with an order that depends on the entry. The
/// fixed rank avoids that without inventing conversions.
fn rank(v: &norte_proto::attrs::AttrValue) -> u8 {
    use norte_proto::attrs::AttrValue;
    match v {
        AttrValue::Bool(_) => 0,
        AttrValue::Uint(_) | AttrValue::Int(_) => 1,
        AttrValue::TimeMs(_) => 2,
        AttrValue::Text(_) => 3,
        AttrValue::Bytes(_) => 4,
        // `Unknown` is not compared: it is treated as missing (see `cmp_attr`).
        AttrValue::Unknown => 5,
    }
}

/// Two values of the SAME rank, in their natural order. `Uint` and `Int`
/// share a rank and are compared as wide signed numbers, which is what they
/// are.
fn cmp_value(
    one: &norte_proto::attrs::AttrValue,
    other: &norte_proto::attrs::AttrValue,
) -> std::cmp::Ordering {
    use norte_proto::attrs::AttrValue;
    let widen = |v: &AttrValue| match v {
        AttrValue::Uint(u) => i128::from(*u),
        AttrValue::Int(i) => i128::from(*i),
        _ => 0,
    };
    match (one, other) {
        (AttrValue::Bool(x), AttrValue::Bool(y)) => x.cmp(y),
        (AttrValue::Uint(_) | AttrValue::Int(_), AttrValue::Uint(_) | AttrValue::Int(_)) => {
            widen(one).cmp(&widen(other))
        }
        (AttrValue::TimeMs(x), AttrValue::TimeMs(y)) => x.cmp(y),
        (AttrValue::Text(x), AttrValue::Text(y)) => x.cmp(y),
        (AttrValue::Bytes(x), AttrValue::Bytes(y)) => x.cmp(y),
        _ => rank(one).cmp(&rank(other)),
    }
}

/// An ATTRIBUTE column (ADR 0144): missing —or `Unknown`, which for sorting
/// is the same: there is no value to compare— goes at the end in both
/// directions, same as an unknown size or date.
fn cmp_attr(
    left: Option<&norte_proto::attrs::AttrValue>,
    right: Option<&norte_proto::attrs::AttrValue>,
    dir: SortDir,
) -> std::cmp::Ordering {
    use norte_proto::attrs::AttrValue;
    use std::cmp::Ordering;
    // A function and not a closure: a closure does not let you write that the
    // borrow it returns is THE SAME as the one it receives, and without that
    // the lifetime does not check out.
    fn known(v: Option<&AttrValue>) -> Option<&AttrValue> {
        v.filter(|v| !matches!(v, AttrValue::Unknown))
    }
    match (known(left), known(right)) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(one), Some(other)) => {
            let order = rank(one)
                .cmp(&rank(other))
                .then_with(|| cmp_value(one, other));
            match dir {
                SortDir::Asc => order,
                SortDir::Desc => order.reverse(),
            }
        }
    }
}

/// The usual NAME order: NFC key and raw-byte tie-break.
fn cmp_name(a: (&SortKey, &Entry), b: (&SortKey, &Entry)) -> std::cmp::Ordering {
    let nfc_a = a.0.nfc.as_deref().unwrap_or_else(|| name_bytes(a.1));
    let nfc_b = b.0.nfc.as_deref().unwrap_or_else(|| name_bytes(b.1));
    nfc_a
        .cmp(nfc_b)
        .then_with(|| name_bytes(a.1).cmp(name_bytes(b.1)))
}

/// Comparison for an optional column with "missing goes at the end in BOTH
/// directions": present values are compared (reversed if `Desc`), a missing
/// one loses against any present one, two missing ones tie (the caller's name
/// tie-break decides).
fn cmp_missing_last<T: Ord>(lhs: Option<T>, rhs: Option<T>, dir: SortDir) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (lhs, rhs) {
        (Some(lv), Some(rv)) => {
            let ord = lv.cmp(&rv);
            if dir == SortDir::Desc {
                ord.reverse()
            } else {
                ord
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Sorts `entries`, computing their keys ONCE, and returns both
/// (index-parallel). Stable, same order as [`sort_entries`].
/// `sort_with_keys_spec` under a [`SortSpec`] (#108 L7).
pub(crate) fn sort_with_keys_spec(
    entries: Vec<Entry>,
    spec: &SortSpec,
) -> (Vec<Entry>, Vec<SortKey>) {
    let mut pairs: Vec<(SortKey, Entry)> = entries.into_iter().map(|e| (sort_key(&e), e)).collect();
    pairs.sort_by(|a, b| cmp_keyed_with((&a.0, &a.1), (&b.0, &b.1), spec));
    pairs.into_iter().map(|(k, e)| (e, k)).unzip()
}

/// `merge_keyed_spec` under a [`SortSpec`] (#108 L7): BOTH runs must already
/// be sorted by the SAME spec.
pub(crate) fn merge_keyed_spec(
    entries: &mut Vec<Entry>,
    keys: &mut Vec<SortKey>,
    batch_entries: Vec<Entry>,
    batch_keys: Vec<SortKey>,
    spec: &SortSpec,
) {
    // The zip below would silently TRUNCATE if the parallel vectors get out
    // of sync (losing listing entries without a sound): a future bug should
    // fail loudly in dev/test, not quietly in production.
    debug_assert_eq!(entries.len(), keys.len(), "entries↔sort_keys out of sync");
    debug_assert_eq!(
        batch_entries.len(),
        batch_keys.len(),
        "batch↔keys out of sync"
    );
    let mut out_e = Vec::with_capacity(entries.len() + batch_entries.len());
    let mut out_k = Vec::with_capacity(keys.len() + batch_keys.len());
    let mut left = std::mem::take(entries)
        .into_iter()
        .zip(std::mem::take(keys))
        .peekable();
    let mut right = batch_entries.into_iter().zip(batch_keys).peekable();
    loop {
        match (left.peek(), right.peek()) {
            (Some(l), Some(r)) => {
                // Left wins the tie (stability).
                if cmp_keyed_with((&l.1, &l.0), (&r.1, &r.0), spec) == std::cmp::Ordering::Greater {
                    let (e, k) = right.next().expect("peek == Some");
                    out_e.push(e);
                    out_k.push(k);
                } else {
                    let (e, k) = left.next().expect("peek == Some");
                    out_e.push(e);
                    out_k.push(k);
                }
            }
            (Some(_), None) => {
                for (e, k) in left.by_ref() {
                    out_e.push(e);
                    out_k.push(k);
                }
            }
            (None, Some(_)) => {
                for (e, k) in right.by_ref() {
                    out_e.push(e);
                    out_k.push(k);
                }
            }
            (None, None) => break,
        }
    }
    *entries = out_e;
    *keys = out_k;
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::VPath;

    fn e(w: &str, k: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(w).unwrap(),
            kind: k,
            size: None,
            mtime_ms: None,
        }
    }

    /// Pins `SortKey.nfc`'s contract (#94): `None` in the common case
    /// (ASCII, already-NFC, non-UTF-8) — zero persisted allocs — and `Some`
    /// ONLY when the NFC form differs from the raw bytes (e.g. NFD).
    #[test]
    fn sort_key_does_not_materialize_nfc_in_the_common_case() {
        // The q+U+0300 case is quick-check=Maybe but IS NFC (there is no
        // precomposed form): it kills the `is_nfc_quick(..) == Yes` mutant,
        // which would materialize every combining mark and silently regress
        // the zero-alloc case (encoding-auditor m1).
        for w in [
            "mem:///ascii.txt",
            "mem:///a%C3%B1o",
            "mem:///%FF%FE",
            "mem:///q%CC%80",
        ] {
            let k = sort_key(&e(w, EntryKind::File));
            assert_eq!(k.nfc, None, "{w}: nfc==raw bytes, must not allocate");
        }
        let nfd = sort_key(&e("mem:///an%CC%83o", EntryKind::File));
        assert_eq!(
            nfd.nfc.as_deref(),
            Some("año".as_bytes()),
            "NFD materializes its NFC form"
        );
        // Singleton U+212B (ANGSTROM SIGN) → U+00C5 "Å": NFC differs without
        // being the classic NFD case — must materialize.
        let singleton = sort_key(&e("mem:///%E2%84%AB", EntryKind::File));
        assert_eq!(
            singleton.nfc.as_deref(),
            Some("Å".as_bytes()),
            "singleton materializes its NFC form"
        );
    }

    /// Deterministic equivalence (no proptest as a dev-dep in this crate, see
    /// `grep proptest crates/norte-frontend/Cargo.toml`): two unsorted
    /// halves, each run through `sort_with_keys`, merged with `merge_keyed`,
    /// must match `sort_entries` over the total EXACTLY — covers dirs/files,
    /// NFD vs NFC, non-UTF-8 and key ties.
    #[test]
    fn merge_keyed_matches_sort_entries() {
        let left = vec![
            e("mem:///zeta", EntryKind::File),
            e("mem:///Adir", EntryKind::Dir),
            e("mem:///an%CC%83o", EntryKind::File), // NFD "año"
            e("mem:///%FF%FE", EntryKind::File),    // non-UTF-8
        ];
        let right = vec![
            e("mem:///a%C3%B1o2", EntryKind::File), // NFC "año2"
            e("mem:///Bdir", EntryKind::Dir),
            e("mem:///a%C3%B1o", EntryKind::File), // NFC "año" — ties in key with the NFD one above
            e("mem:///alpha", EntryKind::File),
        ];

        let (mut entries, mut keys) = sort_with_keys_spec(left.clone(), &SortSpec::default());
        let (batch_entries, batch_keys) = sort_with_keys_spec(right.clone(), &SortSpec::default());
        merge_keyed_spec(
            &mut entries,
            &mut keys,
            batch_entries,
            batch_keys,
            &SortSpec::default(),
        );

        let mut expected: Vec<Entry> = left.into_iter().chain(right).collect();
        sort_entries(&mut expected);

        assert_eq!(entries, expected, "merge_keyed ≡ sort_entries of the total");
        assert_eq!(keys.len(), entries.len());
    }

    #[test]
    fn after_click_same_column_reverses_direction() {
        let s = SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Asc,
            dirs_first: true,
        };
        let t = s.after_click(SortColumn::Size);
        assert_eq!(
            t,
            SortSpec {
                column: SortColumn::Size,
                dir: SortDir::Desc,
                dirs_first: true,
            }
        );
        // And the second click goes back to Asc.
        assert_eq!(t.after_click(SortColumn::Size).dir, SortDir::Asc);
    }

    #[test]
    fn after_click_new_column_asc_and_dirs_first_untouched() {
        let s = SortSpec {
            column: SortColumn::Name,
            dir: SortDir::Desc,
            dirs_first: false,
        };
        let t = s.after_click(SortColumn::Mtime);
        assert_eq!(
            t,
            SortSpec {
                column: SortColumn::Mtime,
                dir: SortDir::Asc,
                dirs_first: false,
            }
        );
    }
}

#[cfg(test)]
mod sort_spec_merge {
    use super::*;
    use norte_proto::attrs::AttrValue;
    use norte_proto::{EntryKind, VPath};

    fn e(name: &str, dir: bool, size: Option<u64>, mtime: Option<i64>) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(&format!("mem:///{name}")).unwrap(),
            kind: if dir { EntryKind::Dir } else { EntryKind::File },
            size,
            mtime_ms: mtime,
        }
    }

    fn all_specs() -> Vec<SortSpec> {
        let mut out = Vec::new();
        for column in [
            SortColumn::Name,
            SortColumn::Size,
            SortColumn::Mtime,
            SortColumn::Attr("posix.mode".to_owned()),
        ] {
            for dir in [SortDir::Asc, SortDir::Desc] {
                for dirs_first in [false, true] {
                    out.push(SortSpec {
                        column: column.clone(),
                        dir,
                        dirs_first,
                    });
                }
            }
        }
        out
    }

    /// #108 L7: incremental merge ≡ full sort under ALL 16 specs — property
    /// #54, generalized from the default to the whole space, over a corpus
    /// with missing values, ties, dirs and pre-1970 dates, split at every
    /// possible point.
    #[test]
    fn merge_matches_sort_under_all_specs() {
        let corpus = vec![
            e("b", false, Some(10), Some(5)),
            e("dir1", true, None, Some(-3)),
            e("a", false, Some(10), None),
            e("z", false, None, Some(5)),
            e("dir2", true, Some(4096), None),
            e("m", false, Some(1), Some(1_000)),
            e("a2", false, None, None),
        ];
        // The attribute column (ADR 0144) with values, ties, a foreign type
        // and an `Unknown`; the rest do not have it.
        let mut corpus = corpus;
        for (i, v) in [
            (0, AttrValue::Uint(0o644)),
            (1, AttrValue::Uint(0o755)),
            (2, AttrValue::Uint(0o644)),
            (3, AttrValue::Text("x".to_owned())),
            (5, AttrValue::Unknown),
        ] {
            corpus[i].attrs.insert("posix.mode".to_owned(), v);
        }
        for spec in all_specs() {
            for cut in 0..=corpus.len() {
                let (left, right) = corpus.split_at(cut);
                let mut total = corpus.clone();
                sort_entries_with(&mut total, &spec);

                let (mut entries, mut keys) = sort_with_keys_spec(left.to_vec(), &spec);
                let (be, bk) = sort_with_keys_spec(right.to_vec(), &spec);
                merge_keyed_spec(&mut entries, &mut keys, be, bk, &spec);
                let a: Vec<_> = total.iter().map(|x| x.path.clone()).collect();
                let b: Vec<_> = entries.iter().map(|x| x.path.clone()).collect();
                assert_eq!(a, b, "spec {spec:?} cut {cut}");
            }
        }
    }
}

#[cfg(test)]
mod sort_spec_tests {
    use super::*;
    use norte_proto::{EntryKind, VPath};

    fn e(name: &str, kind: EntryKind, size: Option<u64>, mtime: Option<i64>) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(&format!("mem:///{name}")).unwrap(),
            kind,
            size,
            mtime_ms: mtime,
        }
    }

    fn names(entries: &[Entry]) -> Vec<&[u8]> {
        entries
            .iter()
            .map(|e| e.path.file_name().map_or(&b""[..], |n| n.as_bytes()))
            .collect()
    }

    /// #108 L7: by size ASC — dirs first (always), None AT THE END,
    /// tie-break by name.
    #[test]
    fn size_asc_none_at_the_end_and_dirs_first() {
        let mut es = vec![
            e("g", EntryKind::File, Some(5), None),
            e("f-sin", EntryKind::File, None, None),
            e("a", EntryKind::File, Some(9), None),
            e("dir", EntryKind::Dir, None, None),
            e("b", EntryKind::File, Some(5), None),
        ];
        let spec = SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Asc,
            dirs_first: true,
        };
        sort_entries_with(&mut es, &spec);
        assert_eq!(
            names(&es),
            vec![
                b"dir".as_slice(), // dir group
                b"b",              // 5, name tie-break asc
                b"g",              // 5
                b"a",              // 9
                b"f-sin",          // None ALWAYS at the end
            ]
        );
    }

    /// #108 L7: DESC reverses ONLY the column — None still goes at the end,
    /// the tie-break is still by name ASC, the dir group is not reversed.
    #[test]
    fn size_desc_reverses_only_the_column() {
        let mut es = vec![
            e("g", EntryKind::File, Some(5), None),
            e("f-sin", EntryKind::File, None, None),
            e("a", EntryKind::File, Some(9), None),
            e("dir", EntryKind::Dir, None, None),
            e("b", EntryKind::File, Some(5), None),
        ];
        let spec = SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Desc,
            dirs_first: true,
        };
        sort_entries_with(&mut es, &spec);
        assert_eq!(
            names(&es),
            vec![
                b"dir".as_slice(),
                b"a",     // 9
                b"b",     // 5, name tie-break ASC even though the column is desc
                b"g",     // 5
                b"f-sin", // None at the end ALSO in desc
            ]
        );
    }

    /// #108 L7: mtime desc = "the newly downloaded on top", negative
    /// (pre-1970) values are valid.
    #[test]
    fn mtime_desc_with_pre_1970() {
        let mut es = vec![
            e("viejo", EntryKind::File, None, Some(-1000)),
            e("nuevo", EntryKind::File, None, Some(2_000_000)),
            e("sin", EntryKind::File, None, None),
            e("medio", EntryKind::File, None, Some(1_000)),
        ];
        let spec = SortSpec {
            column: SortColumn::Mtime,
            dir: SortDir::Desc,
            dirs_first: true,
        };
        sort_entries_with(&mut es, &spec);
        assert_eq!(
            names(&es),
            vec![b"nuevo".as_slice(), b"medio", b"viejo", b"sin"]
        );
    }

    /// The DEFAULT spec reproduces the exact historical order.
    #[test]
    fn spec_default_is_the_usual_order() {
        let mut a = vec![
            e("b", EntryKind::File, Some(1), None),
            e("dir", EntryKind::Dir, None, None),
            e("a", EntryKind::File, Some(2), None),
        ];
        let mut b = a.clone();
        sort_entries(&mut a);
        sort_entries_with(&mut b, &SortSpec::default());
        assert_eq!(a, b);
    }

    /// `dirs_first` = false: dirs compete like any other entry.
    #[test]
    fn without_dirs_first_there_is_no_group() {
        let mut es = vec![
            e("z-dir", EntryKind::Dir, None, None),
            e("a", EntryKind::File, None, None),
        ];
        let spec = SortSpec {
            column: SortColumn::Name,
            dir: SortDir::Asc,
            dirs_first: false,
        };
        sort_entries_with(&mut es, &spec);
        assert_eq!(names(&es), vec![b"a".as_slice(), b"z-dir"]);
    }
}

#[cfg(test)]
mod extension_tests {
    use super::*;

    fn e(name: &str, kind: EntryKind) -> Entry {
        raw(name.as_bytes(), kind)
    }

    fn raw(name: &[u8], kind: EntryKind) -> Entry {
        let seg = norte_proto::Segment::new(name.to_vec()).expect("segment");
        Entry {
            path: norte_proto::VPath::parse("file:///d")
                .expect("vpath")
                .join(seg),
            kind,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::new(),
        }
    }

    fn names(v: &[Entry]) -> Vec<String> {
        v.iter()
            .map(|e| String::from_utf8_lossy(name_bytes(e)).into_owned())
            .collect()
    }

    fn by_extension(dir: SortDir) -> SortSpec {
        SortSpec {
            column: SortColumn::Extension,
            dir,
            dirs_first: true,
        }
    }

    /// `.TXT` goes with `.txt`: grouping the two is the whole point of
    /// sorting by extension. Identity never folds (ADR 0051); ORDER does.
    #[test]
    fn extension_groups_without_distinguishing_case() {
        let mut v = vec![
            e("b.TXT", EntryKind::File),
            e("a.rs", EntryKind::File),
            e("c.txt", EntryKind::File),
        ];
        sort_entries_with(&mut v, &by_extension(SortDir::Asc));
        assert_eq!(names(&v), ["a.rs", "b.TXT", "c.txt"]);
    }

    /// What has no extension goes AT THE END in BOTH directions, like any
    /// missing value: a `desc` does not fill the pane's header with bare
    /// names. And a name that STARTS with a dot has no extension —
    /// `.bashrc` is a whole name—, nor does one that ends in a dot.
    #[test]
    fn what_has_no_extension_goes_at_the_end_in_both_directions() {
        for dir in [SortDir::Asc, SortDir::Desc] {
            let mut v = vec![
                e("readme", EntryKind::File),
                e(".bashrc", EntryKind::File),
                e("a.rs", EntryKind::File),
                e("dot.", EntryKind::File),
            ];
            sort_entries_with(&mut v, &by_extension(dir));
            assert_eq!(
                names(&v)[0],
                "a.rs",
                "the only one with an extension governs ({dir:?})"
            );
        }
    }

    /// A non-UTF-8 extension sorts the same and does not mutate a byte (hard
    /// rule 1).
    #[test]
    fn a_non_utf8_extension_sorts_without_breaking() {
        let mut v = vec![
            raw(b"raro\xff.zz", EntryKind::File),
            e("a.aa", EntryKind::File),
        ];
        sort_entries_with(&mut v, &by_extension(SortDir::Asc));
        assert_eq!(names(&v)[0], "a.aa", "aa < zz");
        assert_eq!(
            name_bytes(&v[1]),
            b"raro\xff.zz",
            "and the bytes come back intact"
        );
    }

    /// The directory group governs before the column, same as with any
    /// other: a dir has no extension and still comes out first.
    #[test]
    fn directories_still_come_first() {
        let mut v = vec![e("z.rs", EntryKind::File), e("dir", EntryKind::Dir)];
        sort_entries_with(&mut v, &by_extension(SortDir::Asc));
        assert_eq!(v[0].kind, EntryKind::Dir);
    }

    /// Extension tie: the name breaks it, ALWAYS ascending, like in the
    /// other columns.
    #[test]
    fn equal_extension_ties_break_by_name() {
        let mut v = vec![
            e("z.rs", EntryKind::File),
            e("a.rs", EntryKind::File),
            e("m.rs", EntryKind::File),
        ];
        sort_entries_with(&mut v, &by_extension(SortDir::Desc));
        assert_eq!(names(&v), ["a.rs", "m.rs", "z.rs"]);
    }
}

#[cfg(test)]
mod attr_tests {
    use super::*;
    use norte_proto::attrs::AttrValue;

    fn e(name: &str, kind: EntryKind, mode: Option<AttrValue>) -> Entry {
        let seg = norte_proto::Segment::new(name.as_bytes().to_vec()).expect("segment");
        let mut attrs = std::collections::BTreeMap::new();
        if let Some(v) = mode {
            attrs.insert("posix.mode".to_owned(), v);
        }
        Entry {
            path: norte_proto::VPath::parse("file:///d")
                .expect("vpath")
                .join(seg),
            kind,
            size: None,
            mtime_ms: None,
            attrs,
        }
    }

    fn names(v: &[Entry]) -> Vec<String> {
        v.iter()
            .map(|e| String::from_utf8_lossy(name_bytes(e)).into_owned())
            .collect()
    }

    fn by_mode(dir: SortDir) -> SortSpec {
        SortSpec {
            column: SortColumn::Attr("posix.mode".to_owned()),
            dir,
            dirs_first: true,
        }
    }

    /// Sorted by VALUE, not by its text: 0o100 (64) comes before 0o77 (63) as
    /// a string, and after as a number.
    #[test]
    fn sorts_by_numeric_value() {
        let mut v = vec![
            e("a", EntryKind::File, Some(AttrValue::Uint(0o100))),
            e("b", EntryKind::File, Some(AttrValue::Uint(0o77))),
            e("c", EntryKind::File, Some(AttrValue::Uint(0o644))),
        ];
        sort_entries_with(&mut v, &by_mode(SortDir::Asc));
        assert_eq!(names(&v), ["b", "a", "c"]);
        sort_entries_with(&mut v, &by_mode(SortDir::Desc));
        assert_eq!(names(&v), ["c", "a", "b"]);
    }

    /// What has no attribute, or has it as `Unknown`, goes at the end in
    /// BOTH directions: reversing must not push the gaps to the top.
    #[test]
    fn missing_and_unknown_go_at_the_end_in_both_directions() {
        for dir in [SortDir::Asc, SortDir::Desc] {
            let mut v = vec![
                e("sin", EntryKind::File, None),
                e("uno", EntryKind::File, Some(AttrValue::Uint(1))),
                e("raro", EntryKind::File, Some(AttrValue::Unknown)),
                e("dos", EntryKind::File, Some(AttrValue::Uint(2))),
            ];
            sort_entries_with(&mut v, &by_mode(dir));
            let n = names(&v);
            assert_eq!(&n[2..], ["raro", "sin"], "{dir:?}: gaps by name");
            let known: Vec<_> = n[..2].to_vec();
            let expected = match dir {
                SortDir::Asc => ["uno", "dos"],
                SortDir::Desc => ["dos", "uno"],
            };
            assert_eq!(known, expected, "{dir:?}");
        }
    }

    /// A provider that emits different types under the same id does not
    /// break the total order: it groups by type (bool < number < date <
    /// text < bytes) and compares within each group; `Int` and `Uint` are
    /// the same group.
    #[test]
    fn mixed_types_give_a_total_order() {
        let mut v = vec![
            e("t", EntryKind::File, Some(AttrValue::Text("a".to_owned()))),
            e("u", EntryKind::File, Some(AttrValue::Uint(3))),
            e("i", EntryKind::File, Some(AttrValue::Int(-1))),
            e("b", EntryKind::File, Some(AttrValue::Bool(true))),
            e("f", EntryKind::File, Some(AttrValue::TimeMs(0))),
            e("x", EntryKind::File, Some(AttrValue::Bytes(vec![0xff]))),
        ];
        sort_entries_with(&mut v, &by_mode(SortDir::Asc));
        assert_eq!(names(&v), ["b", "i", "u", "f", "t", "x"]);
    }

    /// Directories still come first and, at equal value, the name breaks the
    /// tie in ascending order, like in the other columns.
    #[test]
    fn dirs_first_and_tie_by_name() {
        let mut v = vec![
            e("z", EntryKind::File, Some(AttrValue::Uint(1))),
            e("d", EntryKind::Dir, Some(AttrValue::Uint(9))),
            e("a", EntryKind::File, Some(AttrValue::Uint(1))),
        ];
        sort_entries_with(&mut v, &by_mode(SortDir::Desc));
        assert_eq!(names(&v), ["d", "a", "z"]);
    }
}
