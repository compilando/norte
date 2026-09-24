//! Canonical corpus of hostile fixtures (spec §6.1/§12): at least 48
//! filenames + 11 detectable contents + 3 forced-only. EVERY crate that
//! touches paths or text tests against THIS corpus — new fixtures go in
//! here (CLAUDE.md rule: test-first on encoding bugs).
//!
//! The counts are FLOORS (`>=`), not the exact number (#169): it used to be
//! asserted `== N` in two places in this crate —the `tests/corpus.rs` test
//! and [`hostile_names`]'s doctest— which went red at DIFFERENT times.
//! `nextest` does not run doctests, so adding a fixture left the second one
//! red without `just t` ever seeing it; it happened to `cause_join_spoof`
//! (#161, phase C2), which was not known until a full `just ci`, two rounds
//! later. A floor does not need touching as the corpus grows —which is
//! exactly what makes a new fixture cheap— and it still catches the case
//! the assertion exists to catch: someone deleting the corpus.

use serde::Deserialize;

/// A hostile filename from the corpus.
#[derive(Debug, Clone)]
pub struct HostileName {
    /// Stable identifier (for test names and messages).
    pub id: String,
    /// The name's raw bytes, exactly as the OS would give them.
    pub bytes: Vec<u8>,
    /// Why it is hostile (living documentation).
    pub why: String,
}

#[derive(Deserialize)]
struct RawName {
    id: String,
    hex: String,
    why: String,
}

/// The canonical hostile names: at least 48 (#169 — the floor does not rise
/// on its own just because the corpus grows).
///
/// ```
/// let names = norte_testkit::corpus::hostile_names();
/// assert!(names.len() >= 48, "{}", names.len());
/// // All are valid VPath segments (no NUL, no `/`).
/// for n in &names {
///     assert!(norte_proto::Segment::new(n.bytes.clone()).is_ok(), "{}", n.id);
/// }
/// ```
///
/// # Panics
/// Never with the committed corpus: the embedded fixture is validated in
/// tests.
#[must_use]
pub fn hostile_names() -> Vec<HostileName> {
    let raw: Vec<RawName> =
        serde_json::from_str(include_str!("corpus/names.json")).expect("valid names.json");
    raw.into_iter()
        .map(|r| HostileName {
            bytes: hex_decode(&r.hex),
            id: r.id,
            why: r.why,
        })
        .collect()
}

/// Why two corpus names are the SAME spelling for matching purposes
/// (`norte-compare::key`), even though their bytes differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwinKind {
    /// Same text in Unicode NFC vs NFD form — they match even comparing
    /// byte-for-byte with case sensitivity.
    Normalization,
    /// Same text except for a SIMPLE case fold (`char -> char`,
    /// `CaseFolding.txt`), not `to_lowercase()`.
    CaseFold,
    /// FULL fold (`char -> chars`, can lengthen the name): only matches on
    /// filesystems that full-casefold (ext4/f2fs `+F`), not on ones that
    /// simple-fold (APFS, NTFS) — accepted gap, see #145.
    CaseFoldFull,
    /// Match through a SINGLETON decomposition of NFC, and **are not the
    /// same text**: U+212A KELVIN SIGN against the ASCII `K` (#152).
    ///
    /// It is the only `TwinKind` whose pair is NOT one file seen two ways,
    /// but TWO files the key joins together. It exists so tests can tell
    /// apart the matching that is wanted from the one that must be flagged.
    NormalizationSingleton,
}

/// A pair of corpus names that are the same spelling.
#[derive(Debug, Clone, Copy)]
pub struct SpellingTwin {
    /// The left side's `id` in [`hostile_names`].
    pub left: &'static str,
    /// The right side's `id`.
    pub right: &'static str,
    /// Why they match.
    pub kind: TwinKind,
}

/// The corpus's NFC/NFD and case-fold pairs, by `id` — without hardcoding
/// bytes again.
///
/// `norte-compare::key` is what these pairs test, and its own tests used to
/// write `"café".as_bytes()` / `b"cafe\xcc\x81"` by hand instead of reading
/// them from here (#169) — exactly the same hardcoding an exhaustive
/// `dest_rel` test would have repeated a third time. The referenced IDs were
/// ALREADY in the corpus (#129 and the C2–C5 audits); this function is an
/// INDEX over them, not new fixtures.
///
/// # Panics
/// Never with the committed corpus: every referenced `id` is validated in
/// tests.
#[must_use]
pub fn spelling_twins() -> Vec<SpellingTwin> {
    vec![
        // é NFC / é NFD (e + combining acute): the base normalization pair,
        // with no fold in between.
        SpellingTwin {
            left: "nfc_e_acute",
            right: "nfd_e_acute",
            kind: TwinKind::Normalization,
        },
        // The same pair, but WITH AN EXTENSION. It exists separately because
        // what it pins is different: both classify the same —normalization
        // does not touch `.png`'s ASCII bytes— and yet they are two files
        // that coexist on ext4. The day someone "fixes" a sibling search by
        // matching normalized names, it will open whichever comes first.
        SpellingTwin {
            left: "image_ext_nfc",
            right: "image_ext_nfd",
            kind: TwinKind::Normalization,
        },
        // ΟΔΟΣ / οδοσ: `str::to_lowercase` applies Final_Sigma and gives ς,
        // which is NOT the simple fold.
        SpellingTwin {
            left: "greek_uppercase_final_sigma",
            right: "greek_medial_sigma_twin",
            kind: TwinKind::CaseFold,
        },
        // µ (U+00B5 MICRO SIGN) / μ (U+03BC GREEK SMALL LETTER MU): Unicode
        // already calls the micro sign lowercase, so `to_lowercase` does not
        // move it — only the fold does.
        SpellingTwin {
            left: "micro_sign_mu",
            right: "greek_mu_twin",
            kind: TwinKind::CaseFold,
        },
        // Orthodox / orthodox: the age-old fold, in plain ASCII. The other
        // eight pairs are non-ASCII exotica, so a path that only breaks on
        // ordinary uppercase —a layout name compared byte-for-byte and
        // recomposed into a file (#245)— had no fixture to catch it.
        SpellingTwin {
            left: "ascii_case_twin_upper",
            right: "ascii_case_twin_lower",
            kind: TwinKind::CaseFold,
        },
        // ﬅ / ﬆ: the only ligature with a simple fold.
        SpellingTwin {
            left: "ligature_long_st",
            right: "ligature_st",
            kind: TwinKind::CaseFold,
        },
        // J+◌̌ (decomposed) / ǰ (precomposed): folding RECOMPOSES, so the
        // fold and the normalization happen together.
        SpellingTwin {
            left: "nfd_uppercase_composed_only_lowercase",
            right: "precomposed_lowercase_j_caron",
            kind: TwinKind::CaseFold,
        },
        // straße.txt / strasse.txt: ß only has a FULL fold (to "ss"), which
        // expands — matches on ext4 `+F` and not on APFS/NTFS (#145).
        SpellingTwin {
            left: "ext4_full_fold_es_zett",
            right: "ext4_full_fold_ss",
            kind: TwinKind::CaseFoldFull,
        },
        // ﬁle.txt / file.txt: the SAME shape as the ß pair, in the family the
        // ß does not reach. Until #214 the expansion table was only
        // exercised by `ß`, so deleting every other row would not have made
        // a test go red.
        SpellingTwin {
            left: "full_fold_fi_ligature",
            right: "full_fold_fi_plain",
            kind: TwinKind::CaseFoldFull,
        },
        // ﬔ.txt / մե.txt: a row the table was MISSING (#214), and not ASCII
        // on either side — which is what catches an expansion table written
        // as if only ASCII ever came out the other side.
        SpellingTwin {
            left: "full_fold_armenian_ligature",
            right: "full_fold_armenian_plain",
            kind: TwinKind::CaseFoldFull,
        },
        // nom­bre.txt / nombre.txt: the full fold does not only EXPAND, it
        // also DROPS (#214). A soft hyphen is a Default_Ignorable, and the
        // kernel generates its tables in the `nfdicf` variant — the `i` is
        // "ignore default ignorables"—, so under `+F` the two are one file.
        // It is the only pair in the corpus the reader cannot tell apart by
        // looking at it.
        SpellingTwin {
            left: "full_fold_soft_hyphen",
            right: "full_fold_soft_hyphen_plain",
            kind: TwinKind::CaseFoldFull,
        },
        // U+212A KELVIN SIGN / K: the pair that is NOT the same spelling and
        // matches anyway, because NFC has singleton decompositions (#152).
        // The other five pairs in this list are one file written two ways;
        // this one is two files, and they coexist fine on ext4.
        SpellingTwin {
            left: "singleton_kelvin_sign",
            right: "ascii_capital_k",
            kind: TwinKind::NormalizationSingleton,
        },
        // The same pair with a raw tail: the key normalizes the valid prefix
        // of a name that is not whole text (#154), so these two also match —
        // and a singleton detector that demanded UTF-8 over the WHOLE name
        // would call them the same text, which is the expensive false
        // negative.
        SpellingTwin {
            left: "singleton_kelvin_sign_invalid_tail",
            right: "ascii_capital_k_invalid_tail",
            kind: TwinKind::NormalizationSingleton,
        },
    ]
}

