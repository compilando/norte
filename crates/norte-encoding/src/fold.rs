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
    ///
    /// **Known gap: HFS+ also drops the default-ignorables** — its
    /// `FastUnicodeCompare` (Apple TN1150) maps a set very close to
    /// [`is_default_ignorable`] to zero — and this mode does not, because APFS
    /// and NTFS do not. So on an HFS+ volume (old disks, disk images, Time
    /// Machine) norte can still say "no collision" about two names that differ
    /// only by an invisible. Same shape as the gap #214 closed for `+F`, one
    /// filesystem over; it needs a mode of its own rather than bending this
    /// one.
    Simple,
    /// FULL case folding (ext4/f2fs `+F`, whose kernel table is built from
    /// `CaseFolding.txt` status `C + F`). Can EXPAND a name (`ß` -> `ss`), so
    /// the key can grow longer than the input — and DROPS the
    /// default-ignorables (#214), so it can also shrink, down to an empty key
    /// for a name made only of invisibles.
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

/// Las MISMAS filas que [`full_fold_expansion`], en forma recorrible.
///
/// Existe para que un test pueda comprobar la tabla ENTERA en vez de una
/// muestra: un `match` no se itera, y una tabla que solo se prueba por
/// muestreo es una tabla en la que nadie nota la fila que falta. El test que
/// las empareja (`la_tabla_y_el_match_dicen_lo_mismo`) también barre todo el
/// espacio de code points para probar que el `match` no tiene filas de MÁS.
#[cfg(test)]
const FULL_FOLD_ROWS: &[(char, &str)] = &[
    ('\u{00DF}', "ss"),
    ('\u{0149}', "\u{02BC}\u{006E}"),
    ('\u{0587}', "\u{0565}\u{0582}"),
    ('\u{1E9A}', "\u{0061}\u{02BE}"),
    ('\u{1F80}', "\u{1F00}\u{03B9}"),
    ('\u{1F81}', "\u{1F01}\u{03B9}"),
    ('\u{1F82}', "\u{1F02}\u{03B9}"),
    ('\u{1F83}', "\u{1F03}\u{03B9}"),
    ('\u{1F84}', "\u{1F04}\u{03B9}"),
    ('\u{1F85}', "\u{1F05}\u{03B9}"),
    ('\u{1F86}', "\u{1F06}\u{03B9}"),
    ('\u{1F87}', "\u{1F07}\u{03B9}"),
    ('\u{1F90}', "\u{1F20}\u{03B9}"),
    ('\u{1F91}', "\u{1F21}\u{03B9}"),
    ('\u{1F92}', "\u{1F22}\u{03B9}"),
    ('\u{1F93}', "\u{1F23}\u{03B9}"),
    ('\u{1F94}', "\u{1F24}\u{03B9}"),
    ('\u{1F95}', "\u{1F25}\u{03B9}"),
    ('\u{1F96}', "\u{1F26}\u{03B9}"),
    ('\u{1F97}', "\u{1F27}\u{03B9}"),
    ('\u{1FA0}', "\u{1F60}\u{03B9}"),
    ('\u{1FA1}', "\u{1F61}\u{03B9}"),
    ('\u{1FA2}', "\u{1F62}\u{03B9}"),
    ('\u{1FA3}', "\u{1F63}\u{03B9}"),
    ('\u{1FA4}', "\u{1F64}\u{03B9}"),
    ('\u{1FA5}', "\u{1F65}\u{03B9}"),
    ('\u{1FA6}', "\u{1F66}\u{03B9}"),
    ('\u{1FA7}', "\u{1F67}\u{03B9}"),
    ('\u{1FB2}', "\u{1F70}\u{03B9}"),
    ('\u{1FB3}', "\u{03B1}\u{03B9}"),
    ('\u{1FB4}', "\u{03AC}\u{03B9}"),
    ('\u{1FB7}', "\u{03B1}\u{0342}\u{03B9}"),
    ('\u{1FC2}', "\u{1F74}\u{03B9}"),
    ('\u{1FC3}', "\u{03B7}\u{03B9}"),
    ('\u{1FC4}', "\u{03AE}\u{03B9}"),
    ('\u{1FC7}', "\u{03B7}\u{0342}\u{03B9}"),
    ('\u{1FF2}', "\u{1F7C}\u{03B9}"),
    ('\u{1FF3}', "\u{03C9}\u{03B9}"),
    ('\u{1FF4}', "\u{03CE}\u{03B9}"),
    ('\u{1FF7}', "\u{03C9}\u{0342}\u{03B9}"),
    ('\u{FB00}', "ff"),
    ('\u{FB01}', "fi"),
    ('\u{FB02}', "fl"),
    ('\u{FB03}', "ffi"),
    ('\u{FB04}', "ffl"),
    ('\u{FB05}', "st"),
    ('\u{FB06}', "st"),
    ('\u{FB13}', "\u{0574}\u{0576}"),
    ('\u{FB14}', "\u{0574}\u{0565}"),
    ('\u{FB15}', "\u{0574}\u{056B}"),
    ('\u{FB16}', "\u{057E}\u{0576}"),
    ('\u{FB17}', "\u{0574}\u{056D}"),
];

