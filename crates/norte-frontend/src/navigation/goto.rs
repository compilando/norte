//! "Go to anywhere" (phase 6 of the WOW program): a single screen that
//! brings together what used to live in five — the `>` commands list and
//! the `?` help list, history, popular places, favorites and connections —
//! and adds what had no home: a TYPED path and, once it arrives, what the
//! semantic index found.
//!
//! The mode comes from the query's first character: `>` commands, `?`
//! help, anything else places ([`Mode::of`]).
//!
//! What this module contributes and what it leaves out, on purpose:
//!
//! - Contributes the MODEL: what a row is, which section it falls in, what
//!   order the sections go in, how it is filtered and where the cursor
//!   sits. All pure, and tested.
//! - Does not contribute the DATA. Each source brings its own, because each
//!   one knows things this module cannot: where a name's bytes come from
//!   and what encoding paints them, whether the text belongs to the
//!   project or to a third party, and whether it needs masking. A row
//!   arrives with its text already paintable and its [`GotoRow::hostile`]
//!   flag already set — the same criterion as [`crate::palette::Row`], and
//!   for the same reason: masking while painting is masking on every frame
//!   and forgetting it on one.
//!
//! A new source is implementing [`GotoSource`] and putting it in the list.
//! There is nowhere else to touch: filtering, headers, order and the cursor
//! all live here.

/// The prefix that turns the box into the commands list.
pub const PREFIX_COMMANDS: &str = ">";
/// The prefix that turns the box into the help list.
pub const PREFIX_HELP: &str = "?";

/// What the box is listing, chosen by the query's first character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Where to go: the sections in [`ORDEN`].
    Places,
    /// What to do: one flat list of commands, behind [`PREFIX_COMMANDS`].
    Commands,
    /// What to read: one flat list of help rows, behind [`PREFIX_HELP`].
    Help,
}

impl Mode {
    /// The mode `query` asks for. Only the FIRST character counts: a `>`
    /// further on is a letter like any other.
    #[must_use]
    pub fn of(query: &str) -> Self {
        if query.starts_with(PREFIX_COMMANDS) {
            Self::Commands
        } else if query.starts_with(PREFIX_HELP) {
            Self::Help
        } else {
            Self::Places
        }
    }

    /// A stable name for the frontends: `"places"`, `"commands"` or `"help"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Places => "places",
            Self::Commands => "commands",
            Self::Help => "help",
        }
    }
}

/// What is searched for: the query without its mode prefix and without
/// leading spaces.
#[must_use]
pub fn needle(query: &str) -> &str {
    query
        .strip_prefix(PREFIX_COMMANDS)
        .or_else(|| query.strip_prefix(PREFIX_HELP))
        .unwrap_or(query)
        .trim_start()
}

/// A "go to" section: a stable id and its title's Fluent key.
///
/// The id is NOT painted: it is what pairs a row with its header and what
/// fixes the order. The title is, and comes from Fluent like everything
/// else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GotoSection {
    /// The section's stable id.
    pub id: &'static str,
    /// Fluent key of the title painted above its rows.
    pub title_key: &'static str,
}

/// The path the reader just typed. Goes FIRST because it is what they just
/// wrote: no list outranks that.
pub const SECTION_PATH: GotoSection = GotoSection {
    id: "path",
    title_key: "goto-section-path",
};

/// Where this panel has been.
pub const SECTION_HISTORY: GotoSection = GotoSection {
    id: "history",
    title_key: "goto-section-history",
};

/// Where it comes back to most.
pub const SECTION_POPULAR: GotoSection = GotoSection {
    id: "popular",
    title_key: "goto-section-popular",
};

/// The places saved by hand.
pub const SECTION_FAVORITES: GotoSection = GotoSection {
    id: "favorites",
    title_key: "goto-section-favorites",
};

/// The configured remote connections.
pub const SECTION_CONNECTIONS: GotoSection = GotoSection {
    id: "connections",
    title_key: "goto-section-connections",
};

/// The catalogue's commands, behind `>`.
pub const SECTION_COMMANDS: GotoSection = GotoSection {
    id: "commands",
    title_key: "goto-section-commands",
};

/// Plugin command rows: a section of their own so a late `replace_section`
/// does not touch the built-ins; painted in the same flat list.
pub const SECTION_PLUGINS: GotoSection = GotoSection {
    id: "plugins",
    title_key: "goto-section-commands",
};

/// The `?` list: prefixes, then help topics.
pub const SECTION_HELP: GotoSection = GotoSection {
    id: "help",
    title_key: "goto-title-help",
};

/// What the semantic index found. Arrives LATE (it is a question to the
/// core, not a list in memory) and that is why it goes last: a section
/// that appears mid-typing must not push down what the reader was already
/// looking at.
pub const SECTION_INDEX: GotoSection = GotoSection {
    id: "index",
    title_key: "goto-section-index",
};

/// The ORDER the PLACES sections are painted in, and the only place it
/// lives. Commands and help are not here: they are flat lists of their own
/// mode.
///
/// Fixed and not configurable: it is the order a reader searches in — what
/// they just typed, where they have been, what they saved — and a list
/// that reorders itself is a list where nothing can be learned about where
/// anything is.
pub const ORDEN: &[GotoSection] = &[
    SECTION_PATH,
    SECTION_HISTORY,
    SECTION_POPULAR,
    SECTION_FAVORITES,
    SECTION_CONNECTIONS,
    SECTION_INDEX,
];

/// A "go to" row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GotoRow {
    /// Id of the section it belongs to (one of the ones in [`ORDEN`]).
    pub section: &'static str,
    /// DISPATCH key, never painted — the caller consumes it on confirm.
    /// Same contract as [`crate::palette::Row::key`]: for a path it carries
    /// the `VPath`'s wire, for a command its name.
    pub key: String,
    /// What gets painted, ALREADY paintable (masked if it needed to be).
    pub text: String,
    /// The second line, or empty.
    pub desc: String,
    /// `text` is painted differently from what the source bytes say.
    ///
    /// Travels with the row and is not recomputed on paint: a masked
    /// string that travels without its flag reads as faithful, and this is
    /// a screen where where-to-go is chosen.
    pub hostile: bool,
    /// The command's key chord, already painted, if it has one.
    pub chord: Option<String>,
    /// The command's category, already translated, if it has one.
    pub category: Option<String>,
    /// Why the command cannot run here (already translated), or `None`.
    pub unavailable: Option<String>,
    /// Set by the model: the row leads the list because it was run lately.
    pub recent: bool,
    /// Set by the model: the char indices of `text` the query matched.
    pub positions: Vec<u32>,
}

/// A source of "go to" rows.
///
/// The contract is short on purpose: give the rows it contributes for a
/// query. Everything else — fuzzy ranking, putting up the header, ordering
/// the sections, moving the cursor — belongs to the model, so a new source
/// does not have to get any of those four things right.
///
/// `query` is passed in case the source knows how to filter BETTER than the
/// generic fuzzy match (the semantic index asks the core with it, and the
/// typed path IS the query). A source with nothing special to do can
/// return everything: [`Goto::refresh`] ranks afterward, so filtering is a
/// single thing for all of them.
pub trait GotoSource {
    /// The section its rows fall into.
    fn section(&self) -> GotoSection;
    /// The rows it contributes for `query`.
    fn rows(&self, query: &str) -> Vec<GotoRow>;
    /// Whether its rows ALREADY come filtered and the model should not run
    /// the fuzzy match over them again.
    ///
    /// `false` by default, which is what an in-memory list wants. The
    /// semantic index sets it to `true`: it asked the core with the whole
    /// query and its results match by MEANING, not by letters — a
    /// subsequence filter on top would throw away exactly what makes it
    /// useful.
    fn ya_filtered(&self) -> bool {
        false
    }
}

