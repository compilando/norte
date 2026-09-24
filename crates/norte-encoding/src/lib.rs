//! Text-encoding detection and decoding (spec §6): BOM → binary heuristic
//! (NUL) → chardetng pipeline, and decoding with `encoding_rs`. Isolates the
//! surface of those deps (M1 plan): the rest of the workspace consumes THIS
//! API, never `chardetng`/`encoding_rs` directly.
#![forbid(unsafe_code)]

mod fold;

pub use encoding_rs::{Encoding, UTF_8};
pub use fold::{
    FoldMode, fold_delta, full_fold_expansion, has_canonical_singleton, is_canonical_singleton,
    is_default_ignorable, name_key,
};

/// Header sample for chardetng: 64 KiB (spec §6.2).
const SNIFF_LEN: usize = 64 * 1024;

/// Result of detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detection {
    /// Text in the detected encoding.
    Text {
        /// The most likely encoding.
        encoding: &'static Encoding,
        /// `true` if a BOM decided it (certainty, not statistics).
        bom: bool,
    },
    /// Binary (NUL with no BOM): to the hexview, never decode blindly.
    Binary,
}

/// Detects `bytes`'s encoding (spec §6): BOM first (certainty), NUL with no
/// BOM = binary (over the WHOLE buffer: a binary with a long textual
/// preamble does not sneak through), and otherwise chardetng over the
/// header (64 KiB). UTF-16 WITH NO BOM falls to binary by design —
/// recoverable by hand with "reload as…" ([`decode_forced`]).
///
/// ```
/// use norte_encoding::{Detection, detect};
/// assert!(matches!(detect("hola\n".as_bytes()), Detection::Text { .. }));
/// assert!(matches!(detect(b"PK\x03\x04\x00\x00"), Detection::Binary));
/// ```
#[must_use]
pub fn detect(bytes: &[u8]) -> Detection {
    if let Some((enc, _len)) = Encoding::for_bom(bytes) {
        return Detection::Text {
            encoding: enc,
            bom: true,
        };
    }
    if bytes.contains(&0) {
        return Detection::Binary;
    }
    let head = &bytes[..bytes.len().min(SNIFF_LEN)];
    // ISO-2022-JP left out: detecting it opens escape confusions in web
    // contexts; a local file that needs it will use "reload as…". UTF-8
    // allowed: this is not a browser with legacy content to protect — a
    // local file in valid UTF-8 IS UTF-8.
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    det.feed(head, bytes.len() <= SNIFF_LEN);
    Detection::Text {
        encoding: det.guess(None, chardetng::Utf8Detection::Allow),
        bom: false,
    }
}

/// Decoded text.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// The text (with `�` where bytes were invalid).
    pub text: String,
    /// The encoding used (informative, for the status bar).
    pub encoding: &'static Encoding,
    /// `true` if some byte did not decode (the `�` is visible).
    pub had_errors: bool,
}

/// Decodes `bytes` as `encoding`, stripping ITS BOM if it has one.
/// `complete = false` when `bytes` is a truncated HEADER: the multibyte
/// sequence split off the end is left pending instead of being marked as
/// loss (a genuinely valid file that was cut off does NOT "have losses").
///
/// ```
/// use norte_encoding::{UTF_8, decode};
/// assert_eq!(decode("año".as_bytes(), UTF_8, true).text, "año");
/// // ñ split by a truncation: pending, not a loss.
/// assert!(!decode(b"a\xC3", UTF_8, false).had_errors);
/// ```
#[must_use]
pub fn decode(bytes: &[u8], encoding: &'static Encoding, complete: bool) -> Decoded {
    run_decoder(encoding.new_decoder(), encoding, bytes, complete)
}

/// Like [`decode`] but WITHOUT honoring any BOM: what the user forces with
/// "reload as…" RULES (spec §6.2: always correctable by hand) — leading
/// `FE FF` bytes are DATA under the forced encoding.
///
/// ```
/// use norte_encoding::{Encoding, decode_forced};
/// let w1252 = Encoding::for_label(b"windows-1252").unwrap();
/// assert_eq!(decode_forced(b"\xFE\xFF!", w1252, true).text, "þÿ!");
/// ```
#[must_use]
pub fn decode_forced(bytes: &[u8], encoding: &'static Encoding, complete: bool) -> Decoded {
    run_decoder(
        encoding.new_decoder_without_bom_handling(),
        encoding,
        bytes,
        complete,
    )
}

