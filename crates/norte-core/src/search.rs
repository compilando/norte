//! PURE matchers for fs.search (spec 2026-07-18 live search): no I/O, no
//! Tasks. Consumed by T3's walker.
//!
//! - **Name (GLOB axis)**: `lossy → NFC (+ lowercase if case-insensitive) →
//!   NFC` — the SAME discipline as the TUI's quick search
//!   (`norte-tui/src/nav.rs::fold`). The 2nd NFC is critical: `to_lowercase`
//!   can reintroduce decomposed forms (e.g. `J̌`→`ǰ`). The file's IDENTITY is
//!   NEVER normalized; this is DISPLAY matching.
//! - **Name (REGEX axis)**: NFC on input and pattern, but case
//!   insensitivity is resolved by the `regex` ENGINE itself (the engine's own
//!   simple ASCII/Unicode case folding) — NOT the same fold as the glob. It
//!   differs in cases like `İ`/`i` or `ß`/`ss` (the engine does not fold them
//!   the same way `to_lowercase` does). Aware and sufficient.
//! - **Content**: the NEEDLE is transcoded to the candidate encodings from
//!   `norte_encoding::needle_cycle()` (blind) that represent it WITHOUT
//!   loss (`encode_lossless`); the haystack is searched byte-against-byte
//!   with `memmem` in OVERLAPPING chunks — the whole file is NEVER decoded
//!   (spec §17.1a). ENCODING-AWARE mode ([`ContentNeedle::for_encoding`])
//!   for when T3 has already detected the file's encoding.
//!
//! **Content's NFC/NFD limit**: content search is byte-against-byte,
//! sensitive to normalization FORM — a file in NFD (typical of macOS) may
//! not match a needle typed in NFC (and vice versa). Inherent to literal
//! search without decoding; not fixed in v1.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use norte_encoding::{Detection, Encoding, encode_lossless, needle_cycle};
use norte_proto::methods::{FsSearchParams, MatchInfo, SEARCH_HITS_MAX_BATCH, SearchHits};
use norte_proto::{Entry, EntryKind, Error, Segment, TaskId, VPath};
use norte_vfs::{ByteStream, Provider};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;

/// Cap on the compiled regex program (anti-ReDoS): a regex whose machine
/// exceeds 1 MiB is rejected at COMPILE time (it does not hang at runtime).
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// Compilation failure of a name pattern. The message carries the
/// compiler's diagnostic (glob/regex): it is the requester's OWN input, so
/// exposing it is their own diagnostic, not a leak. The daemon (T4) maps it
/// to `INVALID_PARAMS`.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// The name glob does not compile.
    #[error("invalid glob: {0}")]
    BadGlob(String),
    /// The regex (name or content) does not compile or exceeds the `size_limit`.
    #[error("invalid regex: {0}")]
    BadRegex(String),
    /// No search criterion at all (neither name nor content).
    #[error("no search criteria")]
    NoCriteria,
    /// Two mutually exclusive criteria of the SAME axis (`name_glob`+`name_regex`, or
    /// `content`+`content_regex`).
    #[error("conflicting criteria: {0}")]
    Conflicting(&'static str),
    /// More than [`norte_proto::methods::SEARCH_EXCLUDES_MAX`] exclusions
    /// (0.81.0).
    ///
    /// Checked BEFORE compiling any of them, which is where the cost is:
    /// each name is a glob and, with it, a regex with its own budget, in the
    /// task that serves the connection and with no Task to bound it.
    #[error("too many exclusions: {0} (the cap is {1})")]
    TooManyExcludes(usize, usize),
    /// Two filters that cannot both hold at once (0.81.0).
    ///
    /// It is an error and not a zero-result search for the same reason as
    /// the glob and regex of the same axis: zero results reads as "there is
    /// nothing", and here what is missing is the question.
    #[error("contradictory filters: {0}")]
    ImpossibleFilter(&'static str),
    /// The `encoding` name is not any known encoding
    /// (0.81.0).
    ///
    /// It is an error in the REQUEST and not a search that finds nothing: a
    /// misspelled name that fell back to automatic detection would return
    /// perfectly believable results read with another alphabet, and whoever
    /// forced the encoding did so precisely because the automatic one did
    /// not work for them.
    #[error("unknown encoding: {0}")]
    BadEncoding(String),
}

/// Wraps a pattern between word boundaries (0.81.0).
///
/// The `(?:…)` group is not decorative: without it, a pattern with a
/// top-level alternation —`cat|dog`— would read as `\bcat` or `dog\b`, which
/// is a different search and, on top of that, one that matches what the
/// reader asked to exclude.
fn whole_word_pattern(pattern: &str) -> String {
    format!(r"\b(?:{pattern})\b")
}

/// Name fold for the GLOB axis: `lossy → NFC → (lowercase → NFC)`. The 2nd
/// NFC re-canonicalizes what `to_lowercase` may have decomposed. With
/// `case_sensitive` the lowercase step is skipped (but the NFC is kept, so
/// that NFD and NFC forms of the same name match).
fn fold_name(name: &[u8], case_sensitive: bool) -> String {
    let nfc: String = String::from_utf8_lossy(name).nfc().collect();
    if case_sensitive {
        nfc
    } else {
        nfc.chars().flat_map(char::to_lowercase).nfc().collect()
    }
}

/// NFC of the name in bytes for the REGEX axis (case insensitivity is
/// resolved by the regex itself via `case_insensitive`; here we only
/// canonicalize).
fn nfc_name(name: &[u8]) -> String {
    String::from_utf8_lossy(name).nfc().collect()
}

/// Recompiles the byte-mode regex of a [`globset::Glob`] into Unicode mode
/// (#110): globset compiles with `(?-u)`, where `?` consumes ONE BYTE and a
/// class matches byte by byte — `a?o` did not match `año` (ñ = 2 bytes).
/// globset remains the sole authority on syntax; this only translates its
/// output: it strips the `(?-u)` prefix and decodes the runs of `\xNN`
/// escapes with NN ≥ 0x80 — the only way globset emits the pattern's
/// non-ASCII bytes (`&str`, so the runs are always complete UTF-8) — back
/// into their characters, literal inside and outside a class.
///
/// A deliberate COPY of the translator in `norte-frontend::pane` (same
/// criterion as the fold, duplicated core/frontend): there is no common
/// crate below both where it would fit without dragging `globset`+`regex`
/// into an unrelated crate. Each copy pins globset's shape with its own
/// guard test.
///
/// # Errors
/// [`SearchError::BadGlob`] if a decoded run is not valid UTF-8 — should not
/// happen with the pinned globset; fail loud rather than match bytes the
/// user did not type.
fn unicode_glob_regex(glob: &globset::Glob) -> Result<String, SearchError> {
    let src = glob.regex();
    let stripped = src.strip_prefix("(?-u)").unwrap_or(src);
    let mut out = String::with_capacity(stripped.len());
    let mut run: Vec<u8> = Vec::new();
    let flush = |run: &mut Vec<u8>, out: &mut String| -> Result<(), SearchError> {
        if run.is_empty() {
            return Ok(());
        }
        let decoded = std::str::from_utf8(run).map_err(|_| {
            SearchError::BadGlob(
                "internal: the glob compiled to byte escapes that do not \
                 form UTF-8 characters"
                    .to_owned(),
            )
        })?;
        out.push_str(decoded);
        run.clear();
        Ok(())
    };
    let bytes = stripped.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && bytes.get(i + 1) == Some(&b'x')
            && let Some(hex) = stripped.get(i + 2..i + 4)
            && let Ok(b) = u8::from_str_radix(hex, 16)
            && b >= 0x80
        {
            run.push(b);
            i += 4;
            continue;
        }
        flush(&mut run, &mut out)?;
        // Copies the rest verbatim — including ASCII escapes (`\.`), whose
        // meaning is identical in Unicode mode.
        let step = if bytes[i] == b'\\' && i + 1 < bytes.len() {
            1 + stripped[i + 1..].chars().next().map_or(0, char::len_utf8)
        } else {
            stripped[i..].chars().next().map_or(1, char::len_utf8)
        };
        out.push_str(&stripped[i..i + step]);
        i += step;
    }
    flush(&mut run, &mut out)?;
    Ok(out)
}

/// NAME matcher (last path segment): glob OR regex, MUTUALLY EXCLUSIVE per
/// axis. Both apply the NFC fold described in the module before comparing.
#[derive(Debug)]
pub enum NameMatcher {
    /// Glob recompiled in Unicode mode (#110, `unicode_glob_regex`): `?`
    /// and classes count characters. The pattern already comes folded the
    /// same way as the input.
    Glob {
        /// Unicode regex translated from the folded glob.
        matcher: regex::Regex,
        /// If `false`, input and pattern are folded to lowercase.
        case_sensitive: bool,
    },
    /// Regex compiled with `size_limit` and `case_insensitive`.
    Regex(regex::Regex),
}

