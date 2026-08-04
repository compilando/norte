//! Markdown-lite parser (ADR 0040, decision 3): a CLOSED vocabulary. What
//! this parser cannot express, a third-party `help.md` cannot paint on the
//! terminal — that is the security barrier, not an allowlist bolted on
//! afterwards.

use crate::front_matter::{self, FrontMatterError};
use crate::model::{Block, Callout, Origin, Span, Topic, TopicId};

/// Opener of a command reference. Opener and closer are constants so that
/// the needle we search for and the number of bytes we skip can never
/// desynchronise: every offset below is derived from these literals.
///
/// Shared with `check` (crate-visible, nothing more) so that the check for
/// marks written where this parser never looks — a heading, a table cell —
/// searches for the SAME needle this one opens on. Two spellings of the opener
/// would drift, and the drift would be silent in the direction that matters:
/// a mark the parser recognises and the check does not.
pub(crate) const CMD_OPEN: &str = "{{cmd:";

/// Closer of a command reference.
const CMD_CLOSE: &str = "}}";

/// Opener of a topic link. Crate-visible for the same reason [`CMD_OPEN`] is:
/// a link written where this parser never looks is inert exactly as a command
/// mark is, and both checks must search for the needle this file opens on.
pub(crate) const LINK_OPEN: &str = "[[";

/// Closer of a topic link.
const LINK_CLOSE: &str = "]]";

/// Cuts a line into [`Span`]s. A badly closed mark stays literal text: it
/// never produces a reference to a command nobody wrote.
fn spans(line: &str, mode: Mode<'_>) -> Vec<Span> {
    spans_counted(line, mode).0
}

/// [`spans`], plus how many tail scans came back empty.
///
/// The count is the observable form of the [`Closers`] invariant: it is
/// bounded by one per mark kind, whatever the input. Tests assert on it
/// instead of on a stopwatch, because a wall clock measures the machine and
/// this measures the algorithm.
fn spans_counted(line: &str, mode: Mode<'_>) -> (Vec<Span>, usize) {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = line;
    let mut closers = Closers::new();
    while !rest.is_empty() {
        let taken = take_mark(rest, &mut closers, mode).or_else(|| take_delim(rest));
        if let Some((span, len)) = taken {
            if !text.is_empty() {
                out.push(Span::Text(std::mem::take(&mut text)));
            }
            out.push(span);
            rest = &rest[len..];
            continue;
        }
        let ch = rest.chars().next().unwrap_or_default();
        text.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    if !text.is_empty() {
        out.push(Span::Text(text));
    }
    (out, closers.failed_tail_scans)
}

/// What the scan has already ruled out, one flag per mark.
///
/// `rest` only ever shrinks, so a closer missing from one tail is missing
/// from every shorter tail: remembering that turns a line built out of
/// openers from quadratic into linear. Found by the long-line hardening
/// test, where 100k bytes of `{{cmd:` scanned the whole tail once per
/// opener; a plugin `help.md` (task 6) is exactly where such a line comes
/// from.
///
/// INVARIANT: one instance per call, consumed against a strictly shrinking
/// suffix of ONE input. That is the whole basis of the memo — "no closer in
/// this tail" only implies "no closer in any later tail" while later tails
/// are suffixes of this one. Sharing an instance across inputs (hoisting it
/// out of a per-line loop, say) is UNSOUND: line 2 is not a suffix of line
/// 1, so every mark after the first unclosed one would be silently swallowed
/// and no test on line 1 would notice. Hence the private [`Closers::new`]
/// and no `Clone`, `Copy` or `Default`: an instance is only ever born inside
/// [`spans_counted`].
struct Closers {
    /// A `}}` may still lie ahead.
    cmd: bool,
    /// A `]]` may still lie ahead.
    link: bool,
    /// Tail scans that found no closer. Bounded by one per mark kind while
    /// the invariant above holds; unbounded the moment the memo is removed.
    failed_tail_scans: usize,
}

impl Closers {
    /// A fresh memo for ONE input. See the type's INVARIANT.
    fn new() -> Self {
        Self {
            cmd: true,
            link: true,
            failed_tail_scans: 0,
        }
    }
}

/// Characters that paint NOTHING and are not in
/// `norte_encoding::is_terminal_hazard`.
///
/// That predicate is a cross-crate contract (`norte-frontend` names, the
/// viewer, this parser) and widening it is not this crate's call, so the
/// local consequence is handled locally: these characters are treated as
/// BLANK. Without that, `title = "ㅤㅤㅤ"` — three U+3164 HANGUL FILLERs —
/// is a non-empty string that renders as a nameless page.
const INVISIBLE: [char; 5] = [
    '\u{2064}', // INVISIBLE PLUS
    '\u{3164}', // HANGUL FILLER
    '\u{115F}', // HANGUL CHOSEONG FILLER
    '\u{180E}', // MONGOLIAN VOWEL SEPARATOR
    '\u{2800}', // BRAILLE PATTERN BLANK
];

/// Is this id nothing but blanks? An id that is must never become a live
/// mark: the corpus checks read a `CommandRef` as a claim that the command
/// exists.
///
/// `trim().is_empty()` is not enough, and the gap is on the hostile path.
/// Masking runs BEFORE the span parse (see [`spans_masked`]) and several
/// members of the hazard set are WHITESPACE — `\t`, `\r`, U+000B, U+000C,
/// U+0085, U+2028 and U+2029. Masking turns each of them into `U+FFFD`, which
/// is NOT whitespace, so a plain trim finds content where there was none and
/// `{{cmd:\t}}` fabricates a command reference out of a tab.
///
/// `U+FFFD` counts as blank in EVERY mode, trusted included, rather than only
/// when `mask` is on: it is never part of a real command or topic id, and one
/// rule that always holds is worth more than a mode-dependent one nobody can
/// keep in their head. The consequence is deliberate: an id whose bytes were
/// invalid UTF-8 (lossy-decoded to `U+FFFD`) also stays literal text.
fn is_blank_id(id: &str) -> bool {
    id.chars()
        .all(|c| c.is_whitespace() || c == '\u{FFFD}' || INVISIBLE.contains(&c))
}

/// What the parser is reading. It decides three things at once — masking,
/// which live marks may exist, and which command ids belong to the author —
/// because they are one decision: how much this text is trusted to say.
#[derive(Clone, Copy, Debug)]
enum Mode<'a> {
    /// The embedded corpus. Our own text: nothing masked, every mark live.
    Trusted,
    /// A plugin `help.md`, on behalf of the plugin whose HOST-ASSIGNED id
    /// this is. Masked, and the live marks restricted to what a plugin may
    /// say about itself.
    Plugin(&'a str),
}

impl Mode<'_> {
    /// Does this text get its terminal hazards masked?
    fn masks(self) -> bool {
        matches!(self, Self::Plugin(_))
    }

    /// May `{{cmd:id}}` become a live reference here?
    ///
    /// A plugin may only reference ITS OWN commands. Otherwise a `help.md`
    /// writes `> ⚠ Press {{cmd:fs.delete}} to apply the update` and gets the
    /// host's warning chrome, the host's real effective chord resolved at
    /// render time, and a runnable row on the same dispatch path as the
    /// palette — added after approval, since help is outside the approval
    /// digest. The refusal lives HERE, where the data is built, not in the
    /// frontend that will draw it.
    fn allows_command(self, id: &str) -> bool {
        match self {
            Self::Trusted => !is_blank_id(id),
            Self::Plugin(plugin) => is_own_command(id, plugin),
        }
    }

    /// May `[[topic]]` become a live link here?
    ///
    /// Never from a plugin. `see_also` is dropped so a plugin cannot link
    /// into the host's topics; a `[[…]]` in the body is the same jump
    /// through the other door, and the two must agree.
    fn allows_link(self) -> bool {
        matches!(self, Self::Trusted)
    }
}

/// Prefix of a plugin command's dispatch key, as built by the palette
/// (`norte-frontend`): `plugin:{plugin_id}:{command_id}`.
const COMMAND_PREFIX: &str = "plugin:";

/// Ceiling on a command id, in characters. The honest maximum is
/// `plugin:` + a 128-char plugin id + `:` + a 64-char command id = 200, so
/// this cannot bite a real command. Anything longer is not a dispatch key,
/// and it is DROPPED rather than truncated: a truncated key is a different
/// key, which could name something else entirely.
const MAX_COMMAND_ID_CHARS: usize = 200;

/// Is `id` a dispatch key for a command belonging to `plugin`?
///
/// The rule is the palette's: `plugin:{plugin_id}:{command_id}`, where the
/// plugin id is charset-validated by the core and can never contain `:`, so
/// the split is unambiguous. Same reasoning as the palette's mandatory
/// `[extension]` prefix on plugin rows — a plugin naming its command exactly
/// like a built-in must not be able to pass for one.
///
/// Everything here REJECTS; nothing rewrites. A command id is an identity, so
/// masking it would be worse than useless: it would produce a key that
/// resolves to nothing, or to the wrong thing. A key we could not paint
/// safely is a key we refuse — which is also what lets the model promise that
/// every string it carries is free of terminal hazards.
fn is_own_command(id: &str, plugin: &str) -> bool {
    // A plugin id that is empty, or that carries the separator, would make
    // the split ambiguous. Fail closed: this entry point is fed over the
    // wire and does not get to assume the caller validated anything.
    if plugin.is_empty() || plugin.contains(':') {
        return false;
    }
    if id.chars().count() > MAX_COMMAND_ID_CHARS {
        return false;
    }
    // `U+FFFD` too: it is what a masked hazard and an undecodable byte both
    // become, and neither is part of a command anyone can dispatch.
    if id
        .chars()
        .any(|c| c == '\u{FFFD}' || INVISIBLE.contains(&c) || norte_encoding::is_terminal_hazard(c))
    {
        return false;
    }
    id.strip_prefix(COMMAND_PREFIX)
        .and_then(|rest| rest.strip_prefix(plugin))
        .and_then(|rest| rest.strip_prefix(':'))
        .is_some_and(|cmd| !is_blank_id(cmd))
}

