//! The pairing key: which name on one side is measured against which name
//! on the other, and which two names on the SAME side collapse into one.
//!
//! The key exists ONLY to pair. It is never painted, never operated on,
//! never replaces the name's bytes: each [`Entry`] travels in its row with
//! its original bytes intact (hard rule 1). There is not a single lossy
//! conversion here — a name that is not UTF-8 is not text, and it is paired
//! by its bytes.
//!
//! The folding itself — `fold_delta`'s delta, the fold-and-THEN-normalize
//! order, and what happens to a name that is not UTF-8 — lives in
//! [`norte_encoding::name_key`] (ADR 0051, #151): this module was once a
//! second copy of that function, and the copy did manage to cause trouble
//! once (`key_for` shipped the pre-#129 key for a whole release cycle, with
//! nothing to compare it against). What this module adds on top is what
//! belongs to the COMPARISON and not to the text: [`Sides`] decides whether
//! the pair folds based on the two sides' [`Capabilities`], and [`SideIndex`]
//! indexes a listing by its key and separates what pairs from what collides.
use std::borrow::Cow;
use std::collections::BTreeMap;

use norte_encoding::FoldMode;
use norte_proto::Segment;
use norte_proto::methods::{CompareReason, PairTransform};
use norte_vfs::{Capabilities, Entry};

/// How THE PAIR of sides folds, which is not the same as how each one folds
/// on its own.
///
/// Case folding is a property of the pair, not of one side: it is enough
/// for one of the two to be case-insensitive for the whole comparison to
/// have to fold, because that side cannot hold both spellings. And the same
/// with the strength of the fold: if one side **expands** on folding
/// (ext4/f2fs `+F`, #145), there `straße.txt` and `strasse.txt` are a single
/// file, so the whole pair has to expand, or the comparison would say that
/// two names the destination cannot hold at once do not collide.
///
/// ```
/// use norte_compare::Sides;
/// use norte_vfs::{Capabilities, CapabilityFlags};
///
/// let ext4 = Capabilities { flags: CapabilityFlags::CASE_SENSITIVE, max_path: None };
/// let apfs = Capabilities { flags: CapabilityFlags::CASE_PRESERVING, max_path: None };
/// assert!(!Sides::from_capabilities(ext4, ext4).folds_case());
/// assert!(Sides::from_capabilities(ext4, apfs).folds_case(), "ONE side is enough");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sides {
    left: FoldMode,
    right: FoldMode,
}

impl Sides {
    /// From each side's fold mode.
    ///
    /// ```
    /// use norte_compare::Sides;
    /// use norte_encoding::FoldMode;
    /// assert!(Sides::new(FoldMode::None, FoldMode::Simple).folds_case());
    /// assert!(!Sides::new(FoldMode::None, FoldMode::None).folds_case());
    /// ```
    #[must_use]
    pub fn new(left: FoldMode, right: FoldMode) -> Self {
        Self { left, right }
    }

    /// From the [`Capabilities`] the two sides answer **for their roots**
    /// (`Provider::capabilities_at`, ADR 0054 — not `capabilities()`, which
    /// answers for the provider's mount and not for what is being compared).
    #[must_use]
    pub fn from_capabilities(left: Capabilities, right: Capabilities) -> Self {
        Self::new(Self::mode_of(left), Self::mode_of(right))
    }

    /// The fold mode a LOCATION's capabilities declare.
    ///
    /// It is public because it is the ONLY copy: `norte_core::rename::plan::NameCaps`
    /// asks the same question over the same flags and calls here instead of
    /// transcribing it. Transcribing this kind of mapping is exactly what
    /// #151 cost (the fold key copied, diverging for a whole release cycle
    /// with nothing comparing them).
    ///
    /// ```
    /// use norte_compare::Sides;
    /// use norte_encoding::FoldMode;
    /// use norte_vfs::{Capabilities, CapabilityFlags};
    /// let ext4_f = Capabilities {
    ///     flags: CapabilityFlags::CASE_PRESERVING | CapabilityFlags::FULL_FOLD,
    ///     max_path: None,
    /// };
    /// assert_eq!(Sides::mode_of(ext4_f), FoldMode::Full);
    /// ```
    #[must_use]
    pub fn mode_of(c: Capabilities) -> FoldMode {
        // The rule lives in `norte-vfs`, which owns the contract of the
        // `Provider` whose flags are being read. Here it is only forwarded:
        // three layers that cannot see each other ask it —this engine, the
        // core, and the window (#268)— and three copies of three lines is
        // how they would drift apart.
        norte_vfs::fold_mode_of(c)
    }

    /// Both sides are case-sensitive (ext4 against ext4): NO folding.
    #[must_use]
    pub fn both_case_sensitive() -> Self {
        Self::new(FoldMode::None, FoldMode::None)
    }