/// Cap on rows PER SECTION.
///
/// A section longer than this is not read: it is skimmed, and each list
/// has its own screen for skimming, which also lets entries be deleted.
/// The cap lives here, in the model, and not in each source, because if it
/// lived in each source the next one would forget to add it.
pub const CAP_PER_SECTION: usize = 12;

/// A source with the rows already made: a snapshot of a list the frontend
/// already had in memory (history, favorites, commands…).
///
/// The snapshot is taken on opening, as the palette does with its rows,
/// and for the same reason: what is visible while the screen is open must
/// not change under the cursor.
pub struct FixedSource {
    section: GotoSection,
    rows: Vec<GotoRow>,
    ya_filtered: bool,
}

impl FixedSource {
    /// A source of fixed rows in `section`.
    #[must_use]
    pub fn new(section: GotoSection, rows: Vec<GotoRow>) -> Self {
        Self {
            section,
            rows,
            ya_filtered: false,
        }
    }

    /// Like [`Self::new`], but declaring that the rows ALREADY come
    /// filtered by whoever brought them (see [`GotoSource::ya_filtered`]).
    #[must_use]
    pub fn ya_filtered(section: GotoSection, rows: Vec<GotoRow>) -> Self {
        Self {
            section,
            rows,
            ya_filtered: false,
        }
        .with_already_filtered()
    }

    /// Marks its rows as already filtered.
    #[must_use]
    fn with_already_filtered(mut self) -> Self {
        self.ya_filtered = true;
        self
    }
}

impl GotoSource for FixedSource {
    fn section(&self) -> GotoSection {
        self.section
    }
    fn rows(&self, _query: &str) -> Vec<GotoRow> {
        self.rows.clone()
    }
    fn ya_filtered(&self) -> bool {
        self.ya_filtered
    }
}

/// The TYPED PATH source: looks at the query and, if it looks like a path,
/// offers to go there.
///
/// It is the only source with no list behind it — its row IS what the
/// reader just wrote — and that is why it lives here and not in a
/// frontend: the decision of what counts as a path ([`looks_path`]) is a
/// single one for both.
pub struct PathSource {
    desc: String,
}

impl PathSource {
    /// The source, with the detail line that goes with the row (already
    /// translated by the caller: this module does not choose a language).
    #[must_use]
    pub fn new(desc: impl Into<String>) -> Self {
        Self { desc: desc.into() }
    }
}

impl GotoSource for PathSource {
    fn section(&self) -> GotoSection {
        SECTION_PATH
    }
    fn rows(&self, query: &str) -> Vec<GotoRow> {
        looks_path(query).map_or_else(Vec::new, |path| {
            vec![GotoRow {
                section: SECTION_PATH.id,
                key: format!("{K_PATH}{path}"),
                // What was typed is painted AS IS. It belongs to the
                // reader themself, so there is nothing to mask; and
                // changing it while they write is the worst way to tell
                // them they made a mistake.
                text: path.to_owned(),
                desc: self.desc.clone(),
                ..GotoRow::default()
            }]
        })
    }
    /// The path row does not go through the filter: it IS the query, and
    /// asking what was typed whether it resembles itself cannot say
    /// anything useful. It is already trimmed when built (`looks_path`
    /// does `trim`), and that trim alone would be enough for the generic
    /// filter to throw it out.
    fn ya_filtered(&self) -> bool {
        true
    }
}

/// A painted line: a section header, or a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GotoLine {
    /// Section header — never receives the cursor.
    Header(GotoSection),
    /// A row, with its index within [`Goto::rows`].
    Row(usize),
}

/// The "go to" state: the sources, the query, what is visible and where
/// the cursor is.
pub struct Goto {
    sources: Vec<Box<dyn GotoSource + Send>>,
    query: String,
    rows: Vec<GotoRow>,
    lines: Vec<GotoLine>,
    cursor: usize,
    /// Command ids run lately, most recent first (without [`K_CMD`]).
    recent: Vec<String>,
}

/// By hand because [`GotoSource`] is a trait object and cannot derive
/// `Debug`. Of the sources, only their count is printed, which is the only
/// thing a `Debug` could say about them without forcing every future
/// source to implement `Debug` for nothing.
impl std::fmt::Debug for Goto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Goto")
            .field("sources", &self.sources.len())
            .field("query", &self.query)
            .field("rows", &self.rows)
            .field("lines", &self.lines)
            .field("cursor", &self.cursor)
            .field("recent", &self.recent)
            .finish()
    }
}

impl Goto {
    /// A "go to" over these sources, with an empty query.
    #[must_use]
    pub fn new(sources: Vec<Box<dyn GotoSource + Send>>) -> Self {
        let mut goto = Self {
            sources,
            query: String::new(),
            rows: Vec::new(),
            lines: Vec::new(),
            cursor: 0,
            recent: Vec::new(),
        };
        goto.refresh();
        goto
    }

    /// The command ids run lately, most recent first; they lead an empty
    /// `>` list.
    #[must_use]
    pub fn with_recent(mut self, recent: &[String]) -> Self {
        self.recent = recent.to_vec();
        self.refresh();
        self
    }

    /// Asks every source again and rebuilds what is visible.
    ///
    /// The cursor stays on the FIRST row: the query changed, so what was
    /// under the cursor is probably gone, and leaving it where it was is
    /// how an Enter ends up going somewhere the reader never got to read.
    pub fn refresh(&mut self) {
        self.rows.clear();
        self.lines.clear();
        let needle = needle(&self.query).to_owned();
        match self.mode() {
            Mode::Places => self.refresh_places(&needle),
            Mode::Commands => {
                self.refresh_flat(&[SECTION_COMMANDS.id, SECTION_PLUGINS.id], &needle, true);
            }
            Mode::Help => self.refresh_flat(&[SECTION_HELP.id], &needle, false),
        }
        self.cursor = self.first_row().unwrap_or(0);
    }

    /// The places: each section of [`ORDEN`] under its header, ranked
    /// inside itself, capped at [`CAP_PER_SECTION`].
    fn refresh_places(&mut self, needle: &str) {
        for section in ORDEN {
            let mut kept: Vec<GotoRow> = Vec::new();
            let mut to_rank: Vec<GotoRow> = Vec::new();
            for source in &self.sources {
                if source.section().id != section.id {
                    continue;
                }
                let raw = source.rows(&self.query);
                if source.ya_filtered() || needle.is_empty() {
                    kept.extend(raw.into_iter().map(unmarked));
                } else {
                    to_rank.extend(raw);
                }
            }
            kept.extend(rank(to_rank, needle, false));
            if kept.is_empty() {
                continue;
            }
            kept.truncate(CAP_PER_SECTION);
            self.lines.push(GotoLine::Header(*section));
            for row in kept {
                self.push_row(row);
            }
        }
    }

    /// One list with no headers: the commands (built-in and plugin) or the
    /// help rows. With nothing typed, recent commands first, in their order.
    fn refresh_flat(&mut self, sections: &[&str], needle: &str, recents: bool) {
        let mut all: Vec<GotoRow> = Vec::new();
        for source in &self.sources {
            if sections.contains(&source.section().id) {
                all.extend(source.rows(&self.query));
            }
        }
        let rows = if needle.is_empty() {
            let mut all: Vec<GotoRow> = all.into_iter().map(unmarked).collect();
            let mut out = Vec::new();
            if recents {
                for k in &self.recent {
                    if let Some(i) = all
                        .iter()
                        .position(|r| r.key.strip_prefix(K_CMD) == Some(k.as_str()))
                    {
                        let mut r = all.remove(i);
                        r.recent = true;
                        out.push(r);
                    }
                }
            }
            out.extend(all);
            out
        } else {
            rank(all, needle, true)
        };
        for row in rows {
            self.push_row(row);
        }
    }