/// `{{cmd:…}}` and `[[…]]`, the corpus' two own marks.
fn take_mark(rest: &str, closers: &mut Closers, mode: Mode<'_>) -> Option<(Span, usize)> {
    if closers.cmd
        && let Some(after) = rest.strip_prefix(CMD_OPEN)
    {
        let Some(end) = after.find(CMD_CLOSE) else {
            closers.cmd = false;
            closers.failed_tail_scans += 1;
            return None;
        };
        let id = after[..end].trim();
        // A refused mark degrades to literal text, exactly as a malformed
        // one does: the reader still sees the sentence, and the `{{cmd:…}}`
        // left in it is the evidence that something claimed to be a command.
        if !mode.allows_command(id) {
            return None;
        }
        return Some((
            Span::CommandRef(id.to_owned()),
            CMD_OPEN.len() + end + CMD_CLOSE.len(),
        ));
    }
    if closers.link
        && let Some(after) = rest.strip_prefix(LINK_OPEN)
    {
        let Some(end) = after.find(LINK_CLOSE) else {
            closers.link = false;
            closers.failed_tail_scans += 1;
            return None;
        };
        let id = after[..end].trim();
        if !mode.allows_link() || is_blank_id(id) {
            return None;
        }
        return Some((
            Span::TopicLink(TopicId::new(id)),
            LINK_OPEN.len() + end + LINK_CLOSE.len(),
        ));
    }
    None
}

/// `**strong**`, `*emphasis*` and `` `code` ``. Inline code is taken first
/// and its content is NOT reinterpreted.
fn take_delim(rest: &str) -> Option<(Span, usize)> {
    for (open, close, build) in [
        (
            "`",
            "`",
            (|s: &str| Span::Code(s.to_owned())) as fn(&str) -> Span,
        ),
        ("**", "**", |s: &str| Span::Strong(s.to_owned())),
        ("*", "*", |s: &str| Span::Emph(s.to_owned())),
    ] {
        if let Some(after) = rest.strip_prefix(open)
            && let Some(end) = after.find(close)
            && end > 0
        {
            return Some((build(&after[..end]), open.len() + end + close.len()));
        }
    }
    None
}

/// Parser limits. The embedded corpus uses [`Limits::built_in`]; a plugin
/// `help.md` uses [`Limits::untrusted`].
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Ceiling on the SOURCE, in bytes. It is applied by the entry point
    /// BEFORE anything is parsed — `parse_untrusted` (task 6) cuts the input
    /// on a UTF-8 boundary and raises [`Parsed::truncated`]. [`parse_trusted`]
    /// does not apply it: the embedded corpus is a build-time input, and
    /// silently amputating a topic there would hide a corpus bug instead of
    /// reporting it.
    pub max_bytes: usize,
    /// Maximum number of blocks a body may produce. Beyond it the parser
    /// stops and raises [`Parsed::truncated`], keeping everything already
    /// parsed. It bounds the block COUNT and nothing else — a single block
    /// can hold an arbitrary number of cells or items, which is what
    /// [`Limits::max_cells`] is for.
    pub max_blocks: usize,
    /// Maximum bytes of a SOURCE line, code-fence content included. A longer
    /// line is cut on a `char` boundary; the rest of the line is dropped.
    ///
    /// The bound is on the source, not on the model. Masking runs after the
    /// clamp and replaces a 1-byte control with a 3-byte `U+FFFD`, so a line
    /// of nothing but controls reaches the model at three times this number —
    /// and never more, since 3 bytes is the most a single character can grow
    /// to. Clamping again afterwards was the alternative and it is worse: it
    /// would cut a line whose masked form is over the ceiling while its
    /// source was not, which is a loss the source cannot explain.
    pub max_line_bytes: usize,
    /// Maximum table cells a whole body may produce, header cells included.
    ///
    /// This is the only ceiling on the PRODUCT of a table's width by its
    /// height, and without it the row normalisation is a memory amplifier:
    /// rows are padded to the header width, so a 1024-cell header followed by
    /// 31744 one-byte rows turns 64 KiB of source into 32.5M cells and 750
    /// MiB of RSS. Neither of the other ceilings sees it — `max_blocks`
    /// counts a whole table as ONE block and `max_line_bytes` bounds the
    /// width, never the product. The budget runs across the entire body, not
    /// per table, otherwise `max_blocks` tables could each spend it in full.
    pub max_cells: usize,
}

impl Limits {
    /// Limits for the embedded corpus, which we wrote ourselves.
    ///
    /// ```
    /// use norte_help::Limits;
    ///
    /// assert!(Limits::built_in().max_blocks > Limits::untrusted().max_blocks);
    /// ```
    #[must_use]
    pub fn built_in() -> Self {
        Self {
            max_bytes: 256 * 1024,
            max_blocks: 4096,
            max_line_bytes: 8 * 1024,
            max_cells: 64 * 1024,
        }
    }

    /// Limits for a third-party `help.md`: tighter, because the file arrives
    /// from a plugin and the only thing bounding it is this struct.
    ///
    /// ```
    /// use norte_help::Limits;
    ///
    /// assert!(Limits::untrusted().max_bytes < Limits::built_in().max_bytes);
    /// ```
    #[must_use]
    pub fn untrusted() -> Self {
        Self {
            max_bytes: 64 * 1024,
            max_blocks: 512,
            max_line_bytes: 2 * 1024,
            max_cells: 8 * 1024,
        }
    }
}

/// Parse result: the topic plus what had to be cut.
#[derive(Clone, Debug)]
pub struct Parsed {
    /// The parsed topic.
    pub topic: Topic,
    /// A [`Limits`] ceiling was hit and the parser stopped short. It feeds
    /// the UI badge: a reader must never mistake a cut topic for a complete
    /// one.
    ///
    /// It is exactly "a ceiling was hit", not "every byte the source had is
    /// in the model". One shape of loss is deliberately NOT flagged: cells
    /// past the header width of a table, which `normalise_row` drops because
    /// they have no column to live in — flagging them would badge every
    /// topic with a typo'd table row as truncated.
    pub truncated: bool,
    /// Some byte of the source did not decode under the detected encoding and
    /// came out as `U+FFFD`. Always `false` for [`parse_trusted`], whose input
    /// is already a `&str`.
    ///
    /// It is NOT "the bytes were not UTF-8": [`parse_untrusted`] detects the
    /// encoding before decoding, so a windows-1252 or UTF-16 `help.md` reads
    /// correctly and is not lossy. What the flag reports is text that could
    /// not be recovered under ANY reading the parser was willing to make.
    pub lossy: bool,
}

impl Parsed {
    /// ORs in flags a caller learnt out of band, in BOTH places they live:
    /// this struct and the [`Origin::Plugin`] copy a renderer reads.
    ///
    /// This is the seam for the phase H3e wire hop. The host parses a plugin
    /// `help.md` (say `truncated == true`), sends the ALREADY BOUNDED
    /// markdown over the protocol, and the frontend parses that. Both flags
    /// come out structurally false on the second parse — the text arrives
    /// short and clean, so nothing trips a ceiling and nothing fails to
    /// decode — and the badge, which is the whole user-facing mitigation for
    /// hostile help, would silently go clean. Only the sender knows, so the
    /// sender has to be able to say.
    ///
    /// It only ever ORs: a caller cannot clear a flag this parse raised.
    ///
    /// ```
    /// use norte_help::{Origin, parse_untrusted};
    ///
    /// // What the frontend re-parses: already bounded, nothing to flag.
    /// let parsed = parse_untrusted(b"body", "acme.ftp", None);
    /// assert!(!parsed.truncated);
    ///
    /// let parsed = parsed.fold_flags(true, false);
    /// assert!(parsed.truncated && !parsed.lossy);
    /// let Origin::Plugin { truncated, .. } = parsed.topic.origin else {
    ///     unreachable!("a plugin topic")
    /// };
    /// assert!(truncated, "the copy the renderer reads is corrected too");
    /// ```
    #[must_use]
    pub fn fold_flags(mut self, truncated: bool, lossy: bool) -> Self {
        self.truncated |= truncated;
        self.lossy |= lossy;
        if let Origin::Plugin {
            truncated: t,
            lossy: l,
            ..
        } = &mut self.topic.origin
        {
            *t |= truncated;
            *l |= lossy;
        }
        self
    }
}

/// Why a TRUSTED topic could not be parsed.
///
/// `#[non_exhaustive]` on purpose: the corpus checks grow reasons to reject a
/// topic, and adding one must not break the `match`es of whoever reports
/// them. There is no such enum on the hostile path — `parse_untrusted` (task
/// 6) never fails, it degrades.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    /// The front matter is missing, unterminated or not valid TOML.
    #[error("front matter: {0}")]
    FrontMatter(#[from] FrontMatterError),
}

/// Parses a topic from the embedded corpus. A failure here is a build
/// failure: the suite parses the whole corpus (see `tests/corpus.rs`).
///
/// ```
/// use norte_help::{Block, parse_trusted};
///
/// let src = "+++\nid = \"copying\"\ntitle = \"Copying\"\n+++\n# Copying\n";
/// let parsed = parse_trusted(src).expect("a valid topic");
/// assert_eq!(parsed.topic.id.as_str(), "copying");
/// assert!(!parsed.truncated && !parsed.lossy);
/// assert_eq!(
///     parsed.topic.blocks,
///     vec![Block::Heading { level: 1, text: "Copying".to_owned() }]
/// );
/// ```
///
/// The header fields (`title`, `tags`, `see_also`, `commands`, `context`) are
/// copied RAW, which is right here and would be a hole on the hostile path:
/// [`Origin::Plugin`] promises "already masked", so [`parse_untrusted`] masks
/// and caps the header fields it keeps and drops the rest — the body masking
/// never touches them.
///
/// # Errors
/// [`ParseError::FrontMatter`] if the `+++` header is missing, unterminated,
/// carries trailing content on its closing fence, or is not valid TOML. The
/// BODY never fails: whatever the parser cannot express degrades into
/// paragraphs.
pub fn parse_trusted(source: &str) -> Result<Parsed, ParseError> {
    let (fm, body) = front_matter::split(source)?;
    let limits = Limits::built_in();
    let (blocks, truncated) = blocks_of(body, limits, Mode::Trusted);
    Ok(Parsed {
        topic: Topic {
            // Raw, and only sound because this is the TRUSTED corpus.
            // `parse_untrusted` masks them; see the note above.
            id: TopicId::new(fm.id),
            title: fm.title,
            tags: fm.tags,
            see_also: fm.see_also.into_iter().map(TopicId::new).collect(),
            commands: fm.commands,
            context: fm.context,
            blocks,
            origin: Origin::BuiltIn,
        },
        truncated,
        lossy: false,
    })
}