    /// The left side is case-insensitive: it folds (simple).
    #[must_use]
    pub fn left_case_insensitive() -> Self {
        Self::new(FoldMode::Simple, FoldMode::None)
    }

    /// The right side is case-insensitive: it folds (simple).
    #[must_use]
    pub fn right_case_insensitive() -> Self {
        Self::new(FoldMode::None, FoldMode::Simple)
    }

    /// How THE PAIR folds: the stronger mode of the two sides.
    ///
    /// "Stronger" is the order in which each mode groups more names
    /// together — `None` < `Simple` < `Full`— and the criterion is the usual
    /// one: the side that cannot hold two spellings decides for both.
    #[must_use]
    pub fn fold(self) -> FoldMode {
        match (self.left, self.right) {
            (FoldMode::Full, _) | (_, FoldMode::Full) => FoldMode::Full,
            (FoldMode::Simple, _) | (_, FoldMode::Simple) => FoldMode::Simple,
            _ => FoldMode::None,
        }
    }

    /// Does this pairing fold case? (Whether simple or full.)
    #[must_use]
    pub fn folds_case(self) -> bool {
        !matches!(self.fold(), FoldMode::None)
    }
}

/// The key two names pair under.
///
/// Borrows the name's bytes while the transformation changes nothing — the
/// overwhelmingly common case (lowercase ASCII, already-NFC, non-UTF8) —
/// and only materializes when it does change.
///
/// `PartialEq`/`Ord`/`Hash` go by the BYTES, so a borrowed key and an owned
/// one with the same content are the same key.
///
/// ```
/// use norte_compare::{Sides, key_for};
/// let k = key_for(b"README", Sides::right_case_insensitive());
/// assert_eq!(k.as_bytes(), b"readme");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PairKey<'a>(Cow<'a, [u8]>);

impl PairKey<'_> {
    /// The key's bytes. They are NOT the name: they are neither painted nor
    /// operated on.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Releases the borrow on the name, copying if needed.
    #[must_use]
    pub fn into_owned(self) -> PairKey<'static> {
        PairKey(Cow::Owned(self.0.into_owned()))
    }
}

/// The pairing key of a name under some [`Sides`].
///
/// Delegates to [`norte_encoding::name_key`] (ADR 0051, #151): this function
/// ONLY forwards the pair's [`FoldMode`] ([`Sides::fold`]), which since
/// ADR 0054 can be [`FoldMode::Full`] — turned on by a side declaring
/// `FULL_FOLD` for its root, never by an assumption about the filesystem.
///
/// The input bytes are not touched: what comes out is a key, and the name
/// stays the name. A name that is NOT UTF-8 is paired by its bytes; see
/// [`norte_encoding::name_key`]'s rustdoc for what happens with an invalid
/// byte that is not the WHOLE name (#154) and why a Shift-JIS trailing byte
/// is never folded as if it were ASCII.
///
/// ```
/// use norte_compare::{Sides, key_for};
/// // NFD and NFC of the same name pair...
/// let sensitive = Sides::both_case_sensitive();
/// assert_eq!(key_for("café".as_bytes(), sensitive), key_for(b"cafe\xcc\x81", sensitive));
/// // ...and bytes that are not text pass through as is.
/// assert_eq!(key_for(b"roto\xff\xfe", sensitive).as_bytes(), b"roto\xff\xfe");
/// ```
#[must_use]
pub fn key_for(name: &[u8], sides: Sides) -> PairKey<'_> {
    PairKey(norte_encoding::name_key(name, sides.fold()))
}