/// A file content in a non-UTF8 encoding.
#[derive(Debug, Clone)]
pub struct ContentFixture {
    /// Stable identifier.
    pub id: &'static str,
    /// WHATWG label of the encoding (the one `encoding_rs` would understand).
    pub encoding: &'static str,
    /// The file's raw bytes.
    pub bytes: Vec<u8>,
    /// The text a correct decoder must produce.
    pub decoded: &'static str,
}

/// The 11 canonical DETECTABLE contents. Base text: `"año 2026\n"` (ñ outside
/// ASCII), `"テスト\n"` for Shift-JIS, or `"it’s\n"` for windows-1252's
/// divergent 0x80–0x9F zone. Generated in code: deterministic,
/// self-documenting, no opaque binaries in the repo.
#[must_use]
pub fn content_fixtures() -> Vec<ContentFixture> {
    const TEXT: &str = "año 2026\n";
    let utf16 = |big_endian: bool| -> Vec<u8> {
        let bom: [u8; 2] = if big_endian {
            [0xFE, 0xFF]
        } else {
            [0xFF, 0xFE]
        };
        let mut out = bom.to_vec();
        for unit in TEXT.encode_utf16() {
            let b = if big_endian {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            };
            out.extend_from_slice(&b);
        }
        out
    };
    vec![
        ContentFixture {
            id: "utf8_plain",
            encoding: "utf-8",
            bytes: TEXT.as_bytes().to_vec(),
            decoded: TEXT,
        },
        ContentFixture {
            // 0x95 0x32 0x82 0x36 = U+20000 (4 bytes, GB18030-exclusive
            // zone): catches decoders that stay stuck on "classic" GBK.
            id: "gb18030",
            encoding: "gb18030",
            bytes: b"\x95\x32\x82\x36 2026\n".to_vec(),
            decoded: "\u{20000} 2026\n",
        },
        ContentFixture {
            // PURE Cyrillic: KOI8-R and KOI8-U agree on the letters (they
            // differ in box-drawing) — chardetng may say KOI8-U and the
            // decode is still exact.
            id: "koi8_r",
            encoding: "koi8-r",
            bytes: b"\xf0\xd2\xc9\xd7\xc5\xd4 2026\n".to_vec(),
            decoded: "Привет 2026\n",
        },
        ContentFixture {
            id: "utf16le_bom",
            encoding: "utf-16le",
            bytes: utf16(false),
            decoded: TEXT,
        },
        ContentFixture {
            id: "utf16be_bom",
            encoding: "utf-16be",
            bytes: utf16(true),
            decoded: TEXT,
        },
        ContentFixture {
            id: "latin1",
            encoding: "windows-1252",
            bytes: b"a\xF1o 2026\n".to_vec(),
            decoded: TEXT,
        },
        ContentFixture {
            // 0x92 = ’ in windows-1252 but control U+0092 in strict
            // ISO-8859-1: catches decoders that mix up the two (ñ = 0xF1
            // does not distinguish, it is identical in both).
            id: "windows_1252_curly",
            encoding: "windows-1252",
            bytes: b"it\x92s\n".to_vec(),
            decoded: "it\u{2019}s\n",
        },
        ContentFixture {
            id: "shift_jis",
            // テスト in Shift-JIS + newline.
            encoding: "shift_jis",
            bytes: vec![0x83, 0x65, 0x83, 0x58, 0x83, 0x67, 0x0A],
            decoded: "テスト\n",
        },
        ContentFixture {
            id: "utf8_bom",
            encoding: "utf-8",
            bytes: {
                let mut v = vec![0xEF, 0xBB, 0xBF];
                v.extend_from_slice(TEXT.as_bytes());
                v
            },
            decoded: TEXT,
        },
        ContentFixture {
            // False positive of the 1-byte LEGACY needle: "ñ" in
            // windows-1252/ISO-8859-15 = 0xF1, which in UTF-8 appears as the
            // LEAD byte of a 4-byte sequence. `F1 84 80 81` = U+44001: blind
            // literal mode matches 0xF1 BY CHANCE; the ENCODING-AWARE search
            // (needle = 0xC3 0xB1) does NOT. Canonicalizes the boundary that
            // until now only lived inline in engine_search.rs.
            id: "cjk_utf8_lead_f1",
            encoding: "utf-8",
            bytes: CJK_UTF8_LEAD_F1.as_bytes().to_vec(),
            decoded: CJK_UTF8_LEAD_F1,
        },
        ContentFixture {
            // Injection through fs.search's PREVIEW: the needle + RLO
            // (202E) + an UNCLOSED isolate (2066) + ESC+OSC
            // (`\x1b]0;pwn\x07`, changes the terminal's title) + a raw C0
            // (SOH). A consumer that paints the preview directly would
            // execute the ANSI and see the forged visual order. The
            // producer MUST sanitize it at the source
            // (mask_terminal_hazards): no char from is_terminal_hazard
            // survives. Valid UTF-8 and no NUL → detectable as text.
            id: "preview_bidi_ctrl_injection",
            encoding: "utf-8",
            bytes: PREVIEW_BIDI_CTRL_INJECTION.as_bytes().to_vec(),
            decoded: PREVIEW_BIDI_CTRL_INJECTION,
        },
    ]
}