/// Ceiling, in CHARACTERS, on every string a plugin contributes to the model
/// outside the body: the header's `id` and `title`, each entry of its
/// `commands`, and the manifest's `publisher`.
///
/// 280 characters, the number the plugin manifest already applies to
/// `description` and to a command's id and title
/// (`CONFIG_DESCRIPTION_MAX_CHARS`). Reusing it means one number a reviewer
/// has to hold in their head instead of two, and it is generous enough for a
/// real title while far too small to paint a screen.
///
/// CHARACTERS and not bytes for the manifest's own reason: a non-ASCII
/// language must not pay the cap early. Masking replaces one character with
/// exactly one `U+FFFD`, so the cap holds identically before and after it.
///
/// This is a DISPLAY bound, not a memory one — [`Limits::max_bytes`] already
/// bounds the whole source. What it buys is that a plugin cannot hand the UI
/// a 60 KiB "title".
const MAX_HEADER_CHARS: usize = 280;

/// How far the boundary walk may step back: a UTF-8 character is at most four
/// bytes, so three continuation bytes is the most that can ever precede a cut
/// that landed inside one.
const MAX_WALK_BACK: usize = 3;

/// Truncates to `max` bytes without splitting a UTF-8 character (when the
/// bytes are UTF-8 at all).
///
/// The walk back cannot underflow and cannot loop: `cut` only decreases, and
/// the guards stop it at 0. It is also BOUNDED to [`MAX_WALK_BACK`] steps,
/// which is not an optimisation — without it, a source of pure continuation
/// bytes (`0x80` repeated, valid UTF-8 at no offset at all) walks the cut down
/// to ZERO and the whole document vanishes: 64 KiB in, an empty topic out,
/// with `lossy` reporting clean because there was nothing left to decode.
/// Failing to find a boundary within three steps means these are not UTF-8
/// bytes, so the cut stands where it was and the decoder deals with them.
///
/// For a non-UTF-8 encoding this is a best-effort cut and nothing more; the
/// real safety net is `norte_encoding::decode` with `complete = false`, which
/// leaves a sequence split by the cut PENDING instead of calling it a loss.
fn cut_at_boundary(bytes: &[u8], max: usize) -> &[u8] {
    let mut cut = max.min(bytes.len());
    let floor = cut.saturating_sub(MAX_WALK_BACK);
    // Walk back while the cut byte is a `10xxxxxx` continuation byte.
    while cut > floor && cut < bytes.len() && bytes[cut] & 0b1100_0000 == 0b1000_0000 {
        cut -= 1;
    }
    &bytes[..cut]
}

/// Decodes a plugin file into text, plus whether anything failed to decode.
///
/// This is the house text boundary (ADR 0008): `norte_encoding::detect` then
/// `decode`, the same pipeline the viewer uses. Hard-coding a UTF-8 read
/// instead was two bugs at once — a `help.md` saved by a Windows editor
/// carries a `EF BB BF` BOM, which made the `+++` prefix fail and threw the
/// entire front matter away with no badge at all, and a UTF-16 or
/// windows-1252 file rendered as mojibake with every flag clean.
///
/// `head` is the bounded prefix that will actually be parsed; `bytes` is the
/// whole source, used for the DECISION and for the honesty of the flag.
fn decode_source(bytes: &[u8], head: &[u8]) -> (String, bool) {
    let utf8_whole = std::str::from_utf8(bytes).is_ok();
    let encoding = match norte_encoding::detect(bytes) {
        // A BOM is certainty, not statistics.
        norte_encoding::Detection::Text {
            encoding,
            bom: true,
        } => encoding,
        // Valid UTF-8 IS UTF-8. Asking chardetng about a file we can already
        // read invites a statistical guess to mangle a short document.
        _ if utf8_whole => norte_encoding::UTF_8,
        norte_encoding::Detection::Text { encoding, .. } => encoding,
        // NUL and no BOM: `detect` calls that binary, which for a file the
        // user opens means the hexview. There is no hexview for a help topic,
        // and refusing to show a plugin's page over one stray NUL is worse
        // than showing it with the NUL masked.
        norte_encoding::Detection::Binary => norte_encoding::UTF_8,
    };
    let cut = head.len() < bytes.len();
    // `complete = false` when the tail was cut: a multi-byte sequence split
    // by OUR ceiling is pending, not a loss.
    let decoded = norte_encoding::decode(head, encoding, !cut);
    // The cut must not be able to hide a decode failure. Beyond the ceiling
    // nothing is decoded, so for UTF-8 — every `help.md` a human writes — the
    // whole-input check above answers it in constant memory. For any other
    // encoding the discarded tail is genuinely unexamined, which is the price
    // of not decoding an arbitrarily large file to set one bit; `truncated`
    // is already raised in that case, so the badge is never silent.
    let lossy = decoded.had_errors || (cut && encoding == norte_encoding::UTF_8 && !utf8_whole);
    (decoded.text, lossy)
}

/// Masks and bounds ONE third-party string on its way into the model, raising
/// `truncated` if the cap bit.
///
/// Every string a plugin header contributes goes through here. [`Origin`]
/// documents a plugin topic as "third-party text, already masked and bounded
/// by the parser", and that promise is only true if it holds for the header
/// too: the body masking of [`blocks_of`] never touches these fields.
fn header_string(s: &str, truncated: &mut bool) -> String {
    let mut chars = s.chars();
    let cut: String = chars.by_ref().take(MAX_HEADER_CHARS).collect();
    // `by_ref().take(n)` consumed exactly `n`, so this is the (n+1)-th
    // character: its presence is the only evidence that something was cut,
    // and it costs one step instead of a second full count.
    *truncated |= chars.next().is_some();
    mask_if(cut, true)
}

/// Most commands one topic may document. A help page lists the handful of
/// commands its prose is about; a long list belongs in the palette, which is
/// built from the manifest and not from prose.
///
/// The cap is a memory bound as much as a display one: the front matter is
/// TOML and sits outside [`Limits`], so a 64 KiB header of nothing but
/// `commands = ["", "", …]` used to become tens of thousands of entries —
/// measured at 4093 with realistic ids, 21829 with empty ones — every one of
/// them a runnable row on the same dispatch path as the palette.
const MAX_HEADER_COMMANDS: usize = 16;

/// Parses a plugin `help.md`. NEVER fails: it detects the encoding and
/// decodes accordingly, bounds by [`Limits::untrusted`], and masks terminal
/// hazards AT PARSE TIME (masking lives where the data is built, exactly as
/// the palette rows do, instead of being scattered across every frontend).
///
/// `fallback_id` is the plugin's id, supplied by the HOST registry — it is
/// not plugin input, and the caller MUST pass an id it has validated (the
/// catalogue restricts them to `[A-Za-z0-9-]` segments). It is used verbatim,
/// because it is an identity: see the note on ids below.
///
/// ```
/// use norte_help::{Origin, parse_untrusted};
///
/// // A UTF-8 BOM, no header, and a lone invalid byte: still a topic.
/// let parsed = parse_untrusted(b"\xef\xbb\xbfhi \xff", "acme.ftp", None);
/// assert_eq!(parsed.topic.id.as_str(), "acme.ftp");
/// assert_eq!(parsed.topic.title, "acme.ftp");
/// assert!(parsed.lossy && !parsed.truncated);
/// assert!(matches!(parsed.topic.origin, Origin::Plugin { .. }));
/// ```
///
/// # What a plugin may and may not say
///
/// The topic id is HOST-ASSIGNED: whatever the header declares, the id is
/// `fallback_id`. A plugin writing `id = "copying"` would otherwise shadow
/// the built-in topic of that name once the corpus and the registry share one
/// id space, and every built-in `[[copying]]` link would land in plugin prose.
///
/// `tags`, `see_also` and `context` are DROPPED for the same reason, even
/// when the header declares them: a plugin does not get to file itself under
/// the host's index groups (`tags`), link INTO host topics (`see_also`), or
/// claim a UI context so that F1 opens its page instead of the host's
/// (`context`). In the body, `[[topic]]` degrades to literal text — the same
/// jump through the other door — and `{{cmd:id}}` survives only when `id` is
/// this plugin's own dispatch key, `plugin:{fallback_id}:{command}`.
///
/// What a plugin DOES get to say is its `title`, masked and capped at 280
/// characters (the cap the manifest already applies to `description`), and
/// the ids of its own commands, at most sixteen of them.
/// `publisher` is masked and capped the same way.
///
/// Command ids are DISPATCH KEYS: they are never masked, because masking an
/// identity produces a key that resolves to nothing or to something else.
/// They are accepted or REFUSED, never rewritten — which is what lets the
/// model still promise that every string it carries is free of terminal
/// hazards.
#[must_use]
pub fn parse_untrusted(bytes: &[u8], fallback_id: &str, publisher: Option<String>) -> Parsed {
    let limits = Limits::untrusted();
    let mut truncated = bytes.len() > limits.max_bytes;
    let head = cut_at_boundary(bytes, limits.max_bytes);
    let (text, lossy) = decode_source(bytes, head);

    // ANY header failure degrades to "there is no header", never to an error:
    // the whole text becomes the body. A cut that lands mid-header is the
    // common case here, and losing the entire document over it would be a
    // denial of service dressed up as strictness.
    let (fm, body) = match front_matter::split(&text) {
        Ok((fm, body)) => (Some(fm), body),
        Err(_) => (None, text.as_str()),
    };
    let (blocks, cut) = blocks_of(body, limits, Mode::Plugin(fallback_id));
    truncated |= cut;

    // Masked FIRST, then tested for blankness: a title of nothing but bidi
    // overrides is not blank as bytes and is blank as pixels. An empty title
    // renders a nameless page, so the host-assigned id takes over.
    let title = fm
        .as_ref()
        .map_or_else(String::new, |f| header_string(&f.title, &mut truncated));
    let title = if is_blank_id(&title) {
        fallback_id.to_owned()
    } else {
        title
    };
    let commands = fm.map_or_else(Vec::new, |f| {
        let mut out: Vec<String> = f
            .commands
            .into_iter()
            // Same predicate as the body's `{{cmd:…}}`, so the two doors
            // agree: its own commands, non-blank, and never rewritten.
            .filter(|c| is_own_command(c, fallback_id))
            .collect();
        if out.len() > MAX_HEADER_COMMANDS {
            out.truncate(MAX_HEADER_COMMANDS);
            truncated = true;
        }
        out
    });
    // A capped `publisher` does NOT raise `truncated`. The badge is about the
    // PLUGIN'S DOCUMENT — "what you are reading is not all of it" — and
    // `publisher` is manifest metadata the host passed in alongside it.
    // Conflating them badges a byte-complete `help.md` because someone's
    // company name is long, and a badge that cries wolf gets ignored.
    let publisher = publisher.map(|p| header_string(&p, &mut false));

    Parsed {
        topic: Topic {
            // Verbatim, in both copies: this is a LOOKUP KEY, and masking is
            // not injective — `acme<RLO>ftp` and `acme<ZWSP>ftp` would
            // collapse onto the same string, so a registry lookup could match
            // the wrong plugin. It is host-supplied and host-validated, so
            // there is nothing to mask in the first place.
            id: TopicId::new(fallback_id),
            title,
            // Dropped on purpose; see the rustdoc above.
            tags: Vec::new(),
            see_also: Vec::new(),
            commands,
            context: Vec::new(),
            blocks,
            origin: Origin::Plugin {
                id: fallback_id.to_owned(),
                publisher,
                truncated,
                lossy,
            },
        },
        truncated,
        lossy,
    }
}

