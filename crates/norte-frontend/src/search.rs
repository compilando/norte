//! A search's form: what is asked and how it turns into [`FsSearchParams`].
//!
//! It lives here, and not in a frontend, for the same reason as
//! [`crate::search_status`] (ADR 0077): both frontends ask the same search,
//! and the day each one builds its own parameters they silently drift apart.
//! A filter one frontend applies and the other does not does not look like a
//! bug: it looks like a search that found more things.
//!
//! **The clock is TOLD, not read.** "Changed seven days ago" is counted from
//! the instant Enter is pressed, and the caller knows it; a mapping that
//! asked the time on its own could not be tested without waiting.
//!
//! Nothing is painted here: the labels are Fluent KEYS ([`SearchField::key`])
//! and each frontend translates and places them.

use norte_proto::VPath;
use norte_proto::methods::FsSearchParams;

/// Which form field receives what gets typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchField {
    /// Pattern over the NAME (glob or regex).
    Name,
    /// Text/regex over the CONTENT.
    Content,
    /// Directory names not descended into (protocol 0.81.0).
    Exclude,
    /// Minimum size.
    MinSize,
    /// Maximum size.
    MaxSize,
    /// Modified in the last N days.
    Days,
    /// Forced encoding of the content.
    Encoding,
}

impl SearchField {
    /// All of them, in the order they are iterated and painted.
    ///
    /// One single list for both things, and that is deliberate: two
    /// hand-written lists drift apart the moment a field is added, and then
    /// the cursor jumps to a line that is not painted.
    pub const ORDEN: [SearchField; 7] = [
        SearchField::Name,
        SearchField::Content,
        SearchField::Exclude,
        SearchField::MinSize,
        SearchField::MaxSize,
        SearchField::Days,
        SearchField::Encoding,
    ];

    /// Its label's Fluent key.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Name => "search-name",
            Self::Content => "search-content",
            Self::Exclude => "search-exclude",
            Self::MinSize => "search-min-size",
            Self::MaxSize => "search-max-size",
            Self::Days => "search-days",
            Self::Encoding => "search-encoding",
        }
    }

    /// A STABLE id for the wire and for tests.
    ///
    /// It is not the Fluent key —that can change name with the wording— nor
    /// the index, which is renumbered when a field is inserted. It is what
    /// travels in `UiAction::DialogField` and what a test names.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Content => "content",
            Self::Exclude => "exclude",
            Self::MinSize => "min-size",
            Self::MaxSize => "max-size",
            Self::Days => "days",
            Self::Encoding => "encoding",
        }
    }

    /// The field with that id, if any. An unknown id is `None`: a renderer
    /// sends it, and a renderer does not decide which fields exist.
    #[must_use]
    pub fn por_id(id: &str) -> Option<Self> {
        Self::ORDEN.into_iter().find(|f| f.id() == id)
    }
}

/// The STABLE id of the regex switch, for the wire and for tests.
///
/// All five live here, with the model, and not in the frontend that paints
/// them: an id is part of what the form IS —what comes back when someone
/// touches a control— and keeping them in the renderer would leave them at
/// the mercy of whoever reorders the screen.
pub const ID_REGEX: &str = "regex";
/// The stable id of the case switch. See [`ID_REGEX`].
pub const ID_CASE: &str = "case";
/// The stable id of the whole-word switch. See [`ID_REGEX`].
pub const ID_WHOLE_WORD: &str = "whole-word";
/// The stable id of the subdirectories switch. See [`ID_REGEX`].
pub const ID_RECURSIVE: &str = "recursive";
/// The stable id of the entry-kind cycle. See [`ID_REGEX`].
pub const ID_KINDS: &str = "kinds";

/// Which entry kind counts as a search result.
///
/// Three values and not a free list: they are the three answers people
/// actually give, and a selector over `S_IFMT`'s seven classes to find a
/// socket is a bigger dialog serving no one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchKinds {
    /// Anything.
    #[default]
    All,
    /// Files only.
    Files,
    /// Directories only.
    Folders,
}