/// Contents a CORRECT decoder produces WITH LOSS (`had_errors`): detected as
/// text (with a BOM's certainty) but carrying a byte invalid for that
/// encoding, so the canonical decode inserts `U+FFFD`.
///
/// Kept apart from [`content_fixtures`] on purpose — that corpus's contract
/// is "detect as text and decode EXACT and lossless", and its tests assert
/// that in a loop. These are the needle for the plugin preview's `lossy`
/// signal (#101) and for any consumer of the honest "this came from a
/// failed decode, not from the file". `decoded` is what the correct decoder
/// produces: it already carries the `U+FFFD`.
#[must_use]
pub fn lossy_content_fixtures() -> Vec<ContentFixture> {
    vec![ContentFixture {
        // UTF-8 BOM (EF BB BF) → UTF-8 detection with CERTAINTY (not
        // statistical: without the BOM, chardetng would pick windows-1252
        // where 0xFF is `ÿ` and the decode would come out clean). The
        // interior `0xFF` is NEVER valid in UTF-8 → `U+FFFD` with
        // had_errors.
        id: "utf8_bom_invalid",
        encoding: "utf-8",
        bytes: {
            let mut v = vec![0xEF, 0xBB, 0xBF];
            v.extend_from_slice(b"a\xFFo 2026\n");
            v
        },
        decoded: "a\u{FFFD}o 2026\n",
    }]
}

/// A line with `0xF1` as the lead byte of a 4-byte char (`U+44001`): the
/// short Latin needle `ñ` (0xF1 in legacy) matches by chance in blind mode.
pub(crate) const CJK_UTF8_LEAD_F1: &str = "汉字 \u{44001} texto\n";

/// Hostile line for `fs.search`'s preview: needle `aguja` + RLO + an
/// unclosed isolate + ESC+OSC + a raw C0. No terminal-hazard char must
/// survive sanitizing at the source.
pub(crate) const PREVIEW_BIDI_CTRL_INJECTION: &str =
    "aguja \u{202E}reovni\u{2066} \u{1B}]0;pwn\u{07}\u{01}fin\n";

/// A hostile chord from the corpus (encoding audit H1): a token of ONE
/// single codepoint, chosen from [`norte_encoding::is_terminal_hazard`],
/// that `norte_frontend::keymap::parse_chord` accepts outright as a
/// `KeyCode::Char` — ANY loose codepoint parses, the keymap engine does not
/// filter hazards (that is not its job; see the comment in `parse_chord`).
/// A `./.norte/keymap.toml` (PROJECT layer, no trust) can bind one of these
/// to a supported command; `Chord`'s `Display` writes it RAW on purpose
/// (logs/debug want the real chord), so every consumer that paints the
/// FORMATTED chord (generated help, palette) must mask it — this corpus
/// exercises that render-side obligation.
#[derive(Debug, Clone)]
pub struct HostileChord {
    /// Stable identifier (for test names and messages).
    pub id: &'static str,
    /// The token, exactly as it would go in a keymap.toml's `on = [...]`
    /// (a single codepoint).
    pub token: char,
    /// Why it is hostile (living documentation).
    pub why: &'static str,
}

/// The 4 canonical hostile chords: a single codepoint each (two or more
/// codepoints are already rejected by `parse_chord`, see
/// `parse_chord_rechaza_tokens_multi_codepoint_sin_partir` in
/// `norte-frontend`), each a different terminal hazard.
///
/// ```
/// let chords = norte_testkit::corpus::hostile_chords();
/// assert_eq!(chords.len(), 4);
/// // All are terminal hazards detected through the single source.
/// for c in &chords {
///     assert!(norte_encoding::is_terminal_hazard(c.token), "{}", c.id);
/// }
/// ```
#[must_use]
pub fn hostile_chords() -> Vec<HostileChord> {
    vec![
        HostileChord {
            id: "rlo",
            token: '\u{202E}',
            why: "RIGHT-TO-LEFT OVERRIDE: visually reorders EVERYTHING that \
                  follows on the line — in a footer `[chord] label` it can \
                  make cancel/confirm look swapped",
        },
        HostileChord {
            id: "zwsp",
            token: '\u{200B}',
            why: "ZERO WIDTH SPACE: invisible, two chords bound to \
                  different commands can render indistinguishably",
        },
        HostileChord {
            id: "lrm",
            token: '\u{200E}',
            why: "LEFT-TO-RIGHT MARK: an invisible bidi override, alters \
                  the visual order of neighboring RTL text without leaving \
                  a visible mark",
        },
        HostileChord {
            id: "bel",
            token: '\u{0007}',
            why: "BEL (C0 control): an unsanitized terminal EXECUTES it \
                  (bell/beep) instead of painting it as text",
        },
    ]
}

/// A hostile COMMAND NAME: the `run = "..."` side of a keymap binding.
///
/// [`hostile_chords`] covers the KEY side of a `keymap.toml` — one codepoint,
/// because `parse_chord` rejects anything longer. `run` is the other half of
/// the same untrusted line and a different shape: a whole string, from the
/// same file, and since K1 (ADR 0043) the shared catalogue decides whether it
/// becomes a *declared unavailability* (catalogue-known, so byte-equal to a
/// `&'static str`) or an `UnknownCommand` diagnostic (anything else — which
/// is to say, every string in this family). The diagnostic path is the one
/// that prints attacker-controlled bytes, so it is the one that must mask.
#[derive(Debug, Clone)]
pub struct HostileRun {
    /// Stable identifier (for test names and messages).
    pub id: &'static str,
    /// The command name, exactly as it would go in a keymap.toml's
    /// `run = "..."` — all expressible with `\uXXXX` in a basic TOML
    /// string, which is how they would really arrive.
    pub run: &'static str,
    /// Why it is hostile (living documentation).
    pub why: &'static str,
}

