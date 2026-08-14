//! The filename collision key: when two names are the SAME name for pairing
//! or planning purposes (ADR 0051, #151).
//!
//! This used to be two implementations — `norte_compare::key::key_for` and
//! `norte_core::rename::plan::name_key` — each carrying its own copy of the
//! 22-code-point fold delta (#129) and, for a while, disagreeing on both the
//! non-UTF-8 fallback and the order of folding versus normalising. `norte-core`
//! and `norte-compare` are AGPL-3.0-only; this crate is MIT OR Apache-2.0, and
//! both consumers already depend on it, so ADR 0051 relicenses the primitive
//! here rather than inventing a third crate for two functions. `norte-core`
//! keeps a thin wrapper (`norte_core::rename::plan::name_key`) so its callers
//! are untouched.
//!
//! The key is for PAIRING ONLY. It is never painted, never operated on, and
//! never substitutes for a name's bytes (hard rule 1) — a name that is not
//! valid UTF-8 is not text, and the bytes that are not text pass through
//! untouched (#154).
//!
//! Two transformations, in this order:
//!
//! 1. **Case folding**, when [`FoldMode`] asks for it. This is real case
//!    FOLDING (`to_lowercase` plus [`fold_delta`], and — under
//!    [`FoldMode::Full`] — the expansions in [`full_fold_expansion`]), not a
//!    bare `to_lowercase`.
//! 2. **NFC**, when the bytes are valid UTF-8. macOS hands out NFD and Linux
//!    NFC; the same file copied between the two has to pair.
//!
//! **The order is not negotiable.** Folding and THEN normalising is the only
//! order that works: `J`+combining-caron has no precomposed uppercase — NFC
//! leaves it alone — and its lowercase `j`+combining-caron DOES compose, to
//! `ǰ` (U+01F0). Normalising first and folding after answers two keys for two
//! names every case-insensitive volume calls ONE file. Pinned by the corpus
//! pair `nfd_uppercase_composed_only_lowercase` / `precomposed_lowercase_j_caron`.

use std::borrow::Cow;

use unicode_normalization::{UnicodeNormalization, is_nfc};

/// How a directory's case-insensitivity folds letter case (#145).
///
/// The DEFAULT everywhere in this workspace today is [`Self::Simple`] (via
/// [`Self::None`] on a case-sensitive directory) — nothing currently probes a
/// real filesystem for [`Self::Full`], and it must never be assumed for one
/// nobody probed. [`Self::Full`] exists so the key CAN express what ext4/f2fs
/// `+F` actually does; wiring a real capability probe to select it is a
/// separate, larger change (see this module's `full_fold_expansion` doc and
/// <https://github.com/compilando/norte/issues/145>).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FoldMode {
    /// The directory distinguishes case: nothing folds.
    None,
    /// SIMPLE case folding (APFS, HFS+, NTFS, case-insensitive SMB). `char ->
    /// char`: the key never grows.
    Simple,
    /// FULL case folding (ext4/f2fs `+F`, whose kernel table is built from
    /// `CaseFolding.txt` status `C + F`). Can EXPAND a name (`ß` -> `ss`), so
    /// the key can grow longer than the input.
    Full,
}