    fn push_row(&mut self, row: GotoRow) {
        self.lines.push(GotoLine::Row(self.rows.len()));
        self.rows.push(row);
    }

    /// Replaces a section's rows with others, and repaints.
    ///
    /// This is the door for ASYNCHRONOUS sources: the semantic index asks
    /// the core, takes a while, and when it answers its section appears
    /// without touching the others. It replaces instead of adding because
    /// an old answer must not live alongside the new one — they are
    /// answers to different queries, and together they describe neither.
    ///
    /// The cursor stays WHERE IT IS if the line beneath it is still a row.
    /// Letting a late answer move the cursor is how an Enter ends up
    /// somewhere the reader did not choose: they typed, read, went to
    /// confirm, and the index arrived in between.
    pub fn replace_section(&mut self, section: GotoSection, rows: Vec<GotoRow>, ya_filtered: bool) {
        self.sources.retain(|s| s.section().id != section.id);
        self.sources.push(if ya_filtered {
            Box::new(FixedSource::ya_filtered(section, rows))
        } else {
            Box::new(FixedSource::new(section, rows))
        });
        let before = self.selected().cloned();
        self.refresh();
        if let Some(before) = before
            && let Some(i) = self.lines.iter().position(|l| match l {
                GotoLine::Row(i) => self.rows.get(*i) == Some(&before),
                GotoLine::Header(_) => false,
            })
        {
            self.cursor = i;
        }
    }

    /// The index of the first line that is a row, if there is one.
    fn first_row(&self) -> Option<usize> {
        self.lines
            .iter()
            .position(|l| matches!(l, GotoLine::Row(_)))
    }