/// The 4 canonical hostile `run`s. NONE is in the shared catalogue (the
/// lookup is byte equality), so all four are `KeymapError::UnknownCommand` —
/// the classification is correct and what lies is the RENDER. That is why
/// this corpus exercises masking, not lookup.
///
/// ```
/// let runs = norte_testkit::corpus::hostile_runs();
/// assert_eq!(runs.len(), 4);
/// // Each one carries at least one terminal hazard, through the single source.
/// for r in &runs {
///     assert!(
///         r.run.chars().any(norte_encoding::is_terminal_hazard),
///         "{}",
///         r.id
///     );
/// }
/// ```
#[must_use]
pub fn hostile_runs() -> Vec<HostileRun> {
    vec![
        HostileRun {
            id: "run_rlo_catalogue_twin",
            run: "app.\u{202E}tiuq",
            why: "RIGHT-TO-LEFT OVERRIDE: it is PAINTED as `app.quit`. \
                  `norte doctor`'s warning names a command the user cannot \
                  tell apart from the legitimate one, so they \"fix\" the \
                  wrong one. Proves the fix is masking, not a better lookup",
        },
        HostileRun {
            id: "run_zwsp_catalogue_twin",
            run: "app.qu\u{200B}it",
            why: "ZERO WIDTH SPACE: invisible. The printed name is \
                  identical to the real one and different in bytes, so the \
                  diagnostic is literally unactionable",
        },
        HostileRun {
            id: "run_osc_title_injection",
            run: "app.quit\u{001B}]0;pwned\u{0007}",
            why: "ESC + OSC 0 + BEL: an unsanitized terminal EXECUTES the \
                  sequence and changes its title. The C0 half is the one \
                  `escape_debug` DOES catch — which is why `{:?}` alone is \
                  not enough",
        },
        HostileRun {
            id: "run_lo_invisible",
            run: "pane.copy\u{3164}",
            why: "HANGUL FILLER: a norte hazard (#125) that `escape_debug` \
                  does NOT escape, because it is Lo and not Cf. This is the \
                  one that shows the `{:?}` path's accidental protection \
                  does not reach far enough",
        },
    ]
}

/// A hostile DISPLAY TITLE: prose meant to be painted into a narrow column,
/// not a filename.
///
/// The other two families of this corpus cover the surfaces norte had until
/// now: [`hostile_names`] is BYTES off a filesystem, [`hostile_chords`] is a
/// single codepoint out of a keymap. A title is neither — it is a whole
/// string of editorial text, it is TRUNCATED to fit a sidebar or a column,
/// and from the help overlay (H3b) onwards it is also a surface a plugin
/// manifest can feed. The hazards that shape live on the CUT: two titles that
/// become the same string once truncated, a combining mark orphaned onto the
/// ellipsis, a grapheme cluster split down the middle.
///
/// Truncation of a title is by the RIGHT (`norte_tui::ui::right_ellipsis`):
/// head plus tail collides any two labels that agree on both ends, so a label
/// keeps its distinct prefix and loses its tail. [`HostileTitle::twin`] is
/// built for THAT rule — the pair shares everything up to the cut.
#[derive(Debug, Clone)]
pub struct HostileTitle {
    /// Stable identifier (for test names and messages).
    pub id: &'static str,
    /// The title itself, as an author or a plugin manifest would write it.
    pub text: &'static str,
    /// The other half of a COLLIDING pair, when the hazard needs two strings.
    ///
    /// A collision cannot be expressed by one string: it is a property of a
    /// PAIR that renders identically once cut. `None` for the fixtures whose
    /// hazard is internal to a single title.
    pub twin: Option<&'static str>,
    /// Why it is hostile (living documentation).
    pub why: &'static str,
}

/// A line of the horizontal-scroll grid fixture ([`viewer_grid_lines`]).
#[derive(Debug, Clone)]
pub struct GridLine {
    /// Stable identifier (for test names and messages).
    pub id: &'static str,
    /// The line itself, without its newline.
    pub text: String,
    /// Why it is hostile to a cut by the LEFT (living documentation).
    pub why: &'static str,
}

/// The canonical fixture for a viewer that scrolls SIDEWAYS.
///
/// Deliberately NOT part of [`content_fixtures`]: that corpus's contract is
/// «detect as text and decode byte-exact», and it is swept by the encoding and
/// search suites, which have no business with a 200-column emoji line. This
/// one exists for the other invariant — **a cut by the left keeps every row on
/// the same column grid, and never starts a row on something of width zero.**
///
/// Every line is at least 200 cells wide and built so the same marker sits at
/// the same visual column on all of them, which is what makes the property
/// assertable by sweeping `h in 0..max_cols` instead of pinning one magic
/// offset. What each line attacks is in its `why`.
///
/// ```
/// let lines = norte_testkit::corpus::viewer_grid_lines();
/// assert!(lines.len() >= 6);
/// // Every line is wider than any reasonable window.
/// for l in &lines {
///     assert!(l.text.chars().count() >= 20, "{}", l.id);
/// }
/// // And the reference row is plain ASCII: it is what the others align to.
/// let ruler = lines.iter().find(|l| l.id == "ascii_ruler").unwrap();
/// assert!(ruler.text.is_ascii());
/// ```
#[must_use]
pub fn viewer_grid_lines() -> Vec<GridLine> {
    vec![
        GridLine {
            id: "ascii_ruler",
            // `0123456789` repeated twenty times: column N carries digit
            // N % 10, so a shift reads by eye in a test's failure output.
            text: "0123456789".repeat(20),
            why: "the reference row. Plain ASCII, one cell per character, so \
                  it is what every other line's columns must line up with \
                  after the same shift",
        },
        GridLine {
            id: "cjk_double_width",
            text: "漢".repeat(120),
            why: "an ideograph is TWO cells, so at every odd offset the cut \
                  falls in the middle of one. It cannot be painted in half, \
                  so it goes entirely — and the cell it leaves has to be \
                  filled, or this row slides one column against its \
                  neighbours and an aligned log stops being aligned",
        },
        GridLine {
            id: "nfd_combining",
            text: "e\u{301}".repeat(120),
            why: "NFD: the acute is its OWN codepoint of width 0, so at every \
                  odd offset the remainder would BEGIN on a mark whose base \
                  is on the other side of the cut. macOS hands out NFD by \
                  default, so this is the ordinary case",
        },
        GridLine {
            id: "zwj_family",
            text: "👨\u{200D}👩\u{200D}👧\u{200D}👦".repeat(30),
            why: "a ZWJ cluster is several codepoints painted as ONE glyph. A \
                  cut inside it that keeps the joiner paints a DIFFERENT \
                  family from the one in the file, and says nothing — unlike \
                  a truncation by the right, which at least writes a `…`",
        },
        GridLine {
            id: "emoji_presentation",
            text: "✔\u{FE0F}🇪🇸".repeat(60),
            why: "VS16 and a regional-indicator pair paint NARROWER than the \
                  sum of their parts' per-character widths. Whatever a walker \
                  counts, the CAP has to count the same way, or the scroll \
                  reaches where the walker cannot follow and the row goes \
                  blank",
        },
        GridLine {
            id: "wide_before_tab",
            text: "漢\tx".repeat(40),
            why: "a tab stop is a COLUMN, so the ideograph before it spends \
                  two. Counting the stop per character puts the `x` one \
                  column off and every column after it on that line drifts",
        },
    ]
}