/// Los code points cuyo full fold es multi-carácter y que NO están en
/// [`full_fold_expansion`] porque su expansión vuelve a componerse en NFC.
///
/// Van aquí, y con su propio test, porque «ausente» y «ausente a propósito»
/// se distinguen mirando el código y no la intención de quien lo escribió.
#[cfg(test)]
const FULL_FOLD_INERT: &[(char, &str)] = &[
    ('\u{01F0}', "\u{006A}\u{030C}"),
    ('\u{0390}', "\u{03B9}\u{0308}\u{0301}"),
    ('\u{03B0}', "\u{03C5}\u{0308}\u{0301}"),
    ('\u{1E96}', "\u{0068}\u{0331}"),
    ('\u{1E97}', "\u{0074}\u{0308}"),
    ('\u{1E98}', "\u{0077}\u{030A}"),
    ('\u{1E99}', "\u{0079}\u{030A}"),
    ('\u{1F50}', "\u{03C5}\u{0313}"),
    ('\u{1F52}', "\u{03C5}\u{0313}\u{0300}"),
    ('\u{1F54}', "\u{03C5}\u{0313}\u{0301}"),
    ('\u{1F56}', "\u{03C5}\u{0313}\u{0342}"),
    ('\u{1FB6}', "\u{03B1}\u{0342}"),
    ('\u{1FC6}', "\u{03B7}\u{0342}"),
    ('\u{1FD2}', "\u{03B9}\u{0308}\u{0300}"),
    ('\u{1FD3}', "\u{03B9}\u{0308}\u{0301}"),
    ('\u{1FD6}', "\u{03B9}\u{0342}"),
    ('\u{1FD7}', "\u{03B9}\u{0308}\u{0342}"),
    ('\u{1FE2}', "\u{03C5}\u{0308}\u{0300}"),
    ('\u{1FE3}', "\u{03C5}\u{0308}\u{0301}"),
    ('\u{1FE4}', "\u{03C1}\u{0313}"),
    ('\u{1FE6}', "\u{03C5}\u{0342}"),
    ('\u{1FE7}', "\u{03C5}\u{0308}\u{0342}"),
    ('\u{1FF6}', "\u{03C9}\u{0342}"),
];