    /// Types a character into the query.
    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.refresh();
    }

    /// Deletes the query's last character.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.refresh();
    }

    /// Replaces the whole query.
    pub fn set_query(&mut self, q: &str) {
        q.clone_into(&mut self.query);
        self.refresh();
    }

    /// Appends a paste to the query, with ONE refresh.
    pub fn push_str(&mut self, s: &str) {
        self.query.push_str(s);
        self.refresh();
    }

    /// The query as is.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The query for painting, with terminal hazards masked.
    #[must_use]
    pub fn query_display(&self) -> String {
        self.query
            .chars()
            .map(|c| {
                if norte_encoding::is_terminal_hazard(c) {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    }

    /// What the box is listing now.
    #[must_use]
    pub fn mode(&self) -> Mode {
        Mode::of(&self.query)
    }

    /// The Fluent key of the hint an empty places box shows, or `None`.
    #[must_use]
    pub fn hint(&self) -> Option<&'static str> {
        (self.mode() == Mode::Places && self.query.trim().is_empty()).then_some("goto-hint")
    }

    /// What to ask the semantic index, if anything: only a places query of
    /// [`MINIMUM_FOR_THE_INDEX`] chars or more that is not a typed path.
    #[must_use]
    pub fn index_query(&self) -> Option<&str> {
        (self.mode() == Mode::Places)
            .then(|| needle(&self.query))
            .filter(|q| q.chars().count() >= MINIMUM_FOR_THE_INDEX && looks_path(q).is_none())
    }

    /// Moves up to the previous row, skipping headers. Stops at the top.
    pub fn up(&mut self) {
        let mut i = self.cursor;
        while i > 0 {
            i -= 1;
            if matches!(self.lines.get(i), Some(GotoLine::Row(_))) {
                self.cursor = i;
                return;
            }
        }
    }

    /// Moves down to the next row, skipping headers. Stops at the bottom.
    pub fn down(&mut self) {
        let mut i = self.cursor;
        while i + 1 < self.lines.len() {
            i += 1;
            if matches!(self.lines.get(i), Some(GotoLine::Row(_))) {
                self.cursor = i;
                return;
            }
        }
    }

    /// Moves up `n` rows (at least one).
    pub fn page_up(&mut self, n: usize) {
        for _ in 0..n.max(1) {
            self.up();
        }
    }

    /// Moves down `n` rows (at least one).
    pub fn page_down(&mut self, n: usize) {
        for _ in 0..n.max(1) {
            self.down();
        }
    }

    /// Moves to the first row.
    pub fn home(&mut self) {
        if let Some(i) = self.first_row() {
            self.cursor = i;
        }
    }

    /// Moves to the last row.
    pub fn end(&mut self) {
        if let Some(i) = self
            .lines
            .iter()
            .rposition(|l| matches!(l, GotoLine::Row(_)))
        {
            self.cursor = i;
        }
    }

    /// Puts the cursor on `line` if it is a row (a click); says whether it did.
    pub fn point(&mut self, line: usize) -> bool {
        let is_row = matches!(self.lines.get(line), Some(GotoLine::Row(_)));
        if is_row {
            self.cursor = line;
        }
        is_row
    }

    /// What is painted, in order.
    #[must_use]
    pub fn lines(&self) -> &[GotoLine] {
        &self.lines
    }

    /// The rows, indexed by [`GotoLine::Row`].
    #[must_use]
    pub fn rows(&self) -> &[GotoRow] {
        &self.rows
    }

    /// Which line the cursor is on.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The row under the cursor, if the cursor is on one.
    #[must_use]
    pub fn selected(&self) -> Option<&GotoRow> {
        match self.lines.get(self.cursor) {
            Some(GotoLine::Row(i)) => self.rows.get(*i),
            _ => None,
        }
    }

    /// What confirming the selected row means: [`Action::Unavailable`] when
    /// the row is dimmed, otherwise what [`action`] reads from its `key`.
    #[must_use]
    pub fn confirm(&self) -> Option<Action> {
        let row = self.selected()?;
        Some(
            row.unavailable
                .clone()
                .map_or_else(|| action(&row.key), Action::Unavailable),
        )
    }

    /// How many rows are visible right now (headers not counted).
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether no row is visible.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// A row as a source gave it, with what only the model sets cleared.
fn unmarked(mut r: GotoRow) -> GotoRow {
    r.recent = false;
    r.positions.clear();
    r
}

/// Scores `rows` against `needle`, drops what does not match and sorts the
/// rest best first; ties go to the shorter text, then to arrival order
/// (`sort_by` is stable). `with_desc`: a row whose desc matches still counts,
/// at half that score — a word of a command's help line finds it, below a
/// command whose name matches.
fn rank(rows: Vec<GotoRow>, needle: &str, with_desc: bool) -> Vec<GotoRow> {
    let mut scored: Vec<(i32, usize, GotoRow)> = rows
        .into_iter()
        .filter_map(|mut r| {
            let on_text = crate::fuzzy::score(needle, &r.text);
            let on_desc = if with_desc {
                crate::fuzzy::score(needle, &r.desc).map(|m| m.score / 2)
            } else {
                None
            };
            let best = match (&on_text, on_desc) {
                (Some(m), Some(d)) => m.score.max(d),
                (Some(m), None) => m.score,
                (None, Some(d)) => d,
                (None, None) => return None,
            };
            r.positions = on_text.map(|m| m.positions).unwrap_or_default();
            r.recent = false;
            let len = r.text.chars().count();
            Some((best, len, r))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, r)| r).collect()
}

/// Whether what was TYPED looks like a path to go to, and not text to
/// search for.
///
/// Three shapes, and none of them can be confused with a query: an
/// absolute path (`/etc`), home's (`~` or `~/…`) and a URL with a scheme
/// (`sftp://host/…`). Everything else — `etc`, `documents` — is a query,
/// and a relative path is deliberately excluded: "where to" cannot depend
/// on which panel you were in, or the same key would lead to two different
/// places.
///
/// Returns the text AS TYPED. Expanding `~` and validating the scheme
/// belong to the caller, which is the one holding the `VPath` and knowing
/// whether that backend exists: here it is only decided whether to offer
/// the row.
#[must_use]
pub fn looks_path(query: &str) -> Option<&str> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    if q.starts_with('/') || q == "~" || q.starts_with("~/") {
        return Some(q);
    }
    // A scheme with `://` and something after it. `q.find` and not
    // `split_once` so as not to accept `://x`, which names no backend.
    let idx = q.find("://")?;
    (idx > 0 && q.len() > idx + 3).then_some(q)
}

// ---------------------------------------------------------------------------
// What each frontend used to decide on its own (#357): how each kind of row
// is built, what dispatch key it carries and what confirming it means. The
// TUI had it in `norte-tui/src/goto.rs`, and the window needed it just the
// same; two copies of "where does this Enter lead" are two answers.
// ---------------------------------------------------------------------------

/// Dispatch prefix of a row that leads to an already-known `VPath`.
pub const K_IR: &str = "go:";
/// Prefix of a row that leads to a catalogue command.
pub const K_CMD: &str = "cmd:";
/// Prefix of the TYPED PATH row, which still needs resolving. Set by
/// [`PathSource`].
pub const K_PATH: &str = "path:";
/// Prefix of a row that types something into the box (a mode prefix).
pub const K_SET: &str = "set:";
/// Prefix of a row that opens a help topic.
pub const K_HELP: &str = "help:";

/// How many rows are pulled from each long list BEFORE filtering.
///
/// Not the visible cap — that is [`CAP_PER_SECTION`], and the model
/// applies it to every section alike — but how many entries from a list of
/// hundreds are offered to the filter. Looser than the paint one on
/// purpose: filtering over forty finds things filtering over twelve would
/// not, and the model trims the leftovers afterward.
pub const BROUGHT_BY_LIST: usize = 40;

/// From how many characters onward the index gets asked.
///
/// With fewer, the answer cannot be good — one or two letters are not a
/// semantic query — and every question is a call to a provider that costs
/// time and can cost money.
pub const MINIMUM_FOR_THE_INDEX: usize = 3;

/// How many results are requested from the index.
pub const INDEX_CAP: u32 = 8;

/// A row toward a known directory.
///
/// `enc` is the focused panel's name reinterpretation, and is only passed
/// to paths that are OF that panel — its history. The rest (popular,
/// favorites, connections, index) get `None`: they belong to the whole
/// session, and applying one panel's encoding to another's paths invents
/// mojibake. It is the same rule `popular_rows` already wrote where it
/// lives.
#[must_use]
pub fn row_path(
    section: &'static str,
    name: Option<&str>,
    path: &norte_proto::VPath,
    enc: Option<norte_encoding::NameEncoding>,
) -> GotoRow {
    let (content, hostile_name) = crate::path_display_with(path, enc);
    // The name the reader gave it (a favorite, a connection) goes IN FRONT
    // and the path behind: it is searched for by the name one chose
    // oneself, and the path confirms it is the one believed to be.
    let (text, desc, hostile) = match name {
        Some(n) => {
            let (nt, nh) = crate::display_name(n.as_bytes());
            (nt, content, nh || hostile_name)
        }
        None => (content, String::new(), hostile_name),
    };
    GotoRow {
        section,
        key: format!("{K_IR}{}", path.to_wire()),
        text,
        desc,
        hostile,
        ..GotoRow::default()
    }
}

/// The row for a configured connection: its name and its RAW URL from
/// `connections.toml`, which may not parse.
///
/// It is offered anyway and said on confirm — the same thing
/// `pane.connect`'s picker does, with the same message — instead of
/// disappearing: "my connection doesn't show up in go to" is worse than an
/// error on pressing Enter, because it gives nowhere to look. A FAVORITE
/// that does not parse, on the other hand, is not offered: the places list
/// already shows it with its error.
#[must_use]
pub fn row_connection(name: &str, url: &str) -> GotoRow {
    let (content, hostile_name) = norte_proto::VPath::parse(url).map_or_else(
        |_| (norte_encoding::mask_terminal_hazards(url), true),
        |p| crate::path_display_with(&p, None),
    );
    let (nt, nh) = crate::display_name(name.as_bytes());
    GotoRow {
        section: SECTION_CONNECTIONS.id,
        key: format!("{K_IR}{url}"),
        text: nt,
        desc: content,
        hostile: nh || hostile_name,
        ..GotoRow::default()
    }
}

/// The category a command is filed under: its namespace (`pane` in
/// `pane.copy`), translated. `None` for an id without one.
#[must_use]
pub fn category(cmd: &str, lang: norte_i18n::Lang) -> Option<String> {
    let (ns, _) = cmd.split_once('.')?;
    Some(norte_i18n::t_in(lang, &format!("cmd-ns-{ns}")))
}

/// The COMMAND rows: the SAME rows the frontend's palette builds, as go-to
/// rows, because two command lists computed separately diverge. `facts` dims
/// what cannot run here, `None` dims nothing.
#[must_use]
pub fn command_rows(
    rows: Vec<crate::palette::Row>,
    facts: Option<&crate::availability::Facts>,
    lang: norte_i18n::Lang,
) -> Vec<GotoRow> {
    rows.into_iter()
        .map(|r| {
            let unavailable = facts
                .and_then(|f| crate::availability::verdict(&r.key, f).reason())
                .map(|why| norte_i18n::t_in(lang, crate::availability::reason_key(why)));
            GotoRow {
                section: SECTION_COMMANDS.id,
                category: category(&r.key, lang),
                chord: (r.chord != "—").then_some(r.chord),
                unavailable,
                key: format!("{K_CMD}{}", r.key),
                // The human label is the name, the id goes dim behind it
                // (spec 2026-09-10's reading order).
                text: r.desc,
                desc: r.text,
                hostile: r.hostile,
                ..GotoRow::default()
            }
        })
        .collect()
}

/// The PLUGIN command rows (extensions, renamers, organizers): no category
/// and never dimmed — the daemon decides whether they run. Same `key` shape
/// as [`command_rows`], so confirming one is the same [`Action::Command`].
#[must_use]
pub fn plugin_command_rows(rows: Vec<crate::palette::Row>) -> Vec<GotoRow> {
    rows.into_iter()
        .map(|r| GotoRow {
            section: SECTION_PLUGINS.id,
            key: format!("{K_CMD}{}", r.key),
            text: r.text,
            desc: r.desc,
            hostile: r.hostile,
            ..GotoRow::default()
        })
        .collect()
}

/// The rows for a batch of results from the semantic index.
///
/// They are file PATHS the core found, so their name is disk bytes and is
/// painted through the same masked path as the others. No
/// reinterpretation, like the popular ones: what the index returns can be
/// anywhere, not in the focused panel.
#[must_use]
pub fn index_rows(hits: &[norte_proto::methods::SemanticHit]) -> Vec<GotoRow> {
    hits.iter()
        .map(|h| row_path(SECTION_INDEX.id, None, &h.path, None))
        .collect()
}

/// What confirming a row means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Navigate the focused panel to this directory.
    Ir(norte_proto::VPath),
    /// Run this catalogue command, as if its key had been pressed.
    Command(String),
    /// The row leads nowhere resolvable — a typed path that does not
    /// parse, above all. Carries the message's key.
    Nothing(&'static str),
    /// A command that cannot run here: the reason, already translated (the
    /// row's `unavailable`).
    Unavailable(String),
    /// Type this into the box and stay open (a prefix row).
    SetQuery(String),
    /// Open this help topic.
    Help(String),
}

/// What to do with the row the reader just confirmed.
///
/// The `key` is NEVER painted and this is where it is read: the prefix
/// says which class the row is, and each class resolves along its own
/// path. A TYPED path is the only one that can fail to resolve, because it
/// is the only one that did not come out of a list that already existed.
///
/// ```
/// use norte_frontend::goto::{Action, action};
/// assert_eq!(action("cmd:app.quit"), Action::Command("app.quit".to_owned()));
/// assert!(matches!(action("go:file:///tmp"), Action::Ir(_)));
/// assert!(matches!(action("other cosa"), Action::Nothing(_)));
/// ```
#[must_use]
pub fn action(key: &str) -> Action {
    if let Some(cmd) = key.strip_prefix(K_CMD) {
        return Action::Command(cmd.to_owned());
    }
    if let Some(wire) = key.strip_prefix(K_IR) {
        return norte_proto::VPath::parse(wire)
            .map_or(Action::Nothing("msg-goto-bad-path"), Action::Ir);
    }
    if let Some(content) = key.strip_prefix(K_PATH) {
        return resolver_typed(content);
    }
    if let Some(q) = key.strip_prefix(K_SET) {
        return Action::SetQuery(q.to_owned());
    }
    if let Some(id) = key.strip_prefix(K_HELP) {
        return Action::Help(id.to_owned());
    }
    Action::Nothing("msg-goto-bad-path")
}

/// The `?` list: the prefixes first (what typing each one does), then every
/// help topic, matched on its title and, with nothing typed, in corpus order
/// (the corpus is written in reading order; see `norte_help::topic_ids`).
/// Never dimmed: only command rows are.
#[must_use]
pub fn help_rows(lang: norte_i18n::Lang) -> Vec<GotoRow> {
    let prefix = |p: &str, label_key: &str| GotoRow {
        section: SECTION_HELP.id,
        key: format!("{K_SET}{p}"),
        text: if p.is_empty() {
            norte_i18n::t_in(lang, label_key)
        } else {
            format!("{p} {}", norte_i18n::t_in(lang, label_key))
        },
        ..GotoRow::default()
    };
    let mut out = vec![
        prefix(PREFIX_COMMANDS, "goto-prefix-commands"),
        prefix("", "goto-prefix-places"),
    ];
    out.extend(norte_help::topics(lang).iter().map(|t| GotoRow {
        section: SECTION_HELP.id,
        key: format!("{K_HELP}{}", t.id.as_str()),
        // The corpus is this project's own text: nothing to mask.
        text: t.title.clone(),
        ..GotoRow::default()
    }));
    out
}

/// The help page a row leads to (`F1`): a topic row's own, or a built-in
/// command's. `None` for anything else.
#[must_use]
pub fn help_topic(key: &str, lang: norte_i18n::Lang) -> Option<&'static norte_help::Topic> {
    if let Some(id) = key.strip_prefix(K_HELP) {
        return norte_help::topic(lang, id);
    }
    let cmd = key.strip_prefix(K_CMD)?;
    // A plugin's id half comes from a manifest with no charset: never a page
    // (`palette_help_target`'s reason).
    if crate::palette::parse_plugin_key(cmd).is_some()
        || crate::palette::parse_renamer_key(cmd).is_some()
        || crate::palette::parse_organizer_key(cmd).is_some()
    {
        return None;
    }
    norte_help::topic_for_command(lang, cmd)
}