/// The 4 canonical hostile titles.
///
/// ```
/// let titles = norte_testkit::corpus::hostile_titles();
/// assert_eq!(titles.len(), 4);
///
/// // The colliding pair shares a long prefix: cut short enough, both sides
/// // render the same string.
/// let pair = titles.iter().find(|t| t.id == "truncation_twins").unwrap();
/// let twin = pair.twin.expect("a collision needs two strings");
/// assert_ne!(pair.text, twin);
/// assert_eq!(&pair.text[..30], &twin[..30]);
///
/// // The bidi-isolate title is LEGITIMATE editorial text that is made of
/// // terminal hazards: it is what makes a hazard sweep over prose a live
/// // constraint and not a hypothetical.
/// let bidi = titles.iter().find(|t| t.id == "bidi_isolate_url").unwrap();
/// assert!(bidi.text.chars().any(norte_encoding::is_terminal_hazard));
/// ```
#[must_use]
pub fn hostile_titles() -> Vec<HostileTitle> {
    vec![
        HostileTitle {
            id: "truncation_twins",
            text: "Copiar al host remoto (SFTP, puerto 22)",
            twin: Some("Copiar al host remoto (SFTP, puerto 2222)"),
            why: "two DIFFERENT titles that share everything up to the cut: \
                  right-truncated into a sidebar column both read `Copiar al \
                  host remo…`, so a reader picking one of the two rows cannot \
                  tell which page they are opening. Nothing can prevent the \
                  collision in a narrow column — what a frontend owes is that \
                  the cut is MARKED (the `…`), never a silent equality",
        },
        HostileTitle {
            id: "nfd_accent_on_the_cut",
            // `cafe` + U+0301: the accent is its OWN codepoint, of width 0.
            text: "Copiar cafe\u{301}.txt al otro panel",
            twin: None,
            why: "an NFD combining acute is width 0, so a truncator that \
                  walks by CELLS never spends budget on it: the mark can \
                  survive its base character and end up composed onto the \
                  ellipsis (`caf…` painted as `caf´…`), moving an accent onto \
                  a glyph the author never wrote. macOS hands out NFD by \
                  default, so this is the ordinary case, not the exotic one",
        },
        HostileTitle {
            id: "zwj_cluster_on_the_cut",
            text: "Marcar 👨\u{200D}👩\u{200D}👧\u{200D}👦 y copiar",
            twin: None,
            why: "a ZWJ emoji cluster is several codepoints painted as ONE \
                  glyph. A cut that falls inside it turns one family into two \
                  or three unrelated people, and a cut that leaves the tail \
                  starting on the joiner composes the joiner onto the \
                  ellipsis. ZWJ is deliberately NOT masked (see `must_mask`), \
                  so a truncator cannot lean on masking to avoid it",
        },
        HostileTitle {
            id: "bidi_isolate_url",
            // U+2066 LRI … U+2069 PDI: the CORRECT way to put an LTR URL
            // inside RTL prose.
            text: "\u{2066}sftp://host/ruta\u{2069} en el panel derecho",
            twin: None,
            why: "the LEGITIMATE case: `U+2066`..`U+2069` are how an RTL \
                  locale keeps an LTR run (a URL, a path, a command id) from \
                  reordering the sentence around it — and every one of them \
                  is in `norte_encoding::is_terminal_hazard`. So a corpus \
                  hazard sweep is not a hypothetical the day an RTL \
                  translation lands: it is the gate that forces the choice \
                  between isolating the run and shipping raw bidi controls to \
                  a terminal to be a deliberate one",
        },
    ]
}

/// A path of CLEAN UTF-8 segments whose `path_display` goes over the bridge's
/// `MAX_STRING_BYTES` (4096) (#277).
///
/// Returns the segments, not a name: 4096 bytes do not fit in ONE filename on
/// any filesystem (`NAME_MAX` is 255), so a fixture that tried would only be
/// testing the archive writer's refusal. Each segment here is 210 bytes and
/// there are 24 of them, which is an ordinary deep tree.
///
/// It is the only fixture that turns the CLAMP red on its own.
/// `display_expansion_over_clamp` cannot: its `0xFF` bytes take the lossy
/// path, so its `hostile` flag is already `true` for the other reason, and a
/// test using it cannot tell "it was masked" from "it was cut". Here there is
/// nothing to mask — `display_name` returns `false` — and the only thing that
/// happens is that the composed path passes the ceiling and `clamp_display`
/// hands back a value ending in `U+2026`, a character that is legal in a name
/// and that no flag reports. A reader who seeds an editable field with it
/// renames to a name that is not the one they saw.
///
/// ```
/// let segs = norte_testkit::corpus::clean_utf8_path_over_clamp();
/// let total: usize = segs.iter().map(Vec::len).sum::<usize>() + segs.len();
/// assert!(total > 4096, "{total}");
/// // Every segment is a legal filename on its own.
/// for s in &segs {
///     assert!(s.len() <= 255);
///     assert!(std::str::from_utf8(s).is_ok());
///     assert!(norte_proto::Segment::new(s.clone()).is_ok());
/// }
/// ```
#[must_use]
pub fn clean_utf8_path_over_clamp() -> Vec<Vec<u8>> {
    (0..24)
        .map(|i| format!("{}{i:02}", "a".repeat(208)).into_bytes())
        .collect()
}

/// A hostile AUTHORITY: the `host` (and sometimes the scheme) of a remote
/// location, as it reaches a connection banner, a places row or a task detail.
///
/// The corpus had names, chords, runs and titles, and no authorities (#277).
/// An authority is neither a filename nor prose: it is what tells a reader
/// WHICH machine their files are going to, so every hazard here is a hazard
/// about identity rather than about layout.
#[derive(Debug, Clone)]
pub struct HostileHost {
    /// Stable identifier (for test names and messages).
    pub id: &'static str,
    /// The scheme, when the fixture is about the scheme rather than the host.
    pub scheme: &'static str,
    /// The authority, as a URL would carry it.
    pub host: &'static str,
    /// The other half of a COLLIDING pair, when the hazard needs two.
    pub twin: Option<&'static str>,
    /// Why it is hostile (living documentation).
    pub why: &'static str,
}

