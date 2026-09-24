//! "Go to anywhere" (phase 6 of the WOW program): a single screen that
//! brings together what used to live in five — the command palette,
//! history, popular places, favorites and connections — and adds what had
//! no home: a TYPED path and, once it arrives, what the semantic index
//! found.
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

use crate::palette_state::is_subsequence;

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

/// The catalogue's commands — the usual palette, here as one more section.
pub const SECTION_COMMANDS: GotoSection = GotoSection {
    id: "commands",
    title_key: "goto-section-commands",
};

/// What the semantic index found. Arrives LATE (it is a question to the
/// core, not a list in memory) and that is why it goes last: a section
/// that appears mid-typing must not push down what the reader was already
/// looking at.
pub const SECTION_INDEX: GotoSection = GotoSection {
    id: "index",
    title_key: "goto-section-index",
};

/// The ORDER the sections are painted in, and the only place it lives.
///
/// Fixed and not configurable: it is the order a reader searches in — what
/// they just typed, where they have been, what they saved, what they can
/// do — and a list that reorders itself is a list where nothing can be
/// learned about where anything is.
pub const ORDEN: &[GotoSection] = &[
    SECTION_PATH,
    SECTION_HISTORY,
    SECTION_POPULAR,
    SECTION_FAVORITES,
    SECTION_CONNECTIONS,
    SECTION_COMMANDS,
    SECTION_INDEX,
];

/// A "go to" row.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// A source of "go to" rows.
///
/// The contract is short on purpose: give the rows it contributes for a
/// query. Everything else — filtering by subsequence, putting up the
/// header, ordering the sections, moving the cursor — belongs to the
/// model, so a new source does not have to get any of those four things
/// right.
///
/// `query` is passed in case the source knows how to filter BETTER than the
/// generic subsequence (the semantic index asks the core with it, and the
/// typed path IS the query). A source with nothing special to do can
/// return everything: [`Goto::refresh`] filters afterward, so filtering
/// is a single thing for all of them.
pub trait GotoSource {
    /// The section its rows fall into.
    fn section(&self) -> GotoSection;
    /// The rows it contributes for `query`.
    fn rows(&self, query: &str) -> Vec<GotoRow>;
    /// Whether its rows ALREADY come filtered and the model should not run
    /// the subsequence over them again.
    ///
    /// `false` by default, which is what an in-memory list wants. The
    /// semantic index sets it to `true`: it asked the core with the whole
    /// query and its results match by MEANING, not by letters — a
    /// subsequence filter on top would throw away exactly what makes it
    /// useful.
    fn ya_filtered(&self) -> bool {
        false
    }

    /// Whether this source only contributes rows when something has been
    /// typed.
    ///
    /// `false` by default. The COMMANDS section sets it to `true`: with an
    /// empty query that is hundreds of rows burying the four destination
    /// lists, and whoever opens "go to" without typing anything is asking
    /// where they can go, not what verbs exist. As soon as they type
    /// something they come back, and the whole catalogue still lives in
    /// the palette, which is its own screen.
    fn only_with_query(&self) -> bool {
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
    only_with_query: bool,
}

impl FixedSource {
    /// A source of fixed rows in `section`.
    #[must_use]
    pub fn new(section: GotoSection, rows: Vec<GotoRow>) -> Self {
        Self {
            section,
            rows,
            ya_filtered: false,
            only_with_query: false,
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
            only_with_query: false,
        }
        .with_already_filtered()
    }

    /// Declares that this source only contributes with something typed
    /// (see [`GotoSource::only_with_query`]).
    #[must_use]
    pub fn only_with_query(mut self) -> Self {
        self.only_with_query = true;
        self
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
    fn only_with_query(&self) -> bool {
        self.only_with_query
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
                hostile: false,
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
        };
        goto.refresh();
        goto
    }