fn run_decoder(
    mut decoder: encoding_rs::Decoder,
    encoding: &'static Encoding,
    bytes: &[u8],
    complete: bool,
) -> Decoded {
    let mut text = String::with_capacity(
        decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or(bytes.len()),
    );
    let (_result, _read, had_errors) = decoder.decode_to_string(bytes, &mut text, complete);
    Decoded {
        text,
        encoding,
        had_errors,
    }
}

/// STATEFUL decoder for consuming a file in chunks while respecting
/// multibyte sequences split across a chunk boundary (unlike [`decode`],
/// which decodes a complete buffer at once). Essential for splitting on
/// `'\n'` over the DECODED TEXT: in UTF-16 `LF` is `0A 00`/`00 0A` and
/// cutting on the raw byte `0x0A` misaligns the pairs.
///
/// The initial BOM is consumed (it does not appear in the output), same as
/// [`decode`].
///
/// ```
/// use norte_encoding::{Encoding, StreamDecoder};
/// let utf16le = Encoding::for_label(b"utf-16le").unwrap();
/// let mut dec = StreamDecoder::new(utf16le);
/// let mut out = String::new();
/// // "hi" in UTF-16LE with a BOM, split mid code unit.
/// dec.feed(&[0xFF, 0xFE, 0x68], false, &mut out); // BOM + incomplete 'h'
/// dec.feed(&[0x00, 0x69, 0x00], true, &mut out);  // rest of 'h' + 'i'
/// assert_eq!(out, "hi");
/// ```
pub struct StreamDecoder {
    decoder: encoding_rs::Decoder,
    encoding: &'static Encoding,
}

impl StreamDecoder {
    /// Creates a stateful decoder for `encoding` (honors the initial BOM).
    #[must_use]
    pub fn new(encoding: &'static Encoding) -> Self {
        Self {
            decoder: encoding.new_decoder(),
            encoding,
        }
    }

    /// This decoder's encoding.
    #[must_use]
    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    /// Decodes `bytes` and APPENDS the text to `out`, internally retaining
    /// any incomplete multibyte sequence off the end for the next `feed`.
    /// `last = true` on the final chunk flushes whatever is pending (the
    /// dangling bytes come out as `�`). Invalid bytes are replaced by `�`.
    pub fn feed(&mut self, bytes: &[u8], last: bool, out: &mut String) {
        out.reserve(
            self.decoder
                .max_utf8_buffer_length(bytes.len())
                .unwrap_or(bytes.len()),
        );
        let _ = self.decoder.decode_to_string(bytes, out, last);
    }
}

/// Dominant line ending of a text (for the viewer's status bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    /// Only `\n`.
    Lf,
    /// Only `\r\n`.
    CrLf,
    /// Only `\r` (classic Mac).
    Cr,
    /// Mixed (suspicious: worth surfacing).
    Mixed,
    /// No line breaks.
    None,
}

impl std::fmt::Display for Eol {
    /// Stable TECHNICAL identifier (a lib does not localize; the frontend
    /// maps it to UI text).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Lf => "LF",
            Self::CrLf => "CRLF",
            Self::Cr => "CR",
            Self::Mixed => "mixed",
            Self::None => "none",
        })
    }
}

/// Detects an already-decoded text's EOL.
///
/// ```
/// use norte_encoding::{Eol, detect_eol};
/// assert_eq!(detect_eol("a\r\nb\r\n"), Eol::CrLf);
/// ```
#[must_use]
pub fn detect_eol(text: &str) -> Eol {
    let bytes = text.as_bytes();
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 2;
            }
            b'\r' => {
                cr += 1;
                i += 1;
            }
            b'\n' => {
                lf += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    match (lf > 0, crlf > 0, cr > 0) {
        (false, false, false) => Eol::None,
        (true, false, false) => Eol::Lf,
        (false, true, false) => Eol::CrLf,
        (false, false, true) => Eol::Cr,
        _ => Eol::Mixed,
    }
}