impl SearchKinds {
    /// The next one in the cycle.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Files,
            Self::Files => Self::Folders,
            Self::Folders => Self::All,
        }
    }

    /// Its label's Fluent key.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::All => "search-kinds-any",
            Self::Files => "search-kinds-files",
            Self::Folders => "search-kinds-dirs",
        }
    }

    /// What goes in `FsSearchParams::kinds`. Empty = all.
    #[must_use]
    pub fn wire(self) -> Vec<norte_proto::EntryKind> {
        match self {
            Self::All => Vec::new(),
            Self::Files => vec![norte_proto::EntryKind::File],
            Self::Folders => vec![norte_proto::EntryKind::Dir],
        }
    }
}

/// A size typed by hand: `1024`, `500k`, `1M`, `2.5G`, `  3 g  `.
///
/// `None` if it is not understood, and that includes the empty string: the
/// caller tells "typed nothing" apart from "typed something unreadable" by
/// checking whether the field is blank. The units are powers of 1024, which
/// is what the size column shows; a lowercase `k` and an uppercase `K` are
/// the same, because typing a unit's correct case is not a decision.
///
/// ```
/// use norte_frontend::search::parse_size;
/// assert_eq!(parse_size("1k"), Some(1024));
/// assert_eq!(parse_size("2.5G"), Some(2_684_354_560));
/// assert_eq!(parse_size("1 giga"), None);
/// ```
#[must_use]
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (number, mult) = match s.chars().last()?.to_ascii_lowercase() {
        'k' => (&s[..s.len() - 1], 1024_u64),
        'm' => (&s[..s.len() - 1], 1024 * 1024),
        'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        't' => (&s[..s.len() - 1], 1024_u64.pow(4)),
        _ => (s, 1),
    };
    let n: f64 = number.trim().parse().ok()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    // `2.5M` is legitimate and `2.5` bytes is not, so it is rounded to the
    // nearest integer AFTER multiplying.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "bounded right above: finite, non-negative and compared against u64::MAX"
    )]
    {
        let bytes = n * mult as f64;
        (bytes <= u64::MAX as f64).then_some(bytes.round() as u64)
    }
}

/// A number of days: an integer, non-negative, with a cap of a hundred
/// years.
///
/// The cap protects against nothing arithmetic —subtracting a hundred years
/// from 2026 gives 1926, which is a perfectly legal negative `mtime_ms` that
/// the core's filter compares just the same—, but against a typo:
/// `20260920` in the days field is a badly-typed date, and accepting it as
/// "fifty-five thousand years ago" is the same as not filtering at all.
///
/// ```
/// use norte_frontend::search::parse_days;
/// assert_eq!(parse_days("7"), Some(7));
/// assert_eq!(parse_days("36501"), None);
/// ```
#[must_use]
pub fn parse_days(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.parse::<u32>().ok().filter(|d| *d <= 36_500)
}

/// What is asked in a search: seven text fields, four switches and which
/// entry kind counts.
///
/// It is the model, not the screen: which key moves what and how it is
/// painted is decided by each frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "these are the form's SWITCHES: regex, case, whole word and \
              subdirectories. Grouping them in a separate type says nothing \
              their names do not already say"
)]
pub struct SearchForm {
    /// Name pattern (glob, or regex with `regex`).
    pub name: String,
    /// Content text (literal, or regex with `regex`).
    pub content: String,
    /// Directory names NOT descended into, comma-separated: `target,
    /// node_modules, .git` (protocol 0.81.0). Globs, like the name.
    pub exclude: String,
    /// Minimum size, human-typed: `1M`, `500k`, `1024`. Empty = no minimum.
    pub min_size: String,
    /// Maximum size, same format.
    pub max_size: String,
    /// Modified in the last N DAYS. Empty = any date.
    ///
    /// Days and not a date range because it is the question people actually
    /// ask —"what have I touched this week?"— and because a range needs two
    /// fields, a format and a timezone to answer the same thing.
    pub days: String,
    /// The encoding to read the content with. Empty = automatic.
    pub encoding: String,
    /// Field that receives what gets typed.
    pub field: SearchField,
    /// Interprets both patterns as regex instead of glob/literal.
    pub regex: bool,
    /// Case-sensitive matching.
    pub case: bool,
    /// The content match is a whole word.
    pub whole_word: bool,
    /// Descend into subdirectories. On by default.
    pub recursive: bool,
    /// Which entry kind counts as a result.
    pub kinds: SearchKinds,
}