    /// Asks every source again and rebuilds what is visible.
    ///
    /// The cursor stays on the FIRST row: the query changed, so what was
    /// under the cursor is probably gone, and leaving it where it was is
    /// how an Enter ends up going somewhere the reader never got to read.
    pub fn refresh(&mut self) {
        let q = self.query.to_lowercase();
        self.rows.clear();
        self.lines.clear();
        for section in ORDEN {
            let mut from_this: Vec<GotoRow> = Vec::new();
            for source in &self.sources {
                if source.section().id != section.id {
                    continue;
                }
                if source.only_with_query() && q.is_empty() {
                    continue;
                }
                let raw = source.rows(&self.query);
                if source.ya_filtered() || q.is_empty() {
                    from_this.extend(raw);
                } else {
                    from_this.extend(
                        raw.into_iter()
                            .filter(|r| is_subsequence(&q, &r.text.to_lowercase())),
                    );
                }
            }
            if from_this.is_empty() {
                continue;
            }
            from_this.truncate(CAP_PER_SECTION);
            self.lines.push(GotoLine::Header(*section));
            for row in from_this {
                self.lines.push(GotoLine::Row(self.rows.len()));
                self.rows.push(row);
            }
        }
        self.cursor = self.first_row().unwrap_or(0);
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

    /// The query as is.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
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
    }
}

/// The COMMAND rows, built from the palette's: the SAME ones the palette
/// offers in that context, because two command lists computed separately
/// diverge.
#[must_use]
pub fn command_rows(rows: Vec<crate::palette::Row>) -> Vec<GotoRow> {
    rows.into_iter()
        .map(|r| GotoRow {
            section: SECTION_COMMANDS.id,
            key: format!("{K_CMD}{}", r.key),
            text: r.text,
            desc: r.desc,
            hostile: r.hostile,
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
    Action::Nothing("msg-goto-bad-path")
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
    } else if content.starts_with('/') {
        std::path::PathBuf::from(content)
    } else {
        // With a scheme: the wire is already a wire.
        return norte_proto::VPath::parse(content)
            .map_or(Action::Nothing("msg-goto-bad-path"), Action::Ir);
    };
    norte_vfs::native::vpath_from_native(&expanded)
        .map_or(Action::Nothing("msg-goto-bad-path"), Action::Ir)
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
        assert_eq!(
            action(&format!("{K_PATH}/tmp")),
            Action::Ir(VPath::parse("file:///tmp").expect("wire"))
        );
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
        CAP_PER_SECTION, Goto, GotoLine, GotoRow, GotoSection, GotoSource, SECTION_COMMANDS,
        SECTION_HISTORY, SECTION_INDEX, looks_path,
    };

    /// A fake source with fixed rows.
    struct Fixed {
        section: GotoSection,
        texts: Vec<&'static str>,
        ya_filtered: bool,
        only_with_query: bool,
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
                    desc: String::new(),
                    hostile: false,
                })
                .collect()
        }
        fn ya_filtered(&self) -> bool {
            self.ya_filtered
        }
        fn only_with_query(&self) -> bool {
            self.only_with_query
        }
    }

    fn source(section: GotoSection, texts: &[&'static str]) -> Box<dyn GotoSource + Send> {
        Box::new(Fixed {
            section,
            texts: texts.to_vec(),
            ya_filtered: false,
            only_with_query: false,
        })
    }

    /// Sections come out in [`ORDEN`]'s order, not the order the sources
    /// were registered in: what is learned is where each thing is.
    #[test]
    fn sections_come_out_in_fixed_order() {
        let goto = Goto::new(vec![
            source(SECTION_COMMANDS, &["app.quit"]),
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
        assert_eq!(headers, vec!["history", "commands"]);
    }

    /// A section with no rows paints no header: an empty header says
    /// there is something where there is nothing.
    #[test]
    fn an_empty_section_paints_no_header() {
        let mut goto = Goto::new(vec![
            source(SECTION_COMMANDS, &["app.quit"]),
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
        assert_eq!(headers, vec!["commands"], "/etc does not match \"quit\"");
    }

    /// The cursor never lands on a header, whether going down or up.
    #[test]
    fn the_cursor_skips_the_headers() {
        let mut goto = Goto::new(vec![
            source(SECTION_HISTORY, &["/etc", "/var"]),
            source(SECTION_COMMANDS, &["app.quit"]),
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
                Some("app.quit".to_owned()),
                Some("app.quit".to_owned()),
                Some("app.quit".to_owned()),
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
            only_with_query: false,
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
                desc: String::new(),
                hostile: false,
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
                    desc: String::new(),
                    hostile: false,
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

    /// With nothing typed, the commands section does not show up: whoever
    /// opens "go to" and does not type is asking WHERE they can go, and
    /// hundreds of verbs would bury the destination lists above them. With
    /// one letter, they come back.
    #[test]
    fn commands_do_not_show_until_typed() {
        let commands = Box::new(Fixed {
            section: SECTION_COMMANDS,
            texts: vec!["app.quit"],
            ya_filtered: false,
            only_with_query: true,
        });
        let mut goto = Goto::new(vec![commands, source(SECTION_HISTORY, &["/etc"])]);
        assert_eq!(goto.len(), 1, "only history");

        goto.push_char('q');

        assert_eq!(
            goto.rows().first().map(|r| r.text.as_str()),
            Some("app.quit"),
            "with something typed, the command comes back"
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