/// The 6 canonical hostile authorities (#277).
///
/// ```
/// let hosts = norte_testkit::corpus::hostile_hosts();
/// assert!(hosts.len() >= 6);
///
/// // The homograph pair is two DIFFERENT strings that paint the same.
/// let h = hosts.iter().find(|h| h.id == "host_idn_homograph").unwrap();
/// assert_ne!(h.host, h.twin.expect("a homograph needs its twin"));
///
/// // And the userinfo spoof really carries the `@`: everything before it is
/// // decoration, and the host is what comes after.
/// let u = hosts.iter().find(|h| h.id == "host_userinfo_spoof").unwrap();
/// assert!(u.host.contains('@'));
/// ```
#[must_use]
pub fn hostile_hosts() -> Vec<HostileHost> {
    vec![
        HostileHost {
            id: "host_userinfo_spoof",
            scheme: "sftp",
            host: "bank.example@evil.example",
            twin: Some("bank.example"),
            why: "URL userinfo: everything before the `@` is a username and \
                  the machine is what comes AFTER. A banner that paints the \
                  authority whole tells the reader they are connected to \
                  `bank.example` while the bytes go to `evil.example`. The \
                  twin is the legitimate host it impersonates",
        },
        HostileHost {
            id: "host_inband_sentence_break",
            scheme: "sftp",
            host: "host.example, sin cifrar",
            twin: None,
            why: "an authority that IS the rest of the sentence a banner \
                  builds around it. Every byte is ordinary printable text, so \
                  nothing masks it and no `hostile` bit fires: the row reads \
                  as a complete warning the host never wrote. Same family as \
                  `cause_join_spoof`, aimed at an authority instead of a \
                  filename",
        },
        HostileHost {
            id: "host_bidi_override",
            scheme: "sftp",
            host: "evil.example\u{202E}moc.knab",
            twin: None,
            why: "a bidi override inside an authority: painted in a terminal \
                  the tail reverses and the row reads as `bank.com`. Unlike a \
                  filename, an authority is not usually run through the same \
                  masking, which is exactly what this fixture is for",
        },
        HostileHost {
            id: "host_idn_homograph",
            // `ра` here is Cyrillic U+0440 U+0430.
            scheme: "https",
            host: "\u{440}a\u{443}pal.example",
            twin: Some("paypal.example"),
            why: "Cyrillic homographs in an IDN authority. The two strings \
                  are different by every byte and identical to a reader, so \
                  the only defence is showing the punycode or marking the \
                  mixed script — and neither happens by accident",
        },
        HostileHost {
            id: "host_truncation_twins",
            scheme: "sftp",
            host: "produccion.equipo.almacen.interno.example.org",
            twin: Some("produccion.equipo.almacen.interno.example.net"),
            why: "two FQDNs that differ only in the TLD and share the first \
                  44 characters: cut to 48 cells with an ellipsis they still \
                  differ, cut shorter they do not. It is the authority twin \
                  of `truncation_twins`, and what it demands is the same — \
                  that the cut be MARKED, never a silent equality",
        },
        HostileHost {
            id: "scheme_unbounded",
            // 512 characters, none of them a scheme anyone registered.
            scheme: "sftpsftpsftpsftpsftpsftpsftpsftpsftpsftpsftpsftpsftpsftp",
            host: "host.example",
            twin: None,
            why: "a scheme with no ceiling. Banners and places rows compose \
                  `<scheme>://<host>` and size their columns from it, so an \
                  unbounded scheme pushes the host — the part that identifies \
                  the machine — off the visible end of the row. The scheme is \
                  the half a reader ignores, which is what makes it the good \
                  place to hide",
        },
    ]
}

/// A hostile plugin/topic ID: a KEY, not prose.
///
/// The distinction is the whole point of the family. A title is text a human
/// reads and a frontend may mask; an id is what `TopicId` holds, what travels
/// as the argument of `plugin.help` on the wire, and what the sidebar filter
/// folds on every keystroke. `parse_untrusted` deliberately does NOT mask an
/// id — masking a key would change what it addresses — so every hazard here
/// reaches whoever compares, cuts or folds ids.
#[derive(Debug, Clone)]
pub struct HostileTopicId {
    /// Stable identifier (for test names and messages).
    pub id: &'static str,
    /// The id itself, as a plugin manifest would declare it.
    pub text: String,
    /// The other half of a COLLIDING pair, when the hazard needs two ids.
    ///
    /// `None` when the hazard is internal to a single id. For the pairs, the
    /// contract under test is that the two stay DIFFERENT: a step that maps
    /// them to one string (a clamp, an NFC pass) is the regression.
    pub twin: Option<String>,
    /// Why it is hostile (living documentation).
    pub why: &'static str,
}

/// The 4 canonical hostile topic ids (#263).
///
/// ```
/// let ids = norte_testkit::corpus::hostile_topic_ids();
/// assert!(ids.len() >= 4);
///
/// // The clamp twins differ, and only AFTER the byte where a 4096-byte
/// // ceiling would have cut them.
/// let pair = ids.iter().find(|i| i.id == "id_clamp_twins").unwrap();
/// let twin = pair.twin.as_ref().expect("a collision needs two ids");
/// assert_ne!(&pair.text, twin);
/// assert_eq!(&pair.text[..4093], &twin[..4093]);
///
/// // The invisible twin is BLANK by the parser's definition, which is what
/// // `is_blank_id` exists for: a page named with it has no name at all.
/// let blank = ids.iter().find(|i| i.id == "id_hangul_filler").unwrap();
/// assert!(blank.text.ends_with('\u{3164}'));
/// ```
#[must_use]
pub fn hostile_topic_ids() -> Vec<HostileTopicId> {
    // 4093 shared bytes: the clamp that matters is `MAX_STRING_BYTES` (4096),
    // and the pair has to be identical UP TO it and different after.
    let base = format!("org.acme.{}", "a".repeat(4093 - "org.acme.".len()));
    vec![
        HostileTopicId {
            id: "id_clamp_twins",
            text: format!("{base}uno"),
            twin: Some(format!("{base}dos")),
            why: "two reverse-DNS ids identical through byte 4093 and \
                  different after: a clamp to `MAX_STRING_BYTES` maps both to \
                  ONE string. On a title that is cosmetic — the reader sees \
                  an ellipsis and knows something was cut. On a KEY it is \
                  not: two extensions become one row, and activating it \
                  addresses whichever the map happened to keep",
        },
        HostileTopicId {
            id: "id_rlo",
            text: "org.acme.\u{202E}ptfs".to_owned(),
            twin: None,
            why: "a bidi override INSIDE an id. `parse_untrusted` masks prose \
                  and deliberately leaves ids alone — masking a key changes \
                  what it addresses — so this reaches every surface that \
                  paints an id raw. Painted in a terminal it reads `sftp`, \
                  which is the point of writing it that way",
        },
        HostileTopicId {
            id: "id_hangul_filler",
            text: "org.acme.demo\u{3164}".to_owned(),
            twin: Some("org.acme.demo".to_owned()),
            why: "U+3164 HANGUL FILLER is a zero-width character that is NOT \
                  whitespace, so `trim` keeps it and the two ids stay \
                  different while rendering identically. It is the exact case \
                  `norte_help::is_blank_id` exists for, and the twin is the \
                  legitimate id it shadows",
        },
        HostileTopicId {
            id: "id_nfd_pair",
            text: "org.acme.cafe\u{301}".to_owned(),
            twin: Some("org.acme.caf\u{e9}".to_owned()),
            why: "the SAME id in NFD and NFC. These are two ids and must stay \
                  two: nothing on this path normalises, and the fixture is \
                  here so that whoever adds a normalisation step finds an \
                  assertion saying so. macOS hands out NFD by default, so a \
                  plugin authored there and one authored on Linux declare \
                  different bytes for what a human reads as one name",
        },
    ]
}