/// Resolves the path the reader typed.
///
/// `~` expands against THIS PROCESS's HOME, not the panel's directory:
/// "home" is one single place, and making it depend on where you were
/// would mean the same key leads to two places. An absolute path is taken
/// as local, and one with a scheme is parsed as is — if the backend does
/// not exist, the core says so, which beats navigating to something other
/// than what was typed.
fn resolver_typed(content: &str) -> Action {
    let expanded = if content == "~" || content.starts_with("~/") {
        let Some(home) = std::env::var_os("HOME") else {
            return Action::Nothing("msg-goto-no-home");
        };
        let mut p = std::path::PathBuf::from(home);
        if let Some(rest) = content.strip_prefix("~/") {
            p.push(rest);
        }
        p
    } else if typed_local(content) {
        std::path::PathBuf::from(content)
    } else {
        // With a scheme: the wire is already a wire.
        return norte_proto::VPath::parse(content)
            .map_or(Action::Nothing("msg-goto-bad-path"), Action::Ir);
    };
    norte_vfs::native::vpath_from_native(&expanded)
        .map_or(Action::Nothing("msg-goto-bad-path"), Action::Ir)
}

/// A typed LOCAL path: `/…` everywhere (on Windows, the current drive), or
/// an absolute one on a drive or share. `\\.\pipe\x`, `\\.\COM1` and a
/// non-disk `\\?\` name devices, not folders, and stay unparsed.
fn typed_local(content: &str) -> bool {
    use std::path::{Component, Path, Prefix};
    if content.starts_with('/') {
        return true;
    }
    let path = Path::new(content);
    path.is_absolute()
        && !matches!(
            path.components().next(),
            Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::DeviceNS(_) | Prefix::Verbatim(_))
        )
}

#[cfg(test)]
mod dispatch_tests {
    use super::{Action, K_CMD, K_IR, K_PATH, action};
    use norte_proto::VPath;

    /// Each `key` prefix goes its own way, and none is confused with
    /// another: the `key` is never painted, so this is the only thing that
    /// decides where an Enter leads.
    #[test]
    fn every_prefix_resolves_to_its_own() {
        assert_eq!(
            action(&format!("{K_CMD}app.quit")),
            Action::Command("app.quit".to_owned())
        );
        assert_eq!(
            action(&format!("{K_IR}file:///tmp")),
            Action::Ir(VPath::parse("file:///tmp").expect("wire"))
        );
        // A native absolute path: `/tmp` has no drive on Windows.
        let (typed, wire) = if cfg!(windows) {
            (r"C:\tmp", "file:///C:/tmp")
        } else {
            ("/tmp", "file:///tmp")
        };
        assert_eq!(
            action(&format!("{K_PATH}{typed}")),
            Action::Ir(VPath::parse(wire).expect("wire"))
        );
    }

    /// A device or pipe is not a folder to go to (encoding audit).
    #[cfg(windows)]
    #[test]
    fn a_typed_device_is_not_a_place() {
        for device in [r"\\.\pipe\x", r"\\.\COM1", r"\\?\pipe\x"] {
            assert_eq!(
                action(&format!("{K_PATH}{device}")),
                Action::Nothing("msg-goto-bad-path"),
                "{device:?}"
            );
        }
    }

    /// A typed path with a scheme parses as is; one that does NOT parse is
    /// said, instead of navigating to anything else. An unknown scheme
    /// DOES parse: it is the core that says nobody can serve it.
    #[test]
    fn a_typed_path_that_does_not_parse_is_reported() {
        assert!(
            matches!(action(&format!("{K_PATH}sftp://h//x")), Action::Nothing(_)),
            "an empty segment is not a path"
        );
        assert!(matches!(action("otra cosa"), Action::Nothing(_)));
        assert!(matches!(
            action(&format!("{K_PATH}noexiste://h/x")),
            Action::Ir(_)
        ));
    }