/// The viewer's "reload as…" cycle: the encodings a real user needs to try
/// by hand (covers the testkit's corpus; extensible).
///
/// ```
/// assert!(norte_encoding::reload_cycle().len() >= 10);
/// ```
#[must_use]
pub fn reload_cycle() -> &'static [&'static Encoding] {
    const CYCLE: &[&Encoding] = &[
        encoding_rs::UTF_8,
        encoding_rs::WINDOWS_1252,
        encoding_rs::ISO_8859_15,
        // GB18030 (the label the spec names; superset of GBK).
        encoding_rs::GB18030,
        encoding_rs::SHIFT_JIS,
        encoding_rs::EUC_JP,
        encoding_rs::BIG5,
        encoding_rs::KOI8_R,
        encoding_rs::UTF_16LE,
        encoding_rs::UTF_16BE,
    ];
    CYCLE
}

/// The encodings to transcode a NEEDLE into for literal content search
/// (fs.search): [`reload_cycle`] MINUS UTF-16LE/BE.
///
/// The decision lives ALONGSIDE the data (not in the consumer, by name).
/// Encoding to UTF-16 with `encoding_rs` does NOT produce UTF-16 bytes: the
/// WHATWG "output encoding" rule maps UTF-16LE/BE to UTF-8 (see
/// [`encode_lossless`]'s gotcha), so as a needle they would only duplicate
/// the UTF-8 one — useless. A literal in a GENUINELY UTF-16 file is searched
/// by decoding the file (the detector gives UTF-16-with-BOM as text;
/// no-BOM falls to binary and the walker skips it), not with a raw needle.
///
/// ```
/// assert!(norte_encoding::needle_cycle().len() == norte_encoding::reload_cycle().len() - 2);
/// ```
#[must_use]
pub fn needle_cycle() -> &'static [&'static Encoding] {
    const CYCLE: &[&Encoding] = &[
        encoding_rs::UTF_8,
        encoding_rs::WINDOWS_1252,
        encoding_rs::ISO_8859_15,
        encoding_rs::GB18030,
        encoding_rs::SHIFT_JIS,
        encoding_rs::EUC_JP,
        encoding_rs::BIG5,
        encoding_rs::KOI8_R,
    ];
    CYCLE
}

/// Encodes `text` to `enc` WITHOUT loss: `None` if some character is not
/// mappable in that encoding, or if the result is empty. Encapsulates
/// `encoding_rs::Encoding::encode`'s gotcha (which on an unmappable emits an
/// HTML numeric reference `&#NNN;` instead of failing) by exposing an
/// honest contract: representable bytes or nothing.
///
/// **UTF-16 → UTF-8 gotcha**: `encoding_rs` follows the WHATWG "get an
/// output encoding" rule — UTF-16LE/BE and `replacement` are DECODE-ONLY
/// encodings, and when encoding they are substituted with UTF-8. So,
/// `encode_lossless(UTF_16LE, "A")` returns the UTF-8 bytes `[0x41]`, NOT
/// the UTF-16 `[0x41, 0x00]`. Consequence: this function never produces real
/// UTF-16 bytes; to search in a genuinely UTF-16 file it must be decoded.
/// That is why [`needle_cycle`] excludes UTF-16 (it would only duplicate the
/// UTF-8 needle).
///
/// ```
/// use norte_encoding::{encode_lossless, UTF_8};
/// let w1252 = norte_encoding::Encoding::for_label(b"windows-1252").unwrap();
/// assert_eq!(encode_lossless(UTF_8, "año").unwrap(), "año".as_bytes());
/// assert_eq!(encode_lossless(w1252, "año").unwrap(), b"a\xF1o");
/// // π is not mappable in windows-1252 → None (never the lossy "&#960;"):
/// assert!(encode_lossless(w1252, "π").is_none());
/// ```
#[must_use]
pub fn encode_lossless(enc: &'static Encoding, text: &str) -> Option<Vec<u8>> {
    let (bytes, _enc, had_unmappable) = enc.encode(text);
    if had_unmappable || bytes.is_empty() {
        return None;
    }
    Some(bytes.into_owned())
}