impl FoldMode {
    /// Does this mode fold at all?
    const fn folds(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// The delta between `str::to_lowercase` and SIMPLE case folding, applied to
/// one `char`.
///
/// This is the DELTA, not case folding, and the set below was checked by
/// EQUIVALENCE CLASS, not by comparing `to_lowercase(c)` to a fold table's
/// literal target for each `c` in isolation — that cheaper check overcounts.
/// It flags, for instance, every code point in Unicode 8.0's Cherokee
/// lowercase block (`U+AB70..=U+ABBF`, 80 of them) and the six added
/// alongside it (`U+13F8..=U+13FD`), because each one's `to_lowercase` output
/// differs from `CaseFolding.txt`'s literal fold TARGET for it. But
/// `to_lowercase` already merges each such pair correctly, just onto a
/// DIFFERENT — and internally consistent — representative than case folding
/// picks: `to_lowercase(U+13A0) == to_lowercase(U+AB70) == U+AB70`, so a
/// collision key built from `to_lowercase` alone already treats them as one
/// name, and none of the 86 code points in those two Cherokee ranges need an
/// entry here. What actually needs checking is whether every member of a
/// SIMPLE-case-folding group produces the SAME `to_lowercase` output as every
/// other member; when it does not, THAT is the real, 22-code-point set below
/// (verified by generating every such group from `CaseFolding.txt` and
/// comparing `char::to_lowercase` across each group, not against memory or
/// against the GitHub issue that first proposed this fix:
/// <https://github.com/compilando/norte/issues/129>). Three more code points
/// (`U+1FD3`, `U+1FE3`, `U+1FBE`) would also qualify by that same grouping,
/// but each has a singleton canonical NFC decomposition (to `U+0390`,
/// `U+03B0`, `U+03B9` respectively): `to_lowercase` and this table run on
/// them the same as on any other character and leave them unchanged, so no
/// entry is needed — it is the CLOSING NFC pass in [`name_key`], which runs
/// ONCE after folding, that rewrites each to its singleton target. A
/// codepoint with a canonical singleton decomposition is never NFC-normal on
/// its own, so it can never survive that closing pass, folded or not — which
/// is why none of the three need an entry here, not because folding never
/// reaches them.
///
/// It deliberately excludes anything that only has an EXPANDING full fold —
/// `ß->ss`, `ﬁ->fi`, `ﬆ->st`, ligatures like `ﬃ`/`ﬄ`, all `CaseFolding.txt`
/// status `F` with no `C`/`S` alternative — because a `char -> char` remap
/// cannot turn one name into a longer one. [`FoldMode::Full`] and
/// [`full_fold_expansion`] close that gap for the filesystems whose OWN fold
/// table also expands (ext4/f2fs `+F`); see #145.
///
/// A `match` over `char` rather than a `HashMap`: the set is closed and this
/// runs on every name in every plan, so the compiler gets to turn it into a
/// jump table.
#[must_use]
pub const fn fold_delta(c: char) -> char {
    match c {
        // GREEK SMALL LETTER FINAL SIGMA -> SIGMA. `to_lowercase` applies the
        // Final_Sigma context rule and produces this in WORD-FINAL position;
        // case folding does not distinguish position at all.
        '\u{03C2}' => '\u{03C3}',
        // MICRO SIGN -> GREEK SMALL LETTER MU. `to_lowercase` leaves U+00B5
        // alone: Unicode already calls it lowercase.
        '\u{00B5}' => '\u{03BC}',
        // LATIN SMALL LETTER LONG S -> s.
        '\u{017F}' => 's',
        // LATIN SMALL LETTER LONG S WITH DOT ABOVE -> S WITH DOT ABOVE.
        '\u{1E9B}' => '\u{1E61}',
        // The Greek "symbol" variants, each onto its ordinary lowercase
        // letter: beta, theta, kappa, pi, rho, lunate epsilon, phi.
        '\u{03D0}' => '\u{03B2}',
        '\u{03D1}' => '\u{03B8}',
        '\u{03F0}' => '\u{03BA}',
        '\u{03D6}' => '\u{03C0}',
        '\u{03F1}' => '\u{03C1}',
        '\u{03F5}' => '\u{03B5}',
        '\u{03D5}' => '\u{03C6}',
        // U+1C80..=U+1C88: historic Cyrillic letterforms (Unicode 9's
        // "Cyrillic Extended-C" small letters), each onto the ordinary
        // lowercase Cyrillic letter it is a stylistic variant of. Two of
        // them (U+1C84 TALL TE and U+1C85 THREE-LEGGED TE) fold onto the
        // SAME ordinary letter — that is not a bug, folding is many-to-one.
        '\u{1C80}' => '\u{0432}',
        '\u{1C81}' => '\u{0434}',
        '\u{1C82}' => '\u{043E}',
        '\u{1C83}' => '\u{0441}',
        '\u{1C84}' | '\u{1C85}' => '\u{0442}',
        '\u{1C86}' => '\u{044A}',
        '\u{1C87}' => '\u{0463}',
        '\u{1C88}' => '\u{A64B}',
        // COMBINING GREEK YPOGEGRAMMENI -> GREEK SMALL LETTER IOTA. Its
        // sibling GREEK PROSGEGRAMMENI (U+1FBE) is NOT here: it canonically
        // decomposes to U+03B9 by itself, so NFC (which runs after this
        // function, see [`name_key`]) already rewrites it — an entry for it
        // would be unreachable dead code, the same reason U+1FD3/U+1FE3 are
        // absent (see this function's rustdoc).
        '\u{0345}' => '\u{03B9}',
        // LATIN SMALL LIGATURE LONG S T -> LATIN SMALL LIGATURE ST. The only
        // ligature with a single-codepoint SIMPLE fold at all — `CaseFolding
        // .txt` carries BOTH `FB05; F; 0073 0074` (full fold, to "st") and
        // `FB05; S; FB06` (simple fold, to U+FB06) as separate rows. `FB06`
        // itself has only the `F` row, so it does not fold any further under
        // SIMPLE mode; [`full_fold_expansion`] handles both under FULL mode.
        '\u{FB05}' => '\u{FB06}',
        other => other,
    }
}

/// The multi-character expansion a code point folds to under
/// [`FoldMode::Full`] and NOT under [`FoldMode::Simple`] — `CaseFolding.txt`
/// status `F` rows with no `C`/`S` alternative, i.e. exactly the characters
/// whose full fold is not a single `char`.
///
/// This is deliberately the small, VERIFIED subset directly relevant to
/// #145's own examples (`ß -> ss`, `ﬁ -> fi`) plus the rest of the Latin `ff`
/// family and the `ſt`/`st` ligature pair — not a hand-transcribed copy of
/// every `F`-only row in `CaseFolding.txt`. The wider table (Armenian
/// ligatures, a handful of combining-mark expansions in the `1E9x` block, the
/// Greek dialytika-tonos precompositions) exists but was not reachable to
/// verify against the authoritative table from here, and an unverifiable
/// entry is worse than an absent one: it would claim a collision that might
/// not be real. Absent here means [`name_key`] still answers two keys for
/// that pair under [`FoldMode::Full`] — an accepted, narrower gap than #145
/// started with, not a silent one.
///
/// Applied to the character AS `to_lowercase` LEFT IT (i.e. after the
/// ordinary lowercase mapping, before [`fold_delta`]): `ẞ` (U+1E9E LATIN
/// CAPITAL LETTER SHARP S) reaches `ß` via `to_lowercase` on its own, so it
/// does not need its own entry here.
#[must_use]
pub const fn full_fold_expansion(c: char) -> Option<&'static str> {
    match c {
        // LATIN SMALL LETTER SHARP S -> "ss". The example #145 names.
        '\u{00DF}' => Some("ss"),
        // The Latin `ff`-family ligatures, each to its letter sequence.
        '\u{FB00}' => Some("ff"),
        '\u{FB01}' => Some("fi"),
        '\u{FB02}' => Some("fl"),
        '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        // `ſt`/`st`: FULL folding sends BOTH to "st" (unlike SIMPLE, which
        // sends FB05 to FB06 via `fold_delta` and leaves FB06, which has no
        // `C`/`S` row, unchanged).
        '\u{FB05}' | '\u{FB06}' => Some("st"),
        _ => None,
    }
}

/// Does folding `s` under `mode` change anything? Without allocating: compares
/// the folded-character iterator against the original. Covers expansions of
/// more than one character (`İ` -> `i`+combining-dot, already handled by
/// `char::to_lowercase` alone) and titlecase (`ǅ` -> `ǆ`), which
/// `char::is_uppercase` does not see.
///
/// Looks at `char::to_lowercase` and not `str::to_lowercase` on purpose, and
/// so also looks at [`fold_delta`]/[`full_fold_expansion`]: `str::to_lowercase`
/// carries the contextual `Final_Sigma` rule and `char`'s does not, so `ΟΔΟΣ`
/// is caught by its `Σ` (uppercase in any context) and `οδοσ` — unchanged by
/// lowercasing but changed by folding (`ς`->`σ` does not apply to it, but
/// `µ`->`μ` does) — is caught by the delta.
fn folding_changes(s: &str, mode: FoldMode) -> bool {
    if !mode.folds() {
        return false;
    }
    if matches!(mode, FoldMode::Full) {
        return !s
            .chars()
            .flat_map(char::to_lowercase)
            .flat_map(|c| match full_fold_expansion(c) {
                Some(expansion) => FoldedChars::Full(expansion.chars()),
                None => FoldedChars::One(std::iter::once(fold_delta(c))),
            })
            .eq(s.chars());
    }
    !s.chars()
        .flat_map(char::to_lowercase)
        .map(fold_delta)
        .eq(s.chars())
}

/// One character's FULL-fold output, kept generic over the two shapes it can
/// take (`fold_delta`'s one-for-one remap, or `full_fold_expansion`'s
/// multi-character string) without allocating a `Vec` per character.
enum FoldedChars {
    One(std::iter::Once<char>),
    Full(std::str::Chars<'static>),
}

impl Iterator for FoldedChars {
    type Item = char;
    fn next(&mut self) -> Option<char> {
        match self {
            Self::One(it) => it.next(),
            Self::Full(it) => it.next(),
        }
    }
}

/// Folds and NFC-normalises ONE run of valid UTF-8 text — the unit #154's fix
/// operates on, so a run either side of an invalid byte gets exactly the same
/// treatment a fully-valid name would.
///
/// Borrows when nothing changes (the overwhelmingly common case: already
/// lowercase ASCII, already NFC, no fold requested).
fn fold_run(s: &str, mode: FoldMode) -> Cow<'_, str> {
    let folded: Cow<'_, str> = if mode.folds() && folding_changes(s, mode) {
        if matches!(mode, FoldMode::Full) {
            Cow::Owned(
                s.chars()
                    .flat_map(char::to_lowercase)
                    .flat_map(|c| match full_fold_expansion(c) {
                        Some(expansion) => FoldedChars::Full(expansion.chars()),
                        None => FoldedChars::One(std::iter::once(fold_delta(c))),
                    })
                    .collect(),
            )
        } else {
            Cow::Owned(s.to_lowercase().chars().map(fold_delta).collect())
        }
    } else {
        Cow::Borrowed(s)
    };
    if is_nfc(&folded) {
        folded
    } else {
        Cow::Owned(folded.nfc().collect::<String>())
    }
}