/// Turns the body into blocks. `mask` masks terminal hazards in every text
/// produced (hostile mode, task 6).
///
/// Line-oriented and single-pass: the grammar has no nesting, so a block is
/// decided by the prefix of its first line and nothing needs to be
/// backtracked. Returns the blocks and whether a [`Limits`] ceiling was hit.
fn blocks_of(body: &str, limits: Limits, mode: Mode<'_>) -> (Vec<Block>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    let mut lines = body.lines().peekable();
    let mut para: Vec<String> = Vec::new();
    let mut cells_left = limits.max_cells;

    while let Some(raw) = lines.next() {
        if out.len() >= limits.max_blocks {
            // Only content LOST is truncation. A body that ends in blank
            // lines exactly at the cap lost nothing, and a badge that cries
            // "truncated" over trailing whitespace teaches the reader to
            // ignore it.
            truncated |= !raw.trim().is_empty() || lines.any(|l| !l.trim().is_empty());
            break;
        }
        let (line, cut) = clamp_line(raw, limits);
        truncated |= cut;

        if line.trim().is_empty() {
            flush(&mut para, &mut out, mode);
        } else if let Some(rest) = line.strip_prefix("```") {
            flush(&mut para, &mut out, mode);
            let lang =
                (!rest.trim().is_empty()).then(|| mask_if(rest.trim().to_owned(), mode.masks()));
            let mut text = String::new();
            // An unterminated fence deliberately eats the rest of the input:
            // `by_ref` leaves the outer `while let` on an exhausted iterator,
            // so this terminates with EXACTLY one `Code` block. The closing
            // fence, when it exists, is consumed and not re-examined.
            for l in lines.by_ref() {
                if l.starts_with("```") {
                    break;
                }
                let (l, cut) = clamp_line(l, limits);
                truncated |= cut;
                // Mask each line, never the assembled text: `\n` is a control
                // character and therefore a hazard, so masking afterwards
                // would turn the separators the PARSER wrote into `U+FFFD`
                // and collapse the code block into one unreadable line.
                // Masking applies to the plugin's bytes, not to our structure.
                text.push_str(&mask_if(l.to_owned(), mode.masks()));
                text.push('\n');
            }
            out.push(Block::Code { lang, text });
        } else if let Some(rest) = line.strip_prefix('#') {
            flush(&mut para, &mut out, mode);
            // One `#` is already stripped, so the hashes left here are the
            // ones ABOVE level 1. Clamped to 3 — the model documents 1..=3 —
            // by counting, never by arithmetic that could overflow `u8`:
            // `#######` is level 3, not level 7 and not a panic.
            let level = match rest.chars().take_while(|c| *c == '#').count() {
                0 => 1,
                1 => 2,
                _ => 3,
            };
            let text = rest.trim_start_matches('#').trim();
            out.push(Block::Heading {
                level,
                text: mask_if(text.to_owned(), mode.masks()),
            });
        } else if let Some(rest) = line.strip_prefix("> ") {
            flush(&mut para, &mut out, mode);
            let (kind, body) = callout_kind(rest);
            out.push(Block::Callout {
                kind,
                spans: spans_masked(body, mode),
            });
        } else if let Some(rest) = line.strip_prefix("- ") {
            flush(&mut para, &mut out, mode);
            let mut items = vec![spans_masked(rest, mode)];
            // `to_owned` on purpose: `peek` borrows `lines` for the WHOLE body
            // of the `while let`, so the inner `next()` would not compile with
            // a live reference into the buffer.
            while let Some(next) = lines.peek().map(|l| (*l).to_owned()) {
                // Clamp BEFORE stripping the marker, exactly as the first item
                // was clamped before `strip_prefix`: measuring after the strip
                // would hand items 2..n two bytes more budget than item 1.
                let (next, cut) = clamp_line(&next, limits);
                let Some(item) = next.strip_prefix("- ") else {
                    break;
                };
                truncated |= cut;
                items.push(spans_masked(item, mode));
                lines.next();
            }
            out.push(Block::Bullets(items));
        } else if line.starts_with('|') {
            flush(&mut para, &mut out, mode);
            let mut header = cells(line, mode);
            // Every cell of this body, header cells included, comes out of one
            // budget. Rows are padded to the header width, so it is the
            // PRODUCT width × height that has to be bounded, and nothing else
            // here sees that product: see [`Limits::max_cells`].
            if header.len() > cells_left {
                header.truncate(cells_left);
                truncated = true;
            }
            cells_left -= header.len();
            let width = header.len();
            // The `|---|---|` line is SYNTAX, not data. It is skipped only
            // when it really is one: swallowing the next `|` line
            // unconditionally would silently eat the first ROW of a table
            // whose author forgot the separator, and a lost row reads as a
            // fact that was never documented.
            if lines.peek().is_some_and(|l| is_separator_row(l)) {
                lines.next();
            }
            let mut rows = Vec::new();
            while let Some(row) = lines
                .peek()
                .filter(|l| l.starts_with('|'))
                .map(|l| (*l).to_owned())
            {
                lines.next();
                if width == 0 || cells_left < width {
                    // Out of budget: the remaining rows of THIS table are
                    // drained rather than left to the outer loop, which would
                    // otherwise open a fresh empty table per line.
                    truncated = true;
                    continue;
                }
                cells_left -= width;
                let (row, cut) = clamp_line(&row, limits);
                truncated |= cut;
                rows.push(normalise_row(cells(row, mode), width));
            }
            out.push(Block::Table { header, rows });
        } else {
            para.push(line.to_owned());
        }
    }
    flush(&mut para, &mut out, mode);
    // One iteration can push the pending paragraph AND its own block, so the
    // cap may be overshot by one. Clamping here makes `blocks.len() <=
    // max_blocks` hold unconditionally. It bounds the block COUNT only —
    // what a block may hold is bounded by `max_line_bytes` and `max_cells`.
    if out.len() > limits.max_blocks {
        out.truncate(limits.max_blocks);
        truncated = true;
    }
    (out, truncated)
}

/// Emits the paragraph built up so far, if there is one, and empties the
/// buffer. Lines are joined with a single space: a hard-wrapped corpus must
/// reflow to the reader's width, not show ours.
fn flush(para: &mut Vec<String>, out: &mut Vec<Block>, mode: Mode<'_>) {
    if para.is_empty() {
        return;
    }
    let mut spans = Vec::new();
    for (i, line) in para.iter().enumerate() {
        if i > 0 {
            // The joiner is OUR structure, not the author's bytes, so it is
            // written after the line was parsed on its own. That ordering is
            // the point: see [`merge_text`].
            spans.push(Span::Text(" ".to_owned()));
        }
        spans.extend(spans_masked(line, mode));
    }
    out.push(Block::Paragraph(merge_text(spans)));
    para.clear();
}

/// Cuts a line to `max_line_bytes`, always on a `char` boundary. Returns the
/// line and whether anything was dropped.
///
/// The backward walk cannot underflow: index 0 is a boundary of every `&str`,
/// so it stops there at the very latest — even for a line whose FIRST
/// character is multi-byte and already longer than the limit.
fn clamp_line(raw: &str, limits: Limits) -> (&str, bool) {
    if raw.len() <= limits.max_line_bytes {
        return (raw, false);
    }
    let mut cut = limits.max_line_bytes;
    while !raw.is_char_boundary(cut) {
        cut -= 1;
    }
    (&raw[..cut], true)
}

/// The cells of a table line, without their pipes and trimmed.
fn cells(line: &str, mode: Mode<'_>) -> Vec<String> {
    line.trim_matches('|')
        .split('|')
        .map(|c| mask_if(c.trim().to_owned(), mode.masks()))
        .collect()
}

/// Is this the `|---|:--:|` separator? Every cell non-empty and made only of
/// `-` and `:`. A data row never matches, which is what lets a table with no
/// separator keep its first row.
fn is_separator_row(line: &str) -> bool {
    line.starts_with('|')
        && line.trim_matches('|').split('|').all(|c| {
            let c = c.trim();
            !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':')
        })
}