impl NameMatcher {
    /// Compiles a name glob. The pattern is folded (NFC + lowercase if
    /// `!case_sensitive`) the same way as the input, so that case
    /// insensitivity is resolved by the fold and not by `globset`.
    ///
    /// # Errors
    /// [`SearchError::BadGlob`] if the pattern is not a valid glob.
    pub fn glob(pattern: &str, case_sensitive: bool) -> Result<Self, SearchError> {
        let folded = fold_name(pattern.as_bytes(), case_sensitive);
        let glob = globset::GlobBuilder::new(&folded)
            .build()
            .map_err(|e| SearchError::BadGlob(e.to_string()))?;
        // Unicode mode (#110): `?`/classes count CHARACTERS, not bytes.
        // Same `size_limit` as the regex axis — the pattern is user input
        // and this is a public API with no length cap of its own.
        // `dot_matches_new_line`: globset compiles its matcher with that
        // flag, and `*`/`?` translate to `.`-derived forms — without it, a
        // name with `\n` (a legal byte on unix; corpus `control_newline`)
        // would silently stop matching `*`, the inverse of the bug this
        // fixes.
        let matcher = regex::RegexBuilder::new(&unicode_glob_regex(&glob)?)
            .size_limit(REGEX_SIZE_LIMIT)
            .dot_matches_new_line(true)
            .build()
            .map_err(|e| SearchError::BadGlob(e.to_string()))?;
        Ok(Self::Glob {
            matcher,
            case_sensitive,
        })
    }

    /// Compiles a name regex with an anti-ReDoS `size_limit` and
    /// `case_insensitive(!case_sensitive)`. The pattern is passed through NFC.
    ///
    /// # Errors
    /// [`SearchError::BadRegex`] if it does not compile or exceeds the `size_limit`.
    pub fn regex(pattern: &str, case_sensitive: bool) -> Result<Self, SearchError> {
        let pat = nfc_name(pattern.as_bytes());
        let re = regex::RegexBuilder::new(&pat)
            .size_limit(REGEX_SIZE_LIMIT)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| SearchError::BadRegex(e.to_string()))?;
        Ok(Self::Regex(re))
    }

    /// `true` if `name_bytes` (last segment, raw bytes) matches. Never panics
    /// with non-UTF8 bytes (matches over the lossy form).
    #[must_use]
    pub fn matches(&self, name_bytes: &[u8]) -> bool {
        match self {
            Self::Glob {
                matcher,
                case_sensitive,
            } => matcher.is_match(&fold_name(name_bytes, *case_sensitive)),
            Self::Regex(re) => re.is_match(&nfc_name(name_bytes)),
        }
    }
}

/// LITERAL content needle, already transcoded to the bytes of one or more
/// encodings that represent it without loss (pairs `(encoding, bytes)`). The
/// search is byte-against-byte (it never decodes the haystack).
///
/// Two modes:
/// - [`ContentNeedle::literal`]: BLIND — the needle in ALL the encodings of
///   `needle_cycle()`. Fallback for when the file's detection is uncertain.
///   **Trade-off**: a short legacy needle (e.g. `ñ`→1 byte `0xF1` in
///   windows-1252) matches BY CHANCE bytes that in another encoding are part
///   of a different sequence (0xF1 is the lead byte of a 4-byte char in
///   UTF-8) — false positives. See the test that pins this limit.
/// - [`ContentNeedle::for_encoding`]: ENCODING-AWARE — the needle ONLY in the
///   detected encoding (+ UTF-8). T3 uses it after `norte_encoding::detect`
///   to eliminate the blind mode's false positive.
#[derive(Debug, Clone)]
pub struct ContentNeedle {
    /// Pairs `(source encoding, bytes)`: the encoding travels with the
    /// needle so the consumer (T3) can be encoding-aware; `find_in` only
    /// uses the bytes. Deduped by bytes.
    needles: Vec<(&'static Encoding, Vec<u8>)>,
    /// Length of the longest needle (sizes the overlap between chunks).
    max_len: usize,
}

impl ContentNeedle {
    /// Case variants of the text: the text itself, and with
    /// `!case_sensitive` also lowercase and uppercase BEFORE encoding
    /// (simple fold: covers needles of HOMOGENEOUS case; a needle in mixed
    /// case within the haystack is best-effort — documented for v1).
    fn case_variants(text: &str, case_sensitive: bool) -> Vec<String> {
        let mut variants = vec![text.to_string()];
        if !case_sensitive {
            variants.push(text.to_lowercase());
            variants.push(text.to_uppercase());
        }
        variants
    }