    /// `~` expands against the process's HOME, not the panel: home is a
    /// single place.
    #[test]
    fn home_does_not_depend_on_the_pane() {
        let Action::Ir(p) = action(&format!("{K_PATH}~")) else {
            panic!("`~` has to resolve as long as there is a HOME");
        };
        let home = std::env::var("HOME").expect("HOME in the test environment");
        assert_eq!(
            p,
            norte_vfs::native::vpath_from_native(std::path::Path::new(&home))
                .expect("HOME is a path")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CAP_PER_SECTION, Goto, GotoLine, GotoRow, GotoSection, GotoSource, Mode, SECTION_COMMANDS,
        SECTION_FAVORITES, SECTION_HISTORY, SECTION_INDEX, SECTION_PLUGINS, looks_path, needle,
    };

    /// A fake source with fixed rows.
    struct Fixed {
        section: GotoSection,
        texts: Vec<&'static str>,
        ya_filtered: bool,
    }

    impl GotoSource for Fixed {
        fn section(&self) -> GotoSection {
            self.section
        }
        fn rows(&self, _query: &str) -> Vec<GotoRow> {
            self.texts
                .iter()
                .map(|t| GotoRow {
                    section: self.section.id,
                    key: (*t).to_owned(),
                    text: (*t).to_owned(),
                    ..GotoRow::default()
                })
                .collect()
        }
        fn ya_filtered(&self) -> bool {
            self.ya_filtered
        }
    }

    fn source(section: GotoSection, texts: &[&'static str]) -> Box<dyn GotoSource + Send> {
        Box::new(Fixed {
            section,
            texts: texts.to_vec(),
            ya_filtered: false,
        })
    }

    /// Sections come out in [`ORDEN`]'s order, not the order the sources
    /// were registered in: what is learned is where each thing is.
    #[test]
    fn sections_come_out_in_fixed_order() {
        let goto = Goto::new(vec![
            source(SECTION_FAVORITES, &["quitar"]),
            source(SECTION_HISTORY, &["/etc"]),
        ]);
        let headers: Vec<&str> = goto
            .lines()
            .iter()
            .filter_map(|l| match l {
                GotoLine::Header(s) => Some(s.id),
                GotoLine::Row(_) => None,
            })
            .collect();
        assert_eq!(headers, vec!["history", "favorites"]);
    }

    /// A section with no rows paints no header: an empty header says
    /// there is something where there is nothing.
    #[test]
    fn an_empty_section_paints_no_header() {
        let mut goto = Goto::new(vec![
            source(SECTION_FAVORITES, &["quitar"]),
            source(SECTION_HISTORY, &["/etc"]),
        ]);
        for c in "quit".chars() {
            goto.push_char(c);
        }
        let headers: Vec<&str> = goto
            .lines()
            .iter()
            .filter_map(|l| match l {
                GotoLine::Header(s) => Some(s.id),
                GotoLine::Row(_) => None,
            })
            .collect();
        assert_eq!(headers, vec!["favorites"], "/etc does not match \"quit\"");
    }

    /// The cursor never lands on a header, whether going down or up.
    #[test]
    fn the_cursor_skips_the_headers() {
        let mut goto = Goto::new(vec![
            source(SECTION_HISTORY, &["/etc", "/var"]),
            source(SECTION_FAVORITES, &["quitar"]),
        ]);
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(goto.selected().map(|r| r.text.clone()));
            goto.down();
        }
        assert_eq!(
            seen,
            vec![
                Some("/etc".to_owned()),
                Some("/var".to_owned()),
                Some("quitar".to_owned()),
                Some("quitar".to_owned()),
                Some("quitar".to_owned()),
            ],
            "goes down row by row and stops at the last one, without falling into the header"
        );
        for _ in 0..5 {
            goto.up();
        }
        assert_eq!(goto.selected().map(|r| r.text.as_str()), Some("/etc"));
    }

    /// A source that already filtered — the semantic index — does not go
    /// through the subsequence again: its results match by meaning, and
    /// the query's letters may not be in the name.
    #[test]
    fn an_already_filtered_source_is_not_filtered_again() {
        let index = Box::new(Fixed {
            section: SECTION_INDEX,
            texts: vec!["la factura del gas"],
            ya_filtered: true,
        });
        let mut goto = Goto::new(vec![index, source(SECTION_HISTORY, &["/etc"])]);
        for c in "recibo".chars() {
            goto.push_char(c);
        }
        assert_eq!(goto.len(), 1, "the index's survives, history's does not");
        assert_eq!(
            goto.rows().first().map(|r| r.text.as_str()),
            Some("la factura del gas")
        );
    }

    /// Typing moves the cursor to the first row of what is NOW visible.
    /// Leaving it where it was is how an Enter goes somewhere nobody read.
    #[test]
    fn typing_returns_the_cursor_up() {
        let mut goto = Goto::new(vec![source(SECTION_HISTORY, &["/etc", "/var"])]);
        goto.down();
        assert_eq!(goto.selected().map(|r| r.text.as_str()), Some("/var"));
        goto.push_char('e');
        assert_eq!(goto.selected().map(|r| r.text.as_str()), Some("/etc"));
    }

    /// A late answer from the index does not move the cursor out from
    /// under the reader's finger: they typed, read and went to confirm,
    /// and a new section arrived in between.
    #[test]
    fn a_section_that_arrives_late_does_not_move_the_cursor() {
        let mut goto = Goto::new(vec![source(SECTION_HISTORY, &["/etc", "/var"])]);
        goto.down();
        assert_eq!(goto.selected().map(|r| r.text.as_str()), Some("/var"));

        goto.replace_section(
            SECTION_INDEX,
            vec![GotoRow {
                section: SECTION_INDEX.id,
                key: "x".to_owned(),
                text: "lo que encontró el índice".to_owned(),
                ..GotoRow::default()
            }],
            true,
        );

        assert_eq!(
            goto.selected().map(|r| r.text.as_str()),
            Some("/var"),
            "the cursor stays on what the reader was looking at"
        );
        assert_eq!(goto.len(), 3, "and the new section is there");
    }

    /// And a new answer REPLACES the previous one: two answers to
    /// different queries together describe neither.
    #[test]
    fn an_async_section_replaces_it_does_not_accumulate() {
        let mut goto = Goto::new(vec![source(SECTION_HISTORY, &["/etc"])]);
        for text in ["primera", "segunda"] {
            goto.replace_section(
                SECTION_INDEX,
                vec![GotoRow {
                    section: SECTION_INDEX.id,
                    key: text.to_owned(),
                    text: text.to_owned(),
                    ..GotoRow::default()
                }],
                true,
            );
        }
        let from_index: Vec<&str> = goto
            .rows()
            .iter()
            .filter(|r| r.section == SECTION_INDEX.id)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(from_index, vec!["segunda"]);
    }

    fn row(section: GotoSection, key: &str, text: &str, desc: &str) -> GotoRow {
        GotoRow {
            section: section.id,
            key: key.to_owned(),
            text: text.to_owned(),
            desc: desc.to_owned(),
            ..GotoRow::default()
        }
    }

    fn fixed(section: GotoSection, rows: Vec<GotoRow>) -> Box<dyn GotoSource + Send> {
        Box::new(super::FixedSource::new(section, rows))
    }

    fn commands(texts: &[&str]) -> Box<dyn GotoSource + Send> {
        fixed(
            SECTION_COMMANDS,
            texts
                .iter()
                .map(|t| row(SECTION_COMMANDS, &format!("cmd:{t}"), t, ""))
                .collect(),
        )
    }

    fn texts(goto: &Goto) -> Vec<&str> {
        goto.lines()
            .iter()
            .filter_map(|l| match l {
                GotoLine::Row(i) => goto.rows().get(*i).map(|r| r.text.as_str()),
                GotoLine::Header(_) => None,
            })
            .collect()
    }

    /// The mode is the query's FIRST character; a `>` later is a letter.
    #[test]
    fn the_first_character_chooses_the_mode() {
        assert_eq!(Mode::of(">copy"), Mode::Commands);
        assert_eq!(Mode::of("?"), Mode::Help);
        assert_eq!(Mode::of(""), Mode::Places);
        assert_eq!(Mode::of(" >x"), Mode::Places);
        assert_eq!(Mode::of("a>b"), Mode::Places);
        assert_eq!(needle(">  copy"), "copy");
        assert_eq!(needle("  etc"), "etc");
        assert_eq!(needle("?"), "");
    }

    /// Backspace over the `>` switches to places without closing, and places
    /// has no commands: they live behind the prefix.
    #[test]
    fn backspace_over_the_prefix_switches_to_places() {
        let mut goto = Goto::new(vec![
            commands(&["app.quit"]),
            source(SECTION_HISTORY, &["/etc"]),
        ]);
        goto.set_query(">");
        assert_eq!(goto.mode(), Mode::Commands);
        assert_eq!(texts(&goto), vec!["app.quit"]);
        assert!(
            goto.lines().iter().all(|l| matches!(l, GotoLine::Row(_))),
            "one flat list"
        );
        goto.backspace();
        assert_eq!(goto.mode(), Mode::Places);
        assert_eq!(texts(&goto), vec!["/etc"]);
        goto.push_char('q');
        assert!(
            texts(&goto).is_empty(),
            "no commands in places, even typing"
        );
    }

    /// The empty places box says where the other lists are.
    #[test]
    fn an_empty_places_box_shows_the_prefix_hint() {
        let mut goto = Goto::new(vec![source(SECTION_HISTORY, &["/etc"])]);
        assert_eq!(goto.hint(), Some("goto-hint"));
        goto.push_char('e');
        assert_eq!(goto.hint(), None);
        goto.set_query(">");
        assert_eq!(goto.hint(), None);
    }

    /// Ranked by score inside a section, with the matched chars; ties go to
    /// the shorter text.
    #[test]
    fn places_rank_inside_their_section() {
        let mut goto = Goto::new(vec![source(
            SECTION_HISTORY,
            &["/srv/capable", "/srv/copy path"],
        )]);
        goto.set_query("pa");
        assert_eq!(texts(&goto), vec!["/srv/copy path", "/srv/capable"]);
        assert_eq!(
            goto.selected().map(|r| r.positions.clone()),
            Some(vec![10, 11])
        );
        let mut goto = Goto::new(vec![commands(&["copy path", "copy"])]);
        goto.set_query(">copy");
        assert_eq!(
            texts(&goto),
            vec!["copy", "copy path"],
            "a tie goes to the shorter"
        );
    }

    /// The help line still finds a command, below one whose name matches.
    #[test]
    fn a_command_is_found_by_its_desc_too() {
        let mut goto = Goto::new(vec![fixed(
            SECTION_COMMANDS,
            vec![
                row(
                    SECTION_COMMANDS,
                    "cmd:layout.split-h",
                    "split side by side",
                    "layout.split-h",
                ),
                row(SECTION_COMMANDS, "cmd:x", "split-h here", "x"),
            ],
        )]);
        goto.set_query(">split-h");
        assert_eq!(texts(&goto), vec!["split-h here", "split side by side"]);
        assert!(
            goto.rows()[1].positions.is_empty(),
            "matched by desc: nothing to mark in the text"
        );
    }

    /// With nothing typed after `>`, the recent ones go first, in their
    /// order, and say so; a recent key with no row paints nothing; typing
    /// returns to the ranked order and nothing is "recent".
    #[test]
    fn recents_go_first_only_with_an_empty_command_query() {
        let mut goto = Goto::new(vec![commands(&["app.quit", "app.help"])])
            .with_recent(&["app.help".to_owned(), "plugin:gone:x".to_owned()]);
        goto.set_query(">");
        assert_eq!(texts(&goto), vec!["app.help", "app.quit"]);
        assert!(goto.rows()[0].recent && !goto.rows()[1].recent);
        goto.push_char('q');
        assert!(goto.rows().iter().all(|r| !r.recent));
        assert_eq!(goto.selected().map(|r| r.text.as_str()), Some("app.quit"));
    }

    /// Plugin rows arriving late join the commands list without losing the
    /// query or the row under the cursor (Review Focus 3).
    #[test]
    fn late_plugin_rows_keep_the_query_and_the_cursor() {
        let mut goto = Goto::new(vec![commands(&["a1", "a2"])]);
        goto.set_query(">a");
        goto.down();
        assert_eq!(goto.selected().map(|r| r.text.as_str()), Some("a2"));
        goto.replace_section(
            SECTION_PLUGINS,
            vec![row(SECTION_PLUGINS, "cmd:plugin:p:c", "[ext] a3", "")],
            false,
        );
        assert_eq!(goto.query(), ">a");
        assert_eq!(goto.selected().map(|r| r.text.as_str()), Some("a2"));
        assert_eq!(goto.len(), 3);
    }

    #[test]
    fn paging_home_end_and_pointing() {
        let names: Vec<String> = (0..30).map(|i| format!("c{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut goto = Goto::new(vec![commands(&refs)]);
        goto.set_query(">");
        goto.page_down(10);
        assert_eq!(goto.cursor(), 10);
        goto.page_up(3);
        assert_eq!(goto.cursor(), 7);
        goto.end();
        assert_eq!(goto.cursor(), 29);
        goto.home();
        assert_eq!(goto.cursor(), 0);
        assert!(goto.point(5));
        assert_eq!(goto.cursor(), 5);
        assert!(!goto.point(99));
        let mut places = Goto::new(vec![source(SECTION_HISTORY, &["/a"])]);
        assert!(!places.point(0), "a header is not a row");
        assert_eq!(places.cursor(), 1);
    }

    /// Only a places query of three chars that is not a path goes to the
    /// semantic index; a command or help query never does (Review Focus 1).
    #[test]
    fn only_a_places_query_reaches_the_index() {
        let mut goto = Goto::new(Vec::new());
        for (q, want) in [
            (">copy", None),
            ("?copy", None),
            ("fa", None),
            ("/etc", None),
            ("factura", Some("factura")),
            ("  factura", Some("factura")),
        ] {
            goto.set_query(q);
            assert_eq!(goto.index_query(), want, "{q:?}");
        }
    }

    /// A paste is one refresh, and a hostile query paints masked.
    #[test]
    fn push_str_and_a_masked_query() {
        let mut goto = Goto::new(vec![commands(&["copy"])]);
        goto.push_str(">co");
        assert_eq!(texts(&goto), vec!["copy"]);
        goto.set_query("a\u{202E}b");
        assert!(
            !goto
                .query_display()
                .chars()
                .any(norte_encoding::is_terminal_hazard)
        );
    }

    /// No section goes past [`CAP_PER_SECTION`] rows: a longer list is
    /// not read, and its own screens exist for skimming it whole.
    #[test]
    fn no_section_exceeds_the_cap() {
        let many: Vec<&'static str> = vec!["/x"; CAP_PER_SECTION * 3];
        let goto = Goto::new(vec![source(SECTION_HISTORY, &many)]);
        assert_eq!(goto.len(), CAP_PER_SECTION);
    }

    /// The three shapes that ARE a path, and the ones that are not.
    #[test]
    fn what_counts_as_a_typed_path() {
        assert_eq!(looks_path("/etc"), Some("/etc"));
        assert_eq!(looks_path("~"), Some("~"));
        assert_eq!(looks_path("~/notas"), Some("~/notas"));
        assert_eq!(looks_path("sftp://host/tmp"), Some("sftp://host/tmp"));
        assert_eq!(looks_path("  /etc  "), Some("/etc"), "gets trimmed");

        assert_eq!(looks_path(""), None);
        assert_eq!(looks_path("etc"), None, "relative: not known from where");
        assert_eq!(looks_path("~notas"), None, "nobody's home");
        assert_eq!(looks_path("://x"), None, "no scheme, no backend");
        assert_eq!(looks_path("sftp://"), None, "no destination, nowhere to go");
    }
}

#[cfg(test)]
mod command_row_tests {
    use super::{
        Action, Goto, SECTION_COMMANDS, SECTION_PLUGINS, category, command_rows,
        plugin_command_rows,
    };
    use crate::availability::Facts;
    use norte_i18n::Lang;

    fn prow(key: &str, text: &str, desc: &str, chord: &str) -> crate::palette::Row {
        crate::palette::Row {
            key: key.to_owned(),
            text: text.to_owned(),
            desc: desc.to_owned(),
            chord: chord.to_owned(),
            hostile: false,
        }
    }

    fn in_a_zip() -> Facts {
        Facts {
            enterable: false,
            viewable: true,
            rename_single: true,
            source_read_only: true,
            dest_read_only: false,
            degraded: false,
            journalled: true,
            daemon: true,
            windowed: true,
        }
    }

    #[test]
    fn a_command_row_carries_its_category_chord_and_label() {
        let rows = command_rows(
            vec![
                prow(
                    "pane.copy",
                    "pane.copy",
                    "copy selection to the other pane",
                    "F5",
                ),
                prow("app.agents", "app.agents", "agents", "—"),
            ],
            None,
            Lang::En,
        );
        assert_eq!(rows[0].section, SECTION_COMMANDS.id);
        assert_eq!(rows[0].key, "cmd:pane.copy");
        assert_eq!(
            rows[0].text, "copy selection to the other pane",
            "the human label is the name"
        );
        assert_eq!(
            rows[0].desc, "pane.copy",
            "the id goes dim, still searchable"
        );
        assert_eq!(rows[0].category.as_deref(), Some("Panel"));
        assert_eq!(rows[0].chord.as_deref(), Some("F5"));
        assert_eq!(rows[1].chord, None, "no key in this preset: no chord");
        assert_eq!(rows[0].unavailable, None, "no facts: nothing is dimmed");
    }

    /// Inside a zip, deleting cannot run: the row says why, translated, and
    /// confirming it refuses with that reason instead of running.
    #[test]
    fn an_unavailable_command_is_dimmed_and_refuses() {
        let rows = command_rows(
            vec![
                prow("pane.delete", "pane.delete", "delete", "F8"),
                prow("pane.copy", "pane.copy", "copy", "F5"),
            ],
            Some(&in_a_zip()),
            Lang::En,
        );
        let why = norte_i18n::t_in(Lang::En, "reason-read-only");
        assert_eq!(rows[0].unavailable.as_deref(), Some(why.as_str()));
        assert_eq!(rows[1].unavailable, None, "reading from the zip is fine");
        let mut goto = Goto::new(vec![Box::new(super::FixedSource::new(
            SECTION_COMMANDS,
            rows,
        ))]);
        goto.set_query(">");
        assert_eq!(goto.confirm(), Some(Action::Unavailable(why)));
        goto.down();
        assert_eq!(
            goto.confirm(),
            Some(Action::Command("pane.copy".to_owned()))
        );
    }

    /// A plugin row has no category and is never dimmed: the daemon is the
    /// one that decides whether it runs.
    #[test]
    fn plugin_rows_have_no_category_and_are_never_dimmed() {
        let rows = plugin_command_rows(vec![prow(
            "plugin:org.x:greet",
            "[extension] Greet",
            "says hi",
            "—",
        )]);
        assert_eq!(rows[0].section, SECTION_PLUGINS.id);
        assert_eq!(rows[0].key, "cmd:plugin:org.x:greet");
        assert_eq!(rows[0].text, "[extension] Greet");
        assert_eq!(
            (rows[0].category.as_deref(), rows[0].unavailable.as_deref()),
            (None, None)
        );
    }

    /// Every namespace in the catalogue has its category, in both languages:
    /// a new namespace without one would paint `cmd-ns-…` literally.
    #[test]
    fn every_catalogue_namespace_has_a_category_in_both_languages() {
        for lang in [Lang::En, Lang::Es] {
            let ids = norte_i18n::message_ids(lang);
            for def in crate::keymap::catalogue::CATALOGUE {
                let ns = def.name.split_once('.').map_or(def.name, |(ns, _)| ns);
                let key = format!("cmd-ns-{ns}");
                assert!(ids.contains(&key), "{key} missing in {lang:?}");
            }
        }
        assert_eq!(
            category("nothing", Lang::En),
            None,
            "no namespace, no category"
        );
    }
}

#[cfg(test)]
mod help_row_tests {
    use super::{Action, Goto, SECTION_HELP, action, help_rows, help_topic};
    use norte_i18n::Lang;

    /// `?` lists the prefixes first, then every topic; choosing a prefix
    /// types it, choosing a topic opens it.
    #[test]
    fn the_help_list_is_prefixes_then_topics() {
        let rows = help_rows(Lang::En);
        assert_eq!(rows[0].key, "set:>");
        assert_eq!(rows[1].key, "set:");
        let topics = norte_help::topics(Lang::En);
        assert_eq!(rows.len(), 2 + topics.len());
        assert_eq!(rows[2].key, format!("help:{}", topics[0].id.as_str()));
        assert_eq!(rows[2].text, topics[0].title);
        assert!(rows.iter().all(|r| r.section == SECTION_HELP.id));
        assert_eq!(action("set:>"), Action::SetQuery(">".to_owned()));
        assert_eq!(action("help:copying"), Action::Help("copying".to_owned()));

        let mut goto = Goto::new(vec![Box::new(super::FixedSource::new(SECTION_HELP, rows))]);
        goto.set_query("?");
        assert_eq!(goto.confirm(), Some(Action::SetQuery(">".to_owned())));
        goto.set_query("?copying");
        assert!(goto.rows().iter().any(|r| r.key == "help:copying"));
    }

    /// `F1`'s target: a built-in command's page, a topic row's page, never a
    /// plugin's (its id half comes from a manifest with no charset).
    #[test]
    fn the_help_topic_of_a_row() {
        assert_eq!(
            help_topic("cmd:pane.copy", Lang::En).map(|t| t.id.as_str()),
            norte_help::topic_for_command(Lang::En, "pane.copy").map(|t| t.id.as_str())
        );
        assert!(help_topic("cmd:pane.copy", Lang::En).is_some());
        assert_eq!(
            help_topic("help:copying", Lang::En).map(|t| t.id.as_str()),
            Some("copying")
        );
        assert!(help_topic("cmd:plugin:org.x:greet", Lang::En).is_none());
        assert!(help_topic("cmd:renamer:org.x:r", Lang::En).is_none());
        assert!(help_topic("go:file:///tmp", Lang::En).is_none());
    }
}