/// A row with EXACTLY `width` cells: the missing ones padded empty, the extra
/// ones dropped.
///
/// [`Block::Table`] documents this so a renderer can index by column without
/// checking the length. It is enforced here, at the only place that builds a
/// row, because these cells come from a plain `split` over a plugin
/// `help.md`: a ragged row is one keystroke away for a hostile author, and it
/// would land as a panic while drawing.
///
/// The dropped cells are deliberately NOT reported through
/// [`Parsed::truncated`]: a cell past the header width has no column to be
/// drawn in, and flagging it would badge every topic with a typo'd row as
/// truncated. The PADDING side is what has to be paid for, and it is —
/// against [`Limits::max_cells`], by the caller.
fn normalise_row(mut row: Vec<String>, width: usize) -> Vec<String> {
    row.truncate(width);
    row.resize_with(width, String::new);
    row
}

/// Callout kind from its leading emoji; without one, a note.
fn callout_kind(rest: &str) -> (Callout, &str) {
    for (marker, kind) in [("\u{26A0}", Callout::Warn), ("\u{1F4A1}", Callout::Tip)] {
        if let Some(body) = rest.strip_prefix(marker) {
            return (kind, body.trim_start());
        }
    }
    (Callout::Note, rest)
}

/// [`spans`], masking the line FIRST when the content is third-party.
///
/// Before and not after because `TopicId` documents that it never normalises
/// what it is given, so a `[[topic]]` id must arrive already masked — masking
/// the `Span` afterwards would mean tearing the id apart and rebuilding it.
///
/// The ordering is NOT free, and it is worth being precise about the price.
/// No hazard is one of the mark DELIMITERS (`{`, `[`, `*`, `` ` ``), so
/// masking first cannot move where a mark opens or closes. It can still
/// change a decision made about a mark's CONTENT: seven members of the hazard
/// set are whitespace (`\t`, `\r`, U+000B, U+000C, U+0085, U+2028, U+2029)
/// and `U+FFFD` is not, so the "is this id blank?" test would flip and
/// `{{cmd:\t}}` would fabricate a command reference. That is why the test is
/// [`is_blank_id`] and not `trim().is_empty()`.
fn spans_masked(line: &str, mode: Mode<'_>) -> Vec<Span> {
    if mode.masks() {
        spans(&norte_encoding::mask_terminal_hazards(line), mode)
    } else {
        spans(line, mode)
    }
}

/// Concatenates adjacent [`Span::Text`].
///
/// A paragraph is parsed LINE BY LINE and welded back together here. Parsing
/// the joined text instead would let a mark straddle a line break: a body of
/// `{{cmd:` then `fs.copy}}` holds no complete mark on either line, yet the
/// joined string does — a reference nobody wrote, and one that a line-by-line
/// host-side validator could never see coming. This is the mirror of the code
/// block, where masking runs per line and the joining comes after.
///
/// Merging back matters for the reader: a hard-wrapped corpus must reach the
/// renderer as ONE text span, so it reflows to the reader's width.
fn merge_text(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        if let (Some(Span::Text(prev)), Span::Text(next)) = (out.last_mut(), &span) {
            prev.push_str(next);
            continue;
        }
        out.push(span);
    }
    out
}