    /// Builds the needle by encoding each case variant into each encoding
    /// from `encs` that maps it WITHOUT loss ([`encode_lossless`]); deduped
    /// by bytes.
    fn build(text: &str, case_sensitive: bool, encs: &[&'static Encoding]) -> Self {
        let variants = Self::case_variants(text, case_sensitive);
        let mut needles: Vec<(&'static Encoding, Vec<u8>)> = Vec::new();
        for variant in &variants {
            for &enc in encs {
                if let Some(bytes) = encode_lossless(enc, variant)
                    && !needles.iter().any(|(_, b)| *b == bytes)
                {
                    needles.push((enc, bytes));
                }
            }
        }
        let max_len = needles.iter().map(|(_, b)| b.len()).max().unwrap_or(0);
        Self { needles, max_len }
    }

    /// BLIND needle: transcoded to ALL encodings from
    /// `norte_encoding::needle_cycle()` that represent it without loss.
    /// Fallback for when the file's encoding is uncertain; accepts the
    /// trade-off of false positives from short legacy needles (see the
    /// type's doc).
    #[must_use]
    pub fn literal(text: &str, case_sensitive: bool) -> Self {
        Self::build(text, case_sensitive, needle_cycle())
    }

    /// ENCODING-AWARE needle: only in `enc` (the file's ALREADY detected
    /// encoding) PLUS always UTF-8 (a safety net if the detector guessed
    /// wrong toward a legacy encoding with common ASCII). Returns `None` if
    /// the text is empty (no useful needle). For a file detected as UTF-16
    /// pass it `UTF_16LE`/`BE`: `encode_lossless` falls back to UTF-8
    /// (WHATWG gotcha), so T3 must search the DECODED text, not here — the
    /// UTF-8 needle is the best available in that case.
    #[must_use]
    pub fn for_encoding(text: &str, case_sensitive: bool, enc: &'static Encoding) -> Option<Self> {
        let n = Self::build(text, case_sensitive, &[enc, norte_encoding::UTF_8]);
        if n.needles.is_empty() { None } else { Some(n) }
    }

    /// The transcoded needles `(encoding, bytes)`. The walker (T3) consumes
    /// them in a scan that tracks line/offset, which [`Self::find_in`] does
    /// not expose (it needs the accumulated `pos` and `\n` count for
    /// `line`/`preview`).
    #[must_use]
    pub fn needles(&self) -> &[(&'static Encoding, Vec<u8>)] {
        &self.needles
    }

    /// Length of the longest needle (sizes the overlap between chunks).
    #[must_use]
    pub fn max_len(&self) -> usize {
        self.max_len
    }

    /// Searches for the needle in `previous_tail + chunk` with `memmem`;
    /// returns the offset of the first match within that combined buffer
    /// (or `None`). Updates `ov`, retaining the last `max_len - 1` bytes so
    /// that a needle split by the next chunk's boundary can still be found.
    ///
    /// The walker (T3) stops at the first hit per file, so there is no
    /// re-counting of the retained tail between chunks.
    pub fn find_in(&self, ov: &mut Overlap, chunk: &[u8]) -> Option<usize> {
        let mut buf = std::mem::take(&mut ov.tail);
        buf.extend_from_slice(chunk);
        let mut hit: Option<usize> = None;
        for (_enc, needle) in &self.needles {
            if let Some(pos) = memchr::memmem::find(&buf, needle) {
                hit = Some(hit.map_or(pos, |h: usize| h.min(pos)));
            }
        }
        let keep = self.max_len.saturating_sub(1).min(buf.len());
        ov.tail = buf[buf.len() - keep..].to_vec();
        hit
    }
}

/// Overlap state between chunks of a file: retains the previous chunk's
/// tail so that a needle split by the boundary can be found.
#[derive(Debug, Default)]
pub struct Overlap {
    /// Last `max_len - 1` bytes already seen (prefix of the next buffer).
    tail: Vec<u8>,
}

/// CONTENT regex (`content_regex`): only compiled here with `size_limit` and
/// `case_insensitive`; applied over the text already decoded line by line in
/// the walker (T3), where the chunked decoding lives.
#[derive(Debug, Clone)]
pub struct ContentRegex {
    /// Compiled regex; run line by line in the walker.
    re: regex::Regex,
}

impl ContentRegex {
    /// Compiles the content regex with an anti-ReDoS `size_limit` and
    /// `case_insensitive(!case_sensitive)`.
    ///
    /// # Errors
    /// [`SearchError::BadRegex`] if it does not compile or exceeds the `size_limit`.
    pub fn new(pattern: &str, case_sensitive: bool) -> Result<Self, SearchError> {
        let re = regex::RegexBuilder::new(pattern)
            .size_limit(REGEX_SIZE_LIMIT)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| SearchError::BadRegex(e.to_string()))?;
        Ok(Self { re })
    }

    /// `true` if `line` (already decoded text) matches.
    #[must_use]
    pub fn is_match(&self, line: &str) -> bool {
        self.re.is_match(line)
    }
}

// ── fs.search walker (T3): walks a subtree and emits hits in batches. ────

/// Time-based flush of the hit batch (mirrors the TUI's `FILL_INTERVAL`): a
/// non-empty batch is sent once this interval passes even if it has not
/// filled the batch, so the virtual pane "drips" results live.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// Cap on a match's `preview` character count (server-side trimming).
const PREVIEW_MAX_CHARS: usize = 160;
/// RAM budget per line on the decode path (`scan_decode_lines`): a line
/// without `'\n'` that exceeds this size is evaluated TRUNCATED to this
/// prefix and the rest is discarded up to the next `'\n'` (avoids dumping a
/// single giant-line file entirely into memory).
const LINE_MATCH_CAP: usize = 1 << 20;

/// Already-compiled content criterion.
enum ContentSpec {
    /// No content search (name only).
    None,
    /// Multi-encoding literal (the needle is transcoded per file according
    /// to the detected encoding; the raw text is retained for the UTF-16
    /// decode path, see [`run_walk`]).
    Literal(String),
    /// Regex over the content decoded line by line.
    Regex(ContentRegex),
}

/// [`FsSearchParams`] criteria already validated and COMPILED (name/content
/// matchers). Built BEFORE the Task: an invalid glob/regex or an illegal
/// combination is a REQUEST error, not a Task failure.
pub struct SearchMatchers {
    name: Option<NameMatcher>,
    content: ContentSpec,
    case_sensitive: bool,
    /// Hit cap (already converted to `usize`); `None` = no cap.
    max_hits: Option<usize>,
    /// The 0.81.0 filters: what decides whether an entry that already
    /// matched by name and content ALSO counts as a result.
    filters: Filters,
    /// Directory names that are not descended into, already compiled to
    /// glob. Empty = descend into all of them.
    excluded_dir_names: Vec<NameMatcher>,
    /// `false` = root directory only.
    recursive: bool,
    /// The encoding to read content with, if the reader forced one
    /// (0.81.0). `None` = whatever each file's detection says.
    encoding: Option<&'static Encoding>,
}

/// What is asked of an entry IN ADDITION to matching by name or content
/// (protocol 0.81.0).
///
/// Kept separate from the matchers because they are of a different nature:
/// those compile patterns and can fail, and these are comparisons over what
/// the entry already carries. Merging them would make a date range have to
/// go through a `Result`.
#[derive(Debug, Clone, Default)]
struct Filters {
    kinds: Vec<EntryKind>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    mtime_after: Option<i64>,
    mtime_before: Option<i64>,
}

impl Filters {
    /// Does this entry count as a result?
    ///
    /// A value the provider cannot report does NOT pass a filter on it.
    /// Filtering is asserting, and "I don't know" is not "yes": a bucket
    /// that reports no date would return its entire contents under
    /// "modified this week", which is worse than returning nothing, because
    /// it reads exactly like a result.
    fn passes(&self, e: &Entry) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&e.kind) {
            return false;
        }
        if self.min_size.is_some() || self.max_size.is_some() {
            let Some(size) = e.size else { return false };
            if self.min_size.is_some_and(|m| size < m) || self.max_size.is_some_and(|m| size > m) {
                return false;
            }
        }
        if self.mtime_after.is_some() || self.mtime_before.is_some() {
            let Some(t) = e.mtime_ms else { return false };
            if self.mtime_after.is_some_and(|m| t < m) || self.mtime_before.is_some_and(|m| t > m) {
                return false;
            }
        }
        true
    }

    /// Is any filter set? With none, nothing is touched, which is the
    /// 0.80 path.
    fn is_empty(&self) -> bool {
        self.kinds.is_empty()
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.mtime_after.is_none()
            && self.mtime_before.is_none()
    }
}

impl SearchMatchers {
    /// Validates and compiles `params`'s criteria.
    ///
    /// # Errors
    /// - [`SearchError::NoCriteria`] if there is no criterion at all.
    /// - [`SearchError::Conflicting`] if both `name_glob`+`name_regex`, or
    ///   `content`+`content_regex` are given (mutually exclusive per axis).
    /// - [`SearchError::BadGlob`]/[`SearchError::BadRegex`] if a pattern does
    ///   not compile (or exceeds the anti-ReDoS `size_limit`).
    /// - [`SearchError::TooManyExcludes`] above
    ///   [`norte_proto::methods::SEARCH_EXCLUDES_MAX`] (0.81.0).
    /// - [`SearchError::ImpossibleFilter`] if two filters cannot both hold
    ///   at once (0.81.0).
    /// - [`SearchError::BadEncoding`] if `encoding` does not name any known
    ///   encoding (0.81.0).
    pub fn compile(params: &FsSearchParams) -> Result<Self, SearchError> {
        if params.name_glob.is_some() && params.name_regex.is_some() {
            return Err(SearchError::Conflicting(
                "name_glob and name_regex are mutually exclusive",
            ));
        }
        if params.content.is_some() && params.content_regex.is_some() {
            return Err(SearchError::Conflicting(
                "content and content_regex are mutually exclusive",
            ));
        }
        let cs = params.case_sensitive;
        let name = match (&params.name_glob, &params.name_regex) {
            (Some(g), _) => Some(NameMatcher::glob(g, cs)?),
            (_, Some(r)) => Some(NameMatcher::regex(r, cs)?),
            _ => None,
        };
        // An EMPTY content needle is not a criterion (it would match
        // everything): treated as absent for the "at least one criterion"
        // computation. "Whole word" (0.81.0) is ALWAYS implemented as a
        // regex, even for a literal needle, and it is worth saying why: the
        // fast literal path searches BYTES, with the needle transcoded to
        // several candidates and the haystack left undecoded. A word
        // boundary is not a property of the bytes — it depends on what
        // counts as a letter, and that depends on the alphabet — so it
        // cannot be checked there without decoding, which is exactly what
        // that path exists to avoid. Asking for it costs the fast path;
        // not asking for it costs nothing.
        let content = if let Some(c) = &params.content {
            if c.is_empty() {
                ContentSpec::None
            } else if params.whole_word {
                ContentSpec::Regex(ContentRegex::new(
                    &whole_word_pattern(&regex::escape(c)),
                    cs,
                )?)
            } else {
                ContentSpec::Literal(c.clone())
            }
        } else if let Some(r) = &params.content_regex {
            let pattern = if params.whole_word {
                whole_word_pattern(r)
            } else {
                r.clone()
            };
            ContentSpec::Regex(ContentRegex::new(&pattern, cs)?)
        } else {
            ContentSpec::None
        };
        // The exclusion cap, before compiling any of them: that is where
        // the cost is, and an agent can reach it.
        let limit = norte_proto::methods::SEARCH_EXCLUDES_MAX;
        for n in [params.exclude_names.len(), params.exclude_roots.len()] {
            if n > limit {
                return Err(SearchError::TooManyExcludes(n, limit));
            }
        }
        let filters = Filters {
            kinds: params.kinds.clone(),
            min_size: params.min_size,
            max_size: params.max_size,
            mtime_after: params.mtime_after,
            mtime_before: params.mtime_before,
        };
        // Two filters that cannot both hold at once are SAID. Zero results
        // reads as "nothing matched", and here what is missing is the
        // question.
        if let (Some(min), Some(max)) = (filters.min_size, filters.max_size)
            && min > max
        {
            return Err(SearchError::ImpossibleFilter(
                "the minimum size is greater than the maximum",
            ));
        }
        if let (Some(after), Some(before)) = (filters.mtime_after, filters.mtime_before)
            && after > before
        {
            return Err(SearchError::ImpossibleFilter(
                "the start date is after the end date",
            ));
        }
        // Searching CONTENT only in directories cannot match anything:
        // content is read from regular files and nothing else.
        let dirs_only = !filters.kinds.is_empty() && !filters.kinds.contains(&EntryKind::File);
        let wants_content = params.content.is_some() || params.content_regex.is_some();
        if dirs_only && wants_content {
            return Err(SearchError::ImpossibleFilter(
                "content is requested and files are excluded",
            ));
        }
        // A filter ALONE —"everything over a gig"— is a legitimate
        // criterion, and one of the most useful there is. Before, "no name
        // and no content" was always "no criteria"; now it only is when
        // there are no filters either.
        if name.is_none() && matches!(content, ContentSpec::None) && filters.is_empty() {
            return Err(SearchError::NoCriteria);
        }
        // The names not to descend into are globs over the last segment,
        // with the same discipline as `name_glob` — including the fold,
        // which is what makes `Target` exclude `target` on a macOS.
        let excluded_dir_names = params
            .exclude_names
            .iter()
            .map(|g| NameMatcher::glob(g, cs))
            .collect::<Result<Vec<_>, _>>()?;
        // `for_label_no_replacement` and NOT `for_label`: the latter accepts
        // the standard's replacement labels —`utf-7`, `hz-gb-2312`,
        // `iso-2022-cn`— and returns the REPLACEMENT encoding, which decodes
        // the whole file to a single U+FFFD. The search would not fail: it
        // would find nothing, silently, which is exactly what this field's
        // rustdoc promises not to do.
        let encoding = match &params.encoding {
            None => None,
            Some(label) => Some(
                Encoding::for_label_no_replacement(label.as_bytes())
                    .ok_or_else(|| SearchError::BadEncoding(label.clone()))?,
            ),
        };
        Ok(Self {
            name,
            content,
            case_sensitive: cs,
            max_hits: params.max_hits.map(|m| m as usize),
            filters,
            excluded_dir_names,
            recursive: params.recursive,
            encoding,
        })
    }