/// `Default_Ignorable_Code_Point` ranges (UCD `DerivedCoreProperties`,
/// Unicode 16.0) — the property that says "this does not get painted".
///
/// It is a TABLE and not a call to a library because none in the tree
/// exposes it: `unicode-properties` gives general category and emoji, and
/// nothing else. A table derived from the UCD, with its version written
/// alongside and a test that walks it, is auditable; the loose list of
/// codepoints there used to be was not — it was written by hand, claimed in
/// its rustdoc to cover "the Cf/Zl/Zp INVISIBLES", and left eight out
/// (#125), among them the two Hangul fillers, which are **Lo** and no Cf
/// enumeration was ever going to catch.
///
/// Sorted: [`is_terminal_hazard`] walks it with binary search.
const DEFAULT_IGNORABLE: &[(char, char)] = &[
    ('\u{00AD}', '\u{00AD}'),   // SOFT HYPHEN
    ('\u{034F}', '\u{034F}'),   // COMBINING GRAPHEME JOINER
    ('\u{061C}', '\u{061C}'),   // ARABIC LETTER MARK
    ('\u{115F}', '\u{1160}'),   // HANGUL fillers (Lo)
    ('\u{17B4}', '\u{17B5}'),   // Khmer inherent vowels
    ('\u{180B}', '\u{180F}'),   // Mongolian variation selectors + MVS
    ('\u{200B}', '\u{200F}'),   // ZWSP/ZWNJ/ZWJ/LRM/RLM
    ('\u{202A}', '\u{202E}'),   // bidi embedding and overrides
    ('\u{2060}', '\u{2064}'),   // WORD JOINER … INVISIBLE PLUS
    ('\u{2065}', '\u{2069}'),   // unassigned + bidi isolates
    ('\u{206A}', '\u{206F}'),   // deprecated format chars (NATIONAL DIGIT SHAPES…)
    ('\u{3164}', '\u{3164}'),   // HANGUL FILLER (Lo)
    ('\u{FE00}', '\u{FE0F}'),   // variation selectors
    ('\u{FEFF}', '\u{FEFF}'),   // ZWNBSP / BOM
    ('\u{FFA0}', '\u{FFA0}'),   // HALFWIDTH HANGUL FILLER
    ('\u{FFF0}', '\u{FFF8}'),   // reserved unassigned
    ('\u{1BCA0}', '\u{1BCA3}'), // Duployan format controls
    ('\u{1D173}', '\u{1D17A}'), // musical format controls
    ('\u{E0000}', '\u{E0FFF}'), // TAG chars and supplementary variation selectors
];

/// Invisibles `Default_Ignorable_Code_Point` does NOT cover and that still
/// get painted blank. Each with its reason, because each is an exception and
/// an exception with no reason is a list things get added to. Sorted, like
/// the others: walked by the same binary search.
const INVISIBLE_OUTSIDE_DI: &[(char, char)] = &[
    // Zl/Zp: line and paragraph separators, which `is_control` does not catch.
    ('\u{2028}', '\u{2029}'),
    // BRAILLE PATTERN BLANK: category So, neither Cf nor ignorable to
    // anyone — it is simply a braille with no dots, i.e. a full-width blank
    // character.
    ('\u{2800}', '\u{2800}'),
    // Cf, but the UCD EXCLUDES them from DI (they carry annotation
    // semantics). They still get painted blank, so they work to forge a
    // twin.
    ('\u{FFF9}', '\u{FFFB}'),
];

/// Ignorables that are KNOWINGLY ALLOWED: they compose legitimate emoji, and
/// masking them would break real names in exchange for the residual of a
/// twin that only differs in this.
///
/// ZWJ joins the parts of a compound emoji (family, professions); the
/// variation selectors choose emoji presentation over text. Both are
/// `Default_Ignorable`, so without this exception the property would catch
/// them.
const ALLOWED_IGNORABLES: &[(char, char)] = &[
    ('\u{200D}', '\u{200D}'),   // ZERO WIDTH JOINER
    ('\u{FE00}', '\u{FE0F}'),   // variation selectors 1..16
    ('\u{E0100}', '\u{E01EF}'), // supplementary variation selectors
];

/// Is `c` `Default_Ignorable_Code_Point`, per the SAME table the invisibles
/// painting uses?
///
/// Asked by [`fold::is_default_ignorable`], which is the public face: the
/// full fold discards them before comparing (#214). The table is one and the
/// policies are two — `is_terminal_hazard` exempts ZWJ and the variation
/// selectors for emoji fidelity, and the fold cannot exempt anything because
/// the filesystem does not either.
pub(crate) fn is_default_ignorable_impl(c: char) -> bool {
    in_ranges(DEFAULT_IGNORABLE, c)
}