/// The bytes of a third-party `help.md`, before any decoding.
///
/// Bytes and not `&str` on purpose: half the family is about the DECODING
/// (windows-1252, UTF-16LE with a BOM), and a `&str` would have decided that
/// question before the test starts.
#[derive(Debug, Clone)]
pub struct HostileHelpDoc {
    /// Stable identifier (for test names and messages).
    pub id: &'static str,
    /// The raw bytes of the file, exactly as a plugin would ship them.
    pub bytes: Vec<u8>,
    /// The publisher, which is the OTHER input to the same door.
    ///
    /// It is not front matter: it comes from the manifest and reaches
    /// `parse_untrusted` as a parameter. The badge that says WHO wrote the
    /// page a reader is reading is built from both halves, so a fixture about
    /// the badge has to carry both.
    pub publisher: Option<&'static str>,
    /// Why it is hostile (living documentation).
    pub why: &'static str,
}

/// The 6 canonical hostile `help.md` documents (#263).
///
/// Until this family existed every one of these cases was a literal inside
/// `norte-help`'s own parser tests, so neither `norte-ui-host` nor the
/// renderer's contract test could reach one.
///
/// ```
/// let docs = norte_testkit::corpus::hostile_help_docs();
/// assert!(docs.len() >= 6);
///
/// // The UTF-16LE page really carries a BOM: a decoder that reads it as
/// // UTF-8 sees NUL bytes and falls to "binary".
/// let u16 = docs.iter().find(|d| d.id == "doc_utf16le_bom").unwrap();
/// assert_eq!(&u16.bytes[..2], &[0xFF, 0xFE]);
///
/// // And the windows-1252 one is NOT valid UTF-8, which is what makes the
/// // detection step observable.
/// let w = docs.iter().find(|d| d.id == "doc_windows1252_title").unwrap();
/// assert!(std::str::from_utf8(&w.bytes).is_err());
///
/// // The publisher is the OTHER input to the same door: it comes from the
/// // manifest, not from the front matter, so a badge fixture carries both.
/// let pub_ = docs.iter().find(|d| d.id == "doc_publisher_bidi").unwrap();
/// assert!(pub_.publisher.unwrap().contains('\u{202E}'));
/// assert!(!String::from_utf8_lossy(&pub_.bytes).contains("publisher"));
/// ```
#[must_use]
pub fn hostile_help_docs() -> Vec<HostileHelpDoc> {
    // `título` with the accented letter as a single windows-1252 byte (0xED),
    // which is not valid UTF-8 on its own.
    let mut w1252 = b"+++\nid = \"org.acme.demo\"\ntitle = \"T".to_vec();
    w1252.push(0xED);
    w1252.extend_from_slice(b"tulo\"\n+++\ncuerpo\n");

    let utf16le = {
        let text = "+++\nid = \"org.acme.demo\"\ntitle = \"Página\"\n+++\ncuerpo\n";
        let mut out = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    };

    let seventeen = {
        // `plugin:<id>:<cmd>`: a third party's help can only refer to ITS
        // OWN commands, so without the prefix the whole list falls out and
        // the header cap never even gets grazed.
        let list: Vec<String> = (0..17)
            .map(|i| format!("\"plugin:org.acme.demo:c{i}\""))
            .collect();
        format!(
            "+++\nid = \"org.acme.demo\"\ntitle = \"Muchos\"\ncommands = [{}]\n+++\ncuerpo\n",
            list.join(", ")
        )
        .into_bytes()
    };

    vec![
        HostileHelpDoc {
            id: "doc_windows1252_title",
            bytes: w1252,
            publisher: None,
            why: "a front-matter title in windows-1252: the byte 0xED is not \
                  valid UTF-8 on its own, so a reader that skips detection \
                  either rejects the whole page or paints a U+FFFD in the \
                  middle of a name. It pins that the DETECTED decoding \
                  reaches the projection and not just the parser",
        },
        HostileHelpDoc {
            id: "doc_utf16le_bom",
            bytes: utf16le,
            publisher: None,
            why: "the same page in UTF-16LE with a BOM. Read as UTF-8 it is \
                  full of NUL bytes and the heuristic calls it binary, so \
                  this is the case where the BOM is the only thing between a \
                  legible page and `no se puede mostrar`",
        },
        HostileHelpDoc {
            id: "doc_title_all_invisibles",
            bytes:
                "+++\nid = \"org.acme.demo\"\ntitle = \"\u{3164}\u{3164}\u{3164}\"\n+++\ncuerpo\n"
                    .as_bytes()
                    .to_vec(),
            publisher: None,
            why: "a title of three HANGUL FILLERs: not empty, not whitespace, \
                  and painted as nothing at all. The page has to fall back to \
                  the host id — a nameless row in a sidebar is a row nobody \
                  can name to report",
        },
        HostileHelpDoc {
            id: "doc_publisher_bidi",
            bytes: "+++\nid = \"org.acme.demo\"\ntitle = \"Demo\"\n+++\ncuerpo\n"
                .as_bytes()
                .to_vec(),
            publisher: Some("ACME\u{202E} \u{b7} cut short"),
            why: "a publisher carrying a bidi override right before the \
                  separator the badge fabricates. The badge is the surface \
                  that tells a reader WHO wrote the page they are reading, so \
                  a publisher that can reorder the text around the separator \
                  can make one plugin's page look like another's",
        },
        HostileHelpDoc {
            id: "doc_unterminated_fence",
            bytes: "+++\nid = \"org.acme.demo\"\ntitle = \"Valla\"\n+++\n```rust\nfn a() {}\n"
                .as_bytes()
                .to_vec(),
            publisher: None,
            why: "a code fence that never closes: everything after it is code \
                  until the end of the source. It is one edge of `Limits`, \
                  and the one where a parser that keeps looking for the \
                  closing fence reads the whole file into a single block",
        },
        HostileHelpDoc {
            id: "doc_17_commands",
            bytes: seventeen,
            publisher: None,
            why: "one command over `MAX_HEADER_COMMANDS` (16). The cap is a \
                  memory bound as much as a display one — the front matter is \
                  TOML and sits outside `Limits` — and every kept entry is a \
                  runnable row on the palette's dispatch path. The fixture \
                  pins the cut on the SURFACE and not only in the parser",
        },
    ]
}

fn hex_decode(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "even-length hex: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digits"))
        .collect()
}

/// Contents detection CANNOT resolve (spec §6: recoverable ONLY with a
/// forced "reload as…"): UTF-16 without a BOM (falls to binary through the
/// NUL heuristic — a deliberate contract) and a spurious BOM that is DATA.
///
/// The contract: `detect` gives no encoding, but `decode_forced` with the
/// label must be EXACT and lossless.
#[must_use]
pub fn content_fixtures_forced() -> Vec<ContentFixture> {
    const TEXT: &str = "año 2026\n";
    let utf16_nobom = |big_endian: bool| -> Vec<u8> {
        let mut out = Vec::new();
        for unit in TEXT.encode_utf16() {
            let b = if big_endian {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            };
            out.extend_from_slice(&b);
        }
        out
    };
    vec![
        ContentFixture {
            id: "utf16le_nobom",
            encoding: "utf-16le",
            bytes: utf16_nobom(false),
            decoded: TEXT,
        },
        ContentFixture {
            id: "utf16be_nobom",
            encoding: "utf-16be",
            bytes: utf16_nobom(true),
            decoded: TEXT,
        },
        ContentFixture {
            // FE FF as windows-1252 DATA (þÿ): a decode that sniffs the
            // BOM over the FORCED encoding violates "always fixable by
            // hand" (spec §6.2).
            id: "w1252_fake_bom",
            encoding: "windows-1252",
            bytes: b"\xFE\xFF Fahr.\n".to_vec(),
            decoded: "þÿ Fahr.\n",
        },
    ]
}