    /// Does the walk descend into this directory? (protocol 0.81.0)
    ///
    /// By NAME and at any level: the folder that is noise —`target`,
    /// `node_modules`, `.git`— shows up a hundred times in places that are
    /// not known ahead of time, so naming it by path would not help at all.
    fn descends_into(&self, e: &Entry) -> bool {
        if self.excluded_dir_names.is_empty() {
            return true;
        }
        let Some(seg) = e.path.file_name() else {
            return true;
        };
        !self
            .excluded_dir_names
            .iter()
            .any(|m| m.matches(seg.as_bytes()))
    }

    /// `true` if there is a content criterion (the walker must read files).
    fn searches_content(&self) -> bool {
        !matches!(self.content, ContentSpec::None)
    }
}

/// Batch of hits under construction; `matches` is only populated for
/// content searches (aligned 1:1 with `entries`).
struct Batch {
    task_id: TaskId,
    content: bool,
    entries: Vec<Entry>,
    matches: Vec<MatchInfo>,
}

impl Batch {
    fn new(task_id: TaskId, content: bool) -> Self {
        Self {
            task_id,
            content,
            entries: Vec::new(),
            matches: Vec::new(),
        }
    }

    fn push(&mut self, entry: Entry, info: Option<MatchInfo>) {
        self.entries.push(entry);
        if self.content {
            self.matches.push(info.unwrap_or(MatchInfo {
                line: None,
                preview: None,
            }));
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Extracts the accumulated batch as [`SearchHits`], leaving the buffer empty.
    fn take(&mut self) -> SearchHits {
        SearchHits {
            task_id: self.task_id,
            entries: std::mem::take(&mut self.entries),
            matches: if self.content {
                Some(std::mem::take(&mut self.matches))
            } else {
                None
            },
        }
    }
}

/// Outcome of a [`flush`].
enum FlushOutcome {
    /// Sent (or nothing to send): the walk continues.
    Continue,
    /// The receiver died (the search's owner left): ends cleanly.
    ReceiverGone,
    /// Cancelled while `send` was blocked (backpressure): ends as
    /// `Cancelled` (rule 3).
    Cancelled,
}

/// Sends the pending batch (if any). A `send` blocked by backpressure
/// (full channel + slow receiver) does NOT ignore cancellation: it `select`s
/// against `ctx.cancel`.
async fn flush(
    tx: &mpsc::Sender<SearchHits>,
    batch: &mut Batch,
    cancel: &CancellationToken,
) -> FlushOutcome {
    if batch.is_empty() {
        return FlushOutcome::Continue;
    }
    let hits = batch.take();
    tokio::select! {
        biased;
        () = cancel.cancelled() => FlushOutcome::Cancelled,
        r = tx.send(hits) => match r {
            Ok(()) => FlushOutcome::Continue,
            Err(_) => FlushOutcome::ReceiverGone,
        },
    }
}

/// Walks the subtree under `root` in BFS (iterative, no recursion — a
/// hostilely deep hierarchy does not blow the stack) emitting batches of
/// hits via `tx`. Pure read: no journal, no mutations.
///
/// - **Cancellation** (rule 3): `ctx.cancel` is checked per directory, per
///   entry and per content chunk; on cancellation it returns
///   [`Error::Cancelled`] (→ `TaskState::Cancelled`) and `tx` is dropped
///   (the channel closes).
/// - **Symlinks**: NOT followed to descend (avoids cycles); a symlink DOES
///   count as a NAME candidate, but its content is never read.
/// - **`excluded`**: subtrees the walk does NOT look at — no descent, no
///   read, no name hit, and they are not put into `current` (which is
///   broadcast). They count as an examined entry and nothing more. It is
///   what keeps a search over an AGENT's `$HOME` from descending into the
///   daemon's state directory (#165): the read gate only looks at the
///   search's ROOT, so without this a legitimate root would drag the
///   protected subtree along with it.
/// - **Errors per entry**: a `list`/`read` that fails is SKIPPED (counted as
///   an examined entry) and the search continues — an unreadable subdir does
///   not abort it.
/// - **`max_hits`**: once the cap is reached, sends what is pending and
///   ends `Completed` (not `Failed`); the client infers "truncated" by
///   comparing the total received against `max_hits`.
/// - **Progress**: `entries_done` = entries examined (including those
///   skipped on error); `bytes_done` = number of accumulated hits (reuses
///   the field, there are no real bytes in a search); `current` = last
///   entry seen.
/// - **Coalescing**: hits accumulate up to [`SEARCH_HITS_MAX_BATCH`] or
///   drain every `FLUSH_INTERVAL` (whichever comes first).
///
/// # Errors
/// [`Error::Cancelled`] if cancelled; it never propagates per-entry errors
/// (they are skipped). An unreadable `root` counts as one more skip
/// (Completed with 0 hits).
pub async fn run_walk(
    provider: Arc<dyn Provider>,
    root: VPath,
    matchers: SearchMatchers,
    excluded: Vec<VPath>,
    tx: mpsc::Sender<SearchHits>,
    ctx: &crate::scheduler::TaskCtx,
) -> Result<(), Error> {
    // A root that ALREADY falls under an exclusion is not walked at all:
    // without this, the per-entry exclusion would let the protected
    // directory's own listing through.
    if excluded.iter().any(|x| crate::policy::is_under(x, &root)) {
        return Ok(());
    }
    let task_id = ctx.progress.snapshot().task_id;
    let content_search = matchers.searches_content();
    let mut batch = Batch::new(task_id, content_search);
    let mut last_flush = Instant::now();
    let mut hits: usize = 0;

    let mut queue: VecDeque<VPath> = VecDeque::new();
    // HARD boundary of the walk: we do NOT trust `provider.list` to return
    // only byte-genuine descendants of the dir. Every entry is re-verified
    // against `confine` with `is_under` (defense in depth, security T4); a
    // provider with a bug (or malicious) that lists a path outside the root
    // never gets through the filter.
    let confine = root.clone();
    queue.push_back(root);

    while let Some(dir) = queue.pop_front() {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // Unreadable directory: skipped (counted as examined) and continues.
        let Ok(mut stream) = provider.list(&dir).await else {
            ctx.progress.update(|p| p.entries_done += 1);
            continue;
        };
        while let Some(item) = stream.next().await {
            // Time/size flush on EVERY iteration (even if the entry is not a
            // hit): this way the pane drips live even while scanning
            // through failures.
            if !batch.is_empty()
                && (batch.len() >= SEARCH_HITS_MAX_BATCH || last_flush.elapsed() >= FLUSH_INTERVAL)
            {
                match flush(&tx, &mut batch, &ctx.cancel).await {
                    FlushOutcome::Continue => last_flush = Instant::now(),
                    FlushOutcome::ReceiverGone => return Ok(()), // ends cleanly
                    FlushOutcome::Cancelled => return Err(Error::Cancelled),
                }
            }
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let Ok(entry) = item else {
                ctx.progress.update(|p| p.entries_done += 1);
                continue;
            };
            // Belt and suspenders: an entry whose path does NOT fall under
            // the walk's root is ignored ENTIRELY — no descent, no content,
            // no name hit, and it is not filtered into `current` (which is
            // broadcast). The search's scope is a core invariant, not a
            // matter of provider correctness.
            if !crate::policy::is_under(&confine, &entry.path) {
                ctx.progress.update(|p| p.entries_done += 1);
                continue;
            }
            // Excluded subtree (#165): counted as examined and dropped
            // ENTIRELY, before touching `current` — a protected path is not
            // broadcast even in the progress.
            if excluded
                .iter()
                .any(|x| crate::policy::is_under(x, &entry.path))
            {
                ctx.progress.update(|p| p.entries_done += 1);
                continue;
            }
            ctx.progress.update(|p| {
                p.entries_done += 1;
                p.current = Some(entry.path.clone());
            });

            // Descent: dirs yes; symlinks NO (name candidate, not followed).
            //
            // And since 0.81.0, also not if the reader asked not to walk
            // subdirectories or excluded this NAME. Both things stop the
            // descent and only the descent: the folder can still be a
            // result by its name, which is what whoever searches for
            // `node_modules` while excluding what is inside it wants.
            if entry.kind == EntryKind::Dir && matchers.recursive && matchers.descends_into(&entry)
            {
                queue.push_back(entry.path.clone());
            }

            // Name filter (cheap) before touching content.
            let name_bytes = entry.path.file_name().map_or(&[][..], Segment::as_bytes);
            let name_ok = matchers.name.as_ref().is_none_or(|m| m.matches(name_bytes));
            if !name_ok {
                continue;
            }
            // The 0.81.0 filters: kind, size and date. Before content on
            // purpose — they are comparisons over what the entry already
            // carries, and content is a READ per file, which over SFTP is
            // one request per head.
            if !matchers.filters.passes(&entry) {
                continue;
            }

            let info = if content_search {
                // Content only makes sense for regular files.
                if entry.kind != EntryKind::File {
                    continue;
                }
                match search_content(&*provider, &entry.path, &matchers, &ctx.cancel).await {
                    Ok(Some(info)) => Some(info),
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    // No match (or binary), or unreadable: skipped either way.
                    Ok(None) | Err(_) => continue,
                }
            } else {
                None
            };

            batch.push(entry, info);
            hits += 1;
            ctx.progress.update(|p| p.bytes_done = hits as u64);

            if matchers.max_hits.is_some_and(|max| hits >= max) {
                // Truncated: drain what is pending and COMPLETE (the cap
                // was reached).
                let _ = flush(&tx, &mut batch, &ctx.cancel).await;
                return Ok(());
            }
        }
    }
    let _ = flush(&tx, &mut batch, &ctx.cancel).await;
    Ok(())
}

/// Searches for the content criterion in ONE file, streaming. Returns the
/// first match's context (`line`/`preview`) or `None` (no match / binary /
/// empty).
async fn search_content(
    provider: &dyn Provider,
    path: &VPath,
    matchers: &SearchMatchers,
    cancel: &CancellationToken,
) -> Result<Option<MatchInfo>, Error> {
    let mut stream = provider.read(path, None).await?;
    // First NON-empty chunk for encoding detection.
    let first = loop {
        match stream.next().await {
            Some(Ok(c)) if c.is_empty() => {}
            Some(Ok(c)) => break c,
            Some(Err(e)) => return Err(e),
            None => return Ok(None), // empty file: nothing to match.
        }
    };
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let enc = match norte_encoding::detect(first.as_ref()) {
        Detection::Text { encoding, .. } => encoding,
        // Binary (NUL with no BOM): content is NOT searched (spec §17.1a).
        //
        // Even with a FORCED encoding (0.81.0): forcing says which alphabet
        // to read a text with, not that an executable is text. A file with
        // a NUL read as windows-1252 would match by accident against any
        // short needle, and that is noise shaped like a result.
        Detection::Binary => return Ok(None),
    };
    // The encoding the reader FORCED overrides the detected one (0.81.0),
    // the same treatment the viewer gives it: detection is right almost
    // always, and this is for when it is not.
    let enc = matchers.encoding.unwrap_or(enc);
    let cs = matchers.case_sensitive;
    match &matchers.content {
        ContentSpec::None => Ok(None),
        ContentSpec::Literal(text) => {
            // UTF-16 (BOM): the needle is not transcoded to UTF-16
            // (needle_cycle excludes it), so these files are routed through
            // DECODE by lines — they would never match in the byte scan.
            if is_utf16(enc) {
                let needle = text.clone();
                scan_decode_lines(enc, first.to_vec(), stream, cancel, move |line| {
                    line_contains(line, &needle, cs)
                })
                .await
            } else {
                match ContentNeedle::for_encoding(text, cs, enc) {
                    Some(needle) => scan_bytes(&needle, enc, first.to_vec(), stream, cancel).await,
                    None => Ok(None), // empty needle after encoding: no useful match
                }
            }
        }
        ContentSpec::Regex(re) => {
            let re = re.clone();
            scan_decode_lines(enc, first.to_vec(), stream, cancel, move |line| {
                re.is_match(line)
            })
            .await
        }
    }
}

/// LITERAL byte-against-byte scan with the multi-encoding needle and
/// overlap between chunks; tracks the line (counting `\n`) for the match's
/// context. Same mechanics as [`ContentNeedle::find_in`] but inline, because
/// it needs `pos` and the accumulated `\n`s to compute `line`/`preview`
/// (which `find_in` does not expose).
async fn scan_bytes(
    needle: &ContentNeedle,
    enc: &'static Encoding,
    first: Vec<u8>,
    mut stream: ByteStream,
    cancel: &CancellationToken,
) -> Result<Option<MatchInfo>, Error> {
    let mut tail: Vec<u8> = Vec::new();
    // Count of `\n` in bytes that have ALREADY left `tail` (committed front of the file).
    let mut committed_nl: u64 = 0;
    let mut chunk = Some(first);
    loop {
        let c = match chunk.take() {
            Some(c) => c,
            None => match stream.next().await {
                Some(Ok(c)) => c.to_vec(),
                Some(Err(e)) => return Err(e),
                None => break,
            },
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // buf = previous_tail ++ chunk (the match, if it falls, is here).
        let mut buf = std::mem::take(&mut tail);
        buf.extend_from_slice(&c);
        let mut hit: Option<usize> = None;
        for (_e, n) in needle.needles() {
            if let Some(pos) = memchr::memmem::find(&buf, n) {
                hit = Some(hit.map_or(pos, |h: usize| h.min(pos)));
            }
        }
        if let Some(pos) = hit {
            let line = committed_nl + count_nl(&buf[..pos]) + 1;
            let preview = extract_line_preview(&buf, pos, enc);
            return Ok(Some(MatchInfo {
                line: Some(line),
                preview: Some(preview),
            }));
        }
        // Retains the last `max_len-1` bytes (overlap); the rest is committed.
        let keep = needle.max_len().saturating_sub(1).min(buf.len());
        let split = buf.len() - keep;
        committed_nl += count_nl(&buf[..split]);
        tail = buf[split..].to_vec();
    }
    Ok(None)
}

/// Scan over DECODED lines with a STATEFUL decoder: splits on `'\n'` in the
/// decoded TEXT, not on the raw 0x0A byte. Critical for UTF-16 (the `LF` is
/// `0A 00`/`00 0A`: splitting on the lone byte misaligns the pairs and loses
/// the match of any line ≥2). Used by `content_regex` and by the literal
/// path for UTF-16.
///
/// **RAM cap** ([`LINE_MATCH_CAP`]): a line without `'\n'` that exceeds the
/// cap (minified files, single-line CSV) is evaluated TRUNCATED to that
/// prefix and the rest is discarded up to the next `'\n'` — a match beyond
/// the cap within a single giant line is lost (documented limit; the
/// preview is already capped to [`PREVIEW_MAX_CHARS`]).
async fn scan_decode_lines(
    enc: &'static Encoding,
    first: Vec<u8>,
    mut stream: ByteStream,
    cancel: &CancellationToken,
    mut matches: impl FnMut(&str) -> bool,
) -> Result<Option<MatchInfo>, Error> {
    let mut decoder = norte_encoding::StreamDecoder::new(enc);
    let mut pending = String::new();
    let mut line_no: u64 = 0;
    // `true` while discarding bytes of a line already truncated/evaluated.
    let mut skipping = false;
    let mut chunk = Some(first);
    loop {
        let (bytes, last) = match chunk.take() {
            Some(c) => (c, false),
            None => match stream.next().await {
                Some(Ok(c)) => (c.to_vec(), false),
                Some(Err(e)) => return Err(e),
                None => (Vec::new(), true),
            },
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        decoder.feed(&bytes, last, &mut pending);

        // Complete lines (by the '\n' in the decoded text).
        while let Some(nl) = pending.find('\n') {
            let line: String = pending.drain(..=nl).collect();
            line_no += 1;
            if skipping {
                // The giant line was already evaluated truncated: just counted.
                skipping = false;
                continue;
            }
            let l = strip_eol(&line);
            if matches(l) {
                return Ok(Some(MatchInfo {
                    line: Some(line_no),
                    preview: Some(trim_preview(l)),
                }));
            }
        }

        // Line without '\n' that exceeds the cap: evaluate it truncated and
        // discard the rest up to the next '\n' (RAM cap).
        if !skipping && pending.len() > LINE_MATCH_CAP {
            let l = strip_eol(&pending);
            if matches(l) {
                return Ok(Some(MatchInfo {
                    line: Some(line_no + 1),
                    preview: Some(trim_preview(l)),
                }));
            }
            pending.clear();
            skipping = true;
        } else if skipping {
            pending.clear();
        }

        if last {
            // Last line without a final `\n`.
            if !skipping && !pending.is_empty() {
                line_no += 1;
                let l = strip_eol(&pending);
                if matches(l) {
                    return Ok(Some(MatchInfo {
                        line: Some(line_no),
                        preview: Some(trim_preview(l)),
                    }));
                }
            }
            return Ok(None);
        }
    }
}

/// Trims the trailing `\n`/`\r` (CRLF) off a drained line.
fn strip_eol(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// `true` if `line` contains `needle`. NOTE: the case fold on this path
/// (line-by-line decode, UTF-16/`content_regex`) is a simple `to_lowercase`,
/// NOT the same one the literal byte-scan uses, which transcodes case
/// variants via `needle_cycle`. They differ in non-trivial case cases (e.g.
/// `ß`/`SS`); a deliberate asymmetry between the two content paths.
fn line_contains(line: &str, needle: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        line.contains(needle)
    } else {
        line.to_lowercase().contains(&needle.to_lowercase())
    }
}

/// Sanitizes the preview AT THE SOURCE and trims it to `PREVIEW_MAX_CHARS`.
/// The order matters: it masks FIRST (controls/bidi/invisibles → `U+FFFD`
/// via [`norte_encoding::mask_terminal_hazards`]) and trims AFTER, so the
/// cut never lands inside a bidi isolate (already `U+FFFD`) nor leaves an
/// override unclosed. The producer NEVER sends raw hazards over the wire
/// (spec §6): a consumer (e.g. the `fs.search` MCP tool) can paint the
/// preview directly without running ANSI or suffering visual-order spoofing.
///
/// The trim is by char, not by byte (it never splits a multibyte char). It
/// is still not aware of grapheme/terminal cell (a combining cluster or a
/// double-width char can end up cut at the edge): sanitize-first removes
/// the concrete BIDI risk; grapheme/cell remains tied to #79/#81.
fn trim_preview(line: &str) -> String {
    norte_encoding::mask_terminal_hazards(line)
        .chars()
        .take(PREVIEW_MAX_CHARS)
        .collect()
}

/// Extracts the line containing offset `pos` within `buf` (between the
/// surrounding `\n`s, or the buffer's edges), decoded and trimmed. Limit: if
/// the line started before the retained `tail` (a line longer than the
/// overlap), the preview ends up trimmed on the left — best-effort.
fn extract_line_preview(buf: &[u8], pos: usize, enc: &'static Encoding) -> String {
    let start = buf[..pos]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let end = buf[pos..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(buf.len(), |i| pos + i);
    let decoded = norte_encoding::decode(&buf[start..end], enc, true).text;
    let line = decoded.strip_suffix('\r').unwrap_or(&decoded);
    trim_preview(line)
}

/// Number of `\n` in `bytes`.
fn count_nl(bytes: &[u8]) -> u64 {
    memchr::memchr_iter(b'\n', bytes).count() as u64
}

/// `true` if `enc` is UTF-16 (LE or BE) — routed through decode, not
/// byte-scan.
fn is_utf16(enc: &'static Encoding) -> bool {
    enc.name().starts_with("UTF-16")
}

#[cfg(test)]
mod tests {
    use super::*;

    use norte_testkit::MemProvider;

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire")
    }

    /// Minimal `TaskCtx` to call [`run_walk`] without a scheduler.
    fn test_ctx() -> crate::scheduler::TaskCtx {
        let (reporter, _rx) =
            crate::progress::ProgressReporter::new(TaskId::new(1), norte_proto::TaskKind::Search);
        crate::scheduler::TaskCtx {
            pause: crate::scheduler::PauseGate::default(),
            cancel: CancellationToken::new(),
            progress: Arc::new(reporter),
            actor: crate::journal::Actor::User,
        }
    }

    fn params(root: &str) -> FsSearchParams {
        FsSearchParams {
            name_glob: Some("*".into()),
            ..FsSearchParams::new(vp(root))
        }
    }

    async fn tree() -> Arc<MemProvider> {
        let mem = Arc::new(MemProvider::new());
        for d in [
            "mem:///home",
            "mem:///home/u",
            "mem:///home/u/docs",
            "mem:///home/u/.config",
            "mem:///home/u/.config/norte",
            "mem:///home/u/.config/norte-backup",
        ] {
            mem.mkdir(&vp(d)).await.expect("mkdir");
        }
        for f in [
            "mem:///home/u/docs/carta.txt",
            "mem:///home/u/.config/norte/journal.db",
            "mem:///home/u/.config/norte-backup/journal.db",
        ] {
            let mut sink = mem.write(&vp(f)).await.expect("write");
            sink.write(bytes::Bytes::from_static(b"x"))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }
        mem
    }

    async fn walk(excluded: Vec<VPath>, root: &str) -> Vec<VPath> {
        let mem = tree().await;
        let matchers = SearchMatchers::compile(&params(root)).expect("criteria");
        let (tx, mut rx) = mpsc::channel::<SearchHits>(8);
        let ctx = test_ctx();
        let provider: Arc<dyn Provider> = mem;
        let h = tokio::spawn(async move {
            let mut out = Vec::new();
            while let Some(batch) = rx.recv().await {
                out.extend(batch.entries.into_iter().map(|e| e.path));
            }
            out
        });
        run_walk(provider, vp(root), matchers, excluded, tx, &ctx)
            .await
            .expect("walk complete");
        drop(ctx);
        h.await.expect("collector")
    }

    /// Same as [`walk`] but with whatever params are given, for the 0.81.0
    /// filters.
    async fn walk_with(p: FsSearchParams) -> Vec<VPath> {
        let mem = tree().await;
        let root = p.root.clone();
        let matchers = SearchMatchers::compile(&p).expect("criteria");
        let (tx, mut rx) = mpsc::channel::<SearchHits>(8);
        let ctx = test_ctx();
        let provider: Arc<dyn Provider> = mem;
        let h = tokio::spawn(async move {
            let mut out = Vec::new();
            while let Some(batch) = rx.recv().await {
                out.extend(batch.entries.into_iter().map(|e| e.path));
            }
            out
        });
        run_walk(provider, root, matchers, Vec::new(), tx, &ctx)
            .await
            .expect("walk complete");
        drop(ctx);
        h.await.expect("collector")
    }

    /// Excluding a folder NAME skips it at any level, and only stops the
    /// DESCENT: the folder can still be a result.
    #[tokio::test]
    async fn excluding_a_name_stops_descent_but_not_the_folder_as_a_result() {
        let hits = walk_with(FsSearchParams {
            name_glob: Some("*".into()),
            exclude_names: vec!["norte".into()],
            ..FsSearchParams::new(vp("mem:///home/u"))
        })
        .await;
        assert!(
            hits.contains(&vp("mem:///home/u/.config/norte")),
            "the excluded folder is still a result: {hits:?}"
        );
        assert!(
            !hits.contains(&vp("mem:///home/u/.config/norte/journal.db")),
            "but it is not descended into: {hits:?}"
        );
        assert!(
            hits.contains(&vp("mem:///home/u/.config/norte-backup/journal.db")),
            "and the neighbor that only shares a prefix is NOT excluded: {hits:?}"
        );
    }

    /// Without recursion, only the root directory is listed and nothing else.
    #[tokio::test]
    async fn without_recursion_only_the_root_directory() {
        let hits = walk_with(FsSearchParams {
            name_glob: Some("*".into()),
            recursive: false,
            ..FsSearchParams::new(vp("mem:///home/u"))
        })
        .await;
        assert!(hits.contains(&vp("mem:///home/u/docs")), "{hits:?}");
        assert!(
            !hits.contains(&vp("mem:///home/u/docs/carta.txt")),
            "nothing from one level below: {hits:?}"
        );
    }

    /// Filtering by KIND does not stop the walk: what is searched for can
    /// be inside a folder that does not itself count as a result.
    #[tokio::test]
    async fn filtering_by_kind_does_not_stop_the_walk() {
        let hits = walk_with(FsSearchParams {
            name_glob: Some("*".into()),
            kinds: vec![EntryKind::File],
            ..FsSearchParams::new(vp("mem:///home/u"))
        })
        .await;
        assert!(
            hits.contains(&vp("mem:///home/u/docs/carta.txt")),
            "the file two levels down comes out: {hits:?}"
        );
        assert!(
            !hits.contains(&vp("mem:///home/u/docs")),
            "and the folder that had to be traversed does not: {hits:?}"
        );
    }

    /// A filter ALONE, with no name or content, is a legitimate criterion —
    /// and one of the most useful there is ("everything over a gig").
    #[test]
    fn a_filter_alone_is_already_a_criterion() {
        let p = FsSearchParams {
            min_size: Some(1),
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(SearchMatchers::compile(&p).is_ok());
        // And with nothing at all it still is not one.
        let empty = FsSearchParams::new(vp("mem:///"));
        assert!(matches!(
            SearchMatchers::compile(&empty),
            Err(SearchError::NoCriteria)
        ));
    }

    /// A value the provider cannot report does NOT pass a filter on it.
    ///
    /// It is the honest half of the matter: a bucket that reports no date
    /// would return its entire contents under "modified this week", and
    /// that reads exactly like a result.
    #[test]
    fn what_is_unknown_does_not_pass_the_filter() {
        let no_data = Entry {
            path: vp("mem:///x"),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
            attrs: std::collections::BTreeMap::new(),
        };
        let by_size = Filters {
            min_size: Some(0),
            ..Filters::default()
        };
        assert!(!by_size.passes(&no_data), "no size does not pass");
        let by_date = Filters {
            mtime_after: Some(i64::MIN),
            ..Filters::default()
        };
        assert!(!by_date.passes(&no_data), "no date does not pass");
        // And with no filter at all everything passes, which is the 0.80 path.
        assert!(Filters::default().passes(&no_data));
    }

    /// An encoding that is not recognized is a REQUEST error.
    ///
    /// Falling back to the automatic one would return perfectly believable
    /// results read with another alphabet, and whoever forced it did so
    /// because the automatic one did not work for them.
    #[test]
    fn an_unknown_encoding_does_not_fall_back_to_automatic() {
        let p = FsSearchParams {
            content: Some("hola".into()),
            encoding: Some("no-existe-2026".into()),
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(matches!(
            SearchMatchers::compile(&p),
            Err(SearchError::BadEncoding(_))
        ));
    }

    /// Exclusions have a cap, and it is checked BEFORE compiling them.
    ///
    /// `fs.search` can be reached by an agent, and each excluded name
    /// compiles a glob and a regex with its own budget in the task that
    /// serves the connection — no Task yet, so outside the cap on live
    /// tasks.
    #[test]
    fn there_is_a_cap_on_exclusions() {
        let limit = norte_proto::methods::SEARCH_EXCLUDES_MAX;
        let too_many = vec!["x".to_owned(); limit + 1];
        let p = FsSearchParams {
            name_glob: Some("*".into()),
            exclude_names: too_many,
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(matches!(
            SearchMatchers::compile(&p),
            Err(SearchError::TooManyExcludes(_, _))
        ));
        // And right at the cap it passes: the bound is inclusive.
        let exactly = vec!["x".to_owned(); limit];
        let p = FsSearchParams {
            name_glob: Some("*".into()),
            exclude_names: exactly,
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(SearchMatchers::compile(&p).is_ok());
    }

    /// Two filters that cannot both hold at once are SAID.
    ///
    /// Zero results reads as "nothing matched"; here what is missing is the
    /// question, and those are two different things.
    #[test]
    fn impossible_filters_are_said() {
        let impossible = |p: FsSearchParams| {
            assert!(
                matches!(
                    SearchMatchers::compile(&p),
                    Err(SearchError::ImpossibleFilter(_))
                ),
                "should be impossible"
            );
        };
        impossible(FsSearchParams {
            min_size: Some(10),
            max_size: Some(1),
            ..FsSearchParams::new(vp("mem:///"))
        });
        impossible(FsSearchParams {
            mtime_after: Some(100),
            mtime_before: Some(1),
            ..FsSearchParams::new(vp("mem:///"))
        });
        // Content only in folders: content is read from files.
        impossible(FsSearchParams {
            content: Some("hola".into()),
            kinds: vec![EntryKind::Dir],
            ..FsSearchParams::new(vp("mem:///"))
        });
        // And ranges that CAN hold do compile, including a single-value one.
        let p = FsSearchParams {
            min_size: Some(5),
            max_size: Some(5),
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(SearchMatchers::compile(&p).is_ok());
    }

    /// A REPLACEMENT label is not a usable encoding.
    ///
    /// `utf-7` and friends exist in the standard only for a browser to
    /// neutralize them: they decode the whole file to a single U+FFFD.
    /// Accepting them would make the search not fail and not find anything,
    /// which is exactly what forcing an encoding exists to prevent.
    #[test]
    fn a_replacement_label_does_not_work_as_an_encoding() {
        for label in ["utf-7", "hz-gb-2312", "iso-2022-cn"] {
            let p = FsSearchParams {
                content: Some("hola".into()),
                encoding: Some(label.to_owned()),
                ..FsSearchParams::new(vp("mem:///"))
            };
            assert!(
                matches!(
                    SearchMatchers::compile(&p),
                    Err(SearchError::BadEncoding(_))
                ),
                "{label} got through"
            );
        }
        // And a real one does.
        let p = FsSearchParams {
            content: Some("hola".into()),
            encoding: Some("windows-1252".into()),
            ..FsSearchParams::new(vp("mem:///"))
        };
        assert!(SearchMatchers::compile(&p).is_ok());
    }

    /// "Whole word" wraps the pattern in ONE group.
    ///
    /// Without the group, `cat|dog` would read as `\bcat` or `dog\b`: a
    /// different search, and one that matches exactly what was asked to be
    /// excluded.
    #[test]
    fn whole_word_groups_the_alternation() {
        assert_eq!(whole_word_pattern("gato|perro"), r"\b(?:gato|perro)\b");
    }

    /// #165: the read gate looks at the search's ROOT, so a legitimate root
    /// (`$HOME`) would drag the daemon's state directory along with it. The
    /// walk does not go in there — neither the dir nor its content — and the
    /// neighbor that only shares a byte prefix (`norte-backup`) does come out.
    #[tokio::test]
    async fn the_walk_does_not_enter_an_excluded_subtree() {
        let hits = walk(vec![vp("mem:///home/u/.config/norte")], "mem:///home/u").await;
        assert!(
            hits.contains(&vp("mem:///home/u/docs/carta.txt")),
            "what is outside still comes out: {hits:?}"
        );
        assert!(
            hits.contains(&vp("mem:///home/u/.config/norte-backup/journal.db")),
            "the neighbor with the same prefix is NOT protected: {hits:?}"
        );
        assert!(
            !hits
                .iter()
                .any(|p| p.to_wire().starts_with("mem:///home/u/.config/norte/")),
            "nothing from inside the protected subtree: {hits:?}"
        );
        assert!(
            !hits.contains(&vp("mem:///home/u/.config/norte")),
            "not even the protected directory itself: {hits:?}"
        );
    }

    /// And a root that ALREADY falls under an exclusion is not walked at
    /// all: without this, the protected directory's own listing would be
    /// emitted whole.
    #[tokio::test]
    async fn an_excluded_root_gives_not_even_one_row() {
        let hits = walk(
            vec![vp("mem:///home/u/.config/norte")],
            "mem:///home/u/.config/norte",
        )
        .await;
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn glob_and_nfc_insensitivity() {
        let m = NameMatcher::glob("*.RS", false).expect("glob");
        assert!(m.matches(b"main.rs"));
        // NFD vs NFC: "año.rs" with the ñ decomposed matches the glob "año*".
        let m = NameMatcher::glob("año*", false).expect("glob");
        assert!(m.matches("an\u{0303}o.rs".as_bytes()));
        // Non-UTF8 bytes: no panic, matches over the lossy form.
        let m = NameMatcher::glob("*", false).expect("glob");
        assert!(m.matches(b"\xFF\xFE"));
    }

    /// #110 (same fix as the frontend's pattern-based marking): `?` and
    /// classes count CHARACTERS, not UTF-8 bytes — globset alone compiles
    /// `(?-u)` byte-mode, where `a?o` did not match `año` (ñ = 2 bytes) and
    /// `a[ñx]o` matched `axo` but never `año`. The pattern a user learns in
    /// search holds in marking and vice versa.
    #[test]
    fn glob_counts_characters_not_utf8_bytes() {
        let m = NameMatcher::glob("a?o.txt", false).expect("glob");
        assert!(m.matches("a\u{f1}o.txt".as_bytes()), "? = one character");
        assert!(m.matches(b"axo.txt"));
        let m = NameMatcher::glob("a[\u{f1}x]o.txt", false).expect("glob");
        assert!(m.matches("a\u{f1}o.txt".as_bytes()), "class with multibyte");
        assert!(m.matches(b"axo.txt"));
        let m = NameMatcher::glob("a[\u{f0}-\u{f2}]o.txt", false).expect("glob");
        assert!(m.matches("a\u{f1}o.txt".as_bytes()), "multibyte range");
        assert!(!m.matches(b"axo.txt"));
        // Astral (4 UTF-8 bytes): one character, not four.
        let m = NameMatcher::glob("?.txt", false).expect("glob");
        assert!(m.matches("\u{1D11E}.txt".as_bytes()), "𝄞 = ONE character");
    }

    /// The #110 translation must PRESERVE `dot_matches_new_line` (globset
    /// compiles its matcher with it): `\n` is a legal name byte on unix
    /// (corpus `control_newline`) and `*`/`?` translate to `.`-derived forms
    /// — losing the flag would make `*` silently stop matching those names,
    /// the inverse of the byte/character bug.
    #[test]
    fn glob_still_matches_names_with_newline() {
        let m = NameMatcher::glob("*", false).expect("glob");
        assert!(m.matches(b"a\nb"));
        let m = NameMatcher::glob("a?b", false).expect("glob");
        assert!(m.matches(b"a\nb"), "? also crosses \\n, like in globset");
        let m = NameMatcher::glob("*.txt", false).expect("glob");
        assert!(m.matches(b"a\nb.txt"));
    }

    /// Guard for the shape of globset's regex that the #110 translation
    /// decodes (`unicode_glob_regex`): the `(?-u)` prefix and non-ASCII
    /// bytes as `\xNN` escape runs. A globset upgrade that changes either
    /// fails HERE, loudly, instead of silently ceasing to match non-ASCII
    /// names. (The frontend pins its own the same way —
    /// `globset_regex_shape_is_the_one_this_translation_expects` in
    /// `norte-frontend::pane` — because each side has its own copy of the
    /// translator, same criterion as the duplicated fold.)
    #[test]
    fn the_globset_regex_shape_is_the_one_the_translation_expects() {
        let g = globset::GlobBuilder::new("a\u{f1}o").build().expect("glob");
        assert!(g.regex().starts_with("(?-u)"), "{}", g.regex());
        assert!(g.regex().contains(r"\xc3\xb1"), "{}", g.regex());
        assert_eq!(unicode_glob_regex(&g).expect("translation"), "^a\u{f1}o$");
    }

    #[test]
    fn name_regex_with_size_limit() {
        assert!(
            NameMatcher::regex("^ma.n\\.rs$", false)
                .expect("re")
                .matches(b"main.rs")
        );
        // Regex bomb: the size_limit (1 MiB) rejects it at COMPILE time, it
        // does not hang. The `regex` engine is linear (no catastrophic
        // backtracking), so the guard is on the compiled program's MEMORY:
        // `(a|aa)`×8000 exceeds 1 MiB (measured; ×2000 did not reach it).
        assert!(NameMatcher::regex(&"(a|aa)".repeat(8000), false).is_err());
    }

    #[test]
    fn multiencoding_literal_needle() {
        let n = ContentNeedle::literal("año", false);
        // UTF-8:
        assert!(
            n.find_in(&mut Overlap::default(), "hay un año aquí".as_bytes())
                .is_some()
        );
        // Latin-1 (0xF1 = ñ):
        assert!(
            n.find_in(&mut Overlap::default(), b"hay un a\xF1o aqu\xED")
                .is_some()
        );
        // And in two chunks splitting the needle in half (overlap):
        let mut ov = Overlap::default();
        assert!(n.find_in(&mut ov, "hay un a".as_bytes()).is_none());
        assert!(
            n.find_in(&mut ov, "\u{00F1}o aqu\u{00ED}".as_bytes())
                .is_some()
        );
    }

    #[test]
    fn content_case_insensitive_ascii() {
        let n = ContentNeedle::literal("AÑO", false);
        assert!(
            n.find_in(&mut Overlap::default(), "el año".as_bytes())
                .is_some()
        );
    }

    #[test]
    fn a_needle_not_representable_in_an_encoding_is_skipped() {
        let n = ContentNeedle::literal("π", false);
        // UTF-8 (0xCF 0x80) is found.
        assert!(
            n.find_in(&mut Overlap::default(), "un π aquí".as_bytes())
                .is_some()
        );
        // windows-1252's encoder CANNOT map π: it would emit the numeric
        // reference "&#960;". That encoding is DISCARDED (unmappable), so
        // its lossy representation NEVER becomes a needle → it does not match.
        assert!(n.find_in(&mut Overlap::default(), b"&#960;").is_none());
    }

    // The corpus cases with real files ALREADY exist over the content walker
    // (`engine_search.rs`: `contenido_encoding_aware_tres_ficheros`), and the
    // canonical fixture for the CJK false positive lives in norte-testkit's
    // corpus (`cjk_utf8_lead_f1`). Here only the matchers' BEHAVIOR with
    // synthetic bytes is pinned.

    #[test]
    fn short_latin_needle_false_positive_is_a_literal_mode_limit() {
        // "ñ" in windows-1252/ISO-8859-15 = 1 byte 0xF1. In UTF-8 CJK
        // content, 0xF1 shows up as the LEAD byte of a 4-byte sequence
        // (U+40000..U+7FFFF) → blind mode (`literal`) matches BY CHANCE.
        // Known and documented limit of the multi-needle mode.
        let cjk_utf8 = b"texto \xF1\x84\x80\x81 fin"; // char U+44001 (F1 84 80 81)
        let multi = ContentNeedle::literal("ñ", false);
        assert!(
            multi.find_in(&mut Overlap::default(), cjk_utf8).is_some(),
            "known limit: the 1-byte legacy needle matches by chance"
        );
        // ENCODING-AWARE with the DETECTED encoding (UTF-8) does NOT have the
        // false positive: it searches for 0xC3 0xB1 / 0xC3 0x91, absent from
        // that content.
        let aware = ContentNeedle::for_encoding("ñ", false, norte_encoding::UTF_8)
            .expect("non-empty needle");
        assert!(aware.find_in(&mut Overlap::default(), cjk_utf8).is_none());
    }

    #[test]
    fn for_encoding_targeted_finds_in_its_own_encoding() {
        let w1252 = norte_encoding::Encoding::for_label(b"windows-1252").unwrap();
        // File detected as windows-1252: the targeted needle finds 0xF1.
        let aware = ContentNeedle::for_encoding("año", false, w1252).expect("needle");
        assert!(
            aware
                .find_in(&mut Overlap::default(), b"un a\xF1o legacy")
                .is_some()
        );
        // And also its UTF-8 form (safety net always included).
        assert!(
            aware
                .find_in(&mut Overlap::default(), "un año utf8".as_bytes())
                .is_some()
        );
    }

    #[test]
    fn utf16_with_bom_is_not_covered_by_the_literal_needle_documented_limit() {
        // A genuinely UTF-16LE file with a BOM: "año" = FF FE 61 00 F1 00
        // 6F 00. The literal needle does NOT encode to UTF-16 (needle_cycle
        // excludes it; `encode_lossless` falls back to UTF-8 under the
        // WHATWG rule), so it does NOT match — the UTF-8/legacy bytes are
        // not contiguous between the NULs. Documented limit: T3 routes
        // UTF-16-with-BOM files through DECODING (corpus debt: `year_utf16bom`).
        let utf16_bom = b"\xFF\xFE\x61\x00\xF1\x00\x6F\x00";
        let n = ContentNeedle::literal("año", false);
        assert!(n.find_in(&mut Overlap::default(), utf16_bom).is_none());
    }
}