/// Folds and normalises the LEADING valid-UTF-8 run of `name`, and passes
/// everything from the first invalid byte onward through UNCHANGED (#154):
/// one stray byte used to disable NFC and folding for the WHOLE name,
/// because both predecessors of this function ran `str::from_utf8` over the
/// whole buffer and fell back to the raw bytes on any error at all.
///
/// **Deliberately only the leading run, not every run split by an invalid
/// byte.** The obvious generalisation — fold every maximal valid run and
/// splice the invalid bytes back in between — reopens the exact hazard
/// [`name_key`]'s rustdoc warns about: a legacy double-byte encoding's TRAIL
/// byte often falls in `0x40..=0x7E`, where `A`-`Z` live, and after the
/// stream's LEAD byte fails to decode, that trail byte can stand alone as a
/// syntactically-valid one-character UTF-8 "run" that is not independent text
/// at all — it is one half of a foreign character. Shift-JIS `83 58` (ス) is
/// exactly this: byte `83` fails immediately, leaving `58` (`X`) looking like
/// a lone, foldable ASCII letter, and folding it to `x` would be the same
/// wrong-character bug #129 closed for the whole-name case. Every fixture
/// #154 actually ships (`partial_utf8_cased_latin`, `partial_utf8_nfd_twin`,
/// …) has the OTHER shape — a valid prefix, one invalid tail, nothing valid
/// after — which this function covers completely: `error.valid_up_to()` is
/// the split point, and a name that fails at byte 0 (Shift-JIS, `valid_up_to
/// == 0`) degrades to the fully-raw behaviour this function replaces, exactly
/// as before.
///
/// Takes the `Utf8Error` `name_key` already computed deciding to call this,
/// instead of re-running `str::from_utf8(name)` to rediscover it: the caller
/// is the ONLY caller (private function), and the error tells this function
/// everything it needs (`valid_up_to`) without a second full scan of `name`.
fn fold_lossy(name: &[u8], error: std::str::Utf8Error, mode: FoldMode) -> Vec<u8> {
    let valid_up_to = error.valid_up_to();
    let mut out = Vec::with_capacity(name.len());
    if valid_up_to > 0 {
        // SAFETY invariant (no `unsafe`): `Utf8Error` guarantees
        // `name[..valid_up_to]` is valid UTF-8, so re-parsing it cannot fail.
        let text = std::str::from_utf8(&name[..valid_up_to])
            .expect("valid_up_to bytes of a from_utf8 error are valid UTF-8");
        out.extend_from_slice(fold_run(text, mode).as_bytes());
    }
    out.extend_from_slice(&name[valid_up_to..]);
    out
}