/// The multi-character expansion a code point folds to under
/// [`FoldMode::Full`] and NOT under [`FoldMode::Simple`] — `CaseFolding.txt`
/// status `F` rows with no `C`/`S` alternative, i.e. exactly the characters
/// whose full fold is not a single `char`.
///
/// # Complete, and against what (#214)
///
/// This table used to be a small hand-picked subset (`ß`, the `ff` family,
/// `ſt`/`st`) because the authoritative rows "were not reachable to verify
/// from here", and an unverifiable entry claims a collision that may not
/// exist. That stopped being acceptable when ADR 0054 made
/// [`FoldMode::Full`] the answer norte gives about a REAL filesystem: every
/// omitted row is a collision an ext4/f2fs `+F` directory makes and norte
/// does not warn about — a plan approved with no warning that dies mid-batch.
///
/// It is now every `F`-only row **that this crate's pipeline can actually
/// observe**, derived from the Unicode 16 tables (`str::casefold`'s own
/// source data) rather than transcribed by hand.
///
/// # What is deliberately NOT here, and why that is not a gap
///
/// 23 further code points have a multi-character full fold whose expansion
/// **normalises back to the character itself**: `ǰ` → `j`+U+030C, `ẖ` →
/// `h`+U+0331, `ΐ` → `ι`+U+0308+U+0301, and the rest of the Greek and Latin
/// combining-mark family. [`name_key`] folds and THEN normalises to NFC, so
/// an entry for any of them would expand and immediately recompose: dead
/// code that reads as coverage. They pair correctly today, and they pair
/// correctly through this absence, which is why the check that keeps this
/// table honest is written as "the key of the character equals the key of its
/// expansion" rather than "the table has N rows".
///
/// Applied to the character AS `to_lowercase` LEFT IT (i.e. after the
/// ordinary lowercase mapping, before [`fold_delta`]): `ẞ` (U+1E9E LATIN
/// CAPITAL LETTER SHARP S) reaches `ß` via `to_lowercase` on its own, so it
/// does not need its own entry here.
///
#[must_use]
pub const fn full_fold_expansion(c: char) -> Option<&'static str> {
    match c {
        // --- latin ---
        '\u{00DF}' => Some("ss"), // LATIN SMALL LETTER SHARP S
        '\u{0149}' => Some("\u{02BC}\u{006E}"), // LATIN SMALL LETTER N PRECEDED BY APOSTROPHE
        '\u{1E9A}' => Some("\u{0061}\u{02BE}"), // LATIN SMALL LETTER A WITH RIGHT HALF RING
        '\u{FB00}' => Some("ff"), // LATIN SMALL LIGATURE FF
        '\u{FB01}' => Some("fi"), // LATIN SMALL LIGATURE FI
        '\u{FB02}' => Some("fl"), // LATIN SMALL LIGATURE FL
        '\u{FB03}' => Some("ffi"), // LATIN SMALL LIGATURE FFI
        '\u{FB04}' => Some("ffl"), // LATIN SMALL LIGATURE FFL
        // LATIN SMALL LIGATURE LONG S T y LATIN SMALL LIGATURE ST: las dos a
        // "st". Juntas y no en dos brazos porque clippy tiene razón —
        // `match_same_arms`— y porque decirlo así es más exacto: bajo `Full`
        // las dos ligaduras son la misma palabra.
        '\u{FB05}' | '\u{FB06}' => Some("st"),
        // --- armenian ---
        '\u{0587}' => Some("\u{0565}\u{0582}"), // ARMENIAN SMALL LIGATURE ECH YIWN
        '\u{FB13}' => Some("\u{0574}\u{0576}"), // ARMENIAN SMALL LIGATURE MEN NOW
        '\u{FB14}' => Some("\u{0574}\u{0565}"), // ARMENIAN SMALL LIGATURE MEN ECH
        '\u{FB15}' => Some("\u{0574}\u{056B}"), // ARMENIAN SMALL LIGATURE MEN INI
        '\u{FB16}' => Some("\u{057E}\u{0576}"), // ARMENIAN SMALL LIGATURE VEW NOW
        '\u{FB17}' => Some("\u{0574}\u{056D}"), // ARMENIAN SMALL LIGATURE MEN XEH
        // --- greek ---
        '\u{1F80}' => Some("\u{1F00}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH PSILI AND YPOGEGRAMMENI
        '\u{1F81}' => Some("\u{1F01}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH DASIA AND YPOGEGRAMMENI
        '\u{1F82}' => Some("\u{1F02}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH PSILI AND VARIA AND YPOGEGRAMMENI
        '\u{1F83}' => Some("\u{1F03}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH DASIA AND VARIA AND YPOGEGRAMMENI
        '\u{1F84}' => Some("\u{1F04}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH PSILI AND OXIA AND YPOGEGRAMMENI
        '\u{1F85}' => Some("\u{1F05}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH DASIA AND OXIA AND YPOGEGRAMMENI
        '\u{1F86}' => Some("\u{1F06}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH PSILI AND PERISPOMENI AND YPOGEGRAMMENI
        '\u{1F87}' => Some("\u{1F07}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH DASIA AND PERISPOMENI AND YPOGEGRAMMENI
        '\u{1F90}' => Some("\u{1F20}\u{03B9}"), // GREEK SMALL LETTER ETA WITH PSILI AND YPOGEGRAMMENI
        '\u{1F91}' => Some("\u{1F21}\u{03B9}"), // GREEK SMALL LETTER ETA WITH DASIA AND YPOGEGRAMMENI
        '\u{1F92}' => Some("\u{1F22}\u{03B9}"), // GREEK SMALL LETTER ETA WITH PSILI AND VARIA AND YPOGEGRAMMENI
        '\u{1F93}' => Some("\u{1F23}\u{03B9}"), // GREEK SMALL LETTER ETA WITH DASIA AND VARIA AND YPOGEGRAMMENI
        '\u{1F94}' => Some("\u{1F24}\u{03B9}"), // GREEK SMALL LETTER ETA WITH PSILI AND OXIA AND YPOGEGRAMMENI
        '\u{1F95}' => Some("\u{1F25}\u{03B9}"), // GREEK SMALL LETTER ETA WITH DASIA AND OXIA AND YPOGEGRAMMENI
        '\u{1F96}' => Some("\u{1F26}\u{03B9}"), // GREEK SMALL LETTER ETA WITH PSILI AND PERISPOMENI AND YPOGEGRAMMENI
        '\u{1F97}' => Some("\u{1F27}\u{03B9}"), // GREEK SMALL LETTER ETA WITH DASIA AND PERISPOMENI AND YPOGEGRAMMENI
        '\u{1FA0}' => Some("\u{1F60}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH PSILI AND YPOGEGRAMMENI
        '\u{1FA1}' => Some("\u{1F61}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH DASIA AND YPOGEGRAMMENI
        '\u{1FA2}' => Some("\u{1F62}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH PSILI AND VARIA AND YPOGEGRAMMENI
        '\u{1FA3}' => Some("\u{1F63}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH DASIA AND VARIA AND YPOGEGRAMMENI
        '\u{1FA4}' => Some("\u{1F64}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH PSILI AND OXIA AND YPOGEGRAMMENI
        '\u{1FA5}' => Some("\u{1F65}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH DASIA AND OXIA AND YPOGEGRAMMENI
        '\u{1FA6}' => Some("\u{1F66}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH PSILI AND PERISPOMENI AND YPOGEGRAMMENI
        '\u{1FA7}' => Some("\u{1F67}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH DASIA AND PERISPOMENI AND YPOGEGRAMMENI
        '\u{1FB2}' => Some("\u{1F70}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH VARIA AND YPOGEGRAMMENI
        '\u{1FB3}' => Some("\u{03B1}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH YPOGEGRAMMENI
        '\u{1FB4}' => Some("\u{03AC}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH OXIA AND YPOGEGRAMMENI
        '\u{1FB7}' => Some("\u{03B1}\u{0342}\u{03B9}"), // GREEK SMALL LETTER ALPHA WITH PERISPOMENI AND YPOGEGRAMMENI
        '\u{1FC2}' => Some("\u{1F74}\u{03B9}"), // GREEK SMALL LETTER ETA WITH VARIA AND YPOGEGRAMMENI
        '\u{1FC3}' => Some("\u{03B7}\u{03B9}"), // GREEK SMALL LETTER ETA WITH YPOGEGRAMMENI
        '\u{1FC4}' => Some("\u{03AE}\u{03B9}"), // GREEK SMALL LETTER ETA WITH OXIA AND YPOGEGRAMMENI
        '\u{1FC7}' => Some("\u{03B7}\u{0342}\u{03B9}"), // GREEK SMALL LETTER ETA WITH PERISPOMENI AND YPOGEGRAMMENI
        '\u{1FF2}' => Some("\u{1F7C}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH VARIA AND YPOGEGRAMMENI
        '\u{1FF3}' => Some("\u{03C9}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH YPOGEGRAMMENI
        '\u{1FF4}' => Some("\u{03CE}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH OXIA AND YPOGEGRAMMENI
        '\u{1FF7}' => Some("\u{03C9}\u{0342}\u{03B9}"), // GREEK SMALL LETTER OMEGA WITH PERISPOMENI AND YPOGEGRAMMENI
        _ => None,
    }
}

/// ¿Es `c` un **`Default_Ignorable_Code_Point`** de Unicode — de los que el
/// pliegue COMPLETO tira antes de comparar (#214)?
///
/// Existe porque [`FoldMode::Full`] no es una opinión de norte: es lo que hace
/// un directorio `+F` de ext4/f2fs, y el kernel genera sus tablas con
/// `mkutf8data` en la variante **`nfdicf`** — NFD, **ignore default
/// ignorables**, case fold. Esa `i` es esto. Sin descartarlos, `nombre.txt` y
/// `nom<U+00AD>bre.txt` daban dos claves distintas y norte anunciaba «no
/// colisionan» sobre el único sistema de ficheros del que #145 habla: un plan
/// aprobado sin aviso que se muere a mitad del lote.
///
/// Y es trivialmente alcanzable: el corpus ya lleva un ZWJ (U+200D), que está
/// en esta lista.
///
/// Solo en `Full`. Bajo `Simple` —APFS, NTFS— un guion suave es un carácter
/// como otro cualquiera y dos nombres que solo se diferencien en él son dos
/// ficheros; descartarlo ahí sería inventarse una colisión que el sistema de
/// ficheros no ve.
///
/// # Una sola tabla, dos políticas
///
/// Los rangos son los de `DEFAULT_IGNORABLE`, la tabla derivada de la UCD que
/// este mismo crate ya tenía para pintar invisibles, y esta función DELEGA en
/// ella: dos copias de la misma propiedad son dos respuestas que divergen en
/// silencio, que es justo lo que la ADR 0051 centralizó.
///
/// Lo que NO se comparte es la política. `is_terminal_hazard` exime a
/// propósito el ZWJ y los selectores de variación (`IGNORABLES_PERMITIDOS`),
/// porque componen emoji legítimos y enmascararlos rompería nombres reales.
/// Aquí no hay exención posible: el sistema de ficheros los descarta, y una
/// clave que los conservara diría «no colisionan» sobre dos nombres que el
/// disco junta. Misma tabla, preguntas distintas.
///
/// Un matiz de versión: `U+180F` es `Default_Ignorable` desde Unicode 14 y las
/// tablas `utf8data` del kernel son 12.1, así que ahí norte junta un par que
/// ese kernel separa. Un code point, y del lado conservador (avisa de una
/// colisión que no habrá) — que es el lado correcto para avisar.
///
/// ```
/// use norte_encoding::{FoldMode, name_key};
/// // Un guion suave no distingue dos nombres en un directorio `+F`.
/// assert_eq!(
///     name_key("nom\u{00AD}bre.txt".as_bytes(), FoldMode::Full),
///     name_key("nombre.txt".as_bytes(), FoldMode::Full),
/// );
/// // Y sí los distingue en uno que pliega SIMPLE, que es lo que ese
/// // filesystem hace.
/// assert_ne!(
///     name_key("nom\u{00AD}bre.txt".as_bytes(), FoldMode::Simple),
///     name_key("nombre.txt".as_bytes(), FoldMode::Simple),
/// );
/// ```
#[must_use]
pub fn is_default_ignorable(c: char) -> bool {
    crate::es_ignorable_por_defecto(c)
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
            .filter(|c| !is_default_ignorable(*c))
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
                    .filter(|c| !is_default_ignorable(*c))
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

/// Does this `char` have a canonical SINGLETON decomposition — does NFC rewrite
/// it, on its own, to one DIFFERENT character (#152)?
///
/// U+212A KELVIN SIGN becomes `K`, U+2126 OHM SIGN becomes U+03A9, U+212B
/// ANGSTROM SIGN becomes U+00C5. Unicode calls each pair canonically
/// equivalent; a filesystem does not, and neither does a reader — the two names
/// are two files on ext4 and two different characters on screen.
///
/// This is the wart that makes [`name_key`]'s closing NFC pass non-injective,
/// and the reason the pairing key can put two UNRELATED files in one row. The
/// key still normalises — the macOS-NFD/Linux-NFC case is the whole point of it
/// — so what this predicate buys is the ability to SAY which of the two
/// happened.
///
/// The test is `NFC(c)` rather than a canonical decomposition, on purpose:
/// `unicode_normalization::char::decompose_canonical` decomposes FULLY and
/// recursively, so U+212B comes back as `A` plus a combining ring — two chars —
/// and a "decomposes to exactly one other char" test would miss it. Composing
/// instead catches every singleton, including the ones whose target decomposes
/// further, and leaves every already-NFC character (`é`, a bare combining
/// acute, ASCII) alone.
///
/// ```
/// use norte_encoding::is_canonical_singleton;
/// assert!(is_canonical_singleton('\u{212a}'), "KELVIN SIGN");
/// assert!(is_canonical_singleton('\u{2126}'), "OHM SIGN");
/// assert!(is_canonical_singleton('\u{212b}'), "ANGSTROM SIGN");
/// // Not singletons: a precomposed letter, a combining mark, plain ASCII.
/// assert!(!is_canonical_singleton('é'));
/// assert!(!is_canonical_singleton('\u{301}'));
/// assert!(!is_canonical_singleton('K'));
/// ```
#[must_use]
pub fn is_canonical_singleton(c: char) -> bool {
    let mut nfc = [c].into_iter().nfc();
    matches!((nfc.next(), nfc.next()), (Some(target), None) if target != c)
}

/// Does this NAME contain a character with a canonical singleton decomposition
/// ([`is_canonical_singleton`])?
///
/// **It asks over exactly the run [`name_key`] normalises, and that is not the
/// whole name.** A name that is not valid UTF-8 still gets its leading valid
/// run folded and NFC'd — that is what `fold_lossy` does, and what the corpus
/// pair `partial_utf8_nfd_twin` / `partial_utf8_nfc_twin` pins (#154) — so a
/// singleton inside that prefix DOES decide a pairing, and answering `false`
/// for the whole name because of a stray byte at the end would miss it. Asking
/// `str::from_utf8(name).is_ok()` first is exactly that miss, and the shape it
/// misses is the file-losing one: `K.txt` (U+212A) and `K.txt` with the same
/// invalid tail pair, differ in bytes, and would be reported as one text
/// (`protocol-guardian`, W4b, MAJOR-1).
///
/// Bytes past the first invalid one pass through raw, so NFC cannot resolve a
/// singleton there and they are not scanned.
///
/// ```
/// use norte_encoding::has_canonical_singleton;
/// assert!(has_canonical_singleton("\u{212a}.txt".as_bytes()));
/// assert!(!has_canonical_singleton(b"K.txt"));
/// assert!(!has_canonical_singleton(b"roto\xff\xfe"), "sin singleton en el texto");
/// // Y el tramo VÁLIDO de un nombre que no es texto entero sí se mira.
/// let mut roto = "\u{212a}.txt".as_bytes().to_vec();
/// roto.push(0xFF);
/// assert!(has_canonical_singleton(&roto));
/// ```
///
/// # Panics
/// Never: the only `expect` re-parses the bytes a `Utf8Error` has just
/// certified valid, which is the same invariant `fold_lossy` runs on.
#[must_use]
pub fn has_canonical_singleton(name: &[u8]) -> bool {
    let text = match std::str::from_utf8(name) {
        Ok(text) => text,
        // INVARIANTE (regla dura 6): `Utf8Error` garantiza que
        // `name[..valid_up_to]` es UTF-8 válido, así que re-parsearlo no puede
        // fallar. Es el mismo corte que hace `fold_lossy`, a propósito: las dos
        // funciones tienen que mirar el MISMO tramo o la marca dice una cosa de
        // una clave que se calculó de otra.
        Err(error) => std::str::from_utf8(&name[..error.valid_up_to()])
            .expect("valid_up_to bytes of a from_utf8 error are valid UTF-8"),
    };
    text.chars().any(is_canonical_singleton)
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

    // ---- canonical singletons (#152) ----

    /// The corpus pair, and the property that makes it dangerous: the two
    /// names share a key WITHOUT any case folding, and they are not the same
    /// text. `has_canonical_singleton` is what lets a caller tell this pairing
    /// apart from the NFC/NFD one it must keep.
    #[test]
    fn the_kelvin_pair_shares_a_key_and_is_not_one_text() {
        let kelvin = corpus("singleton_kelvin_sign");
        let ascii = corpus("ascii_capital_k");
        assert_ne!(kelvin, ascii);
        assert_eq!(
            name_key(&kelvin, FoldMode::None),
            name_key(&ascii, FoldMode::None),
        );
        assert!(has_canonical_singleton(&kelvin));
        assert!(!has_canonical_singleton(&ascii));
    }

    /// The NFC/NFD pair the key exists to serve is NOT flagged: it is the same
    /// text, and a predicate that answered `true` here would make the marker
    /// useless — every macOS-to-Linux comparison would carry it.
    #[test]
    fn the_nfc_nfd_pair_is_not_a_singleton() {
        assert!(!has_canonical_singleton(&corpus("nfc_e_acute")));
        assert!(!has_canonical_singleton(&corpus("nfd_e_acute")));
    }

    /// The two other singletons of the same family, and the one that a
    /// "decomposes to exactly one char" test would have missed: U+212B
    /// decomposes FULLY to `A` plus a combining ring.
    #[test]
    fn ohm_and_angstrom_are_singletons_too() {
        assert!(is_canonical_singleton('\u{2126}'), "OHM SIGN");
        assert!(is_canonical_singleton('\u{212b}'), "ANGSTROM SIGN");
        assert!(
            !is_canonical_singleton('\u{c5}'),
            "its NFC target is stable"
        );
    }

    /// Bytes that are not text have no characters to normalise, and a name with
    /// no singleton in its TEXT answers `false` however broken its tail is
    /// (rule 1, #154).
    #[test]
    fn a_name_that_is_not_utf8_has_no_singleton() {
        assert!(!has_canonical_singleton(b"roto\xff\xfe"));
        assert!(!has_canonical_singleton(&corpus("partial_utf8_nfd_twin")));
    }

    /// **The false negative that would have cost a file** (`protocol-guardian`,
    /// W4b MAJOR-1): `name_key` normalises the leading valid run of a name that
    /// is not text all the way through, so a singleton inside that run decides
    /// the pairing. The two corpus names below share a key with both sides
    /// case-sensitive and are two different files; a predicate that required
    /// the WHOLE name to be UTF-8 would have called the pair one text.
    #[test]
    fn a_singleton_inside_the_valid_run_of_a_broken_name_still_counts() {
        let kelvin = corpus("singleton_kelvin_sign_invalid_tail");
        let ascii = corpus("ascii_capital_k_invalid_tail");
        assert!(std::str::from_utf8(&kelvin).is_err(), "no es texto entero");
        assert_ne!(kelvin, ascii);
        assert_eq!(
            name_key(&kelvin, FoldMode::None),
            name_key(&ascii, FoldMode::None),
            "y aun así emparejan: la clave normaliza el prefijo válido"
        );
        assert!(has_canonical_singleton(&kelvin));
        assert!(!has_canonical_singleton(&ascii));
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

    /// Las DOS ligaduras st van a `st` bajo `Full`, y eso no lo decía nada
    /// (#214): el par estaba pineado bajo `Simple`, y un cambio de modo que
    /// perdiera una de las dos filas habría pasado la suite entera.
    #[test]
    fn las_dos_ligaduras_st_van_a_lo_mismo_bajo_full() {
        let (largo, corto) = (corpus("ligature_long_st"), corpus("ligature_st"));
        assert_eq!(
            name_key(&largo, FoldMode::Full),
            name_key(&corto, FoldMode::Full)
        );
        assert_eq!(full_fold_expansion('\u{FB05}'), Some("st"));
        assert_eq!(full_fold_expansion('\u{FB06}'), Some("st"));
    }

    // ---- #214: los ignorables por defecto, que `+F` tampoco ve ----

    /// Un guion suave no distingue dos nombres en `Full`, y sí en `Simple`.
    ///
    /// Es la mitad de `+F` que faltaba: la tabla del kernel se genera como
    /// `nfdicf` —NFD, **i**gnore default ignorables, case fold—, así que sin
    /// descartarlos norte contestaba «no colisionan» sobre el único sistema de
    /// ficheros del que #145 habla.
    #[test]
    fn un_ignorable_no_distingue_dos_nombres_bajo_full() {
        let (con, sin) = (
            corpus("full_fold_soft_hyphen"),
            corpus("full_fold_soft_hyphen_plain"),
        );
        assert_eq!(
            name_key(&con, FoldMode::Full),
            name_key(&sin, FoldMode::Full),
            "en `+F` son un fichero"
        );
        assert_ne!(
            name_key(&con, FoldMode::Simple),
            name_key(&sin, FoldMode::Simple),
            "en APFS/NTFS son dos, y decir lo contrario sería inventarse una colisión"
        );
    }

    /// Y el ZWJ que ya estaba en el corpus también: es lo que hacía a esto
    /// trivialmente alcanzable.
    #[test]
    fn el_zwj_tampoco_cuenta_bajo_full() {
        assert_eq!(
            name_key("a\u{200D}b".as_bytes(), FoldMode::Full),
            name_key(b"ab", FoldMode::Full),
        );
    }

    /// El único punto donde el ORDEN importa: descartar el ignorable y luego
    /// componer NFC junta lo que el CGJ existía para separar.
    ///
    /// `a` + COMBINING GRAPHEME JOINER + acento agudo pliega a `á` bajo
    /// `Full`, porque el ignorable se va antes de la pasada de NFC. Es lo que
    /// hace el kernel (`nfdicf`: descompone, ignora, pliega), así que es la
    /// respuesta correcta para un `+F` — y es la que alguien «arreglaría» sin
    /// este test.
    #[test]
    fn componer_a_traves_de_un_ignorable_borrado_es_deliberado() {
        assert_eq!(
            name_key("a\u{034F}\u{0301}".as_bytes(), FoldMode::Full),
            name_key("á".as_bytes(), FoldMode::Full),
        );
        assert_ne!(
            name_key("a\u{034F}\u{0301}".as_bytes(), FoldMode::Simple),
            name_key("á".as_bytes(), FoldMode::Simple),
            "bajo pliegue simple el CGJ sigue separando, que es para lo que está"
        );
    }

    /// Un nombre ENTERO de invisibles da una clave vacía bajo `Full`, y eso es
    /// una salida nueva de `name_key` que conviene tener escrita: los
    /// consumidores la usan como clave de mapa, y dos nombres así emparejan.
    #[test]
    fn un_nombre_todo_invisible_da_clave_vacia_bajo_full() {
        let a = "\u{3164}\u{3164}".as_bytes();
        let b = "\u{200B}".as_bytes();
        assert!(name_key(a, FoldMode::Full).is_empty());
        assert_eq!(name_key(a, FoldMode::Full), name_key(b, FoldMode::Full));
        assert_ne!(
            name_key(a, FoldMode::Simple),
            name_key(b, FoldMode::Simple),
            "y bajo simple siguen siendo dos nombres distintos"
        );
    }

    /// La lista de ignorables cubre lo conocido y no se pasa: un `char`
    /// corriente NO es ignorable, y descartarlo sería juntar dos ficheros que
    /// el sistema de ficheros ve por separado.
    #[test]
    fn la_lista_de_ignorables_cubre_lo_conocido() {
        for c in [
            '\u{00AD}',
            '\u{034F}',
            '\u{061C}',
            '\u{115F}',
            '\u{1160}',
            '\u{17B4}',
            '\u{180E}',
            '\u{200B}',
            '\u{200D}',
            '\u{200F}',
            '\u{202E}',
            '\u{2060}',
            '\u{206F}',
            '\u{3164}',
            '\u{FE00}',
            '\u{FE0F}',
            '\u{FEFF}',
            '\u{FFA0}',
            '\u{FFF8}',
            '\u{1BCA0}',
            '\u{1D173}',
            '\u{E0001}',
            '\u{E0FFF}',
        ] {
            assert!(is_default_ignorable(c), "U+{:04X}", u32::from(c));
        }
        for c in [
            'a', 'ß', '\u{0301}', '\u{200A}', '\u{2010}', '\u{FE10}', '\u{FDFF}', '☃',
        ] {
            assert!(!is_default_ignorable(c), "U+{:04X}", u32::from(c));
        }
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

    /// La tabla recorrible y el `match` dicen lo MISMO, en las dos
    /// direcciones: cada fila está en el `match`, y el `match` no tiene filas
    /// que la tabla no declare. Lo segundo barre el espacio entero de code
    /// points, que es lo único que lo demuestra.
    #[test]
    fn la_tabla_y_el_match_dicen_lo_mismo() {
        for (c, esperado) in FULL_FOLD_ROWS {
            assert_eq!(
                full_fold_expansion(*c),
                Some(*esperado),
                "U+{:04X} no está en el match",
                u32::from(*c)
            );
        }
        for cp in 0..=0x0010_FFFF_u32 {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            if full_fold_expansion(c).is_some() {
                assert!(
                    FULL_FOLD_ROWS.iter().any(|(k, _)| *k == c),
                    "U+{cp:04X} está en el match y no en la tabla"
                );
            }
        }
    }

    /// Lo que la tabla PROMETE: bajo `Full`, un carácter y su expansión dan la
    /// misma clave. Es la aserción que importa —«esto colisiona en un `+F`»— y
    /// se comprueba fila a fila, no por muestreo.
    #[test]
    fn cada_fila_empareja_su_expansion() {
        for (c, expansion) in FULL_FOLD_ROWS {
            let texto = c.to_string();
            let uno = name_key(texto.as_bytes(), FoldMode::Full);
            let otro = name_key(expansion.as_bytes(), FoldMode::Full);
            assert_eq!(
                uno,
                otro,
                "U+{:04X} y {expansion:?} tendrían que colisionar bajo Full",
                u32::from(*c)
            );
        }
    }

    /// Y lo que la AUSENCIA promete: los 23 que no están en la tabla emparejan
    /// igual, porque el paso NFC de `name_key` recompone su expansión. Sin este
    /// test, «lo quitamos porque NFC ya lo hace» es una afirmación sin
    /// comprobar, que es como una tabla incompleta se disfraza de decisión.
    #[test]
    fn los_ausentes_emparejan_por_nfc_y_no_por_la_tabla() {
        for (c, expansion) in FULL_FOLD_INERT {
            assert_eq!(
                full_fold_expansion(*c),
                None,
                "U+{:04X} no debería estar en la tabla",
                u32::from(*c)
            );
            let texto = c.to_string();
            let uno = name_key(texto.as_bytes(), FoldMode::Full);
            let otro = name_key(expansion.as_bytes(), FoldMode::Full);
            assert_eq!(
                uno,
                otro,
                "U+{:04X} tiene que emparejar igual, por NFC",
                u32::from(*c)
            );
        }
    }
}