/// Is `c` within any of `table`'s SORTED ranges?
fn in_ranges(table: &[(char, char)], c: char) -> bool {
    table
        .binary_search_by(|(lo, hi)| {
            if c < *lo {
                std::cmp::Ordering::Greater
            } else if c > *hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Is `c` a hazard for a terminal? Three families:
///
/// - **Cc, the controls** (`\n`, ESC): a direct frontend would EXECUTE
///   them — ANSI/OSC injection.
/// - **The bidi overrides**: falsify the text's VISUAL ORDER without
///   touching its bytes (`202A..=202E`, `2066..=2069`).
/// - **The invisibles**: two visually identical texts that differ in
///   bytes fool a human, and through them any "approve what you already
///   saw". Decided by the Unicode `Default_Ignorable_Code_Point` property
///   (`DEFAULT_IGNORABLE`) plus the ones that get painted blank without
///   being ignorable (`INVISIBLE_OUTSIDE_DI`).
///
/// ZWJ (`U+200D`) and the variation selectors are KNOWINGLY ALLOWED
/// (`ALLOWED_IGNORABLES`): masking them would break compound emoji — emoji
/// fidelity > the residual of a twin that only differs in that.
///
/// SINGLE source of the set (spec §6: never raw controls/bidi in terminal
/// surfaces). Consumed by `fs.search`'s preview sanitization (producer, at
/// the source) and the frontends' `display_name`/`must_mask`.
///
/// ```
/// use norte_encoding::is_terminal_hazard;
/// assert!(is_terminal_hazard('\u{202E}')); // RLO (bidi)
/// assert!(is_terminal_hazard('\u{001B}')); // ESC (control)
/// assert!(is_terminal_hazard('\u{FEFF}')); // BOM/ZWNBSP (invisible)
/// assert!(is_terminal_hazard('\u{3164}')); // HANGUL FILLER (Lo, #125)
/// assert!(is_terminal_hazard('\u{2800}')); // BRAILLE BLANK (So, #125)
/// assert!(!is_terminal_hazard('\u{200D}')); // ZWJ permitido (emoji)
/// assert!(!is_terminal_hazard('\u{FE0F}')); // VS16 permitido (emoji)
/// assert!(!is_terminal_hazard('a'));
/// ```
#[must_use]
pub fn is_terminal_hazard(c: char) -> bool {
    if in_ranges(ALLOWED_IGNORABLES, c) {
        return false;
    }
    c.is_control() || in_ranges(DEFAULT_IGNORABLE, c) || in_ranges(INVISIBLE_OUTSIDE_DI, c)
}

/// Replaces every [`is_terminal_hazard`] char with `U+FFFD` (`�`). Sanitizes
/// AT THE SOURCE a text meant to be painted: controls, bidi overrides and
/// invisibles never come out raw. Same set and same ZWJ decision as
/// [`is_terminal_hazard`].
///
/// ```
/// use norte_encoding::mask_terminal_hazards;
/// // RLO + isolate sin cerrar + ESC+OSC + C0 → all a U+FFFD:
/// let out = mask_terminal_hazards("ok \u{202E}\u{2066}\u{1B}]0;x\u{07}\u{01}");
/// assert_eq!(out, "ok \u{FFFD}\u{FFFD}\u{FFFD}]0;x\u{FFFD}\u{FFFD}");
/// // ZWJ (emoji) se preserva:
/// assert_eq!(mask_terminal_hazards("a\u{200D}b"), "a\u{200D}b");
/// ```
#[must_use]
pub fn mask_terminal_hazards(s: &str) -> String {
    s.chars()
        .map(|c| if is_terminal_hazard(c) { '\u{FFFD}' } else { c })
        .collect()
}

/// Encoding for REINTERPRETING filenames as text (#57, spec §6.1, ZIP row):
/// display ONLY — the name's bytes are never mutated (rule 1) and the choice
/// is an explicit user action ("view names as…"), never a blind decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameEncoding {
    /// IBM cp437 (DOS US), the classic legacy of pre-Unicode zips.
    /// `encoding_rs` does NOT carry it (WHATWG does not include it): a
    /// TOTAL table of our own (all 256 bytes map, decoding never fails).
    Cp437,
    /// An `encoding_rs` encoding (IBM866, `Shift_JIS`, GBK…).
    Rs(&'static Encoding),
}

impl NameEncoding {
    /// Short UI label (`cp437`, `IBM866`, `Shift_JIS`…).
    ///
    /// ```
    /// use norte_encoding::NameEncoding;
    /// assert_eq!(NameEncoding::Cp437.label(), "cp437");
    /// assert_eq!(NameEncoding::Rs(encoding_rs::GBK).label(), "GBK");
    /// ```
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Cp437 => "cp437",
            Self::Rs(e) => e.name(),
        }
    }
}

/// cp437, high half (0x80–0xFF). The low half is ASCII as-is (the
/// 0x00–0x1F controls are left as controls: display masking covers them
/// upstream, same as in a UTF-8 name with controls).
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ',
    'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ',
    'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕',
    '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦',
    '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐',
    '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', '≡', '±',
    '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{00A0}',
];