/// A POSIX mode word and how the listing must read it ([`posix_modes`]).
#[derive(Debug, Clone)]
pub struct PosixMode {
    /// Stable identifier (for test names and messages).
    pub id: &'static str,
    /// The mode word, as `st_mode` or an SFTP `permissions` field carries it.
    pub mode: u64,
    /// The ten cells the listing must paint, `ls` style.
    pub rwx: &'static str,
    /// The other half of a COLLIDING pair, when the hazard needs two modes.
    ///
    /// Like `HostileTitle::twin`: a collision is a property of a PAIR, not of
    /// one value. `None` when the hazard lives inside a single mode.
    pub twin: Option<u64>,
    /// Why it is hostile (living documentation).
    pub why: &'static str,
}

/// The canonical POSIX mode corpus.
///
/// It exists because the permissions column stopped being opt-in: it is now
/// on by default wherever a provider announces POSIX permissions, so every
/// mode word a real filesystem or a real SFTP server can produce is on
/// screen, in the column next to the name, on every listing.
///
/// Every entry renders to exactly ten cells, and that is the invariant the
/// column's `Fixed` width rests on:
///
/// ```
/// for m in norte_testkit::corpus::posix_modes() {
///     assert_eq!(m.rwx.chars().count(), 10, "{}", m.id);
///     assert!(m.rwx.is_ascii(), "{}", m.id);
/// }
/// ```
///
/// Two of them are the point of the exercise and are worth reading before
/// changing the formatter:
///
/// ```
/// let modes = norte_testkit::corpus::posix_modes();
/// // A mode with no type bits is NOT a regular file.
/// let classless = modes.iter().find(|m| m.id == "no_type_bits").unwrap();
/// assert!(classless.rwx.starts_with('?'));
/// assert_eq!(classless.twin, Some(0o100_644), "and its twin is one");
/// // Two modes whose first seven cells are identical: cut there, they lie.
/// let pair = modes.iter().find(|m| m.id == "truncation_twins").unwrap();
/// assert!(pair.twin.is_some());
/// ```
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "it is a TABLE: fifteen modes with their reason written beside \
              them. Splitting it in two halves would only hide half the list"
)]
pub fn posix_modes() -> Vec<PosixMode> {
    vec![
        PosixMode {
            id: "regular",
            mode: 0o100_644,
            rwx: "-rw-r--r--",
            twin: None,
            why: "the ordinary case, and the baseline every other entry is read against",
        },
        PosixMode {
            id: "dir",
            mode: 0o040_755,
            rwx: "drwxr-xr-x",
            twin: None,
            why: "a directory: the class the icon and the trailing `/` also claim, \
                  so a disagreement here is visible on the same row",
        },
        PosixMode {
            id: "symlink",
            mode: 0o120_777,
            rwx: "lrwxrwxrwx",
            twin: None,
            why: "a symlink always reports 0777; its own permissions mean nothing, \
                  the target's do",
        },
        PosixMode {
            id: "fifo",
            mode: 0o010_644,
            rwx: "prw-r--r--",
            twin: None,
            why: "a named pipe. Painted as a regular file it invites a copy that \
                  blocks forever on a reader that will never come",
        },
        PosixMode {
            id: "chardev",
            mode: 0o020_666,
            rwx: "crw-rw-rw-",
            twin: None,
            why: "`/dev/null` and every terminal. A whole directory of these read \
                  as ordinary files is the failure this fixture exists for",
        },
        PosixMode {
            id: "blockdev",
            mode: 0o060_660,
            rwx: "brw-rw----",
            twin: None,
            why: "`/dev/sda`. Copying one is not copying a file, and the class is \
                  the only thing on the row that says so",
        },
        PosixMode {
            id: "socket",
            mode: 0o140_755,
            rwx: "srwxr-xr-x",
            twin: None,
            why: "a unix socket, which `/run` and `/tmp` are full of",
        },
        PosixMode {
            id: "setuid_no_x",
            mode: 0o104_711,
            rwx: "-rws--x--x",
            twin: None,
            why: "setuid WITH execute: lowercase `s`, and a security-relevant bit \
                  that must never render as an ordinary permission",
        },
        PosixMode {
            id: "setgid_no_x",
            mode: 0o102_745,
            rwx: "-rwxr-Sr-x",
            twin: None,
            why: "setgid WITHOUT execute: capital `S`. The capital is what says the \
                  bit is set on something that cannot use it",
        },
        PosixMode {
            id: "sticky_dir",
            mode: 0o041_777,
            rwx: "drwxrwxrwt",
            twin: None,
            why: "`/tmp`: world-writable but you may only delete your own",
        },
        PosixMode {
            id: "sticky_no_x",
            mode: 0o041_776,
            rwx: "drwxrwxrwT",
            twin: None,
            why: "sticky without execute for others: capital `T`, same rule as `S`",
        },
        PosixMode {
            id: "no_type_bits",
            mode: 0o644,
            rwx: "?rw-r--r--",
            twin: Some(0o100_644),
            why: "an SFTP server that reports only the permission bits — Windows \
                  OpenSSH and several appliances do — and what `MemProvider` emits \
                  for every node. Its twin IS a regular file. Rendered `-`, the two \
                  are indistinguishable and a directory reads as a file while the \
                  icon beside it says otherwise. Absence of a class is not the \
                  regular class, and `?` is the only honest cell",
        },
        PosixMode {
            id: "unknown_type",
            mode: 0o170_644,
            rwx: "?rw-r--r--",
            twin: None,
            why: "a type word no `S_IFMT` defines: a forged archive, a FUSE bridge, \
                  a future kernel. Guessing `-` for it is inventing an answer",
        },
        PosixMode {
            id: "truncation_twins",
            mode: 0o100_644,
            rwx: "-rw-r--r--",
            twin: Some(0o100_640),
            why: "`-rw-r--r--` and `-rw-r-----` share their first seven cells. Cut \
                  into a column the reader narrowed by dragging its border, \
                  world-readable and group-only become the same string. Same demand \
                  as `truncation_twins` over titles: the cut must be MARKED",
        },
        PosixMode {
            id: "u32_max",
            mode: u64::from(u32::MAX),
            rwx: "?rwsrwsrwt",
            twin: None,
            why: "every bit set: setuid, setgid and sticky included, so the three \
                  execute cells are `s`, `s` and `t`. Still exactly ten cells, and \
                  still not a class the formatter recognises",
        },
    ]
}