impl Default for SearchForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            content: String::new(),
            exclude: String::new(),
            min_size: String::new(),
            max_size: String::new(),
            days: String::new(),
            encoding: String::new(),
            field: SearchField::Name,
            regex: false,
            case: false,
            whole_word: false,
            recursive: true,
            kinds: SearchKinds::All,
        }
    }
}

impl SearchForm {
    /// An empty form with focus on the name field.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The active text field, mutable.
    fn active_mut(&mut self) -> &mut String {
        self.field_mut(self.field)
    }

    /// Any text field, mutable.
    fn field_mut(&mut self, f: SearchField) -> &mut String {
        match f {
            SearchField::Name => &mut self.name,
            SearchField::Content => &mut self.content,
            SearchField::Exclude => &mut self.exclude,
            SearchField::MinSize => &mut self.min_size,
            SearchField::MaxSize => &mut self.max_size,
            SearchField::Days => &mut self.days,
            SearchField::Encoding => &mut self.encoding,
        }
    }

    /// A field's text, to paint it.
    #[must_use]
    pub fn text(&self, f: SearchField) -> &str {
        match f {
            SearchField::Name => &self.name,
            SearchField::Content => &self.content,
            SearchField::Exclude => &self.exclude,
            SearchField::MinSize => &self.min_size,
            SearchField::MaxSize => &self.max_size,
            SearchField::Days => &self.days,
            SearchField::Encoding => &self.encoding,
        }
    }

    /// Sets a field's WHOLE text.
    ///
    /// It is what a frontend whose text field is edited by the toolkit
    /// itself needs: there the caret's owner is the `<input>`, and what
    /// arrives is the resulting text, not the keystroke. The terminal uses
    /// [`Self::push_char`]/[`Self::backspace`], which is what happens there.
    pub fn set_text(&mut self, f: SearchField, text: String) {
        *self.field_mut(f) = text;
    }

    /// A printable character into the active field.
    pub fn push_char(&mut self, c: char) {
        self.active_mut().push(c);
    }

    /// Backspace in the active field.
    pub fn backspace(&mut self) {
        self.active_mut().pop();
    }

    /// To the next field, in a circle.
    pub fn toggle_field(&mut self) {
        let i = SearchField::ORDEN
            .iter()
            .position(|f| *f == self.field)
            .unwrap_or(0);
        self.field = SearchField::ORDEN[(i + 1) % SearchField::ORDEN.len()];
    }

    /// Toggles glob/literal ⇄ regex (applies to BOTH axes).
    pub fn toggle_regex(&mut self) {
        self.regex = !self.regex;
    }

    /// Toggles case sensitivity.
    pub fn toggle_case(&mut self) {
        self.case = !self.case;
    }

    /// Toggles "whole word" in the content search.
    pub fn toggle_whole_word(&mut self) {
        self.whole_word = !self.whole_word;
    }

    /// Toggles descending into subdirectories.
    pub fn toggle_recursive(&mut self) {
        self.recursive = !self.recursive;
    }

    /// Cycles which entry kind counts.
    pub fn cycle_kinds(&mut self) {
        self.kinds = self.kinds.next();
    }

    /// Is there any criterion? With none, nothing is launched: a search with
    /// no criterion is a recursive listing under another name.
    ///
    /// A FILTER counts as a criterion since 0.81.0: "everything heavier than
    /// a gig" is a legitimate search and one of the most useful there is.
    /// What does not count is excluding directories —that removes, it does
    /// not ask— nor the encoding, which says HOW to read something nobody
    /// has asked for yet.
    #[must_use]
    pub fn has_criteria(&self) -> bool {
        !self.name.is_empty()
            || !self.content.is_empty()
            || self.kinds != SearchKinds::All
            || parse_size(&self.min_size).is_some()
            || parse_size(&self.max_size).is_some()
            || parse_days(&self.days).is_some()
    }