/// Under which transformation two names paired, when they are NOT the same
/// bytes (#152).
///
/// **PRECONDITION: the two names paired** — they are the two halves of a
/// pair that [`index_side`] and the merge-join put together. With two names
/// that do not pair, the answer is `None`, same as with two identical names:
/// there is no transformation to name, and saying one would be inventing it.
///
/// The order of the questions IS the contract, and the singleton wins:
///
/// 1. **Equal bytes** → `None`. The ordinary case, and why
///    [`CompareRow::paired_under`](norte_proto::methods::CompareRow::paired_under)
///    is omitted on the wire.
/// 2. **Either of the two names carries a character with a singleton
///    decomposition** ([`norte_encoding::has_canonical_singleton`]) →
///    [`PairTransform::NormalizationSingleton`], even if it also folds case.
///    It is the only one of the three that can be joining two DIFFERENT
///    files, so a consumer that only looks at that variant has to see it.
/// 3. **They pair WITHOUT folding** → [`PairTransform::Normalization`]: they
///    are the same text in NFC and in NFD.
/// 4. **Otherwise, folding was needed** → [`PairTransform::CaseFold`].
///
/// Step 2 errs to the safe side on purpose: it looks at whether the name
/// CONTAINS a singleton, not whether that character is exactly the one that
/// separates the two. A NFC/NFD pair that also carried an identical OHM SIGN
/// on both sides comes out marked as singleton. The character set is tiny
/// and none of them appears in an ordinary name, so that false positive
/// costs one extra warning — and the false negative would cost a file.
///
/// ```
/// use norte_compare::pair_transform;
/// use norte_proto::methods::PairTransform;
///
/// // The ordinary case: the same bytes, nothing to say.
/// assert_eq!(pair_transform(b"a.txt", b"a.txt"), None);
/// // NFC against NFD of the same text.
/// assert_eq!(
///     pair_transform("café".as_bytes(), b"cafe\xcc\x81"),
///     Some(PairTransform::Normalization)
/// );
/// // Case: they pair because one side cannot hold both spellings.
/// assert_eq!(pair_transform(b"README", b"readme"), Some(PairTransform::CaseFold));
/// // #152: U+212A KELVIN SIGN against the ASCII `K` — two files that
/// // coexist on ext4 and that NFC joins.
/// assert_eq!(
///     pair_transform("\u{212a}.txt".as_bytes(), b"K.txt"),
///     Some(PairTransform::NormalizationSingleton)
/// );
/// // Two names that do not pair have no transformation to name.
/// assert_eq!(pair_transform(b"a.txt", b"b.txt"), None);
/// ```
#[must_use]
pub fn pair_transform(left: &[u8], right: &[u8]) -> Option<PairTransform> {
    if left == right {
        return None;
    }
    let unfolded = Sides::both_case_sensitive();
    let normalizes = key_for(left, unfolded) == key_for(right, unfolded);
    // Both folds are asked about, and the difference between them is not a
    // nuance: a pair that only pairs by EXPANDING (`straße`/`strasse` on an
    // ext4 `+F`, #145) does not name the same text — they are two texts that
    // volume cannot hold at once, and the other side CAN have both files,
    // distinct. Answering `CaseFold` there would be saying they are the same
    // name, and whoever reads that answer (`names_one_text`, and with it
    // ADR 0053's gate in `norte-sync`) would overwrite over it.
    let simple = Sides::new(FoldMode::Simple, FoldMode::None);
    let full = Sides::new(FoldMode::Full, FoldMode::None);
    let folds_simple = key_for(left, simple) == key_for(right, simple);
    let folds_full = folds_simple || key_for(left, full) == key_for(right, full);
    if !normalizes && !folds_full {
        return None;
    }
    if norte_encoding::has_canonical_singleton(left)
        || norte_encoding::has_canonical_singleton(right)
    {
        return Some(PairTransform::NormalizationSingleton);
    }
    if normalizes {
        return Some(PairTransform::Normalization);
    }
    Some(if folds_simple {
        PairTransform::CaseFold
    } else {
        PairTransform::FullFold
    })
}

/// What [`index_side`] needs from an entry: its name's BYTES.
///
/// Exists so the pairing tests can talk about loose names (`&[u8]`) and the
/// walk about [`Entry`], without two copies of the same logic.
pub trait PairName {
    /// The name's bytes, exactly as the provider gave them.
    fn pair_name(&self) -> &[u8];
}

impl PairName for &[u8] {
    fn pair_name(&self) -> &[u8] {
        self
    }
}

impl PairName for Vec<u8> {
    fn pair_name(&self) -> &[u8] {
        self
    }
}

impl PairName for Entry {
    /// The `VPath`'s last segment, in bytes. The root — which has no name —
    /// pairs by the empty name; the walk never puts it into a listing.
    fn pair_name(&self) -> &[u8] {
        self.path.file_name().map_or(&[][..], Segment::as_bytes)
    }
}

/// One side ready to pair: its entries indexed by their key, and which
/// entries collide with which.
///
/// Borrows the listing; it neither copies nor reorders it.
#[derive(Debug, Clone)]
pub struct SideIndex<'a, T> {
    entries: &'a [T],
    /// Key → indices into `entries`, in listing order. `BTreeMap` because the
    /// merge-join wants the keys SORTED and a provider's listing guarantees
    /// no order at all.
    by_key: BTreeMap<PairKey<'a>, Vec<usize>>,
    /// Reason per entry. `None` = this entry does not collide with any
    /// other.
    reasons: Vec<Option<CompareReason>>,
}