/// The key two names are compared by when deciding whether they COLLIDE.
///
/// See the module docs for the two transformations and why their order is
/// fixed. A name that is not UTF-8 passes through its OWN BYTES where it is
/// not text, and folds/normalises the runs either side of an invalid byte
/// independently (#154) — never a lossy conversion, never ASCII-folded: in
/// legacy double-byte encodings the trail byte of a multi-byte character can
/// fall in `0x40..=0x7E`, where `A`-`Z` live, so folding it would rewrite a
/// character that never was a letter.
///
/// ```
/// use norte_encoding::{FoldMode, name_key};
/// let k = name_key(b"README", FoldMode::Simple);
/// assert_eq!(k.as_ref(), b"readme");
///
/// // NFD and NFC of the same name share a key...
/// assert_eq!(
///     name_key("café".as_bytes(), FoldMode::None),
///     name_key(b"cafe\xcc\x81", FoldMode::None),
/// );
/// // ...and bytes that are not text pass through, even around a valid run.
/// assert_eq!(
///     name_key(b"CAFE\xff", FoldMode::Simple).as_ref(),
///     b"cafe\xff",
/// );
/// ```
#[must_use]
pub fn name_key(name: &[u8], mode: FoldMode) -> Cow<'_, [u8]> {
    match std::str::from_utf8(name) {
        Ok(text) => match fold_run(text, mode) {
            Cow::Borrowed(s) => Cow::Borrowed(s.as_bytes()),
            Cow::Owned(s) => Cow::Owned(s.into_bytes()),
        },
        Err(error) => Cow::Owned(fold_lossy(name, error, mode)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture's bytes from `norte-testkit`'s canonical corpus, by id.
    fn corpus(id: &str) -> Vec<u8> {
        norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} in the corpus"))
            .bytes
    }

    // ---- the basics ----

    #[test]
    fn no_fold_no_change_on_ascii() {
        assert_eq!(name_key(b"Foo", FoldMode::None).as_ref(), b"Foo");
    }

    #[test]
    fn simple_fold_lowercases() {
        assert_eq!(name_key(b"Foo", FoldMode::Simple).as_ref(), b"foo");
    }

    #[test]
    fn nfd_and_nfc_share_a_key_even_unfolded() {
        let nfc = "café".as_bytes();
        let nfd = b"cafe\xcc\x81";
        assert_eq!(name_key(nfc, FoldMode::None), name_key(nfd, FoldMode::None));
    }

    #[test]
    fn non_utf8_names_pass_through_raw_when_fully_invalid() {
        let raw = b"broken\xff\xfename";
        assert_eq!(
            name_key(raw, FoldMode::Simple).as_ref(),
            raw,
            "no valid run to fold, and no ASCII-fold of the trail bytes either"
        );
    }

    /// A legacy double-byte trail byte in `0x40..=0x7E` must NOT get ASCII
    /// folded: ア (U+30A2, Shift-JIS `83 41`) and ヂ (Shift-JIS `83 61`) are
    /// different characters, not the same one in two cases.
    #[test]
    fn non_utf8_bytes_are_never_ascii_folded() {
        let mixto = FoldMode::Simple;
        let tesuto = corpus("shift_jis_tesuto");
        assert_eq!(
            name_key(&tesuto, mixto).as_ref(),
            &tesuto[..],
            "the trail byte `58` of ス is an ASCII `X`"
        );
        assert_ne!(
            name_key(b"\x83\x41", mixto),
            name_key(b"\x83\x61", mixto),
            "ア and ヂ differ only in their trail byte"
        );
    }

    // ---- #154: one invalid byte must not disable the whole name ----

    #[test]
    fn a_trailing_invalid_byte_does_not_disable_folding_of_the_valid_run() {
        assert_eq!(
            name_key(b"CAFE\xff", FoldMode::Simple).as_ref(),
            b"cafe\xff",
        );
    }

    #[test]
    fn a_trailing_invalid_byte_does_not_disable_nfc_of_the_valid_run() {
        let decomposed_plus_invalid = corpus("partial_utf8_nfd_twin");
        let composed_plus_invalid = corpus("partial_utf8_nfc_twin");
        assert_eq!(
            name_key(&decomposed_plus_invalid, FoldMode::None),
            name_key(&composed_plus_invalid, FoldMode::None),
            "e + combining acute + invalid byte must still compose to é",
        );
    }

    #[test]
    fn folding_and_a_lone_surrogate_can_coexist_in_one_name() {
        let upper = corpus("partial_utf8_cased_latin");
        let lower = corpus("partial_utf8_cased_latin_lower");
        assert_eq!(
            name_key(&upper, FoldMode::Simple),
            name_key(&lower, FoldMode::Simple),
            "Ä/ä fold even though the lone surrogate after them is not text",
        );
        // And un-folded, they still differ only in the valid run's case.
        assert_ne!(
            name_key(&upper, FoldMode::None),
            name_key(&lower, FoldMode::None)
        );
    }

    /// The deliberate scope boundary: only the LEADING run folds. Text after
    /// the first invalid byte passes through raw, uppercase and all — even
    /// though it is itself valid UTF-8 — because folding it would reopen the
    /// Shift-JIS trail-byte hazard `fold_lossy`'s rustdoc explains (#154).
    #[test]
    fn only_the_leading_run_folds_the_rest_of_the_name_passes_through_raw() {
        let name = b"AB\xffCD";
        assert_eq!(
            name_key(name, FoldMode::Simple).as_ref(),
            b"ab\xffCD",
            "the trailing CD is untouched, not lowercased",
        );
    }

    #[test]
    fn a_truncated_multibyte_sequence_at_the_end_passes_through() {
        // 0xC3 alone is the LEAD byte of a 2-byte sequence with nothing after
        // it: `error_len()` is `None` for this one, exercising that branch.
        let name = b"caf\xc3";
        assert_eq!(name_key(name, FoldMode::Simple).as_ref(), b"caf\xc3");
    }

    // ---- order: fold, THEN normalise — the only order that works ----

    #[test]
    fn folding_can_recompose_so_normalising_must_come_after() {
        let nfd_upper = corpus("nfd_uppercase_composed_only_lowercase");
        let nfc_lower = corpus("precomposed_lowercase_j_caron");
        assert_eq!(
            name_key(&nfd_upper, FoldMode::Simple),
            name_key(&nfc_lower, FoldMode::Simple),
        );
    }

    // ---- #129: case folding, not `to_lowercase` ----

    #[test]
    fn folding_uses_case_folding_not_the_lowercase_mapping() {
        let mixto = FoldMode::Simple;
        for (left, right) in [
            ("greek_uppercase_final_sigma", "greek_medial_sigma_twin"),
            ("micro_sign_mu", "greek_mu_twin"),
            ("ligature_long_st", "ligature_st"),
        ] {
            let (a, b) = (corpus(left), corpus(right));
            assert_eq!(name_key(&a, mixto), name_key(&b, mixto), "{left}");
        }
    }

    #[test]
    fn the_fold_delta_does_not_leak_into_an_unfolded_key() {
        assert_ne!(
            name_key("ΟΔΟΣ".as_bytes(), FoldMode::None),
            name_key("οδοσ".as_bytes(), FoldMode::None),
        );
    }

    // ---- #145: FULL fold expands, SIMPLE fold does not ----

    #[test]
    fn simple_fold_does_not_close_the_ext4_full_fold_gap() {
        let (zett, ss) = (
            corpus("ext4_full_fold_es_zett"),
            corpus("ext4_full_fold_ss"),
        );
        assert_ne!(
            name_key(&zett, FoldMode::Simple),
            name_key(&ss, FoldMode::Simple),
            "an ACCEPTED gap under simple fold — see FoldMode::Full",
        );
    }

    #[test]
    fn full_fold_closes_the_ext4_gap() {
        let (zett, ss) = (
            corpus("ext4_full_fold_es_zett"),
            corpus("ext4_full_fold_ss"),
        );
        assert_eq!(
            name_key(&zett, FoldMode::Full),
            name_key(&ss, FoldMode::Full),
            "straße.txt / strasse.txt are one file on a real ext4 +F directory",
        );
    }

    #[test]
    fn full_fold_expands_the_ligature_family_too() {
        assert_eq!(name_key("ﬁle".as_bytes(), FoldMode::Full).as_ref(), b"file",);
        assert_eq!(name_key("ﬀ".as_bytes(), FoldMode::Full).as_ref(), b"ff",);
    }

    /// `fold_delta` is `char -> char` and cannot express this at all — the
    /// whole reason [`full_fold_expansion`] exists as a SEPARATE table
    /// returning `&str`. UTF-8's own multi-byte encoding of the input
    /// character means the BYTE count does not necessarily grow (`ß` is 2
    /// UTF-8 bytes and folds to `"ss"`, also 2 bytes) — what changes is the
    /// CHARACTER count, which is the property `char -> char` cannot hold.
    #[test]
    fn full_fold_expansion_turns_one_char_into_several() {
        assert_eq!(full_fold_expansion('\u{DF}'), Some("ss"));
        assert_eq!(
            name_key("ß".as_bytes(), FoldMode::Full).iter().count(),
            2,
            "one input character, two output bytes: char -> char cannot say that",
        );
    }

    /// Simple mode never allocates more than the `fold_delta` remap needs —
    /// confirms Full mode's growth is opt-in, not a regression for everyone
    /// who stays on the default.
    #[test]
    fn simple_fold_key_never_grows() {
        for id in ["ext4_full_fold_es_zett", "ligature_long_st"] {
            let raw = corpus(id);
            assert!(name_key(&raw, FoldMode::Simple).len() <= raw.len());
        }
    }
}