    /// The directory names to exclude, one per comma, with empties dropped.
    #[must_use]
    pub fn exclude_names(&self) -> Vec<String> {
        self.exclude
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// The field that was typed and cannot be understood, if any.
    ///
    /// Checked BEFORE launching: a search that silently ignores a
    /// badly-typed `1 gigabyte` returns the whole tree and reads just like a
    /// result, which is exactly what the 0.81.0 version notice exists to
    /// prevent against an old daemon. The same trap at home is no better.
    #[must_use]
    pub fn field_unreadable(&self) -> Option<SearchField> {
        if !self.min_size.trim().is_empty() && parse_size(&self.min_size).is_none() {
            return Some(SearchField::MinSize);
        }
        if !self.max_size.trim().is_empty() && parse_size(&self.max_size).is_none() {
            return Some(SearchField::MaxSize);
        }
        if !self.days.trim().is_empty() && parse_days(&self.days).is_none() {
            return Some(SearchField::Days);
        }
        // The exclusion cap is the PROTOCOL's
        // ([`norte_proto::methods::SEARCH_EXCLUDES_MAX`]): going over it is
        // not a daemon error that can be read, it is a generic
        // `InvalidParams` that arrives AFTER launching and that the reader
        // cannot map to a field. Said here, the field is flagged and
        // nothing launches — which is the rule for all the others.
        if self.exclude_names().len() > norte_proto::methods::SEARCH_EXCLUDES_MAX {
            return Some(SearchField::Exclude);
        }
        // The encoding too, and it is the one that needs it most: the three
        // above degrade quietly and this one takes down the whole search
        // with a request error, which the frontend paints with the
        // `InvalidPath` category — i.e. "invalid path" for a badly-typed
        // encoding name. Said here, the field is flagged.
        let enc = self.encoding.trim();
        if !enc.is_empty()
            && norte_encoding::Encoding::for_label_no_replacement(enc.as_bytes()).is_none()
        {
            return Some(SearchField::Encoding);
        }
        None
    }
}

/// A form's [`FsSearchParams`], with the clock TOLD.
///
/// The `regex` toggle decides, per axis, `name_glob` vs `name_regex` and
/// `content` vs `content_regex`; an empty field contributes no criterion.
///
/// `now_ms` is the instant days are counted from, and the caller states it:
/// "changed seven ago" is counted from when Enter is pressed, and the core
/// has no business knowing at what moment the question was asked. That it is
/// a parameter and not a clock read is what makes this testable.
///
/// ```
/// use norte_frontend::search::{SearchForm, params};
/// use norte_proto::VPath;
///
/// let mut f = SearchForm::new();
/// f.name = "*.rs".to_owned();
/// f.days = "7".to_owned();
/// let p = params(&f, VPath::parse("mem:///home").unwrap(), 7 * 86_400_000, 10_000);
/// assert_eq!(p.name_glob.as_deref(), Some("*.rs"));
/// assert_eq!(p.name_regex, None);
/// // Seven days counted from the stated instant, not from the clock.
/// assert_eq!(p.mtime_after, Some(0));
/// ```
#[must_use]
pub fn params(form: &SearchForm, root: VPath, now_ms: i64, max_hits: u32) -> FsSearchParams {
    let (name_glob, name_regex) = match (form.name.is_empty(), form.regex) {
        (true, _) => (None, None),
        (false, false) => (Some(form.name.clone()), None),
        (false, true) => (None, Some(form.name.clone())),
    };
    let (content, content_regex) = match (form.content.is_empty(), form.regex) {
        (true, _) => (None, None),
        (false, false) => (Some(form.content.clone()), None),
        (false, true) => (None, Some(form.content.clone())),
    };
    let mtime_after = parse_days(&form.days)
        .map(|d| now_ms.saturating_sub(i64::from(d).saturating_mul(86_400_000)));
    let encoding = {
        let e = form.encoding.trim();
        (!e.is_empty()).then(|| e.to_owned())
    };
    FsSearchParams {
        name_glob,
        name_regex,
        content,
        content_regex,
        case_sensitive: form.case,
        max_hits: Some(max_hits),
        kinds: form.kinds.wire(),
        min_size: parse_size(&form.min_size),
        max_size: parse_size(&form.max_size),
        mtime_after,
        mtime_before: None,
        exclude_roots: Vec::new(),
        exclude_names: form.exclude_names(),
        whole_word: form.whole_word,
        recursive: form.recursive,
        encoding,
        ..FsSearchParams::new(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> VPath {
        VPath::parse("mem:///casa").expect("test wire")
    }

    /// The sizes people actually type.
    #[test]
    fn parse_size_understands_what_gets_typed() {
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size("1k"), Some(1024));
        assert_eq!(
            parse_size("1K"),
            Some(1024),
            "the unit's case does not matter"
        );
        assert_eq!(parse_size(" 1 M "), Some(1024 * 1024), "and the spaces");
        assert_eq!(parse_size("2.5G"), Some(2_684_354_560), "and the decimals");
        assert_eq!(parse_size("1T"), Some(1024_u64.pow(4)));
    }

    /// And what is NOT understood is said to be not understood, instead of
    /// counting as zero and returning the whole tree.
    #[test]
    fn parse_size_does_not_guess() {
        for bad in ["", "  ", "mucho", "1 giga", "-5", "1kk", "k", "inf", "NaN"] {
            assert_eq!(parse_size(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn parse_days_is_a_bounded_integer() {
        assert_eq!(parse_days("7"), Some(7));
        assert_eq!(parse_days(" 0 "), Some(0));
        assert_eq!(parse_days("36500"), Some(36_500));
        for bad in ["", "-1", "1.5", "36501", "ayer"] {
            assert_eq!(parse_days(bad), None, "{bad:?}");
        }
    }

    /// An unreadable field is named BEFORE launching, and says which one.
    #[test]
    fn an_unreadable_field_is_named_and_nothing_launches() {
        let mut d = SearchForm::new();
        d.min_size = "mucho".into();
        assert_eq!(d.field_unreadable(), Some(SearchField::MinSize));
        d.min_size = "1M".into();
        assert_eq!(d.field_unreadable(), None, "now it is understood");
        d.days = "ayer".into();
        assert_eq!(d.field_unreadable(), Some(SearchField::Days));
        // Empty is not unreadable: it is "typed nothing".
        d.days = "   ".into();
        assert_eq!(d.field_unreadable(), None);
        // And the encoding, which needs it most: it is the only one whose
        // error takes down the whole search with an `InvalidPath` that the
        // frontend paints as "invalid path".
        d.encoding = "utf-eight".into();
        assert_eq!(d.field_unreadable(), Some(SearchField::Encoding));
        // Including a REPLACEMENT label, which does not fail but finds
        // nothing: it decodes the whole file into a single U+FFFD.
        d.encoding = "utf-7".into();
        assert_eq!(d.field_unreadable(), Some(SearchField::Encoding));
        d.encoding = "windows-1252".into();
        assert_eq!(d.field_unreadable(), None);
    }

    /// A FILTER alone is already a criterion (0.81.0); excluding directories
    /// is not, because it removes instead of asking.
    #[test]
    fn a_filter_alone_is_enough_to_launch() {
        let mut d = SearchForm::new();
        assert!(!d.has_criteria(), "completely empty, no");
        d.exclude = "target".into();
        assert!(!d.has_criteria(), "excluding is not asking");
        d.encoding = "utf-8".into();
        assert!(
            !d.has_criteria(),
            "how to read something is not what to search for"
        );
        d.min_size = "1G".into();
        assert!(d.has_criteria(), "\"everything heavier than a gig\" does");
    }

    /// **Days are counted from the instant that is STATED.**
    ///
    /// This is what the terminal's mapping could not test: it read
    /// `SystemTime::now()` on its own, so a test's expected value had to be
    /// computed with the same clock as the code under test.
    #[test]
    fn days_are_counted_from_the_stated_instant() {
        let mut f = SearchForm::new();
        f.days = "7".into();
        let now = 10 * 86_400_000_i64;
        let p = params(&f, root(), now, 10_000);
        assert_eq!(p.mtime_after, Some(3 * 86_400_000));
        assert_eq!(
            p.mtime_before, None,
            "the form does not ask about the other side"
        );
        // With no days there is no date filter, and not "since the
        // beginning of time": a filter that was not asked for is not sent.
        f.days = String::new();
        assert_eq!(params(&f, root(), now, 10_000).mtime_after, None);
    }

    /// The `regex` switch changes axis for BOTH patterns at once, which is
    /// what its label says.
    #[test]
    fn the_regex_toggle_moves_both_axes() {
        let mut f = SearchForm::new();
        f.name = "*.rs".into();
        f.content = "TODO".into();
        let p = params(&f, root(), 0, 10_000);
        assert_eq!(p.name_glob.as_deref(), Some("*.rs"));
        assert_eq!(p.content.as_deref(), Some("TODO"));
        assert!(p.name_regex.is_none() && p.content_regex.is_none());

        f.toggle_regex();
        let p = params(&f, root(), 0, 10_000);
        assert_eq!(p.name_regex.as_deref(), Some("*.rs"));
        assert_eq!(p.content_regex.as_deref(), Some("TODO"));
        assert!(p.name_glob.is_none() && p.content.is_none());
    }

    /// What the form does NOT ask does not travel: a freshly made
    /// `FsSearchParams` has to still be that of an ordinary search.
    #[test]
    fn an_empty_form_sends_no_filters() {
        let p = params(&SearchForm::new(), root(), 0, 10_000);
        assert!(p.kinds.is_empty());
        assert!(p.exclude_names.is_empty());
        assert!(p.min_size.is_none() && p.max_size.is_none());
        assert!(p.encoding.is_none());
        assert!(!p.whole_word);
        assert!(
            p.recursive,
            "descending into subdirectories is what it used to do"
        );
    }

    /// Exclusions are split by commas and blanks do not count.
    #[test]
    fn exclusions_are_split_by_commas() {
        let mut f = SearchForm::new();
        f.exclude = " target , node_modules ,, .git ".into();
        assert_eq!(f.exclude_names(), ["target", "node_modules", ".git"]);
    }

    /// **Going over the exclusion cap is said BEFORE launching.**
    ///
    /// The cap belongs to the protocol, and the daemon enforces it with a
    /// generic `InvalidParams` that arrives after launching: the reader sees
    /// a search that failed and cannot tell which field caused it. Checked
    /// here, the field is flagged — and both frontends inherit it, which is
    /// what this module exists for.
    #[test]
    fn going_over_the_exclusion_cap_flags_the_field() {
        let cap = norte_proto::methods::SEARCH_EXCLUDES_MAX;
        let mut f = SearchForm::new();
        f.name = "*.rs".into();
        f.exclude = vec!["a"; cap].join(",");
        assert_eq!(
            f.field_unreadable(),
            None,
            "right at the cap it is accepted"
        );
        f.exclude = vec!["a"; cap + 1].join(",");
        assert_eq!(f.field_unreadable(), Some(SearchField::Exclude));
    }

    /// A field's id is STABLE and round-trips; one that does not exist is
    /// not invented (a renderer sends it).
    #[test]
    fn field_ids_round_trip() {
        for f in SearchField::ORDEN {
            assert_eq!(SearchField::por_id(f.id()), Some(f), "{:?}", f.id());
        }
        assert_eq!(SearchField::por_id("no-existe"), None);
    }

    /// **Switch ids must not collide with field ids.**
    ///
    /// The host resolves an id by asking [`SearchField::por_id`] first and
    /// falling back to the five `ID_*` if it is none of them. The day a
    /// field was named `case`, the switch would stop working without
    /// anything turning red: this is what turns red.
    #[test]
    fn switch_ids_do_not_collide_with_field_ids() {
        for id in [ID_REGEX, ID_CASE, ID_WHOLE_WORD, ID_RECURSIVE, ID_KINDS] {
            assert_eq!(SearchField::por_id(id), None, "{id} collides with a field");
        }
    }

    /// Focus cycles through all seven, in the order they are painted.
    #[test]
    fn focus_cycles_through_all_seven() {
        let mut f = SearchForm::new();
        assert_eq!(f.field, SearchField::Name);
        for expected in SearchField::ORDEN.into_iter().skip(1) {
            f.toggle_field();
            assert_eq!(f.field, expected);
        }
        f.toggle_field();
        assert_eq!(f.field, SearchField::Name, "and comes back to the first");
    }

    /// Writing a whole field is what the window does; typing, what the
    /// terminal does. Both end up in the same place.
    #[test]
    fn a_field_can_be_written_whole_or_key_by_key() {
        let mut f = SearchForm::new();
        f.set_text(SearchField::Content, "hola".into());
        assert_eq!(f.text(SearchField::Content), "hola");
        assert_eq!(f.text(SearchField::Name), "", "and only that field");
        f.push_char('a');
        f.push_char('b');
        f.backspace();
        assert_eq!(
            f.text(SearchField::Name),
            "a",
            "the active one is still the name"
        );
    }
}