/// Indexes ONE side by its pairing key.
///
/// Entries that collide are **never paired and never deduplicated**: ALL of
/// them are kept, one by one, because each one is a real file that a later
/// synchronization could write over. Losing a name here is losing exactly
/// that file. They come out via [`SideIndex::collisions`], one per entry,
/// which is the normative shape of a
/// [`CompareVerdict::Ambiguous`](norte_proto::methods::CompareVerdict::Ambiguous)
/// row: one row per involved entry, the other side as `None`.
///
/// ```
/// use norte_compare::{CompareReason, Sides, index_side};
/// let listing: Vec<&[u8]> = vec![b"README", b"readme", b"NOTES"];
/// let side = index_side(&listing, Sides::right_case_insensitive());
///
/// // Two collisions: TWO rows, neither deduplicated.
/// let colliding: Vec<_> = side.collisions().collect();
/// assert_eq!(colliding.len(), 2);
/// assert!(colliding.iter().all(|(_, r)| *r == CompareReason::CaseFold));
///
/// // And what does not collide does pair.
/// assert_eq!(side.unique().count(), 1);
/// ```
#[must_use]
pub fn index_side<T: PairName>(entries: &[T], sides: Sides) -> SideIndex<'_, T> {
    let mut by_key: BTreeMap<PairKey<'_>, Vec<usize>> = BTreeMap::new();
    for (i, entry) in entries.iter().enumerate() {
        by_key
            .entry(key_for(entry.pair_name(), sides))
            .or_default()
            .push(i);
    }

    let mut reasons = vec![None; entries.len()];
    // The reason for EACH colliding entry: does the collision survive
    // without folding? Then normalization caused it. Does it go away
    // without folding? Then folding caused it. Computed per entry and not
    // per group because a group of three can have a different cause for
    // each pair.
    let unfolded = Sides::both_case_sensitive();
    for idxs in by_key.values() {
        if idxs.len() < 2 {
            continue;
        }
        let raw: Vec<PairKey<'_>> = idxs
            .iter()
            .map(|&i| key_for(entries[i].pair_name(), unfolded))
            .collect();
        for (pos, &i) in idxs.iter().enumerate() {
            let twin = raw
                .iter()
                .enumerate()
                .any(|(other, k)| other != pos && *k == raw[pos]);
            reasons[i] = Some(if twin {
                CompareReason::Normalization
            } else {
                CompareReason::CaseFold
            });
        }
    }

    SideIndex {
        entries,
        by_key,
        reasons,
    }
}

impl<'a, T: PairName> SideIndex<'a, T> {
    /// The listing that was indexed, in its original order.
    #[must_use]
    pub fn entries(&self) -> &'a [T] {
        self.entries
    }

    /// How many entries the listing carries (colliding ones included).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Empty listing?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The PAIRABLE entries, by key and in key order: the ones that do not
    /// share theirs with any other on their side.
    ///
    /// It is half of the merge-join. What is missing —the collided ones—
    /// comes out via [`SideIndex::collisions`] and pairs with nothing.
    pub fn unique(&self) -> impl Iterator<Item = (&PairKey<'a>, &'a T)> {
        self.by_key.iter().filter_map(|(k, idxs)| match idxs[..] {
            [i] => Some((k, &self.entries[i])),
            _ => None,
        })
    }

    /// Looks up a pairable entry by its key. `None` if it is not there or if
    /// its key collides (an ambiguous key does NOT pair).
    #[must_use]
    pub fn get(&self, key: &PairKey<'a>) -> Option<&'a T> {
        match self.by_key.get(key)?[..] {
            [i] => Some(&self.entries[i]),
            _ => None,
        }
    }

    /// ALL collided entries, in listing order, with their reason. One per
    /// entry: two names that collapse are TWO, never a merge and never a
    /// deduplication.
    pub fn collisions(&self) -> impl Iterator<Item = (&'a T, CompareReason)> {
        self.entries
            .iter()
            .zip(self.reasons.iter())
            .filter_map(|(e, r)| r.map(|r| (e, r)))
    }

    /// Why the entry whose name bytes are `name` collides, or `None` if it
    /// does not collide (or is not in the listing).
    ///
    /// Walks the listing: it is a test and diagnostic query. The walk uses
    /// [`SideIndex::collisions`], which runs in one pass.
    #[must_use]
    pub fn ambiguous_reason(&self, name: &[u8]) -> Option<CompareReason> {
        let i = self.entries.iter().position(|e| e.pair_name() == name)?;
        self.reasons[i]
    }
}

#[cfg(test)]
mod tests {
    use norte_vfs::CapabilityFlags;

    use super::*;