/// Masks terminal hazards when the content is third-party.
///
/// `norte_encoding::mask_terminal_hazards` is the SINGLE source of the hazard
/// set (controls, bidi overrides, invisibles Cf/Zl/Zp, tag chars; ZWJ is
/// deliberately allowed for composed emoji) — the same one `norte-frontend`
/// uses for names. Takes the `String` by value so the trusted path pays
/// nothing: with `mask == false` it hands the very same buffer back.
fn mask_if(s: String, mask: bool) -> String {
    if mask {
        norte_encoding::mask_terminal_hazards(&s)
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, Callout, Span, TopicId};

    /// [`super::spans`] in TRUSTED mode: most of these cases are about the
    /// grammar, which is the same in both modes. The hostile-mode
    /// restrictions have their own tests, here and in `tests/hostile.rs`.
    fn spans(line: &str) -> Vec<Span> {
        super::spans(line, Mode::Trusted)
    }

    /// [`super::spans_counted`] in trusted mode.
    fn spans_counted(line: &str) -> (Vec<Span>, usize) {
        super::spans_counted(line, Mode::Trusted)
    }

    #[test]
    fn plain_text_is_a_single_span() {
        assert_eq!(
            spans("plain text"),
            vec![Span::Text("plain text".to_owned())]
        );
    }

    #[test]
    fn recognises_the_four_inline_marks() {
        assert_eq!(
            spans("a **b** c *d* e `f` g"),
            vec![
                Span::Text("a ".to_owned()),
                Span::Strong("b".to_owned()),
                Span::Text(" c ".to_owned()),
                Span::Emph("d".to_owned()),
                Span::Text(" e ".to_owned()),
                Span::Code("f".to_owned()),
                Span::Text(" g".to_owned()),
            ]
        );
    }

    #[test]
    fn recognises_the_live_marks_without_resolving_them() {
        assert_eq!(
            spans("press {{cmd:fs.copy}} then see [[selection]]"),
            vec![
                Span::Text("press ".to_owned()),
                Span::CommandRef("fs.copy".to_owned()),
                Span::Text(" then see ".to_owned()),
                Span::TopicLink(TopicId::new("selection")),
            ]
        );
    }

    #[test]
    fn an_unclosed_mark_stays_literal_text() {
        assert_eq!(
            spans("{{cmd:fs.copy oops"),
            vec![Span::Text("{{cmd:fs.copy oops".to_owned())],
            "a broken mark must never invent a phantom command"
        );
    }

    #[test]
    fn inline_code_does_not_interpret_marks() {
        assert_eq!(
            spans("`{{cmd:x}}`"),
            vec![Span::Code("{{cmd:x}}".to_owned())]
        );
    }

    // --- Hardening: this parser sits on the path of third-party plugin help
    // (task 6), so hostile input must DEGRADE into literal text — never
    // panic, never cut a multi-byte character in half, and above all never
    // fabricate a `CommandRef`, which the corpus checks of task 8 read as a
    // claim that the command exists.

    #[test]
    fn multibyte_content_survives_byte_for_byte_inside_every_mark() {
        // Every offset in the parser is a BYTE offset: a mark whose content
        // abuts the closer with a multi-byte character is where an
        // off-by-one panics or truncates.
        assert_eq!(
            spans("{{cmd:ñ.copy}}"),
            vec![Span::CommandRef("ñ.copy".to_owned())]
        );
        assert_eq!(
            spans("[[ñ]]"),
            vec![Span::TopicLink(TopicId::new("ñ"))],
            "the id keeps its bytes: the model does not normalise"
        );
        assert_eq!(spans("**ñ**"), vec![Span::Strong("ñ".to_owned())]);
        assert_eq!(spans("`ñ`"), vec![Span::Code("ñ".to_owned())]);
    }

    #[test]
    fn a_line_of_emoji_and_cjk_never_panics() {
        let line = "👨‍👩‍👧 日本語 — ñ 🇪🇸";
        assert_eq!(
            spans(line),
            vec![Span::Text(line.to_owned())],
            "nothing to mark up: it comes back whole, byte for byte"
        );
    }

    /// Asserts that `line` produces no live mark in EITHER mode, and that
    /// whatever it does produce still carries every byte of the (possibly
    /// masked) line.
    ///
    /// Both modes, because masking runs before the span parse and can
    /// therefore change what the parser decides about a mark's content — see
    /// [`is_blank_id`]. A test that only ever ran trusted let exactly that
    /// bug through.
    fn no_live_mark_either_way(line: &str) {
        for mode in [Mode::Trusted, Mode::Plugin("acme")] {
            let out = spans_masked(line, mode);
            assert!(
                !out.iter()
                    .any(|s| matches!(s, Span::CommandRef(_) | Span::TopicLink(_))),
                "{mode:?}: {line:?} fabricated a live mark: {out:?}"
            );
            let expected = if mode.masks() {
                norte_encoding::mask_terminal_hazards(line)
            } else {
                line.to_owned()
            };
            assert_eq!(
                payloads(&out).concat(),
                expected,
                "{mode:?}: {line:?} lost bytes"
            );
        }
    }

    #[test]
    fn an_empty_id_stays_literal_text() {
        assert_eq!(
            spans("{{cmd:}}"),
            vec![Span::Text("{{cmd:}}".to_owned())],
            "an empty CommandRef would claim a command with no name exists"
        );
        assert_eq!(spans("[[]]"), vec![Span::Text("[[]]".to_owned())]);
        no_live_mark_either_way("{{cmd:}}");
        no_live_mark_either_way("[[]]");
    }

    #[test]
    fn a_whitespace_only_id_stays_literal_text() {
        assert_eq!(
            spans("{{cmd:   }}"),
            vec![Span::Text("{{cmd:   }}".to_owned())],
            "trimming to nothing is still nothing"
        );
        assert_eq!(spans("[[   ]]"), vec![Span::Text("[[   ]]".to_owned())]);

        // The whitespace that is ALSO a terminal hazard. Under masking each
        // of these becomes `U+FFFD`, which is not whitespace: with a plain
        // `trim().is_empty()` the id would stop looking blank and the parser
        // would invent a command out of a tab.
        for ws in [
            "\t", "\r", "\u{000B}", "\u{000C}", "\u{0085}", "\u{2028}", "\u{2029}",
        ] {
            no_live_mark_either_way(&format!("{{{{cmd:{ws}}}}}"));
            no_live_mark_either_way(&format!("[[{ws}]]"));
            // Mixed with ordinary spaces, and with the replacement character
            // written out literally: still blank, still not a mark.
            no_live_mark_either_way(&format!("{{{{cmd: {ws} }}}}"));
        }
        no_live_mark_either_way("{{cmd:\u{FFFD}}}");
        no_live_mark_either_way("[[\u{FFFD}\u{FFFD}]]");
    }

    #[test]
    fn unmatched_emphasis_delimiters_stay_literal() {
        assert_eq!(spans("a ** b"), vec![Span::Text("a ** b".to_owned())]);
        assert_eq!(spans("a * b"), vec![Span::Text("a * b".to_owned())]);
        assert_eq!(spans("**"), vec![Span::Text("**".to_owned())]);
        assert_eq!(spans("*"), vec![Span::Text("*".to_owned())]);
        assert_eq!(
            spans("a `b"),
            vec![Span::Text("a `b".to_owned())],
            "an unclosed backtick is a backtick, not a code span to the end"
        );
    }

    #[test]
    fn a_mark_that_looks_nested_yields_exactly_one_span() {
        // The inner `[[b]]` is content of the command reference, not a
        // second mark: emitting a topic link here would invent a jump the
        // author never wrote.
        assert_eq!(
            spans("{{cmd:a[[b]]c}}"),
            vec![Span::CommandRef("a[[b]]c".to_owned())]
        );
    }

    #[test]
    fn the_reverse_nesting_also_yields_exactly_one_span() {
        // The other direction, where a topic id could swallow a command
        // mark: the `{{cmd:b}}` inside is content of the link, and the line
        // must NOT produce a command reference nobody wrote.
        let out = spans("[[a{{cmd:b}}c]]");
        assert_eq!(
            out,
            vec![Span::TopicLink(TopicId::new("a{{cmd:b}}c"))],
            "the id keeps the inner mark as literal bytes"
        );
        assert!(!out.iter().any(|s| matches!(s, Span::CommandRef(_))));
    }

    #[test]
    fn a_padded_id_is_trimmed_and_only_at_the_edges() {
        // This `trim` is the ONE place the parser normalises an id, and it
        // deliberately contradicts `TopicId`'s documented contract that ids
        // are never normalised. Pinned on purpose, on BOTH sides: a change
        // to `trim_start()` (or to no trim at all) must fail here rather
        // than silently split one topic into two ids that render the same.
        assert_eq!(
            spans("{{cmd: fs.copy }}"),
            vec![Span::CommandRef("fs.copy".to_owned())]
        );
        assert_eq!(
            spans("[[ selection ]]"),
            vec![Span::TopicLink(TopicId::new("selection"))]
        );
        // Only at the edges: inner whitespace is part of the id.
        assert_eq!(
            spans("{{cmd: fs copy }}"),
            vec![Span::CommandRef("fs copy".to_owned())]
        );
        assert_eq!(
            spans("[[ a b ]]"),
            vec![Span::TopicLink(TopicId::new("a b"))]
        );
        // Delimiters do NOT trim: `**  x  **` is emphasis over the spaces.
        assert_eq!(spans("** x **"), vec![Span::Strong(" x ".to_owned())]);
        assert_eq!(spans("` x `"), vec![Span::Code(" x ".to_owned())]);
    }

    /// The bytes each span carries, in order: no syntax, just what a
    /// renderer would put on screen (or, for the live marks, the id it would
    /// resolve).
    fn payloads(spans: &[Span]) -> Vec<&str> {
        spans
            .iter()
            .map(|s| match s {
                Span::Text(t) | Span::Strong(t) | Span::Emph(t) | Span::Code(t) => t.as_str(),
                Span::CommandRef(id) => id.as_str(),
                Span::TopicLink(id) => id.as_str(),
            })
            .collect()
    }

    /// The lossy decode of every hostile name in the canonical corpus. The
    /// names are raw bytes and the parser only ever sees `&str`, so the
    /// lossy decode is exactly what the hostile path (task 6) feeds it.
    fn hostile_lines() -> Vec<(String, String)> {
        norte_testkit::corpus::hostile_names()
            .into_iter()
            .map(|n| (n.id, String::from_utf8_lossy(&n.bytes).into_owned()))
            .collect()
    }

    /// Does this text carry any of the syntax the parser reacts to?
    fn has_mark_syntax(s: &str) -> bool {
        s.contains(['{', '[', '*', '`'])
    }

    #[test]
    fn a_mark_free_hostile_line_round_trips_byte_for_byte() {
        // Not panicking is the cheap half of the claim; the other half is
        // that nothing is DROPPED. Bidi overrides, zero-width joiners, tag
        // characters and lone invalid bytes must come back out whole —
        // masking is NOT this parser's job (task 6 masks BEFORE span
        // parsing), so any byte that goes missing here went missing for good.
        let names = hostile_lines();
        assert!(names.len() >= 32, "the canonical corpus must not shrink");

        let mut joined = String::new();
        for (id, text) in &names {
            if has_mark_syntax(text) {
                continue;
            }
            assert_eq!(
                payloads(&spans(text)).concat(),
                *text,
                "[{id}] lost or altered bytes"
            );
            joined.push_str(text);
            joined.push(' ');
        }
        assert!(!joined.is_empty(), "the corpus must not be empty");
        assert_eq!(
            payloads(&spans(&joined)).concat(),
            joined,
            "the whole line comes back byte for byte"
        );
    }

    #[test]
    fn hostile_names_spliced_into_marks_neither_fabricate_nor_mangle() {
        // Bare hostile names carry no mark syntax, so they can only prove
        // the parser survives them. Splicing each one INTO a mark is what
        // gives the no-fabrication claim something to bite on.
        for (id, text) in hostile_lines() {
            for (open, close) in [
                (CMD_OPEN, CMD_CLOSE),
                (LINK_OPEN, LINK_CLOSE),
                ("`", "`"),
                ("**", "**"),
            ] {
                let line = format!("{open}{text}{close}");
                let out = spans(&line);
                for p in payloads(&out) {
                    assert!(
                        line.contains(p),
                        "[{id}] in {open}…{close}: payload {p:?} is not in the input — \
                         the parser invented or mangled bytes"
                    );
                }
                // The exact shape, whenever the splice cannot end the mark
                // early: one span, payload byte-identical to the name.
                let live_mark = open == CMD_OPEN || open == LINK_OPEN;
                let trimmed = if live_mark {
                    text.trim()
                } else {
                    text.as_str()
                };
                if !text.contains(close) && !text.trim().is_empty() {
                    if live_mark && is_blank_id(trimmed) {
                        // A name that lossy-decoded to nothing but `U+FFFD`
                        // (`latin1_e_acute` is a lone `0xE9`) is a BLANK id:
                        // an id made only of replacement characters names no
                        // command and no topic, so the mark stays literal
                        // text. Not fabricating is the whole point of this
                        // test — see `is_blank_id`.
                        assert_eq!(
                            payloads(&out),
                            vec![line.as_str()],
                            "[{id}] in {open}…{close}: a blank id must stay literal"
                        );
                    } else {
                        assert_eq!(
                            payloads(&out),
                            vec![trimmed],
                            "[{id}] in {open}…{close}: expected one span with the name verbatim"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_corpus_bidi_command_mark_keeps_its_payload_byte_for_byte() {
        // `cmd_mark_bidi_payload`: a whole `{{cmd:…}}` whose id carries a
        // RIGHT-TO-LEFT OVERRIDE. The parser must hand the id over with its
        // bytes intact and UNRESOLVED — it neither masks (task 6 does, on
        // the way in) nor validates (task 8's corpus check does, byte-exact,
        // which is precisely why the spoofed id will match no real command).
        let (_, text) = hostile_lines()
            .into_iter()
            .find(|(id, _)| id == "cmd_mark_bidi_payload")
            .expect("the fixture lives in the canonical corpus");

        let out = spans(&text);
        assert_eq!(out, vec![Span::CommandRef("\u{202e}fs.copy".to_owned())]);
        let Some(Span::CommandRef(cmd)) = out.first() else {
            panic!("a command reference");
        };
        assert_eq!(
            cmd.as_bytes(),
            "\u{202e}fs.copy".as_bytes(),
            "the override is carried, not stripped and not normalised"
        );
        assert_ne!(cmd, "fs.copy", "it must never pass for the real command");
    }

    #[test]
    fn a_long_line_with_unterminated_marks_scans_each_tail_once_per_mark() {
        // Two shapes of the same trap: one very long unterminated mark, and
        // many openers that each tempt the parser into rescanning the whole
        // tail (the quadratic shape). The claim is deterministic and about
        // the algorithm, not the machine: a tail scan that finds no closer
        // may happen at most ONCE per mark kind, because `Closers` remembers
        // it. Delete the memo and this count becomes the number of openers.
        let one = format!("{CMD_OPEN}{}", "a".repeat(100_000));
        let (out, scans) = spans_counted(&one);
        assert_eq!(out.len(), 1, "one literal text span");
        assert!(matches!(out.first(), Some(Span::Text(_))));
        assert_eq!(scans, 1, "a single fruitless scan of the tail");

        let many = CMD_OPEN.repeat(100_000 / CMD_OPEN.len());
        let (out, scans) = spans_counted(&many);
        assert!(
            !out.iter().any(|s| matches!(s, Span::CommandRef(_))),
            "no closer anywhere: nothing may be fabricated"
        );
        assert_eq!(scans, 1, "~16k openers, ONE scan");

        let links = LINK_OPEN.repeat(100_000 / LINK_OPEN.len());
        let (out, scans) = spans_counted(&links);
        assert!(!out.iter().any(|s| matches!(s, Span::TopicLink(_))));
        assert_eq!(scans, 1);

        // Both kinds in one line: one scan each, and they are independent.
        let (out, scans) = spans_counted(&format!("{many}{links}"));
        assert!(
            !out.iter()
                .any(|s| matches!(s, Span::CommandRef(_) | Span::TopicLink(_))),
            "still nothing fabricated"
        );
        assert_eq!(scans, 2, "one per mark kind, whatever the length");
    }

    // --- The block parser.

    const DOC: &str = "+++\n\
id = \"t\"\n\
title = \"T\"\n\
+++\n\
# Heading\n\
\n\
A paragraph with {{cmd:fs.copy}}.\n\
\n\
- one\n\
- two\n\
\n\
```toml\n\
key = 1\n\
```\n\
\n\
> \u{26A0} careful\n\
\n\
| a | b |\n\
|---|---|\n\
| 1 | 2 |\n";

    #[test]
    fn parses_the_six_block_kinds() {
        let parsed = parse_trusted(DOC).expect("valid document");
        let t = parsed.topic;
        assert_eq!(t.id.as_str(), "t");
        assert_eq!(t.blocks.len(), 6, "blocks: {:?}", t.blocks);
        assert_eq!(
            t.blocks[0],
            Block::Heading {
                level: 1,
                text: "Heading".to_owned()
            }
        );
        assert!(matches!(t.blocks[1], Block::Paragraph(_)));
        assert_eq!(
            t.blocks[2],
            Block::Bullets(vec![
                vec![Span::Text("one".to_owned())],
                vec![Span::Text("two".to_owned())],
            ])
        );
        assert_eq!(
            t.blocks[3],
            Block::Code {
                lang: Some("toml".to_owned()),
                text: "key = 1\n".to_owned()
            }
        );
        assert_eq!(
            t.blocks[4],
            Block::Callout {
                kind: Callout::Warn,
                spans: vec![Span::Text("careful".to_owned())]
            }
        );
        assert_eq!(
            t.blocks[5],
            Block::Table {
                header: vec!["a".to_owned(), "b".to_owned()],
                rows: vec![vec!["1".to_owned(), "2".to_owned()]],
            }
        );
    }

    #[test]
    fn trusted_mode_fails_on_a_broken_header() {
        assert!(parse_trusted("no fences here").is_err());
    }

    /// The blocks of a body under the built-in limits, trusted mode.
    ///
    /// The `!truncated` assertion is part of the claim: none of these bodies
    /// comes near a ceiling. Cells dropped by `normalise_row` are the one
    /// loss `Parsed::truncated` deliberately does not report — its rustdoc
    /// says so — which is why the ragged-table test can use this helper.
    fn blocks(body: &str) -> Vec<Block> {
        let (out, truncated) = blocks_of(body, Limits::built_in(), Mode::Trusted);
        assert!(!truncated, "this body fits: {out:?}");
        out
    }

    /// Every piece of text the blocks carry, in order.
    ///
    /// The `match` is exhaustive on purpose: a new [`Block`] variant that
    /// carries text must be added here, or the masking assertions would
    /// quietly stop covering it.
    fn texts(blocks: &[Block]) -> Vec<String> {
        fn owned(spans: &[Span]) -> Vec<String> {
            payloads(spans).into_iter().map(str::to_owned).collect()
        }
        let mut out = Vec::new();
        for b in blocks {
            match b {
                Block::Heading { text, .. } => out.push(text.clone()),
                Block::Paragraph(spans) | Block::Callout { spans, .. } => {
                    out.extend(owned(spans));
                }
                Block::Bullets(items) => out.extend(items.iter().flat_map(|i| owned(i))),
                Block::Code { lang, text } => {
                    out.extend(lang.clone());
                    out.push(text.clone());
                }
                Block::Table { header, rows } => {
                    out.extend(header.iter().cloned());
                    out.extend(rows.iter().flat_map(|r| r.iter().cloned()));
                }
            }
        }
        out
    }

    /// Cells held by a list of blocks, header cells included.
    fn cell_count(out: &[Block]) -> usize {
        out.iter()
            .map(|b| match b {
                Block::Table { header, rows } => {
                    header.len() + rows.iter().map(Vec::len).sum::<usize>()
                }
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn a_wide_header_over_many_rows_cannot_amplify_memory() {
        // The row normalisation pads every row to the header width, so a
        // table costs width × height cells while the source pays for width +
        // height BYTES. Measured before `max_cells` existed: 64 KiB of this
        // shape produced 32_505_856 cells and 749 MiB of RSS. `max_blocks`
        // does not see it (one table is one block) and `max_line_bytes` bounds
        // the width, never the product.
        let limits = Limits::untrusted();
        let mut body = "|a".repeat(1024);
        body.push_str("|\n");
        body.push_str(&"|\n".repeat(40_000));
        body.truncate(limits.max_bytes);

        let (out, truncated) = blocks_of(&body, limits, Mode::Plugin("acme"));
        assert!(truncated, "a table cut short must say so");
        assert!(
            cell_count(&out) <= limits.max_cells,
            "cells={} over the ceiling of {}",
            cell_count(&out),
            limits.max_cells
        );
        // The budget is spent across the WHOLE body, not per table: many
        // tables cannot each claim a full ceiling.
        let many = "| a | b |\n| 1 | 2 |\n\n".repeat(4000);
        let tight = Limits {
            max_cells: 100,
            ..Limits::built_in()
        };
        let (out, truncated) = blocks_of(&many, tight, Mode::Trusted);
        assert!(truncated);
        assert!(cell_count(&out) <= 100, "cells={}", cell_count(&out));
        // Still bounded when the budget runs out mid-table, and the leftover
        // rows do not each open a table of their own.
        let (out, _) = blocks_of(
            "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n",
            tight,
            Mode::Trusted,
        );
        assert_eq!(out.len(), 1, "{out:?}");
    }

    #[test]
    fn masking_reaches_every_text_a_block_carries() {
        // One smoke test over the hostile path: every block kind carries a
        // RIGHT-TO-LEFT OVERRIDE, and none may reach a renderer. Task 6 owns
        // the full hostile suite; this exists so the masked path is exercised
        // at all, which is what would have caught the `is_blank_id` bug.
        let body = "# h\u{202E}1\n\n\
p\u{202E}ara\n\n\
- it\u{202E}em\n\n\
> \u{26A0} car\u{202E}eful\n\n\
| a\u{202E}1 | b |\n|---|---|\n| c\u{202E}1 | d |\n\n\
```to\u{202E}ml\nco\u{202E}de\n```\n";
        let (out, _) = blocks_of(body, Limits::untrusted(), Mode::Plugin("acme"));
        assert_eq!(out.len(), 6, "{out:?}");
        // The REAL strings, never `{:?}`: Debug escapes `\u{202e}` into ASCII,
        // so an assertion over formatted output would pass just as happily
        // with masking switched off.
        let masked = texts(&out);
        assert_eq!(masked.len(), 10, "every text of every block: {masked:?}");
        for t in &masked {
            assert!(
                !t.contains('\u{202E}'),
                "a bidi override survived masking: {t:?}"
            );
        }
        assert_eq!(
            masked.iter().filter(|t| t.contains('\u{FFFD}')).count(),
            8,
            "one per override, and the two clean cells untouched: {masked:?}"
        );
        // The parser's own structure is NOT content: the `\n` it writes
        // between the lines of a code block is a control character, and
        // masking the assembled text would have eaten it.
        assert_eq!(
            masked.last().map(String::as_str),
            Some("co\u{FFFD}de\n"),
            "the code block keeps its line breaks: {masked:?}"
        );
        // The same body unmasked keeps its bytes: masking is the ONLY
        // difference, not a change of shape.
        let (plain, _) = blocks_of(body, Limits::untrusted(), Mode::Trusted);
        assert_eq!(plain.len(), out.len());
        let plain = texts(&plain);
        assert_eq!(
            plain.iter().filter(|t| t.contains('\u{202E}')).count(),
            8,
            "the trusted path carries them raw: {plain:?}"
        );
        for (a, b) in plain.iter().zip(&masked) {
            assert_eq!(
                a.replace('\u{202E}', "\u{FFFD}"),
                *b,
                "masking replaced exactly the hazard, one char for one char"
            );
        }
    }

    #[test]
    fn a_ragged_table_row_is_normalised_to_the_header_width() {
        // `Block::Table` PROMISES every row has `header.len()` cells so a
        // renderer may index by column with no length check. Both directions
        // of raggedness are attacker-reachable through a plugin `help.md`:
        // the short row would panic on `row[2]`, the long one would paint a
        // column the header never announced.
        let out = blocks("| a | b | c |\n|---|---|---|\n| 1 |\n| 1 | 2 | 3 | 4 | 5 |\n");
        assert_eq!(
            out,
            vec![Block::Table {
                header: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
                rows: vec![
                    vec!["1".to_owned(), String::new(), String::new()],
                    vec!["1".to_owned(), "2".to_owned(), "3".to_owned()],
                ],
            }]
        );
        let Some(Block::Table { header, rows }) = out.first() else {
            panic!("a table");
        };
        assert!(
            rows.iter().all(|r| r.len() == header.len()),
            "the contract holds for EVERY row, not just the ones we spelled out"
        );
    }

    #[test]
    fn an_unterminated_code_fence_consumes_to_the_end() {
        // The fence opens and never closes. It must swallow the rest of the
        // input as ONE code block: no panic, no infinite loop, and above all
        // no re-entry into the outer loop with the same line.
        let out = blocks("```sh\nrm -rf /\n# not a heading\n- not a bullet\n");
        assert_eq!(
            out,
            vec![Block::Code {
                lang: Some("sh".to_owned()),
                text: "rm -rf /\n# not a heading\n- not a bullet\n".to_owned(),
            }],
            "everything after the opener is content, not markup"
        );
        // The degenerate shapes: a bare fence, and a fence as the last line.
        assert_eq!(
            blocks("```\n"),
            vec![Block::Code {
                lang: None,
                text: String::new()
            }]
        );
        assert_eq!(
            blocks("text\n```"),
            vec![
                Block::Paragraph(vec![Span::Text("text".to_owned())]),
                Block::Code {
                    lang: None,
                    text: String::new()
                },
            ]
        );
    }

    #[test]
    fn seven_hashes_clamp_to_level_three() {
        // The model documents levels 1..=3. The count is clamped, never
        // arithmetic on a `u8` that could overflow, and `#` alone must not
        // underflow the "hashes above level 1" count either.
        assert_eq!(
            blocks("####### seven hashes\n"),
            vec![Block::Heading {
                level: 3,
                text: "seven hashes".to_owned()
            }]
        );
        for (src, level) in [("# a\n", 1), ("## a\n", 2), ("### a\n", 3), ("#### a\n", 3)] {
            assert_eq!(
                blocks(src),
                vec![Block::Heading {
                    level,
                    text: "a".to_owned()
                }],
                "{src:?}"
            );
        }
        assert_eq!(
            blocks("#\n"),
            vec![Block::Heading {
                level: 1,
                text: String::new()
            }],
            "a lone hash is an empty heading, not a panic"
        );
        assert_eq!(
            blocks("#no space\n"),
            vec![Block::Heading {
                level: 1,
                text: "no space".to_owned()
            }],
            "the space is not part of the grammar"
        );
    }

    #[test]
    fn an_overlong_line_is_cut_on_a_char_boundary() {
        let limits = Limits {
            max_line_bytes: 5,
            ..Limits::built_in()
        };
        // `ñ` is two bytes straddling the cut at 5: the walk back lands on 4.
        let (line, cut) = clamp_line("aaaañ", limits);
        assert_eq!((line, cut), ("aaaa", true), "never half a character");

        // The worst case for the backward walk: the FIRST character is
        // multi-byte and already longer than the limit, so the only boundary
        // at or below the cut is index 0. It stops there — the loop cannot
        // step below 0 and panic on underflow.
        let tight = Limits {
            max_line_bytes: 1,
            ..Limits::built_in()
        };
        assert_eq!(clamp_line("ñx", tight), ("", true));
        assert_eq!(clamp_line("日本語", tight), ("", true));
        // Exactly at the limit is not a cut.
        assert_eq!(clamp_line("aaaaa", limits), ("aaaaa", false));

        // And through the parser: the flag reaches the caller.
        let (out, truncated) = blocks_of("aaaañ tail\n", limits, Mode::Trusted);
        assert!(truncated, "a cut line must raise the flag");
        assert_eq!(
            out,
            vec![Block::Paragraph(vec![Span::Text("aaaa".to_owned())])]
        );
        // A line cut down to nothing is a blank line, not a block.
        let (out, truncated) = blocks_of("ñx\n", tight, Mode::Trusted);
        assert!(truncated);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn more_blocks_than_the_cap_truncate_and_keep_the_prefix() {
        let limits = Limits {
            max_blocks: 2,
            ..Limits::built_in()
        };
        let (out, truncated) = blocks_of("# a\n# b\n# c\n# d\n", limits, Mode::Trusted);
        assert!(truncated);
        assert_eq!(
            out,
            vec![
                Block::Heading {
                    level: 1,
                    text: "a".to_owned()
                },
                Block::Heading {
                    level: 1,
                    text: "b".to_owned()
                },
            ],
            "what was already parsed is kept: truncating is not discarding"
        );

        // One iteration can emit the pending paragraph AND its own block, so
        // the cap is checked again at the end: `blocks.len() <= max_blocks`
        // holds unconditionally, which on the hostile path is a memory bound.
        let one = Limits {
            max_blocks: 1,
            ..Limits::built_in()
        };
        let (out, truncated) = blocks_of("para\n# h\n", one, Mode::Trusted);
        assert!(truncated);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(
            out[0],
            Block::Paragraph(vec![Span::Text("para".to_owned())])
        );

        // Reaching the cap is not the same as LOSING something. A body that
        // ends in blank lines exactly at the cap dropped nothing, and a badge
        // that cries wolf over trailing whitespace teaches the reader to
        // ignore it.
        let (out, truncated) = blocks_of("# a\n# b\n\n\n", limits, Mode::Trusted);
        assert_eq!(out.len(), 2);
        assert!(!truncated, "only blank lines were left: nothing was lost");
        let (_, truncated) = blocks_of("# a\n# b\n\n\n# c\n", limits, Mode::Trusted);
        assert!(truncated, "a heading was left behind: that IS truncation");
    }

    #[test]
    fn a_table_survives_a_missing_separator() {
        // Header alone: a table with no rows, never a panic on `rows[0]`.
        assert_eq!(
            blocks("| a | b |\n"),
            vec![Block::Table {
                header: vec!["a".to_owned(), "b".to_owned()],
                rows: Vec::new(),
            }]
        );
        // Separator missing entirely: the first data row is DATA. Skipping
        // the second line unconditionally would eat it silently — a row that
        // vanishes is a documented fact the reader never gets.
        assert_eq!(
            blocks("| a | b |\n| 1 | 2 |\n"),
            vec![Block::Table {
                header: vec!["a".to_owned(), "b".to_owned()],
                rows: vec![vec!["1".to_owned(), "2".to_owned()]],
            }]
        );
        // An alignment separator is still syntax, in either spelling.
        assert_eq!(
            blocks("| a | b |\n|:--|--:|\n| 1 | 2 |\n"),
            blocks("| a | b |\n|---|---|\n| 1 | 2 |\n")
        );
    }

    #[test]
    fn the_callout_grammar_needs_a_space_after_the_marker() {
        // Emoji with no space after it: still a warning, and the body is
        // trimmed on the way out.
        assert_eq!(
            blocks("> \u{26A0}careful\n"),
            vec![Block::Callout {
                kind: Callout::Warn,
                spans: vec![Span::Text("careful".to_owned())],
            }]
        );
        assert_eq!(
            blocks("> \u{1F4A1} faster\n"),
            vec![Block::Callout {
                kind: Callout::Tip,
                spans: vec![Span::Text("faster".to_owned())],
            }]
        );
        assert_eq!(
            blocks("> plain\n"),
            vec![Block::Callout {
                kind: Callout::Note,
                spans: vec![Span::Text("plain".to_owned())],
            }],
            "no marker, neutral note"
        );
        // `>` with NO space is NOT a callout under this grammar: it falls
        // through to a paragraph, keeping the `>` as literal text. Pinned so
        // that widening the grammar is a deliberate change, not a drift.
        assert_eq!(
            blocks(">no space\n"),
            vec![Block::Paragraph(vec![Span::Text(">no space".to_owned())])]
        );
        assert_eq!(
            blocks(">\n"),
            vec![Block::Paragraph(vec![Span::Text(">".to_owned())])]
        );
        // `"> "` alone: a callout whose body is empty, not a panic.
        assert_eq!(
            blocks("> \n"),
            vec![Block::Callout {
                kind: Callout::Note,
                spans: Vec::new()
            }]
        );
    }

    #[test]
    fn a_plain_line_after_bullets_becomes_its_own_paragraph() {
        // The bullet run must stop at the first non-bullet line and NOT
        // swallow it: text eaten into a list is text the reader never sees.
        assert_eq!(
            blocks("- one\n- two\nnot a bullet\n- three\n"),
            vec![
                Block::Bullets(vec![
                    vec![Span::Text("one".to_owned())],
                    vec![Span::Text("two".to_owned())],
                ]),
                Block::Paragraph(vec![Span::Text("not a bullet".to_owned())]),
                Block::Bullets(vec![vec![Span::Text("three".to_owned())]]),
            ]
        );
        // A dash with no space is not a bullet either.
        assert_eq!(
            blocks("-nope\n"),
            vec![Block::Paragraph(vec![Span::Text("-nope".to_owned())])]
        );
    }

    #[test]
    fn consecutive_lines_join_into_one_paragraph() {
        // Blank lines are the only paragraph separator; the joiner is a
        // single space, so a hard-wrapped corpus reflows instead of showing
        // its wrapping.
        assert_eq!(
            blocks("one\ntwo\n\nthree\n"),
            vec![
                Block::Paragraph(vec![Span::Text("one two".to_owned())]),
                Block::Paragraph(vec![Span::Text("three".to_owned())]),
            ]
        );
    }

    #[test]
    fn the_byte_cut_never_underflows_and_never_loops() {
        // The three degenerate inputs of the backward walk. Each one used to
        // be a plausible panic or hang on the path that reads a plugin file.
        assert_eq!(cut_at_boundary(b"abc", 0), b"", "max == 0 examines nothing");
        assert_eq!(
            cut_at_boundary(b"abc", 99),
            b"abc",
            "max > len is not a cut, and the guard keeps the index in range"
        );
        // Pure continuation bytes: not valid UTF-8 at ANY offset, so the walk
        // has no boundary to find. It must stop after `MAX_WALK_BACK` steps
        // rather than march the cut down to zero and hand back an EMPTY
        // document — which is what it used to do, silently, for a 64 KiB
        // file. The bytes are exactly what the decoder later turns into
        // `U+FFFD`.
        let all_cont = [0x80_u8; 16];
        assert_eq!(
            cut_at_boundary(&all_cont, 8).len(),
            8 - MAX_WALK_BACK,
            "the walk is bounded: content survives bytes that are not UTF-8"
        );
        assert_eq!(
            cut_at_boundary(&all_cont, 2).len(),
            0,
            "and it still cannot step below zero"
        );
        assert_eq!(
            cut_at_boundary(&all_cont, 16),
            &all_cont,
            "a cut at the end examines nothing, invalid or not"
        );
        assert_eq!(cut_at_boundary(&[], 0), b"");
        assert_eq!(cut_at_boundary(&[], 8), b"");

        // The real job: a multi-byte character straddling the cut.
        assert_eq!(cut_at_boundary("añ".as_bytes(), 2), b"a");
        assert_eq!(cut_at_boundary("añ".as_bytes(), 3), "añ".as_bytes());
        for max in 0..=4 {
            let out = cut_at_boundary("日x".as_bytes(), max);
            assert!(
                std::str::from_utf8(out).is_ok(),
                "max={max}: cut a character in half"
            );
        }
    }

    #[test]
    fn a_body_of_hostile_names_parses_into_blocks_without_panicking() {
        // Every hostile name of the canonical corpus, one per line and again
        // spliced into each block prefix. Trusted mode (no masking) on
        // purpose: this pins the STRUCTURE — task 6 pins the masking on the
        // same parser.
        let names: Vec<String> = norte_testkit::corpus::hostile_names()
            .into_iter()
            .map(|n| String::from_utf8_lossy(&n.bytes).into_owned())
            .collect();
        assert!(names.len() >= 32, "the canonical corpus must not shrink");
        for name in &names {
            for prefix in ["", "# ", "> ", "- ", "|", "```"] {
                let body = format!("{prefix}{name}\n");
                let (out, _) = blocks_of(&body, Limits::built_in(), Mode::Trusted);
                assert!(out.len() <= 2, "{prefix:?} + {name:?} -> {out:?}");
                if let Some(Block::Table { header, rows }) = out.first() {
                    assert!(rows.iter().all(|r| r.len() == header.len()));
                }
            }
        }
        let all = names.join("\n");
        let (out, truncated) = blocks_of(&all, Limits::built_in(), Mode::Trusted);
        assert!(!truncated);
        assert!(!out.is_empty());
    }
}