/// Decodes a NAME with `enc` for display. TOTAL: always produces text
/// (cp437 maps all 256 bytes; `encoding_rs` replaces what is invalid with
/// `U+FFFD`). Sanitizing hazards (controls/bidi/invisibles) is the display
/// CALLER's job, same as with UTF-8 names.
///
/// ```
/// use norte_encoding::{NameEncoding, decode_name};
/// // "CAFÉ.TXT" in cp437 (É = 0x90):
/// assert_eq!(decode_name(b"CAF\x90.TXT", NameEncoding::Cp437), "CAFÉ.TXT");
/// // Cyrillic in IBM866:
/// let russian = decode_name(b"\x8f\xa0\xaf\xaa\xa0", NameEncoding::Rs(encoding_rs::IBM866));
/// assert_eq!(russian, "Папка");
/// ```
#[must_use]
pub fn decode_name(bytes: &[u8], enc: NameEncoding) -> String {
    match enc {
        NameEncoding::Cp437 => bytes
            .iter()
            .map(|&b| {
                if b < 0x80 {
                    b as char
                } else {
                    CP437_HIGH[(b - 0x80) as usize]
                }
            })
            .collect(),
        NameEncoding::Rs(e) => e.decode_without_bom_handling(bytes).0.into_owned(),
    }
}

/// The "view names as…" cycle (#57): the name encodings a real user needs
/// to try over a pre-Unicode zip/tar. cp437 first (the zip format's
/// historical default when bit 11 is off).
///
/// ```
/// assert_eq!(norte_encoding::name_reinterpret_cycle().len(), 5);
/// ```
#[must_use]
pub fn name_reinterpret_cycle() -> &'static [NameEncoding] {
    const CYCLE: &[NameEncoding] = &[
        NameEncoding::Cp437,
        NameEncoding::Rs(encoding_rs::IBM866),
        NameEncoding::Rs(encoding_rs::SHIFT_JIS),
        NameEncoding::Rs(encoding_rs::GBK),
        NameEncoding::Rs(encoding_rs::WINDOWS_1252),
    ];
    CYCLE
}

/// Suggests a cycle encoding for a set of non-UTF8 NAMES (chardetng over the
/// samples). `None` = no useful suggestion (the guess fell outside the
/// cycle — e.g. UTF-8 — or there are no samples). cp437 is never suggested
/// (chardetng does not model it); the frontend's cycle goes all the way
/// around, so it stays reachable by hand.
///
/// ```
/// use norte_encoding::{NameEncoding, suggest_name_encoding};
/// // "Папка" in cp866 → IBM866 (a cycle member):
/// let s = suggest_name_encoding(&[b"\x8f\xa0\xaf\xaa\xa0"]);
/// assert_eq!(s, Some(NameEncoding::Rs(encoding_rs::IBM866)));
/// assert_eq!(suggest_name_encoding(&[]), None);
/// ```
#[must_use]
pub fn suggest_name_encoding(samples: &[&[u8]]) -> Option<NameEncoding> {
    if samples.is_empty() {
        return None;
    }
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    for (i, s) in samples.iter().enumerate() {
        det.feed(s, false);
        // Neutral ASCII separator between samples (audit finding F3):
        // without it, a dangling lead byte at the end of a name would pair
        // up with the next one's first byte, forging phantom multibyte
        // sequences that bias the guess.
        det.feed(b" ", i + 1 == samples.len());
    }
    let guess = det.guess(None, chardetng::Utf8Detection::Deny);
    name_reinterpret_cycle()
        .iter()
        .copied()
        .find(|e| matches!(e, NameEncoding::Rs(rs) if *rs == guess))
}