    /// The bytes of a fixture from `norte-testkit`'s canonical corpus.
    ///
    /// Hostile names are taken from there and never written by hand: half of
    /// this module tests things that only show up with the exact byte, and
    /// the corpus already carries —with its reason written down— the pairs
    /// that cost #129.
    fn corpus(id: &str) -> Vec<u8> {
        norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} in the corpus"))
            .bytes
    }

    /// #152 in one line: the corpus's two names pair —the key is the same,
    /// without case folding— and they are NOT the same text.
    #[test]
    fn the_nfc_singleton_pairs_two_different_files() {
        let kelvin = corpus("singleton_kelvin_sign");
        let ascii = corpus("ascii_capital_k");
        let sensitive = Sides::both_case_sensitive();
        assert_ne!(kelvin, ascii, "they are two files, and coexist on ext4");
        assert_eq!(
            key_for(&kelvin, sensitive),
            key_for(&ascii, sensitive),
            "and they still pair: NFC is not injective"
        );
        assert_eq!(
            pair_transform(&kelvin, &ascii),
            Some(PairTransform::NormalizationSingleton),
            "and the row has to be able to say so"
        );
    }

    /// Every twin pair in the corpus is classified as what the corpus says
    /// it is. It is the crossing that keeps the two lists —the wire's
    /// vocabulary and the fixture index— from drifting apart with nothing
    /// to warn about it.
    ///
    /// The FULL fold pair comes in from ADR 0054 with its OWN variant: a
    /// side that declares `FULL_FOLD` for its root pairs it, and the answer
    /// says it was the FULL fold — which is what lets `names_one_text`
    /// answer `false` about it.
    #[test]
    fn the_corpus_twins_are_classified_as_the_corpus_says() {
        use norte_testkit::corpus::TwinKind;
        for twin in norte_testkit::corpus::spelling_twins() {
            let left = corpus(twin.left);
            let right = corpus(twin.right);
            let expected = match twin.kind {
                TwinKind::Normalization => Some(PairTransform::Normalization),
                TwinKind::CaseFold => Some(PairTransform::CaseFold),
                // The FULL fold has its OWN variant (0.45.0): it joins two
                // names that can be two files, so it cannot answer the same
                // as the simple fold, which does name a single text. Before
                // ADR 0054 this was `None` because the engine did not know
                // how to expand in any case.
                TwinKind::CaseFoldFull => Some(PairTransform::FullFold),
                TwinKind::NormalizationSingleton => Some(PairTransform::NormalizationSingleton),
            };
            assert_eq!(
                pair_transform(&left, &right),
                expected,
                "[{} / {}] {:?}",
                twin.left,
                twin.right,
                twin.kind
            );
        }
    }

    /// #145: on a directory that folds FULLY (ext4/f2fs `+F`) `straße.txt`
    /// and `strasse.txt` are ONE file, and the key has to say the same —
    /// while on one that folds simply (APFS, NTFS) they are still two.
    #[test]
    fn a_side_that_expands_makes_the_pair_expand() {
        let zett = corpus("ext4_full_fold_es_zett");
        let ss = corpus("ext4_full_fold_ss");

        let ext4_f = Capabilities {
            flags: CapabilityFlags::CASE_PRESERVING | CapabilityFlags::FULL_FOLD,
            max_path: None,
        };
        let ext4 = Capabilities {
            flags: CapabilityFlags::CASE_SENSITIVE,
            max_path: None,
        };
        let apfs = Capabilities {
            flags: CapabilityFlags::CASE_PRESERVING,
            max_path: None,
        };

        let with_full_fold = Sides::from_capabilities(ext4, ext4_f);
        assert_eq!(
            key_for(&zett, with_full_fold),
            key_for(&ss, with_full_fold),
            "ONE side expanding is enough"
        );

        let without_full_fold = Sides::from_capabilities(ext4, apfs);
        assert_ne!(
            key_for(&zett, without_full_fold),
            key_for(&ss, without_full_fold),
            "the simple fold does not expand"
        );
    }

    /// The pair's mode is the STRONGER of the two sides, and `folds_case`
    /// still means what it meant.
    #[test]
    fn the_pairs_mode_is_the_stronger_of_the_two() {
        use norte_encoding::FoldMode;
        assert_eq!(
            Sides::new(FoldMode::None, FoldMode::None).fold(),
            FoldMode::None
        );
        assert_eq!(
            Sides::new(FoldMode::None, FoldMode::Simple).fold(),
            FoldMode::Simple
        );
        assert_eq!(
            Sides::new(FoldMode::Simple, FoldMode::Full).fold(),
            FoldMode::Full
        );
        assert!(!Sides::new(FoldMode::None, FoldMode::None).folds_case());
        assert!(Sides::new(FoldMode::Full, FoldMode::None).folds_case());
    }

    /// Two names that do NOT pair carry no transformation to name, and
    /// answering one would be worse than staying silent: whoever reads it
    /// will believe the pair exists.
    #[test]
    fn two_names_that_do_not_pair_carry_no_transformation() {
        assert_eq!(pair_transform(b"a.txt", b"b.txt"), None);
        assert_eq!(pair_transform(b"a.txt", b"a.txt"), None, "same bytes");
        // Not even when one of the two carries a singleton: the singleton
        // wins AMONG the three answers, not over the question of whether
        // they pair.
        let kelvin = corpus("singleton_kelvin_sign");
        assert_eq!(pair_transform(&kelvin, b"otra.txt"), None);
    }

    /// macOS hands out NFD, Linux NFC. The same file copied between them must
    /// pair, and BOTH original byte strings must survive for display — the key
    /// is for pairing and nothing else (rule 1).
    #[test]
    fn nfd_and_nfc_of_one_name_share_a_key() {
        let nfc = "café".as_bytes(); // e-acute as one code point
        let nfd = b"cafe\xcc\x81"; // e + combining acute
        let sensitive = Sides::both_case_sensitive();
        assert_eq!(key_for(nfc, sensitive), key_for(nfd, sensitive));
    }

    /// Bytes that are not UTF-8 are not text, cannot be normalised, and must
    /// pass through untouched rather than through a lossy conversion.
    #[test]
    fn non_utf8_names_pass_through_raw() {
        let raw = b"broken\xff\xfename";
        assert_eq!(key_for(raw, Sides::both_case_sensitive()).as_bytes(), raw);
    }

    /// Case folding is decided by the PAIR, not by one side: a
    /// case-insensitive side cannot hold both spellings, so pairing against it
    /// must fold even when the other side is ext4.
    #[test]
    fn one_case_insensitive_side_folds_the_pairing() {
        let both = Sides::both_case_sensitive();
        assert_ne!(key_for(b"README", both), key_for(b"readme", both));
        let mixed = Sides::right_case_insensitive();
        assert_eq!(key_for(b"README", mixed), key_for(b"readme", mixed));
    }

    /// Two entries on ONE side collapsing to one key is the collision a later
    /// synchronisation has to see BEFORE it writes. They are reported, never
    /// paired, and never silently deduplicated.
    #[test]
    fn same_side_collision_is_reported_with_its_reason() {
        let names: Vec<&[u8]> = vec![b"README", b"readme", b"NOTES"];
        let folded = index_side(&names, Sides::right_case_insensitive());
        assert_eq!(
            folded.ambiguous_reason(b"README"),
            Some(CompareReason::CaseFold)
        );
        assert_eq!(
            folded.ambiguous_reason(b"readme"),
            Some(CompareReason::CaseFold)
        );
        assert_eq!(folded.ambiguous_reason(b"NOTES"), None);

        let nfd: Vec<&[u8]> = vec!["café".as_bytes(), b"cafe\xcc\x81"];
        let normalised = index_side(&nfd, Sides::both_case_sensitive());
        assert_eq!(
            normalised.ambiguous_reason("café".as_bytes()),
            Some(CompareReason::Normalization)
        );
    }

    // ---- what the four above do not fix ----

    /// The key never mutates the INPUT bytes (rule 1): `key_for` borrows or
    /// copies to build the key, but `name` stays what it was. This is what
    /// makes pairing by key and painting by bytes legitimate.
    ///
    /// The KEY itself, on the other hand, does fold and normalize the valid
    /// prefix — `CAFE`+U+0301 is real UTF-8 text, and the `\xff` that
    /// follows does not invalidate it (#154): before the fix, a single
    /// broken byte at the end disabled folding for the whole rest of it.
    #[test]
    fn the_key_does_not_touch_the_names_bytes() {
        let name = b"CAFE\xcc\x81\xff";
        let key = key_for(name, Sides::right_case_insensitive());
        assert_eq!(
            key.as_bytes(),
            "café"
                .as_bytes()
                .iter()
                .chain(b"\xff")
                .copied()
                .collect::<Vec<u8>>(),
            "the valid prefix folds and normalizes; the broken byte passes through as is",
        );
        assert_eq!(
            name, b"CAFE\xcc\x81\xff",
            "the INPUT name has not been touched"
        );
    }

    /// A name that is not UTF-8 in ANY prefix does not fold, not even in
    /// ASCII — `ROTO\xff` does fold since #154, because `ROTO` is a valid
    /// prefix; see `the_key_does_not_touch_the_names_bytes` for that half.
    /// This test is the other one: when there is not even one byte of valid
    /// prefix, folding would be the bug #129 closed.
    ///
    /// It looks harmless and it is not: in legacy double-byte encodings the
    /// trailing byte lands where `A`–`Z` live. `shift_jis_tesuto` (テスト)
    /// is `83 65 83 58 83 67` and that `58` is an `X`; folding it turns ス
    /// into ベ, which is a different character. And
    /// `norte_core::rename::plan::name_key` does not fold it either: the two
    /// answers to "do these two names collide?" have to be the same (#151).
    #[test]
    fn non_utf8_bytes_are_not_folded() {
        let mixed = Sides::left_case_insensitive();
        // Valid prefix: it folds. See #154 — a broken byte at the end no
        // longer disables folding for the text that IS valid.
        assert_eq!(key_for(b"ROTO\xff", mixed), key_for(b"roto\xff", mixed));

        let tesuto = corpus("shift_jis_tesuto");
        assert_eq!(
            key_for(&tesuto, mixed).as_bytes(),
            &tesuto[..],
            "ス's trailing byte `58` is an `X` in ASCII"
        );

        // And the pair that really proves it: ア and ヂ differ only in their
        // trailing byte, `41` against `61`. Folding would declare them the
        // same file.
        assert_ne!(key_for(b"\x83\x41", mixed), key_for(b"\x83\x61", mixed));
    }

    /// Folding is CASE folding, not `to_lowercase`, and the corpus already
    /// carried the pairs that tell them apart (#129, C2–C5 review).
    ///
    /// Each pair is ONE file on APFS, NTFS and on an ext4 `+F` directory.
    /// With plain `to_lowercase` two keys came out — i.e. two `OnlyLeft`/
    /// `OnlyRight` rows that a synchronization plan would copy one on top of
    /// the other.
    #[test]
    fn folding_is_case_folding_and_not_lowercase_mapping() {
        let mixed = Sides::right_case_insensitive();
        for (left, right) in [
            // ΟΔΟΣ / οδοσ: `str::to_lowercase` applies Final_Sigma and
            // produces ς.
            ("greek_uppercase_final_sigma", "greek_medial_sigma_twin"),
            // µm.txt (U+00B5) / μm.txt (U+03BC): Unicode already calls the
            // micro sign lowercase, so `to_lowercase` does not move it.
            ("micro_sign_mu", "greek_mu_twin"),
            // ﬅ.txt / ﬆ.txt: the only ligature with a simple fold.
            ("ligature_long_st", "ligature_st"),
            // J+◌̌ / ǰ: folding RECOMPOSES, so NFC comes afterward.
            (
                "nfd_uppercase_composed_only_lowercase",
                "precomposed_lowercase_j_caron",
            ),
        ] {
            let (a, b) = (corpus(left), corpus(right));
            assert_eq!(key_for(&a, mixed), key_for(&b, mixed), "{left}");
        }

        // And the ACCEPTED gap, which still is one: `ß` only has a FULL
        // fold (to `ss`), which expands, and this key is `char → char`. On
        // APFS/NTFS they are two files and here too; on ext4 `+F` they are
        // not, and that is #145.
        let (zett, ss) = (
            corpus("ext4_full_fold_es_zett"),
            corpus("ext4_full_fold_ss"),
        );
        assert_ne!(key_for(&zett, mixed), key_for(&ss, mixed), "#145");
    }

    /// Folding and normalizing COMMUTE in the key: `É` (NFC) and `E`+`◌́`
    /// (NFD) land in the same place, folding or not.
    #[test]
    fn folding_and_nfc_compose_in_both_orders() {
        let mixed = Sides::right_case_insensitive();
        let nfc_upper = "CAFÉ".as_bytes();
        let nfd_lower = b"cafe\xcc\x81";
        assert_eq!(key_for(nfc_upper, mixed), key_for(nfd_lower, mixed));
        // Without folding they do NOT pair: the uppercase is a real
        // difference on ext4.
        let sensitive = Sides::both_case_sensitive();
        assert_ne!(key_for(nfc_upper, sensitive), key_for(nfd_lower, sensitive));
    }

    /// Unicode folding does not stop at ASCII.
    #[test]
    fn folding_covers_more_than_ascii() {
        let mixed = Sides::left_case_insensitive();
        assert_eq!(
            key_for("AÑO".as_bytes(), mixed),
            key_for("año".as_bytes(), mixed)
        );
        // Titlecase: `char::is_uppercase` would say no, and it does fold.
        assert_eq!(
            key_for("ǅ".as_bytes(), mixed),
            key_for("ǆ".as_bytes(), mixed)
        );
    }

    /// A collided entry does NOT pair: neither via `unique`, nor via `get`.
    /// Pairing it would be blindly choosing which of the two files is "the"
    /// match.
    #[test]
    fn a_collided_key_pairs_with_nobody() {
        let names: Vec<&[u8]> = vec![b"README", b"readme", b"NOTES"];
        let side = index_side(&names, Sides::right_case_insensitive());

        let pairable: Vec<&[u8]> = side.unique().map(|(_, e)| *e).collect();
        assert_eq!(pairable, vec![&b"NOTES"[..]]);
        assert!(
            side.get(&key_for(b"readme", Sides::both_case_sensitive()))
                .is_none()
        );
        assert!(
            side.get(&key_for(b"NOTES", Sides::right_case_insensitive()))
                .is_some()
        );
    }

    /// Three names that collapse are THREE rows. None is merged and none is
    /// lost: each is a file that spec 2 could write to.
    #[test]
    fn each_collided_entry_comes_out_once() {
        let names: Vec<&[u8]> = vec![b"A", b"a", b"A", b"b"];
        let side = index_side(&names, Sides::right_case_insensitive());
        let colliding: Vec<&[u8]> = side.collisions().map(|(e, _)| *e).collect();
        assert_eq!(colliding, vec![&b"A"[..], &b"a"[..], &b"A"[..]]);
        assert_eq!(side.unique().count(), 1, "only `b` pairs");
        assert_eq!(side.len(), 4, "the listing has not been deduplicated");
    }

    /// The reason is PER ENTRY, not per group: in a mixed group, whoever has
    /// a twin by normalization says `Normalization` and whoever only
    /// collapsed by folding says `CaseFold`.
    #[test]
    fn the_reason_is_given_by_the_transformation_that_collapsed_that_entry() {
        let composed = "café".as_bytes();
        let decomposed = b"cafe\xcc\x81";
        let uppercase = "CAFÉ".as_bytes();
        let names: Vec<&[u8]> = vec![composed, decomposed, uppercase];
        let side = index_side(&names, Sides::right_case_insensitive());

        assert_eq!(
            side.ambiguous_reason(composed),
            Some(CompareReason::Normalization),
            "has a twin without folding"
        );
        assert_eq!(
            side.ambiguous_reason(decomposed),
            Some(CompareReason::Normalization)
        );
        assert_eq!(
            side.ambiguous_reason(uppercase),
            Some(CompareReason::CaseFold),
            "without folding it collided with nobody"
        );
    }

    /// Without folding there is no `CaseFold`: two different spellings are
    /// two different files and each pairs on its own.
    #[test]
    fn without_folding_the_two_spellings_are_two_entries() {
        let names: Vec<&[u8]> = vec![b"README", b"readme"];
        let side = index_side(&names, Sides::both_case_sensitive());
        assert_eq!(side.collisions().count(), 0);
        assert_eq!(side.unique().count(), 2);
    }

    /// An empty listing neither collides nor pairs, and does not blow up.
    #[test]
    fn an_empty_side_is_a_side() {
        let names: Vec<&[u8]> = vec![];
        let side = index_side(&names, Sides::both_case_sensitive());
        assert!(side.is_empty());
        assert_eq!(side.unique().count(), 0);
        assert_eq!(side.collisions().count(), 0);
        assert_eq!(side.ambiguous_reason(b"nada"), None);
    }

    /// `Entry` pairs by its `VPath`'s LAST segment, with its raw bytes: it
    /// is what the walk is going to pass it.
    #[test]
    fn an_entry_pairs_by_its_last_segments_bytes() {
        use norte_proto::{EntryKind, VPath};

        let entry = |wire: &str| Entry {
            path: VPath::parse(wire).expect("path"),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::default(),
        };
        // `informe\xff.dat` percent-encoded: the bytes come back exact.
        let raw = entry("file:///a/informe%FF.dat");
        assert_eq!(raw.pair_name(), b"informe\xff.dat");

        let nfd = entry("file:///a/cafe%CC%81");
        let nfc = entry("file:///b/caf%C3%A9");
        let sensitive = Sides::both_case_sensitive();
        assert_eq!(
            key_for(nfd.pair_name(), sensitive),
            key_for(nfc.pair_name(), sensitive)
        );
        assert_ne!(nfd.pair_name(), nfc.pair_name(), "the bytes are still two");
    }

    /// The order of the keys is the merge-join's, not the listing's (which
    /// guarantees none at all).
    #[test]
    fn the_keys_come_out_sorted() {
        let names: Vec<&[u8]> = vec![b"zeta", b"alfa", b"Mu"];
        let side = index_side(&names, Sides::right_case_insensitive());
        let keys: Vec<Vec<u8>> = side.unique().map(|(k, _)| k.as_bytes().to_vec()).collect();
        assert_eq!(
            keys,
            vec![b"alfa".to_vec(), b"mu".to_vec(), b"zeta".to_vec()]
        );
    }

    /// A borrowed key and a copied one with the same bytes are THE SAME
    /// key: otherwise, a side that normalized would not find the other one
    /// that did not.
    #[test]
    fn borrowed_and_owned_are_the_same_key() {
        let sensitive = Sides::both_case_sensitive();
        let borrowed = key_for(b"cafe", sensitive);
        let owned = key_for(b"cafe\xcc\x81", sensitive).into_owned();
        assert_ne!(borrowed, owned);
        assert_eq!(borrowed.clone().into_owned(), borrowed);
        assert_eq!(owned.as_bytes(), "café".as_bytes());
    }
}
