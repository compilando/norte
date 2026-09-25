//! What the renderer SEES. Nothing more, and above all, nothing with authority.
//!
//! Three rules govern this module, and all three exist for the same reason
//! —that the renderer cannot be the authority on anything (ADR 0066)—:
//!
//! 1. **No raw path crosses.** Not `VPath`, not `PathBuf`, not `OsString`.
//!    What travels is text ALREADY sanitized and a flag for whether it
//!    differs from the real name. To act on a row, its opaque [`RowKey`] is
//!    used.
//! 2. **Everything paintable is scoped in Rust** ([`crate::bridge`]).
//! 3. **Nothing here decides.** An `enabled: false` is what the host
//!    resolved; the renderer paints it, it does not compute it.
//!
//! And a rule about NUMBERS, which costs nothing today and will tomorrow
//! (#258). Every `u64` in this module —`RowKey`, `ModalId`, `sequence`,
//! `generation`, `task_id`, `total_rows`, `first_visible`, `marks`,
//! `first_line`— arrives at the renderer as a JavaScript `number`, i.e. an
//! `f64`: exact only up to 2^53. All of them are small counters (a row
//! index, a listing epoch, the scheduler's counter), so nothing is broken
//! today. **The day one of them stops being a small counter —a hash, a
//! random id, a value with the time inside— it becomes a `String` on the
//! wire BEFORE it changes nature**, because otherwise the renderer rounds it
//! and two different rows collide without anything turning red. Making
//! `RowKey` unforgeable was considered and dropped in ADR 0068; if anyone
//! picks it back up, this is the paragraph to read first.

use serde::{Deserialize, Serialize};

use crate::bridge::{ModalId, RowKey};

/// The COMPLETE state of the screen.
///
/// A `Snapshot` replaces whatever the renderer had: it is the only way to
/// recover from a gap in the sequence, and that is why it is sent whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewSnapshot {
    /// State of the connection to the daemon.
    pub connection: ConnectionView,
    /// Where each slot goes and with what role. The renderer does NOT lay
    /// out the screen: it receives it already laid out (ADR 0066, decision
    /// D14).
    pub layout: LayoutView,
    /// The layout's slots, by id.
    pub slots: Vec<SlotView>,
    /// Slot with keyboard focus.
    pub focus: Option<u32>,
    /// The status bar.
    pub status: StatusView,
    /// Open dialogs, in opening order.
    pub dialogs: Vec<DialogView>,
    /// Live tasks and the ones that just finished.
    pub tasks: Vec<TaskView>,
    /// The menu bar: the titles, and the open one if any.
    pub menu: MenuView,
    /// The panel bar (#324): which panels exist, their state, and whether
    /// any has something to report. Bridge 51.
    pub panel_bar: PanelBarView,
    /// The RIGHT half of the status bar (ADR 0132, bridge 85): the
    /// `[ui] status_items` elements that fit, already worded and in their
    /// order. Absent in an older host = none.
    #[serde(default)]
    pub status_items: Vec<StatusItemView>,
    /// The layout buttons on the right of the menu bar (ADR 0133, bridge
    /// 86), in their order. Absent in an older host = none.
    #[serde(default)]
    pub layout_buttons: Vec<ChromeButtonView>,
    /// `[ui] row_stripes` (spec 2026-09-20): whether the odd rows of a
    /// listing sit on a band. Bridge 80.
    ///
    /// The SWITCH travels, not the color: the color is the theme's `stripe`
    /// role and already crosses with the rest, in `--stripe-bg`. Parity is
    /// up to the renderer, which is the one that knows which row ended up
    /// painted where.
    #[serde(default)]
    pub row_stripes: bool,
    /// The profile picker, if open.
    pub profiles: Option<ProfilePickerView>,
    /// The command palette, if open.
    pub palette: Option<PaletteView>,
    /// "Go to anywhere", if open (#357). Bridge 77.
    #[serde(default)]
    pub goto: Option<GotoView>,
    /// The first-launch wizard (spec 2026-09-10), if open. Bridge 63.
    #[serde(default)]
    pub wizard: Option<WizardView>,
    /// The splash screen (spec 2026-09-15, ADR 0115), if it is up. Bridge 69
    /// (with `RowView::progress` and `TaskView`'s pace).
    ///
    /// From the HOST, not the webview: what makes it worth having —where
    /// you were, where you usually go— only this side knows, and a second
    /// splash screen in the renderer would end up saying something else.
    #[serde(default)]
    pub splash: Option<SplashView>,
    /// The which-key panel, if there is a prefix half typed.
    pub whichkey: Option<WhichKeyView>,
    /// Help, if open. Like the viewer, it occupies the screen: while it is
    /// up, the keys are its own.
    pub help: Option<HelpView>,
    /// The theme, if being viewed. READ-ONLY only: it shows which colors
    /// each role has and which effects it declares that this renderer
    /// cannot paint.
    pub theme: Option<ThemeView>,
    /// A search, if one is open.
    pub search: Option<SearchView>,
    /// The diff panel, if a comparison is open.
    pub compare: Option<CompareView>,
    /// The sync panel, if a plan is open.
    pub sync: Option<SyncView>,
    /// The layout picker, if open.
    pub layouts: Option<LayoutPickerView>,
    /// The COLUMNS picker, if open.
    pub columns: Option<ColumnsPickerView>,
    /// An open picker (connections or volumes), if any.
    pub picker: Option<PickerView>,
    /// Extensions, if open. Since 6.4 they GOVERN: approved, revoked,
    /// turned on, turned off and configured — with the same effects switch
    /// that decides whether this window writes.
    pub extensions: Option<ExtensionsView>,
    /// Agent sessions, if the panel is open.
    pub agents: Option<AgentsView>,
    /// The output of the last extension command, if it is still on screen.
    ///
    /// Deliberately outside the manager: a command is launched from the
    /// PALETTE, and an output stored inside a screen that is not open is
    /// seen by nobody.
    pub plugin_output: Option<ExtensionOutputView>,
    /// The output of a PROGRAM this window ran and waited on (#312, bridge
    /// 52): today, the two-file comparator. `None` if none is on screen.
    pub program_output: Option<ProgramOutputView>,
    /// Settings, if open. READ-ONLY only: this window shows what exists and
    /// writes nothing until phase 5 gives it a safe path.
    pub settings: Option<SettingsView>,
    /// The viewer, if one is open. It occupies the screen: while it is up,
    /// the keys are its own and the listing does not move underneath.
    pub viewer: Option<ViewerView>,
    /// The rename plan under review, if any. It opens over the listing and
    /// the keys are its own until it is approved or discarded.
    pub ai_rename: Option<AiRenameView>,
    /// The ORGANIZE plan under review (phase 8), if any. Same screen shape
    /// as the rename one and for the same reason — a document read before
    /// approving it—, with different content: a TREE.
    pub organize: Option<OrganizeView>,
    /// Negotiated locale, so the renderer requests the right catalogue.
    pub locale: String,
}

/// The PROFILE picker (ADR 0079).
///
/// The rows and what is said about each are
/// `norte_frontend::profile_picker`, the same model that paints the
/// terminal: a row that cannot be used is SHOWN with its reason instead of
/// disappearing, because hiding a directory the reader created is worse than
/// showing it broken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilePickerView {
    /// The rows, in order.
    pub rows: Vec<ProfileRowView>,
    /// Which one is marked.
    pub cursor: u64,
    /// The generation they were painted with. The list is filled from a
    /// background task —reading `profiles/` is disk—, so a row named by
    /// index can name something else (ADR 0068).
    pub generation: u64,
}

/// A row of the profile picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRowView {
    /// The directory name, already paintable.
    pub name: String,
    /// What is painted differs from the directory's bytes (#266).
    pub name_hostile: bool,
    /// Its `[profile] title`, if it declares one. Never INSTEAD of the name:
    /// two profiles can share a title and still be two.
    pub title: Option<String>,
    /// It is the one currently set.
    pub active: bool,
    /// What OTHER norte thing shares this name, already said in the
    /// reader's language. Empty = it is only a profile.
    ///
    /// Flagged because it is a trap if not said: choosing the `far` profile
    /// does not bind a single key of the `far` preset.
    pub clash: String,
    /// This profile CANNOT save where you left each pane (its name is not
    /// UTF-8, D4). Said BEFORE choosing it, not after losing it.
    pub no_state: bool,
    /// Why it cannot be loaded, already sanitized. Empty = it can.
    pub problem: String,
}

/// The menu bar.
///
/// The menus and their entries are `norte_frontend::menu`, the SAME model
/// that paints the TUI: what is in each menu and in what order is not
/// decided twice. What is contributed here is the projection — titles and
/// labels already translated, each entry's shortcut, and whether this
/// window can run it.
///
/// It adds no capabilities: it adds a way to FIND them. The palette requires
/// knowing the name of what you're looking for and help requires reading; a
/// menu is browsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuView {
    /// Is the bar painted? Set by `[ui] menu_bar` in the configuration.
    ///
    /// Turned off, the bar takes no row and the menu only opens by its key —
    /// but it does open: turning it off hides the bar, not the menu.
    pub bar: bool,
    /// The titles, left to right, already translated.
    pub titles: Vec<String>,
    /// Which one is OPEN, if any. `None` = only the bar.
    pub open: Option<u64>,
    /// The open menu's entries, empty if none is open.
    pub items: Vec<MenuItemView>,
    /// Which entry is highlighted inside the open menu.
    pub cursor: u64,
}

/// An entry of a menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuItemView {
    /// The SHORT label (`menu-item-*`), not the help sentence: that is a
    /// description, and with it the dropdown would reach seventy columns.
    pub label: String,
    /// The shortcut that runs it, or empty if it has none in this preset
    /// (bridge 74: previously a dash, which read as "disabled").
    pub chord: String,
    /// This window can run it.
    ///
    /// A disabled entry STILL shows: the menu is where what exists is seen,
    /// and hiding what this frontend does not do would turn a limitation
    /// into a mystery. Same rule the palette applies to rows it cannot run.
    pub enabled: bool,
    /// Whether a section STARTS with this entry (ADR 0125, bridge 74):
    /// `None` continues the previous one's, `Some("")` is a rule with no
    /// label and `Some(r)` one with label `r`, already translated. It lives
    /// on the entry and not as its own element so the cursor keeps counting
    /// entries.
    pub section: Option<String>,
    /// `normal`, `destructive` (painted in the danger color) or `ai`
    /// (carries the AI mark). Decided by `norte_frontend::menu::role`, the
    /// same one the terminal reads.
    pub role: String,
}

/// The panel bar (#324, bridge 51): a row of buttons, one per panel that
/// opens and closes, that SHOWS the panels instead of waiting for the
/// reader to know they exist.
///
/// Which buttons exist and in what order is decided by
/// `norte_frontend::panelbar` —the same code as the TUI (ADR 0077)—; this
/// host only collects the state and translates it. It travels whole with
/// every change: six buttons are not worth a delta protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelBarView {
    /// `[ui] panel_bar`: whether the bar is painted. Turned off, panels
    /// still open by their key, their menu and the palette.
    pub bar: bool,
    /// `[ui] panel_bar_style = "names"` (spec 2026-09-10): each button shows
    /// its name with the access letter marked; `false` = letter only.
    /// Absent in a host older than bridge 63 = names.
    #[serde(default = "default_true")]
    pub names: bool,
    /// `[ui] panel_bar_position` already resolved (bridge 84): `true` = a
    /// column on the left edge, the activity bar; `false` = the row under
    /// the menu. `auto` is resolved by the HOST, to column: the window
    /// lacks height, not width. Absent in an older host = row.
    #[serde(default)]
    pub vertical: bool,
    /// The buttons, in the order they are painted. A click comes back as
    /// the INDEX in this list (`UiAction::PanelBarActivate`), never as a
    /// command: the renderer does not dispatch (ADR 0069).
    pub buttons: Vec<PanelButtonView>,
}

/// An element of the right half of the status bar (ADR 0132).
///
/// What it says, at what priority it yields and what a click runs is
/// decided by `norte_frontend::statusbar`, the same code as the TUI. A click
/// comes back as the `id` (`UiAction::StatusItemActivate`), never as a
/// command: the renderer does not dispatch (ADR 0069).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusItemView {
    /// The stable id (`position`, `tasks`…).
    pub id: String,
    /// The text, in the session's language.
    pub text: String,
    /// What it is and what pressing it does, for the tooltip.
    pub tooltip: String,
    /// Whether pressing it does anything.
    pub clickable: bool,
    /// The lightweight progress bar, behind the text (ADR 0146, bridge 92).
    /// Only on the `tasks` item with work underway; absent on the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<StatusProgressView>,
}

/// The `tasks` item's bar (ADR 0146).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusProgressView {
    /// Out of the burst's total, 0–100; `None` = unknown, and the renderer
    /// animates the bar instead of painting it empty.
    pub percent: Option<u8>,
    /// `running`, `done` or `failed`.
    pub phase: String,
}

/// A chrome button that runs a command (ADR 0133): the layout ones.
///
/// Which buttons exist and what they run is decided by
/// `norte_frontend::layoutbar`; a click comes back as the `id`, never as the
/// command (ADR 0069).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChromeButtonView {
    /// Stable id (`split-h`, `pick`…): comes back with the click and picks
    /// the icon.
    pub id: String,
    /// Its short name, the one from its menu entry.
    pub label: String,
    /// The shortcut that does the same, or `—`.
    pub chord: String,
}

/// `true` for a field an older host did not send and that being on is the
/// usual thing.
fn default_true() -> bool {
    true
}

/// A button of the panel bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelButtonView {
    /// The kind it opens. Layout text, already masked: a kind can come from
    /// a file or a plugin, and ends up in a DOM attribute.
    pub kind: String,
    /// The short name, in the session's language.
    pub label: String,
    /// The letter the TUI paints; here it accompanies the label so both
    /// surfaces read the same.
    pub letter: String,
    /// The shortcut that does the same as the button, or `—` if it has
    /// none.
    pub chord: String,
    /// Closed, open, or open AND with the keyboard.
    pub state: PanelButtonState,
    /// Has something to report without being visible: the log with unread
    /// notices, processes with tasks on the board.
    pub attention: bool,
    /// HOW MANY things it has to report (bridge 84): the badge's count.
    /// `0` with `attention` off; absent in an older host = `0`, and the
    /// renderer then paints the mark with no count.
    #[serde(default)]
    pub count: u32,
}

/// The state of a button's panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelButtonState {
    /// Not even in the layout.
    Closed,
    /// Placed and visible, but the keyboard goes elsewhere.
    Open,
    /// Placed, visible, and with the keyboard.
    Focused,
}

/// The open command palette.
///
/// Filtering, the cursor and what is selected are decided by
/// `norte_frontend::palette_state`, the same model as the TUI: typing to
/// narrow a list is a presentation rule, and two copies are two palettes
/// that behave differently without anyone noticing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteView {
    /// What was typed, already sanitized for painting.
    pub query: String,
    /// The rows that MATCH, in order.
    pub rows: Vec<PaletteRowView>,
    /// Which one is selected, if any.
    pub cursor: Option<u64>,
    /// How many rows there are in total, to say how much is being narrowed.
    pub total: u64,
}

/// "Go to anywhere" open (#357, bridge 77): the typed path, the pane's
/// history, the popular ones, the favorites, the connections, the commands
/// and what the semantic index found, in SECTIONS.
///
/// The sections, their order, the filtering and the cursor are decided by
/// `norte_frontend::goto`, the same model as the TUI; the renderer paints
/// the lines in order and marks the cursor's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GotoView {
    /// What was typed, already narrowed for painting.
    pub query: String,
    /// The lines in order: section headers and rows.
    pub lines: Vec<GotoLineView>,
    /// The index, in `lines`, of the selected row. Never a header.
    pub cursor: Option<u64>,
    /// What is painted when `lines` is empty, already translated: "nothing
    /// matches that" is not the same as a blank screen.
    pub empty: String,
}

/// A "go to" line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "line")]
pub enum GotoLineView {
    /// A section's header, ALREADY translated. Does not receive the cursor.
    Header {
        /// The section's title.
        title: String,
    },
    /// A row that can be gone to.
    Row {
        /// What is painted, already masked if needed.
        text: String,
        /// The second line (a favorite's or a connection's path, what a
        /// command does), or empty.
        desc: String,
        /// What is painted DIFFERS from the source bytes. Travels with the
        /// row: this is a screen where you choose where to go.
        hostile: bool,
    },
}

/// The first-launch wizard (spec 2026-09-10, bridge 63): a step, its rows
/// and the cursor. Everything already translated: the renderer paints and
/// returns rows or keys, and the host writes what was chosen through its
/// settings path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WizardView {
    /// `Welcome to norte · 1/3 · keys`.
    pub title: String,
    /// The step's question.
    pub question: String,
    /// The step's rows, in order. A click comes back as the INDEX
    /// (`UiAction::WizardActivateRow`).
    pub rows: Vec<String>,
    /// Which one is chosen.
    pub cursor: u64,
    /// The keys line.
    pub hint: String,
}

/// A command offered by the palette.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteRowView {
    /// What is shown (the command's name, or a plugin command's already
    /// masked title). NEVER the dispatch key.
    pub text: String,
    /// What it does, in the user's language.
    pub desc: String,
    /// The shortcut that runs it, or `—` if it has none in this preset.
    pub chord: String,
    /// This frontend can run it.
    pub enabled: bool,
    /// What is painted DIFFERS from what the row's contributor declares.
    ///
    /// Can only be true on a PLUGIN row: its title and description are
    /// written by a manifest, and this is the screen where you choose what
    /// third-party code to run. A masked text that travels without its flag
    /// reads as faithful.
    pub hostile: bool,
    /// Goes to the top for being among the last launched (spec 2026-09-10).
    /// Only with an empty query; with a query, the order is by what
    /// matches.
    #[serde(default)]
    pub recent: bool,
}

/// What can follow a half-typed prefix.
///
/// Built with `norte_frontend::whichkey`, the same model that paints the
/// TUI: which keys continue the sequence, what each is called in the user's
/// language, which open another sequence and which cannot be done here. The
/// renderer paints it; it does not know how to resolve a prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhichKeyView {
    /// The typed prefix, already painted, with the counter in front if
    /// there is one.
    pub title: String,
    /// A row per key that can follow, in the shared order.
    pub rows: Vec<WhichKeyRowView>,
}

/// A possible continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhichKeyRowView {
    /// The key, written to be read (`F5`) and masked: a project's
    /// `keymap.toml` can bind any code point.
    pub chord: String,
    /// What it does, in the user's language.
    pub label: String,
    /// Can be done here.
    pub enabled: bool,
    /// Opens ANOTHER sequence instead of running something. The renderer
    /// marks it instead of naming a command the key does not run.
    pub opens_sequence: bool,
    /// Why it cannot be done, already translated. Empty when it can.
    pub reason: String,
}

/// Help, open (F1).
///
/// The corpus, the overlay's model and the resolution of live marks are the
/// SHARED ones (`norte_help`, `norte_frontend::help`,
/// `norte_frontend::help_chords`): which pages exist, which is open, which
/// rows can be run and with which key THIS reader runs them. The renderer
/// does not interpret markdown and does not resolve a key: it receives
/// closed blocks and paints them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpView {
    /// The title of the open page, already scoped.
    pub title: String,
    /// Its id: an OPAQUE IDENTITY, not something painted.
    ///
    /// Travels WHOLE or does not travel. It does not go through the screen
    /// truncation the rest of this module does, because truncating is not
    /// injective and this is a key: two ids that matched in their first
    /// thousand bytes would arrive as one (ADR 0061, and the same reason
    /// `norte_help::parse_untrusted` copies the id verbatim). An id that
    /// would not fit travels EMPTY, which is an identity that matches
    /// nothing, instead of one that matches the wrong thing.
    ///
    /// Also not masked, and that is why **the renderer never paints it**:
    /// whoever wants to mark the sidebar's live row has
    /// [`HelpSidebarRowView::Topic::current`], which already comes
    /// resolved.
    pub topic_id: String,
    /// A plugin page's provenance line (who publishes it, whether it was
    /// truncated, whether there were bytes that did not decode). `None` on a
    /// binary page: a plugin page ALWAYS carries a line, and one that
    /// sometimes appears shows the opposite of the truth when it is
    /// missing.
    pub badge: Option<String>,
    /// The sidebar: group headers and pages, in the model's order.
    pub sidebar: Vec<HelpSidebarRowView>,
    /// Which sidebar row has the cursor.
    pub cursor: u64,
    /// Which half has the keyboard.
    pub focus: HelpFocusView,
    /// The page body, in blocks of a CLOSED vocabulary.
    pub blocks: Vec<HelpBlockView>,
    /// What `enter` can do over the body: run a command or open another
    /// page.
    pub actions: Vec<HelpActionView>,
    /// Which one is chosen, if any.
    pub action_cursor: Option<u64>,
    /// What was typed in the filter, already sanitized for painting.
    pub filter: String,
    /// The filter is open: text keys are its own.
    pub filtering: bool,
    /// There is somewhere to go back to (`⌫`). When there is not, `⌫`
    /// closes.
    pub can_back: bool,
    /// The last request to scroll the BODY (bridge 76), or `None` if none
    /// has happened in this opening.
    ///
    /// The body is scrolled by the DOM, which is the one that knows its
    /// measurements (#267); what the HOST decides is which key means what,
    /// with the reader's keymap. Previously the renderer handled `PgDn`,
    /// `Home`, `[`… as fixed keys, and a reader who rebound them saw the
    /// change in the terminal and not here. The renderer applies the
    /// request ONCE: `seq` grows with each one, and a patch that repaints
    /// help with the same one does not repeat it.
    pub scroll: Option<HelpScrollView>,
}

/// A request to scroll the help body (bridge 76).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpScrollView {
    /// Where to.
    pub to: HelpScrollTo,
    /// Grows with every request of this opening: what the renderer compares
    /// to avoid applying the same one twice.
    pub seq: u64,
}

/// Where to scroll the body. CLOSED vocabulary: how much a line, a page, or
/// where a section starts is measured by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpScrollTo {
    /// One line up (arrow, on a page with nothing runnable).
    LineUp,
    /// One line down.
    LineDown,
    /// One screen up.
    PageUp,
    /// One screen down.
    PageDown,
    /// To the beginning.
    Top,
    /// To the end.
    Bottom,
    /// To the previous heading.
    SectionPrev,
    /// To the next heading.
    SectionNext,
}

/// Which half of the overlay has the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpFocusView {
    /// The sidebar: up and down change page.
    Topics,
    /// The body: up and down move through what is runnable, `enter` acts.
    Body,
}

/// A row of the sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "row")]
pub enum HelpSidebarRowView {
    /// Group header, ALREADY translated. Cannot be chosen.
    Group {
        /// The header's text.
        label: String,
    },
    /// A page the reader can open.
    Topic {
        /// Its title, already scoped.
        title: String,
        /// It is the one open.
        current: bool,
    },
}

/// A block of the body. CLOSED vocabulary (ADR 0040): a hostile `help.md`
/// being unable to express anything outside this list is exactly what makes
/// it safe, and the renderer builds DOM nodes one at a time — never HTML —
/// because a block is not markup, it is data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "block")]
pub enum HelpBlockView {
    /// Level 1..=3 heading.
    Heading {
        /// The level, already scoped to 1..=3.
        level: u8,
        /// Its text.
        text: String,
    },
    /// A paragraph.
    Paragraph {
        /// Its spans.
        spans: Vec<HelpSpanView>,
    },
    /// A single-level bullet list.
    Bullets {
        /// Each bullet, with its spans.
        items: Vec<Vec<HelpSpanView>>,
    },
    /// A literal code block.
    Code {
        /// The language the fence declared, if it declared one.
        lang: Option<String>,
        /// The content, with no marks interpreted.
        text: String,
    },
    /// A simple table. Rows arrive ALREADY normalized to the header's
    /// width, so the renderer indexes by column without checking anything.
    Table {
        /// The header.
        header: Vec<String>,
        /// The rows.
        rows: Vec<Vec<String>>,
    },
    /// A highlighted callout.
    Callout {
        /// Which kind.
        kind: HelpCalloutView,
        /// Its content.
        spans: Vec<HelpSpanView>,
    },
    /// The keyboard reference sheet: every key bound on a screen, in the
    /// REAL precedence order of the effective map.
    ///
    /// A block of its own and not a table, because it is not corpus prose:
    /// it is generated from the reader's keymap, so a rebind changes it, and
    /// its rows carry availability and a reason that a table cell has
    /// nowhere to put.
    Keys {
        /// The rows, in order.
        rows: Vec<HelpKeyRowView>,
    },
}

/// What kind a highlighted callout is.
///
/// An enum and not a string: the renderer composes a Fluent key from this
/// (`help-callout-{kind}`) and `t` answers a key it does not have with the
/// key itself, so an unexpected kind would paint `help-callout-…` for the
/// reader — the same echo the rest of this module is careful not to
/// produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpCalloutView {
    /// Neutral note.
    Note,
    /// Warning: something can go wrong.
    Warn,
    /// Tip: something goes faster.
    Tip,
}

/// A span inside a block.
///
/// The corpus's two LIVE marks (`{{cmd:id}}` and `[[topic]]`) arrive here
/// already resolved against THIS reader's keymap and language: the prose
/// cannot lie about a key because it never carries one written out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "span")]
pub enum HelpSpanView {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
    /// Strong emphasis.
    Strong {
        /// The text.
        text: String,
    },
    /// Emphasis.
    Emph {
        /// The text.
        text: String,
    },
    /// Inline code.
    Code {
        /// The text.
        text: String,
    },
    /// A command, already resolved: the key that runs it for this reader,
    /// or its name when it has none (never an invented key).
    Command {
        /// What is painted.
        text: String,
        /// It is a KEY and not a name. The renderer paints it as such.
        is_chord: bool,
    },
    /// A link to another page, ALREADY resolved to its title.
    ///
    /// It does not carry the destination id, and that is not an oversight: a
    /// `[[topic]]` mark in the prose is not in the action list —that is
    /// formed by the page's commands and its "see also"—, so there is
    /// nothing to activate with it. Sending the key to a renderer that
    /// cannot use it would only get a third-party id, which nobody masks
    /// because it is a key, ending up in a DOM attribute.
    Link {
        /// Its title, or the id if this language's corpus does not have it.
        text: String,
        /// The `HelpView::actions` row that follows it (bridge 75): pressing
        /// the link is activating THAT row. An INDEX and not the id, for the
        /// same reason as above: the renderer does not receive keys it
        /// cannot use. `None` if the page does not have that row —does not
        /// happen in the corpus, which adds every link in the prose to its
        /// actions—.
        action: Option<u64>,
    },
}

/// A row of the keyboard sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpKeyRowView {
    /// The PAINTED sequence (`F5`, `g g`) and masked: a project's
    /// `keymap.toml` can bind any code point.
    pub chord: String,
    /// What it does, in the reader's language. Can come from a user's
    /// `keymap.toml`, so it is masked.
    pub label: String,
    /// The painted label differs from the one in the file (#266).
    pub label_hostile: bool,
    /// This build can run it.
    pub enabled: bool,
    /// Why not, already translated. Empty when it can.
    pub reason: String,
}

/// Something `enter` can do over the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpActionView {
    /// Its name, in the reader's language.
    pub label: String,
    /// The shortcut that runs it, empty if it has none or if it opens a
    /// page.
    pub chord: String,
    /// Can be done NOW, with the facts frozen when help was opened.
    pub enabled: bool,
    /// Why not, already translated. Empty when it can.
    pub reason: String,
    /// Opens another page instead of running a command.
    pub opens_topic: bool,
}

/// Settings, open (read-only).
///
/// The registry, each entry's effective value and its localized text are
/// the SHARED ones (`norte_frontend::settings`): the same catalogue that
/// paints the TUI, with the same stable ids. What this host adds is the
/// projection and one more section —where each thing lives—, which is
/// diagnostics, not configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsView {
    /// The sections, in their order. Only the ones with rows to show with
    /// the filter applied.
    pub sections: Vec<SettingsSectionView>,
    /// The index on the left: ALL the sections this surface has, whether or
    /// not the filter hides their rows.
    ///
    /// Kept apart from [`Self::sections`] on purpose: a section the filter
    /// emptied stays in the index —dimmed— because an index that changes
    /// length while you type cannot be used as a map, and it is not in
    /// `sections` because there is nothing to paint under its label.
    pub index: Vec<SectionIndexView>,
    /// Which half has the keyboard: `"index"` or `"list"`.
    ///
    /// Travels because BOTH cursors are always painted and the one without
    /// the keyboard is dimmed — the same rule as help's two halves (ADR
    /// 0128). Without this, the renderer could only paint one, which is
    /// exactly what makes it impossible to know where focus is.
    pub focus: String,
    /// Which row has the cursor, counting ALL rows of all sections in order
    /// (headers do not count: they cannot be chosen).
    pub cursor: u64,
    /// What is typed in the search box, ALREADY MASKED.
    pub query: String,
    /// How many settings are visible with the filter applied.
    pub shown: u64,
    /// How many there are in total. With [`Self::shown`] they are the two
    /// figures in "7 of 33": without the second, "there is nothing" and "I
    /// hid it with a letter" read the same.
    pub total: u64,
}

/// A section in the settings index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionIndexView {
    /// Its STABLE key (`appearance`, `open-with`…): an identity, not
    /// something painted. It is what comes back in `settings_jump_section`,
    /// and that is why it does not go through screen truncation.
    pub key: String,
    /// Its label, in the reader's language.
    pub title: String,
    /// How many of its rows are visible with the filter applied. Zero =
    /// dimmed.
    pub visible: u64,
}

/// A settings section: registry entries, or locations.
///
/// An enum and not a struct with two lists: a section is of one kind or the
/// other, and a struct with `rows` and `paths` would force every renderer to
/// decide what to do when both arrive full — a combination that does not
/// exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "section")]
pub enum SettingsSectionView {
    /// Registry entries with their effective value.
    Settings {
        /// Its STABLE key, the same as the index entry.
        ///
        /// It is what matches a section to its index row. Matching them by
        /// the translated title would work today and break the day two
        /// sections have similar names or someone tweaks a string: a label
        /// is prose, not an identity.
        key: String,
        /// Its title, already translated.
        title: String,
        /// Its rows.
        rows: Vec<SettingRowView>,
    },
    /// Where each thing lives.
    Paths {
        /// Its title, already translated.
        title: String,
        /// Its rows.
        rows: Vec<PathRowView>,
    },
}

/// A registry entry with its effective value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingRowView {
    /// The catalogue's stable id (`ui.confirm-quit`). An IDENTITY, not
    /// something painted: it travels so a renderer can anchor a row between
    /// two paints, and that is why it does not go through screen
    /// truncation.
    pub id: String,
    /// Its name, in the reader's language.
    pub name: String,
    /// What it does.
    pub desc: String,
    /// Its EFFECTIVE value, already resolved over the configuration layers
    /// and as text ready to paint. ALREADY MASKED.
    pub value: String,
    /// The value is painted DIFFERENT from what it is.
    ///
    /// Comes from a `norte.toml` that can be the PROJECT one, and that layer
    /// means "I opened this repository," not "I vouch for this string" (ADR
    /// 0026). `PathRowView` rows live in the same list, and they always had
    /// their flag: two kinds of row promising different things about the
    /// same column was the inconsistency there used to be.
    pub hostile: bool,
    /// Changing it requires restarting the window: what is written is
    /// saved, and takes effect on the next one.
    pub restart_required: bool,
    /// Its FACTORY value, as text.
    ///
    /// Painted as an empty field's placeholder: "empty" is not a gap, it is
    /// this value. Saying it with a datum —`Inter`, `14`— informs; saying it
    /// with a phrase ("what norte ships with") takes the datum's place and
    /// does not say which one it is.
    pub default: String,
    /// What control it needs: `toggle`, `choice`, `number`, `text` or
    /// `args`.
    ///
    /// Travels because a window has real controls and cannot guess the kind
    /// by looking at the value's text. Live lists —themes and presets—
    /// arrive already resolved in [`Self::choices`], so the renderer does
    /// not distinguish "catalogue enum" from "installed themes": to it both
    /// are a dropdown.
    pub control: String,
    /// The admitted values, if the control is `choice`. Empty if not.
    pub choices: Vec<String>,
    /// A `number`'s bounds, both inclusive.
    pub min: Option<i64>,
    /// See [`Self::min`].
    pub max: Option<i64>,
    /// Its value is NOT the factory one — the "you touched this" flag.
    ///
    /// Computed against the default value, not against "there is a key in
    /// your file": a key written with the same value it already had is not
    /// a change, and marking it as one would send someone to reset
    /// something that does nothing.
    pub modified: bool,
}

/// Where each thing lives: the configuration layers, the state, the logs
/// and the daemon's socket.
///
/// It is a settings section and not a separate view because it answers the
/// same question as the rest —"where does what I'm seeing come from?"— and
/// because the catalogue has no command to open it.
///
/// It carries PATHS and therefore carries the same mark as a file name: the
/// already sanitized text, and a flag for whether it differs from the real
/// one. No secret value enters here: these are locations, not contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRowView {
    /// What it is, already translated.
    pub label: String,
    /// Where, already sanitized for painting.
    pub display: String,
    /// The text above DIFFERS from the real path.
    pub hostile: bool,
    /// The place does not exist (a layer nobody created). It is SAID,
    /// instead of showing a path that looks like it is there.
    pub missing: bool,
}

/// The extensions manager, open (read-only).
///
/// Approving a capability is a SECURITY decision and a mutation: this
/// window shows it and does not make it, just as it does not delete. Phase
/// 5 gives the safe path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionsView {
    /// What is installed, in the order the catalogue gave.
    pub rows: Vec<ExtensionRowView>,
    /// Which one is chosen. Walks `rows` and, after it, `errors` (bridge
    /// 79): `rows.len() + j` is `errors[j]`.
    pub cursor: u64,
    /// The chosen one's detail card, once its schema has arrived. `None`
    /// while it is being requested, or if it was not requested.
    pub detail: Option<ExtensionDetailView>,
    /// The catalogue has not arrived yet. It is SAID, instead of showing an
    /// empty list that reads as "you have none."
    pub loading: bool,
    /// Directories the daemon could not load, already sanitized. Shown:
    /// an extension that fails to load and vanishes silently is an
    /// extension the user believes they have.
    pub errors: Vec<ExtensionErrorView>,
}

/// The output of ONE extension command.
///
/// Everything here is written by a third party: the text is what the plugin
/// printed and the title is its manifest's. Both enter masked and scoped,
/// and `truncated` travels because the receiver CANNOT deduce it — the text
/// arrives already short.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionOutputView {
    /// Which extension: its name already masked, with its flag.
    pub plugin: MaskedTextView,
    /// Its reverse-DNS id, which the core DOES validate.
    ///
    /// Travels with the name because the name does not identify: two
    /// extensions can share a name, and this is the one that says who
    /// printed this.
    pub plugin_id: String,
    /// Which command: its title already masked, with its flag.
    pub command: MaskedTextView,
    /// What it printed, LINE BY LINE, each one masked and scoped.
    ///
    /// By lines and not as one string: a line break is a C0 control, i.e. a
    /// terminal hazard, so masking the whole output would flag ANY output
    /// over one line as hostile — a flag that is true for everything honest
    /// says nothing. Empty = it printed nothing, which is SAID: a blank
    /// panel reads as if it never got to run.
    pub lines: Vec<String>,
    /// Some line is painted different from what the plugin printed.
    pub text_hostile: bool,
    /// The output did not fit whole and was cut.
    pub truncated: bool,
}

/// The output of a PROGRAM the window ran and waited on (#312).
///
/// The terminal has a path the browser does not: suspending itself, running
/// `diff -u` and waiting for a key. This is its honest equivalent: the
/// hosting process runs the program, captures what it printed and shows it
/// here until the reader closes it. What it printed was written by another
/// program over files anyone could have named: it enters masked, by lines
/// and scoped, like an extension's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramOutputView {
    /// Fluent key of the title: what was done ("compare two files").
    pub title_key: String,
    /// The program and its arguments, already masked, to say WHAT ran.
    pub command: MaskedTextView,
    /// What it printed (stdout and stderr, in that order), LINE BY LINE.
    pub lines: Vec<String>,
    /// Some line is painted different from what the program printed.
    pub text_hostile: bool,
    /// The output did not fit whole and was cut.
    pub truncated: bool,
    /// The program could not run, or exited with an error. A comparator
    /// returns 1 when the files differ, so this is NOT "nonzero": it is "did
    /// not start" or "ran past the deadline."
    pub failed: bool,
}

/// The AGENT sessions this window has seen request permission.
///
/// What the list IS goes INSIDE it (`note`): there is no protocol method
/// that lists live sessions, so these are the ones seen BY THIS WINDOW, not
/// the system's census of agents. An empty list without that note reads as
/// "no agent has touched anything," a claim this window cannot make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsView {
    /// The sessions, from most recently seen to oldest.
    pub rows: Vec<AgentRowView>,
    /// Which one is chosen.
    pub cursor: u64,
    /// How many times this list has changed.
    ///
    /// Comes back with the click: the list reorders ITSELF —a permission
    /// request bumps its session to the top spot— and a click has to be
    /// resolved against the one the reader was looking at. Here "this row"
    /// is whose work gets undone.
    pub generation: u64,
    /// How many sessions have been forgotten due to the cap.
    ///
    /// Said out loud: the session id is chosen by the agent, so flooding the
    /// list to push a specific one out is within its reach, and a truncated
    /// list presented as complete is what turns that into "that session
    /// doesn't exist."
    pub forgotten: u64,
    /// What this list is, already translated.
    pub note: String,
    /// What to say when there is no row, already translated.
    ///
    /// Composed by the HOST because it is not always the same sentence: a
    /// read-only window does not even subscribe to the approvals channel,
    /// so its empty list means "this window isn't listening," not "no agent
    /// has requested anything" — a claim it cannot make.
    pub empty: String,
}

/// An agent session seen by this window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRowView {
    /// Its id, already masked: it is an OPAQUE daemon key and can carry any
    /// byte. What travels back is the raw id, not this.
    pub session: String,
    /// The id is painted different from what it is.
    pub session_hostile: bool,
    /// How many it requested and how many were approved from here, already
    /// as a translated sentence.
    ///
    /// Composed HERE and not in the renderer: the catalogue that crosses is
    /// already-translated strings, with no variable substitution, so a
    /// `{ $n }` on the other side would paint literally. And the two counts
    /// are not the same thing — another window might have answered, or it
    /// was denied, or it expired.
    pub counts: String,
    /// An undo has already been launched for it and is still running.
    ///
    /// Said, and also refuses to launch another: two `policy.undo_session`
    /// calls for the same session walk the same list of entries, and the
    /// second produces a report full of blocks that belong to nobody.
    pub undoing: bool,
    /// The last op-kind it requested (`copy`, `delete`…), already masked.
    pub last_op: String,
    /// The op-kind is painted different from what it is.
    pub last_op_hostile: bool,
}

/// A third-party string ready to paint, with its flag alongside.
///
/// Both together and not in sibling fields: a loose flag ends up describing
/// the string next to it —which is exactly what happened here, where a
/// single flag for three strings was computed by one of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskedTextView {
    /// What is painted, already masked and scoped.
    pub text: String,
    /// What is painted DIFFERS from what its author wrote.
    pub hostile: bool,
}

/// A command an extension contributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionCommandView {
    /// Its dispatch id. NEVER painted: the manifest does not validate its
    /// charset, so it can carry any byte —line breaks included—, and masking
    /// it would break it as a key.
    pub id: String,
    /// Its title, already masked and scoped.
    pub title: String,
    /// The title is painted different from what the manifest declares.
    pub hostile: bool,
}

/// An extension from the catalogue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionRowView {
    /// Its id, validated reverse-DNS. It is an IDENTITY: travels whole and
    /// untruncated, and is used to request its detail card.
    pub id: String,
    /// Its name, already masked and scoped (third-party text).
    pub name: String,
    /// Who publishes it, already masked. Empty if it does not declare one.
    pub publisher: String,
    /// Its version, already masked: declared by the manifest, i.e. a third
    /// party, and ends up in a row.
    pub version: String,
    /// What role it plays (`previewer`, `indexer`…), from the core's
    /// vocabulary.
    pub category: String,
    /// What it does, already masked and scoped. Empty if it does not
    /// declare one.
    pub description: String,
    /// A human approved its capabilities.
    pub approved: bool,
    /// A human has it turned on.
    pub enabled: bool,
    /// It ships a help page (`F1` opens it at its section).
    pub has_help: bool,
    /// How many commands it contributes.
    pub commands: u32,
    /// How many columns it contributes.
    pub columns: u32,
    /// The capabilities it requests, as it declares them.
    ///
    /// In the ROW and not only in the detail card, on purpose: they are the
    /// decision a human approves, and hiding them behind a second gesture
    /// turns "this can read your files" into something you have to go look
    /// for.
    pub capabilities: Vec<String>,
}

/// An extension directory that failed to load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionErrorView {
    /// Where, already sanitized.
    pub dir: String,
    /// The text above DIFFERS from the real path.
    pub hostile: bool,
    /// Why, already sanitized: written by the core, but may quote the
    /// plugin's manifest.
    pub reason: String,
    /// The REASON is painted different from what it is.
    ///
    /// Separate from `dir`'s because they are two strings with two origins,
    /// and a single flag for both leaves the reader not knowing which one it
    /// refers to.
    pub reason_hostile: bool,
    /// The id it is uninstalled with (bridge 79), or `None` if the
    /// directory's name is not an id: then there is no button, and the host
    /// refuses it saying why. The rule is `norte_frontend::broken_plugin`.
    pub id: Option<String>,
}

/// An extension's detail card: what it REQUESTS and what has been
/// configured for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionDetailView {
    /// Whose detail card this is.
    pub id: String,
    /// Its `[config]` keys with their effective value. Empty if it declares
    /// none.
    pub config: Vec<ExtensionConfigRowView>,
    /// The commands it contributes, in manifest order. Empty if it
    /// contributes none.
    pub commands: Vec<ExtensionCommandView>,
    /// Which key is chosen inside the detail card.
    pub cursor: u64,
    /// The open edit buffer (`string`/`int`), ALREADY MASKED. `None` =
    /// nothing is being edited.
    pub editing: Option<String>,
    /// The buffer is painted different from what will be written. A
    /// starting value was written by the PLUGIN, so it can carry anything;
    /// what travels back to the daemon is the raw operand, not this.
    pub editing_hostile: bool,
}

/// A `[config.<key>]` key with its schema and effective value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionConfigRowView {
    /// The key. Charset validated by the manifest, safe as is.
    pub key: String,
    /// Its type (`string`, `bool`, `int`, `enum`). A type this frontend
    /// does not know —a newer peer— is painted as text and does not blow
    /// up.
    pub kind: String,
    /// The EFFECTIVE value: the schema's defaults with `config.toml`
    /// overlaid. ALREADY MASKED.
    pub value: String,
    /// The schema's default value, so what has changed can be seen. ALREADY
    /// MASKED.
    pub default: String,
    /// What it is, already masked (manifest text). Empty if it does not say.
    pub description: String,
    /// An `enum`'s valid values, or an `int`'s bounds, already as text.
    /// Empty when the type has nothing to bound.
    pub domain: String,
    /// One of the three free-text fields —value, default, domain— is
    /// painted DIFFERENT from what it is.
    ///
    /// All three are written by the plugin in its `plugin.toml` and the
    /// manifest only bounds their LENGTH, not their charset: an `enum`
    /// value with a bidi override inside reached the DOM as is while three
    /// rustdocs claimed that could not happen.
    pub hostile: bool,
    /// This build knows how to edit this `kind`.
    ///
    /// `false` for a type it does not know —a newer peer—: the shared model
    /// treats it as read-only, and saying so keeps the screen from offering
    /// an `Enter` that will not change anything.
    pub editable: bool,
}

/// The COLUMNS picker: which columns exist, in what order and with what
/// format.
///
/// The model is the shared one (`norte_frontend::columns_picker`), which the
/// TUI wraps in an overlay and this window in a panel: the same machine, and
/// therefore the same rules —the name goes first and can be neither turned
/// off nor moved, an id that does not parse is PRESERVED because it is user
/// intent, and an attr the provider announces and nobody configured is
/// OFFERED off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnsPickerView {
    /// Its title, already translated, WITH the scope inside: the scheme the
    /// choice applies to (`sftp`, `zip+file`…) or "all schemes."
    ///
    /// The scope goes in the title and not in a separate field because it is
    /// the first thing you need to know to understand what is being
    /// touched, and there is no way to guess it from inside the panel.
    pub title: String,
    /// The rows, in paint order.
    pub rows: Vec<ColumnsPickerRowView>,
    /// Which row has the cursor.
    pub cursor: u64,
    /// The sentence explaining what applies and what does NOT, already
    /// translated.
    ///
    /// This window does not write configuration yet: what is chosen holds
    /// for THIS window and is lost when it closes. Staying silent about it
    /// would leave the user believing they just configured norte.
    pub note: String,
    /// The footer with the keys, painted from the KEYMAP (#287).
    ///
    /// Comes from the host and not from a renderer string because the
    /// `dialog.*` verbs can be rebound: a footer that says `Shift+↑/↓` over a
    /// keymap that binds something else is a lie only testing uncovers.
    pub hint: String,
}

/// A row of the columns picker.
// Four bools, each an independent fact painted differently: the label
// differs from the real one, the column is on, its format is fixed by the
// schema, and the row cannot be touched. See `RowView`.
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent states of a cell; see `RowView`"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnsPickerRowView {
    /// Its id, exactly as it travels to configuration (`size`,
    /// `attr:posix.mode`). An IDENTITY: whole or empty, never truncated.
    pub id: String,
    /// Its name, already translated and sanitized. For an `attr:` or a
    /// `plugin:`, the label given by its catalogue, which is third-party
    /// text.
    pub label: String,
    /// The label is painted DIFFERENT from what it is.
    pub hostile: bool,
    /// Is painted in the listing.
    pub enabled: bool,
    /// The current format (`iec`, `iso`…), closed ASCII vocabulary. Empty =
    /// this column admits no format.
    pub format: String,
    /// The format is FIXED by a schema setting and cannot be cycled here.
    /// Painted dimmed instead of disappearing: a key that does nothing and
    /// does not say why is worse than one that says no.
    pub format_locked: bool,
    /// Cannot be turned off or moved. The NAME's case, which is the first
    /// column by the render's contract.
    pub fixed: bool,
}

/// The active theme, seen from the inside.
///
/// ROLES are the shared part: a norte theme does not name colors, it names
/// roles (`selection`, `error`…), and each frontend paints them with its own
/// technology. EFFECTS are not: they are a free block each renderer
/// interprets, so what this view says about them is what the theme declares
/// and what of that THIS window knows how to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeView {
    /// The active theme's name, or the default preset's name.
    pub name: String,
    /// Each role with its resolved color (`#rrggbb`), in order.
    pub roles: Vec<ThemeRoleView>,
    /// The effects the theme declares that this renderer does NOT know how
    /// to paint.
    ///
    /// Said, instead of ignored: a retro theme that does not look different
    /// is a theme the user believes is broken. Empty = the theme declares
    /// none.
    ///
    /// Each key with its flag: they come from the theme file (#266).
    pub unsupported_effects: Vec<ThemeEffectView>,
    /// Which themes can be chosen from, in order.
    ///
    /// This screen CHOOSES since the catalogue can cross again: it used to
    /// only show, because whatever hosted it resolved the theme once at
    /// startup and there was no way to tell it it had changed.
    pub choices: Vec<String>,
    /// Which one is under the cursor. Moving the cursor previews LIVE, just
    /// like the terminal: a theme picker that does not show the theme forces
    /// choosing blind.
    pub cursor: u64,
}

/// An effect the theme declares that this renderer does not paint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeEffectView {
    /// The key, already masked.
    pub key: String,
    /// What is painted differs from what the file says.
    pub hostile: bool,
}

/// A theme role with its color.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeRoleView {
    /// What role it plays (`selection`, `error`…). norte's vocabulary.
    pub role: String,
    /// Its color, `#rrggbb`. The renderer paints it as a swatch; it does not
    /// parse it to decide anything.
    pub color: String,
}

/// The host's VOLUME picker: choose one and the pane navigates to it.
///
/// Volumes only, today. The connection picker task 4.5 names alongside it
/// is not here, and the absence is a decision: reading `connections.toml`
/// forces pulling the connections crate —with russh, opendal, suppaftp, age
/// and the keyring— into this window, for a list that cannot open any of
/// them yet. It arrives with phase 5, which needs that crate anyway. Until
/// then `pane.connect` answers "not here," which is true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerView {
    /// Its title, already translated.
    pub title: String,
    /// The rows.
    pub rows: Vec<PickerRowView>,
    /// Which one is chosen, if any.
    pub cursor: Option<u64>,
    /// The list is empty and why, already translated. Empty when there are
    /// rows.
    ///
    /// "There are none" and "hasn't answered yet" are not the same, and an
    /// empty list without a sentence always reads as the first.
    pub empty: String,
    /// Rises every time the SET of rows changes.
    ///
    /// The volume picker opens EMPTY and fills when the daemon answers, so
    /// it has the same race as the sidebar: a click painted over one list
    /// and handled over another. See [`PlacesSlotView::generation`].
    pub generation: u64,
}

/// A row of a picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerRowView {
    /// What is shown, already sanitized.
    pub label: String,
    /// The text above DIFFERS from the real one (a mount point is BYTES).
    pub hostile: bool,
    /// The detail on the right, already sanitized: a connection's URL, or a
    /// volume's file system and space.
    pub detail: String,
}

/// An entry's attribute sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataSlotView {
    /// Slot id.
    pub slot_id: u32,
    /// The fields, in order: first the ones every entry has, then the
    /// attributes the provider brought with the listing.
    pub fields: Vec<MetadataFieldView>,
    /// There is nothing to show, and this is the sentence that says so (the
    /// pane it follows is empty). Empty when there are fields.
    pub note: String,
    /// The path of the listing this sheet FOLLOWS, already paintable.
    ///
    /// "Details" alone does not say details of what: with two listings open
    /// there was no way to know which one was being described short of
    /// moving the cursor and watching whether the sheet moved. Travels apart
    /// from the fields because it does not describe the ENTRY but the pane,
    /// and goes in the title.
    ///
    /// Empty if the link does not resolve to any listing.
    pub follows_display: String,
    /// The path above DIFFERS from the real bytes.
    pub follows_hostile: bool,
}

/// A field of the sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataFieldView {
    /// Its name, already translated (or the attribute catalogue's header).
    pub label: String,
    /// Its value, already formatted and sanitized.
    pub value: String,
    /// The value DIFFERS from the real one (only the name can).
    pub hostile: bool,
}

/// The directory tree panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeSlotView {
    /// Slot id.
    pub slot_id: u32,
    /// The visible branches, in paint order.
    pub rows: Vec<TreeRowView>,
    /// Which row has the cursor.
    pub cursor: u64,
    /// Rises every time the SET of rows changes.
    ///
    /// And changes on its own: expanding a branch requests its listing, and
    /// that listing arrives from a background task and inserts rows IN THE
    /// MIDDLE. Between the reader releasing the button over one and the host
    /// handling the action, that row can be another one — the same hazard as
    /// the places bar, and the same cure (ADR 0068).
    pub generation: u64,
}

/// A branch of the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeRowView {
    /// The directory's name, sanitized. The root carries its whole path:
    /// "`/`" alone does not say where this hangs from.
    pub label: String,
    /// The PAINTED name differs from the real bytes.
    pub hostile: bool,
    /// How many levels below the root (the root is 0).
    pub depth: u32,
    /// Is expanded.
    pub expanded: bool,
    /// Has children to show. `None` = not looked at yet, and these are
    /// three distinct states for the reader: a branch that can be opened, a
    /// leaf that cannot, and one that is not known yet. Painting "leaf" on
    /// something not yet read is a made-up answer.
    pub children: Option<bool>,
}

/// The places sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacesSlotView {
    /// Slot id.
    pub slot_id: u32,
    /// Its rows, in order: volumes header, the volumes, favorites header,
    /// the favorites. A collapsed section does not list its own, but its
    /// header STAYS: without it the list jumps when they arrive.
    pub rows: Vec<PlaceRowView>,
    /// Which row has the cursor.
    pub cursor: u64,
    /// Rises every time the SET of rows changes.
    ///
    /// Without this a click was not safe. Volumes arrive from a background
    /// task and are inserted IN THE MIDDLE of the list —volumes go before
    /// favorites—, so between the user releasing the button over
    /// `~/projects` and the host handling the action, that row can be
    /// `/boot`. The index travels accompanied by the generation it was
    /// painted with, and one that does not match is rejected instead of
    /// navigating somewhere else (ADR 0068).
    pub generation: u64,
}

/// A row of the sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "row")]
pub enum PlaceRowView {
    /// A section's header. Does not navigate.
    Header {
        /// Its text, already translated.
        label: String,
        /// Is collapsed.
        folded: bool,
    },
    /// A host volume.
    Drive {
        /// What it is called: its label if it has one, or its mount point.
        /// Already sanitized — no platform promises a label is UTF-8.
        label: String,
        /// The text above DIFFERS from the real one.
        hostile: bool,
        /// The space and whether it is read-only, already formatted. A size
        /// the system did not answer is SAID; a `0` is never painted.
        detail: String,
        /// The SHORT free space (`159G`, or `?` with no answer), for the
        /// row's right column (bridge 87). Absent in an older host.
        #[serde(default)]
        free: String,
        /// The whole mount point, sanitized, for the row's title: since
        /// bridge 87, `label` is the SHORT name.
        #[serde(default)]
        mount: String,
        /// The drive's kind (`fixed`, `removable`, `network`, `unknown`):
        /// picks the icon (bridge 87).
        #[serde(default)]
        kind: String,
    },
    /// A hotlist favorite.
    Favorite {
        /// The name the user gave it, already sanitized.
        name: String,
        /// Where it goes, already sanitized. Empty if its path does not
        /// parse.
        target: String,
        /// The text above DIFFERS from the real one.
        hostile: bool,
        /// Its path does not parse, and this is the already translated
        /// reason. Empty when the favorite is fine.
        ///
        /// A broken favorite is PAINTED with its reason: one that
        /// disappears silently is a configuration failure nobody can see.
        broken: String,
    },
}

/// The layout picker, with the chosen one's preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutPickerView {
    /// Its title, already translated.
    pub title: String,
    /// The rows: first the five factory ones, then the user's.
    pub rows: Vec<LayoutRowView>,
    /// Which one is chosen.
    pub cursor: u64,
    /// The SHAPE of the chosen layout, in characters: one line per
    /// thumbnail row, all the same width.
    ///
    /// Painted by the host with the same engine that lays out the real
    /// screen, so the preview cannot lie about what will come out.
    pub preview: Vec<String>,
    /// Why the chosen one has no preview, already translated. Empty when it
    /// has one.
    ///
    /// QUOTES the user's file (the TOML parser's diagnostic), so it travels
    /// masked and with its flag: what is masked is said (#266).
    pub problem: String,
    /// The painted diagnostic differs from what the file contains.
    pub problem_hostile: bool,
}

/// An offered layout.
///
/// Four flags and not a state: each is an independent FACT —factory, the
/// name differs from the real one, shares a name with a keyboard preset, its
/// file does not parse— and combining them into an enum would force
/// inventing combinations that do not exist.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent warnings for a row; an enum would invent combinations"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutRowView {
    /// Its name, already sanitized. The REAL name is bytes —ends in
    /// `layouts/<name>.toml`— and does not travel: choosing a row sends its
    /// index, not its name.
    pub name: String,
    /// The text above DIFFERS from the real name.
    pub hostile: bool,
    /// Is one of the factory ones.
    pub factory: bool,
    /// Its name matches a KEYBOARD preset's, and choosing it does not
    /// change a single key. Flagged: without the line, the coincidence is a
    /// trap instead of a convenience.
    pub shares_keymap_name: bool,
    /// Its file does not parse.
    pub broken: bool,
}

/// A search across the subtree, with what it has found so far.
///
/// Results arrive in BATCHES while the search runs: the view can be browsed
/// and used before it finishes, which is half the value of searching a large
/// tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchView {
    /// What was searched for, already sanitized.
    pub query: String,
    /// Where, already sanitized.
    pub root: String,
    /// The text above DIFFERS from the real path.
    pub root_hostile: bool,
    /// What has been found so far.
    pub rows: Vec<SearchRowView>,
    /// Which one is chosen, if any.
    pub cursor: Option<u64>,
    /// It was asked by MEANING against the index, not by name against the
    /// tree.
    ///
    /// The renderer needs it for two things: titling the view and deciding
    /// whether to paint the similarity column. And to avoid promising what
    /// is not there: a semantic search does not walk a subtree, so its scope
    /// is the whole index and not [`Self::root`].
    pub semantic: bool,
    /// What state it is in, ALREADY said: how many so far and whether it is
    /// still running, finished, or stopped at its cap.
    ///
    /// Composed in Rust with the SAME family of sentences the TUI uses
    /// (`search-status-*`): the catalogue reaches the renderer with the
    /// texts already resolved, so interpolating a number is the host's job.
    ///
    /// The three states are said differently because they are different: a
    /// short list that is no longer growing, one still growing and one that
    /// stopped at the cap read the same if nobody names them.
    pub status: String,
    /// Still running. Kept apart from [`Self::status`] because the renderer
    /// uses it to paint, not to read.
    pub running: bool,
}

/// A result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchRowView {
    /// The file's name, already sanitized.
    pub name: String,
    /// The text above DIFFERS from the real name.
    pub hostile: bool,
    /// Where it is, already sanitized: the directory containing it.
    pub parent: String,
    /// The directory above DIFFERS from the real one.
    pub parent_hostile: bool,
    /// Is a directory.
    ///
    /// `false` also when it is NOT known: a semantic hit brings a path and a
    /// similarity, not a kind, and activating it opens the folder with the
    /// cursor on it —which is what should be done with a file— instead of
    /// trying to enter something that might not be a directory.
    pub is_dir: bool,
    /// How similar it is to what was asked, in `[-1, 1]`, higher = more.
    ///
    /// `None` in a search BY NAME: there are no degrees there, either the
    /// pattern matches or it does not, and painting a made-up number would
    /// turn an arrival order into a ranking.
    pub score: Option<f64>,
}

/// What the viewer shows.
///
/// Five flags and not a state: each is an independent FACT the host
/// resolved (is hexadecimal, the encoding was forced by the user, the
/// decoding had errors, the file kept going, the name differs from the real
/// one), and combining them into an enum would force inventing combinations
/// that do not exist.
///
/// The text comes DECODED and in lines from `norte_frontend::viewer`, the
/// same model that paints the TUI: encoding detection, a binary's jump to
/// hexadecimal and the visible window's truncation are its own, not the
/// renderer's.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the viewer: hex, truncation and the window are its own, not the renderer's"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerView {
    /// The file, already sanitized for painting.
    pub path_display: String,
    /// The text above DIFFERS from the real name.
    pub path_hostile: bool,
    /// Name of the encoding it is being read with.
    pub encoding: String,
    /// Detected line ending (`lf`, `crlf`, `cr`, `mixed`).
    pub eol: String,
    /// Is being shown in hexadecimal (binary, or by hand).
    pub hex: bool,
    /// The encoding was forced by the user, not detection.
    pub forced: bool,
    /// The decoding had errors: there are bytes that did not belong to that
    /// encoding.
    pub had_errors: bool,
    /// Only a header was read: the file kept going.
    pub truncated: bool,
    /// Total lines of what was read.
    pub total_rows: u64,
    /// First visible line.
    pub first_line: u64,
    /// Width of the longest line, in CELLS.
    ///
    /// With `first_col`, it is what the renderer needs to draw a horizontal
    /// scrollbar. `0` in hexadecimal, which has a fixed width and does not
    /// scroll. Travels even though `lines` already arrive truncated: the
    /// truncation says what is seen, and this says how much there is —
    /// without the latter, a file cut off on the right reads as a short
    /// file.
    ///
    /// Without `serde(default)`, like the rest of this view: backward
    /// compatibility is resolved by `bridge_version` in the envelope, and a
    /// `default` here would only weaken the golden — if someone stopped
    /// serializing them, the round-trip would pass with zeros.
    pub total_cols: u64,
    /// First visible column, in cells.
    pub first_col: u64,
    /// The visible window's lines, already sanitized and scoped.
    pub lines: Vec<String>,
    /// "via ‹plugin›", already translated with the name masked inside.
    /// Empty = it is the file, read by norte.
    ///
    /// Said whenever there is one. A previewer can show anything —that is
    /// its job: a PDF as text, a formatted JSON— and whoever is looking has
    /// the right to know they are not seeing the file's own bytes.
    ///
    /// Translated here because it interpolates the name, and a renderer
    /// does not translate.
    pub preview_by: String,
    /// The decoding of the file given to the previewer was LOSSY: the `�`
    /// in its output come from there, not from the file.
    ///
    /// Kept apart from `had_errors`, which is the raw view's: they are two
    /// different decodings, and confusing them blames the file for what the
    /// read did.
    pub preview_lossy: bool,
    /// This is an IMAGE that can be painted, and this is how big it claims
    /// to be.
    ///
    /// `None` = it is not an image, or it is one this window REFUSES to
    /// paint; in the second case [`Self::image_refused`] says why. The
    /// renderer requests the bytes separately —they do not travel in the
    /// snapshot— and shows the raw view until they arrive.
    pub image: Option<ImageView>,
    /// Why an image that WAS recognized will NOT be painted, already
    /// translated. Empty = there is nothing to explain.
    ///
    /// Said instead of silently falling back to the hexview: a file the
    /// user knows is a photo that appears as bytes with no word looks like
    /// broken norte, not cautious norte.
    pub image_refused: String,
    /// The image's ZOOM, as a percentage of what FIT would occupy (bridge
    /// 80). `100` = fit, which is how it opens.
    ///
    /// A percentage and not a size in pixels because the one who knows how
    /// much "fit" is is the renderer, which is the one with the slot. The
    /// host keeps count of the steps and tells it; the multiplication
    /// belongs to the stylesheet.
    ///
    /// A renderer older than bridge 80 does not read it and always paints
    /// the image fit, which is what it used to do.
    #[serde(default = "zoom_fit")]
    pub image_zoom: u16,
    /// The visible lines WITH STYLE when what is shown was produced by a
    /// previewer (bridge 49): one entry per row of [`Self::lines`], each the
    /// ordered list of its spans. Empty in the raw view.
    ///
    /// The same text as `lines`, split and with its role or color: the TUI
    /// painted it from day one and the window flattened it. A renderer that
    /// does not paint spans keeps using `lines` and loses nothing.
    pub styled: Vec<Vec<SpanView>>,
}

/// The zoom that means FIT, for [`ViewerView::image_zoom`]'s
/// `serde(default)`: a host older than bridge 80 does not send the field,
/// and what it used to do was paint fit.
const fn zoom_fit() -> u16 {
    100
}

/// A span of a styled preview line (ADR 0037).
///
/// `role` WINS over `fg` when both are present, as in the TUI: the reader's
/// theme rules over a plugin's fixed color. A role the theme does not know
/// does not reach here: the shared model already left it as `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanView {
    /// The text, already masked on input and scoped here.
    pub text: String,
    /// The theme role in kebab-case (`title`, `error`, `match`…), validated.
    pub role: Option<String>,
    /// The plugin's own color, `#rrggbb`. Only counts without `role`.
    pub fg: Option<String>,
    /// The span's BACKGROUND, `#rrggbb` (bridge 50): an image previewer
    /// paints half-blocks with the top pixel in `fg` and the bottom one
    /// here. No role rules over it.
    pub bg: Option<String>,
}

/// A recognized and accepted image: what it is and how big it claims to be.
///
/// What its HEADER declares, not what it truly measures — nobody has
/// decoded it yet, and that is exactly the point: the declared size is what
/// gets compared against the budget BEFORE handing it to a decoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageView {
    /// Its format, recognized by MAGIC bytes and never by the extension: an
    /// extension is a claim made by whoever named the file.
    pub format: String,
    /// Declared width, in pixels.
    pub width: u32,
    /// Declared height, in pixels.
    pub height: u32,
}

/// The screen's layout: who is painted, where, and with what role.
///
/// Measured in layout CELLS and not pixels, which is how each pane's
/// minimums are declared and how the TUI shares them: "this doesn't fit"
/// means the same on both surfaces. The renderer multiplies by its own cell
/// size —that part is its own— and paints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutView {
    /// The size that was laid out, in cells.
    pub cells: (u16, u16),
    /// The slots that are painted, in paint order. A slot not here either
    /// does not fit or is an inactive tab: it is not painted, and that was
    /// decided by the same layout engine the TUI uses.
    pub placements: Vec<SlotPlacement>,
    /// The TABS of each group on screen.
    ///
    /// Kept apart from `placements` because an inactive tab is NOT placed —
    /// its content is not painted— and yet it must be shown to exist: a
    /// window with three tabs that only shows the front one and does not say
    /// there are two more is a window hiding open work.
    pub tabs: Vec<TabGroupView>,
    /// Whether the TARGET mark says anything with the listings currently
    /// visible.
    ///
    /// Whether the role EXISTS and whether it is PAINTED are two questions.
    /// [`SlotPlacement::role`] answers the first, which is the model; this
    /// is the second, and it travels precomputed because it is decided by
    /// the shared crate (`layout::target_worth_marking`) and not the
    /// renderer: written there it was a number duplicated in TypeScript,
    /// i.e. the same decision in two places that this field exists to stop
    /// having (ADR 0077).
    ///
    /// With two listings the target is "the other one" and nobody needs to
    /// be told; a mark that always shows stops being read, and then the day
    /// there are three and a copy toward the one the engine's tiebreak
    /// picks is silent data loss (ADR 0058 D7).
    ///
    /// `#[serde(default)]`: absent = `false`, which is not marking. The safe
    /// direction, because an extra mark is the one that teaches ignoring
    /// it.
    #[serde(default)]
    pub mark_target: bool,
}

/// A group of tabs and which one is in front.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabGroupView {
    /// The PLACED slot this group belongs to: the active tab's, which is
    /// the one the renderer is painting.
    pub slot_id: u32,
    /// Its tabs, in the tree's order.
    pub tabs: Vec<TabView>,
    /// Which one is in front, as an index into `tabs`.
    pub active: u64,
    /// Is a group of PANELS (phase F, bridge 88), not of listings: panels
    /// on the same edge share a spot. No `+`: a new tab is a listing, and
    /// does nothing in a panel group. Absent in an older host = of
    /// listings.
    #[serde(default)]
    pub panels: bool,
}

/// A tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabView {
    /// The slot inside. Comes back when chosen with the mouse.
    pub slot_id: u32,
    /// Its label: its listing's directory name, already masked —a
    /// directory with a hostile name inside a tab is as hostile as inside a
    /// listing—. For something that is not a listing, its kind's name.
    pub title: String,
    /// The label is painted different from what it is.
    pub title_hostile: bool,
}

/// A placed slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotPlacement {
    /// Slot id.
    pub slot_id: u32,
    /// Column of the top-left corner, in cells.
    pub x: u16,
    /// Row of the top-left corner, in cells.
    pub y: u16,
    /// Width in cells.
    pub width: u16,
    /// Height in cells.
    pub height: u16,
    /// Its role NOW, if it has one.
    pub role: Option<SlotRole>,
    /// Tab order. The renderer does not compute it: moving focus with tab
    /// is the same rule on both surfaces.
    pub focus_index: u32,
}

/// A slot's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotRole {
    /// Has keyboard focus.
    Active,
    /// Is the TARGET of an operation that needs a second spot.
    Target,
}

/// Connection state, as painted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ConnectionView {
    /// Talking to the daemon.
    Connected,
    /// Was lost and is being retried.
    Reconnecting,
    /// No connection and no retry.
    Lost {
        /// Fluent key of the reason.
        reason_key: String,
    },
}

/// A layout slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SlotView {
    /// A listing.
    ///
    /// Boxed: a listing with its row window is an order of magnitude
    /// bigger than an unprojected slot, and an enum sizes what its biggest
    /// variant costs into every `Vec<SlotView>` that gets built.
    Browser(Box<BrowserSlotView>),
    /// The attribute sheet: what the listing already knows about the entry
    /// under the cursor of the pane this slot follows.
    ///
    /// **Reads nothing.** The `Entry` is already in the listing, and a pane
    /// that followed the cursor requesting data per row would turn walking
    /// down a directory into a storm of requests.
    Metadata(Box<MetadataSlotView>),
    /// The places sidebar: the host's volumes and the user's favorites,
    /// with its cursor.
    Places(Box<PlacesSlotView>),
    /// The directory tree: which branches are open and which has the
    /// cursor.
    ///
    /// **Directories only**, and **lazy**: expanding a branch lists THAT
    /// directory and nothing else. A tree read whole on opening would take
    /// minutes on a large `$HOME` and hours against a remote.
    Tree(Box<TreeSlotView>),
    /// The processes panel: the SAME tasks the strip paints, with its own
    /// cursor.
    ///
    /// Keeps no second copy: two task lists drift apart, and the one you
    /// see stops being the one that gets cancelled.
    Processes {
        /// Slot id.
        slot_id: u32,
        /// Which row has the cursor, if any.
        cursor: Option<u64>,
    },
    /// The log panel: what this process is logging (#326).
    Log(Box<LogSlotView>),
    /// The TERMINAL panel (#362, bridge 95): a shell's grid.
    ///
    /// What crosses are ALREADY PAINTED ROWS, not the pty's bytes. The
    /// emulation —bytes to cells— is done once by `norte-term`, on the same
    /// side the terminal does it, so both frontends show the same thing by
    /// construction and not because someone compares two emulators.
    Terminal(Box<TerminalSlotView>),
    /// The DOCKED viewer (#291, bridge 51): the file under the cursor of
    /// the listing this slot follows, read-only. Kind `viewer` in the
    /// layout; `preview` on the wire, which is what it is.
    Preview(Box<PreviewSlotView>),
    /// The panel a PLUGIN paints (phase 3): the frame its guest described.
    Panel(Box<PanelSlotView>),
    /// The disk map (phase 4): what the directory is made of, laid out into
    /// rectangles.
    ///
    /// The layout is done by the HOST with `norte_frontend::treemap::squarify`,
    /// not the renderer: a treemap computed twice is two different treemaps
    /// the moment someone touches a rounding (ADR 0077). What crosses are the
    /// already styled lines and their zones, just like a plugin panel.
    DiskMap(Box<DiskMapSlotView>),
    /// The journal timeline (phase 7, #359, bridge 78): what has been done
    /// on this machine, newest to oldest, with the cursor on the point it
    /// would revert to.
    ///
    /// The rows, how a batch is grouped and what a cut would take are
    /// decided by `norte_frontend::timeline`, the same model as the TUI.
    Timeline(Box<TimelineSlotView>),
    /// A slot of a kind this host does not project yet. Shown empty and
    /// with its name: preserving what is not understood is the session's
    /// rule (ADR 0059), and disappearing would be worse than being grayed
    /// out.
    Unsupported {
        /// Slot id.
        slot_id: u32,
        /// Kind's name, to say it. Written by the user's layout, so it
        /// travels masked.
        kind_name: String,
        /// The painted name differs from the one in the file (#266).
        kind_name_hostile: bool,
    },
}

/// The docked viewer (#291): what a `viewer` slot shows.
///
/// The SAME [`ViewerView`] as the full-screen viewer —it is the same viewer
/// elsewhere, as in the TUI—, with two differences that belong to the link,
/// not the content: it follows the listing's cursor instead of opening with
/// a key, and the lines arrive WHOLE up to the bridge's cap so the slot can
/// scroll them on its own, because it has no viewer keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewSlotView {
    /// Slot id.
    pub slot_id: u32,
    /// The viewer with what was read, or `None` if there is no file to
    /// show.
    pub viewer: Option<ViewerView>,
    /// Why there is no file, ALREADY SAID: a directory, nothing under the
    /// cursor, a read error. Empty when there is a viewer.
    pub note: String,
}

/// The panel a PLUGIN paints (phase 3): what its guest described.
///
/// The guest does not draw, it DESCRIBES: styled lines and clickable zones.
/// The border, the title and focus are set by the window, which is what
/// stops a plugin from impersonating another panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelSlotView {
    /// Slot id.
    pub slot_id: u32,
    /// Which panel it is: the `<kind>`, without the prefix, MASKED and
    /// scoped.
    ///
    /// Declaring it requires an alphabet (`KindRegistry::insert_panels`),
    /// but this does not come from there: it comes from the TREE, which can
    /// come from a layout file or from the session, and a hand-written kind
    /// has had nothing required of it by anyone. Treated like the name of
    /// any kind the host does not know.
    pub title: String,
    /// The frame's lines, each with its spans. Empty while the first frame
    /// has not arrived, or if the plugin failed: the slot is painted with
    /// its border and nothing inside, never blank with no frame.
    pub lines: Vec<Vec<SpanView>>,
    /// The clickable zones, in cells INSIDE the frame.
    pub hits: Vec<HitView>,
}

/// The disk map (phase 4): the treemap already laid out, ready to paint.
///
/// Same layout as a plugin panel —styled lines and zones in cells INSIDE
/// the frame— and for the same reason: the renderer paints what it is given
/// and says WHERE it was clicked; who each rectangle is is resolved by the
/// host against its own frame.
///
/// It matters more here than there, because what is resolved is a file's
/// NAME: sending it over the wire would force choosing between the painted
/// form —masked, which identifies nothing— and the reversible one, and it
/// would be a name anyone talking to the renderer could send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskMapSlotView {
    /// Slot id.
    pub slot_id: u32,
    /// Which directory is being described, for the title. Masked and
    /// scoped: comes from a file name.
    pub title: String,
    /// The painted name differs from the one on disk (#266).
    pub title_hostile: bool,
    /// The treemap's lines, each with its spans. Empty while nothing has
    /// been measured: the slot is painted with its border and nothing
    /// inside.
    pub lines: Vec<Vec<SpanView>>,
    /// One rectangle per zone, in cells INSIDE the frame.
    pub hits: Vec<HitView>,
    /// The measurement is still running.
    ///
    /// Travels because a half-done map without saying so reads as a small
    /// directory, which is the wrong answer and a believable one at that.
    pub measuring: bool,
}

/// The journal timeline (#359, bridge 78).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSlotView {
    /// Slot id.
    pub slot_id: u32,
    /// The panel's title, already translated.
    pub title: String,
    /// The rows, newest to oldest. A batch is ONE row.
    pub rows: Vec<TimelineRowView>,
    /// The row with the cursor: the point it would revert to.
    pub cursor: Option<u64>,
    /// What is said when there are no rows, already translated: "nothing
    /// has been done yet" only once it has been CHECKED, "loading" before
    /// that, and the reason if there is no history to show. An empty panel
    /// with no explanation reads as "you haven't done anything," which is a
    /// different claim.
    pub empty: String,
    /// What an `Enter` here would take, already translated; empty with no
    /// rows. It is the only number that matters before pressing.
    pub footer: String,
}

/// A row of the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineRowView {
    /// The time, already formatted.
    pub time: String,
    /// Who: `user`, `agent`, `plugin`… For the dot's COLOR: what it
    /// separates is "me" from "something in my name."
    pub actor: String,
    /// The verb.
    pub op: String,
    /// On what, as the server painted it (already masked).
    pub path: String,
    /// The server had to mask `path`.
    pub hostile: bool,
    /// What sets it apart, already translated: how many entries it brings
    /// if it is a batch, and whether it has no way back. Empty if nothing.
    pub tail: String,
}

/// A plugin panel's clickable zone: where it is, and nothing more.
///
/// **Without its command, on purpose.** The renderer says WHERE was
/// clicked and the host resolves which zone it was and which command
/// applies, with the same filter as the terminal. It is this window's rule
/// —the renderer reports what happened, the host decides what it means—,
/// and here it also closes a door: a command that traveled over the wire
/// would be a command anyone talking to the renderer could send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HitView {
    /// Row inside the frame, counting from zero.
    pub row: u16,
    /// Column where it starts.
    pub col: u16,
    /// How many cells it spans in width.
    pub width: u16,
}

/// The terminal panel (#362, bridge 95): what the shell has painted.
///
/// **It is FOREIGN content**, and that is why it does not look like the
/// other panels: it carries not a single theme role. What a program paints
/// inside is its own, and a theme that changed the colors of an `ls
/// --color` would be lying about what that program said. Ours is the
/// frame, set by the renderer.
///
/// What whoever sends it does guarantee is the same the grid guarantees: no
/// control byte can have ended up in a cell, because the parser eats the
/// escapes and drops the C0s that do not move the cursor. That is why these
/// strings do not go through masking again: there is nothing left to mask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSlotView {
    /// Slot id.
    pub slot_id: u32,
    /// The rows, top to bottom, each with its spans.
    ///
    /// ALWAYS all the grid has: a terminal does not scroll like a list, it
    /// repaints, and sending "from row N" would force the renderer to keep
    /// a copy that can fall out of sync.
    pub rows: Vec<Vec<TerminalSpanView>>,
    /// Where the cursor is: row and column, from zero.
    ///
    /// `None` = not painted, and these are two cases the renderer treats
    /// alike: the shell hid it (`CSI ?25l`, what any full-screen program
    /// does while painting) or the keyboard is not on this panel.
    pub cursor: Option<(u16, u16)>,
    /// There is no shell: it exited, or could not be started.
    ///
    /// The slot stays as is, and the renderer says so. Closing it on its
    /// own would move someone's layout without them having touched it.
    #[serde(default)]
    pub no_shell: bool,
}

/// A span of a terminal row: text with what the shell requested.
///
/// Its own type and not [`SpanView`], and the reason is one field: a
/// terminal says "color 4," and which blue that is is decided by the
/// painter's palette. `SpanView` only knows theme roles and hex colors, so
/// putting it there would force resolving the index HERE — and then the
/// panel would stop obeying the reader's theme, with no way to fix it from
/// the theme. It also carries attributes (bold, underline…) `SpanView` does
/// not have.
// All six are independent SGR flags: the shell sets and clears them one at
// a time (`SGR 1` / `SGR 22`), so a struct of bools IS that representation.
// Same criterion as `norte_theme::Style` and `norte_term::Style`, from
// which this one is translated field by field.
#[expect(
    clippy::struct_excessive_bools,
    reason = "six independent SGR attributes, as sent by the shell"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSpanView {
    /// The span's text.
    pub text: String,
    /// Text color. Absent = the painter's normal one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<TerminalColorView>,
    /// Background color. Absent = the painter's normal one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<TerminalColorView>,
    /// `SGR 1`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    /// `SGR 2`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dim: bool,
    /// `SGR 3`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    /// `SGR 4`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    /// `SGR 7`: colors are swapped WHEN PAINTING, not here. Resolving it
    /// earlier would lose which was which, and `SGR 27` has to be able to
    /// undo it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reverse: bool,
    /// `SGR 9`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strike: bool,
}

/// A color exactly as the shell SAID it, unresolved.
///
/// The two cases are the two that exist on a terminal's wire, and are kept
/// distinct on purpose: see [`TerminalSpanView`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TerminalColorView {
    /// One of the palette's 256; 0 through 15 are the "usual" ones.
    Indexed {
        /// The index.
        index: u8,
    },
    /// An exact one, chosen by the program (`CSI 38;2;r;g;b m`), in
    /// `#rrggbb`.
    Rgb {
        /// The color, in hex with a hash sign.
        hex: String,
    },
}

/// The log panel (#326): the visible window of the in-memory ring.
///
/// Only the WINDOW, like the listing: a two-thousand-line ring sent whole
/// on every patch is the waste decision D7 exists to avoid, and the log
/// moves more than a directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSlotView {
    /// Slot id.
    pub slot_id: u32,
    /// The visible lines, top to bottom and already sanitized.
    pub lines: Vec<LogLineView>,
    /// Up to which level is being SHOWN, in its wire form.
    ///
    /// Closed vocabulary (`error`, `warn`, `info`, `debug`, `trace`) and not
    /// the translated label: the renderer marks which one is set, and
    /// comparing translated sentences for that would force the renderer to
    /// know the host's language.
    pub level: String,
    /// That same level exactly as PAINTED (`TRACE`). See
    /// [`LogLineView::level_label`]: the one above is compared, this one is
    /// read.
    #[serde(default)]
    pub level_label: String,
    /// The current text filter, masked and scoped. Empty = everything.
    ///
    /// Typed by the reader, so it can carry controls and direction marks:
    /// it is text to paint like any other.
    pub filter: String,
    /// Is stuck to the end and follows what arrives.
    ///
    /// Said because it is the difference between "nothing's happening" and
    /// "you've scrolled away and this is history": without it, a panel
    /// still during a long operation reads the same in both cases.
    pub following: bool,
    /// How many lines pass the filter, to be able to place the window.
    pub total: u64,
    /// Index of the first line travelling in `lines`, among the filtered
    /// ones.
    pub first_visible: u64,
    /// How many lines were lost, and from WHICH ring, already SAID.
    ///
    /// Said out loud: a log with a silent hole lies about what happened,
    /// and the absence of a line is indistinguishable from the event never
    /// occurring.
    ///
    /// With both sources visible (#328) these are **two numbers, not one**,
    /// each naming its ring, because they do not mean the same thing nor
    /// live the same lifetime: the window's counts what its ring has
    /// evicted since the process started and never resets; the daemon's
    /// counts what THIS opening of the panel lost. Adding them gave a
    /// number that was neither.
    ///
    /// Translated here with the NUMBER inside, not a `u64` for the renderer
    /// to compose the sentence: a renderer does not translate or
    /// substitute numbers. Same rule as `BrowserSlotView::skipped_note`.
    /// Empty = none.
    pub dropped_note: String,
    /// Which ring is CAPTURING more than what is shown, and up to where.
    /// Already translated; empty = none.
    ///
    /// Exists because the two levels are kept separate on purpose —lowering
    /// what is shown does not stop capturing, and raising it back would
    /// show a hole— and then the panel can say "info" while the process
    /// keeps TRACE in memory. Whoever is looking has the right to know more
    /// is being collected than they see, especially before taking a
    /// screenshot.
    ///
    /// And since #328 it is also where the DAEMON's level is stated, naming
    /// it: its own is global to all its clients, another one could have
    /// raised it and it never lowers, so it can be well above what this
    /// panel shows. It does not fit in [`Self::level`] —that is the one
    /// that FILTERS the list and the one the buttons move— and putting it
    /// there would have left a level marked as set that the panel was not
    /// applying.
    pub capturing: String,
    /// Which PROCESS these lines belong to, already translated.
    ///
    /// Exists because in the window the answer is not obvious and, worse,
    /// not the one you'd expect: `norte-gui` starts its own daemon (#300),
    /// so this ring carries the WINDOW process's, **not** the daemon's,
    /// which is where the interesting half happens —the providers, the
    /// journal, the policy. In the embedded TUI they are the same process
    /// and it goes unnoticed.
    ///
    /// Staying silent about it would make the panel look broken: someone
    /// opens the log while a connection fails, does not see the line that
    /// explains it, and concludes the panel does not work instead of that
    /// they are looking at another process. Since #328 the daemon's lines
    /// also arrive, and this says which ones are visible.
    pub source: String,
    /// The EFFECTIVE source, in closed vocabulary: `window`, `daemon` or
    /// `both` (#328).
    ///
    /// Effective and not the saved preference: without a second ring on
    /// the other side —a daemon without the `logging` feature— the `both`
    /// preference is shown as `window`, because that is what the reader is
    /// actually looking at. A panel that said "both" over one ring's lines
    /// alone would lie exactly where it costs most: whoever opens the log
    /// looking for what they can't find.
    ///
    /// Closed and untranslated, like `level`: the renderer marks which one
    /// is set, and comparing translated sentences for that would tie it to
    /// the language.
    pub source_mode: String,
    /// There really is a SECOND source to offer.
    ///
    /// `false` while the daemon has never answered its own log, and then
    /// the picker is not painted: offering three sources where there is
    /// only one is a control that does nothing, which is worse than not
    /// having it.
    pub sources_available: bool,
    /// What needs to be said about the source, already translated. Empty =
    /// nothing.
    ///
    /// Two sentences, and they are exclusive. That the daemon **has no log
    /// to serve**, which is half of #326 applied to the other shore: the
    /// panel falls back to the local ring and says so, instead of staying
    /// silent. And, when what is shown is the daemon's, **whose level it
    /// is**: it is global to the process, another client could have raised
    /// it, and it only goes up — so the number next to it is not "what you
    /// asked for," and staying silent would leave the reader believing
    /// their request was applied as is.
    pub source_note: String,
}

/// A log line, already ready to paint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLineView {
    /// The time `HH:MM:SS`, in UTC.
    ///
    /// UTC and not local, same as the date column in ISO: this tree carries
    /// no timezone database, and a local time invented from a fixed offset
    /// would be a lie twice a year. What is compared here are lines against
    /// each other, and for that the zone does not matter as long as it is
    /// the same one.
    pub time: String,
    /// The level, in its wire form — the renderer colors it by this.
    ///
    /// It is an IDENTITY, not text: it is compared, not painted. What is
    /// painted is [`Self::level_label`].
    pub level: String,
    /// The level exactly as PAINTED (`TRACE`), which is what the terminal
    /// paints.
    ///
    /// Separate from the one above because they are two things: a stable
    /// identity the renderer uses to color and a label meant to be read.
    /// Painting the identity is what had the window showing `trace` in the
    /// lines, `trace` in the title chip and a translated word in its
    /// buttons — three vocabularies for the same level, all three on screen
    /// at once.
    ///
    /// NOT translated, and that is the decision: `TRACE` is what is
    /// written in `RUST_LOG`, what shows up pasted into a bug report and
    /// what someone will scan for by eye in a long list. The window's level
    /// BUTTONS are translated: they are a control, not data, and the
    /// terminal has none to disagree with.
    ///
    /// `#[serde(default)]`: empty = an older bridge, and then the renderer
    /// falls back to the identity, which is what it used to paint.
    #[serde(default)]
    pub level_label: String,
    /// The module that emitted it, masked and scoped.
    pub target: String,
    /// The message, masked and scoped.
    ///
    /// Masked like any other text that is painted, and here for a reason of
    /// its own: a log message can carry inside a file name someone chose,
    /// and a `U+202E` there reorders the panel's whole line.
    pub message: String,
    /// What is painted differs from what is there, in the module or the
    /// message.
    pub hostile: bool,
    /// Which PROCESS it came from: `window` or `daemon` (#328).
    ///
    /// Per line and not only in the header, because in a mixed list it is
    /// half the information: "the provider failed" and "the window
    /// couldn't paint it" read the same without knowing who wrote it, and
    /// they are two different failures. Closed and untranslated: the
    /// renderer marks the row, it does not read it aloud.
    pub source: String,
}

/// Into how many equal spans a listing's mark ruler is split (ADR 0135).
///
/// More than the screen rows of any reasonable window, so each span falls
/// on one or two pixels of the ruler; and fixed, so ten thousand marks do
/// not cross the bridge as ten thousand numbers.
pub const MARK_RULER_SPANS: u16 = 256;

/// A slot's listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserSlotView {
    /// Slot id.
    pub slot_id: u32,
    /// Rises on every re-listing. A [`RowKey`] from another generation is
    /// stale.
    pub generation: u64,
    /// Work arriving AT THIS directory, 0–100 (ADR 0148, bridge 94): a thin
    /// line on the panel's edge, like a browser's loading bar. `None` =
    /// nothing to paint. Moves through its own change
    /// ([`ViewChange::SlotProgress`]), not by resending the listing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    /// The location, already sanitized for painting.
    pub path_display: String,
    /// The text above DIFFERS from the real path (non-UTF-8 bytes, masked
    /// controls). The renderer MUST mark it.
    pub path_hostile: bool,
    /// Rows in the directory, if known.
    pub total_rows: Option<u64>,
    /// First row travelling in `rows`.
    pub first_visible: u64,
    /// The visible window's rows (plus whatever overscan the renderer
    /// requests). NEVER the whole directory.
    pub rows: Vec<RowView>,
    /// The icon column is open in this listing (bridge 62, ADR 0105): SOME
    /// of its entries —visible or not— have an icon, so every row carries
    /// the cell, empty or not, and names stay aligned. Decided by the host
    /// from the whole pane; a renderer that deduced it from visible rows
    /// would close the column when scrolling to an icon-free page and run
    /// every name together.
    pub icon_column: bool,
    /// Row under the cursor, if any.
    pub cursor: Option<RowKey>,
    /// How many rows are marked in the slot (not only in the window).
    pub marks: u64,
    /// Which spans of the listing carry a mark (bridge 89, ADR 0135): the
    /// ruler next to the scrollbar, to see where the marks the window does
    /// not show are. The listing is split into [`MARK_RULER_SPANS`] equal
    /// spans by position; bounded by that and not by the number of marks.
    /// Empty with no marks.
    #[serde(default)]
    pub mark_ruler: Vec<u16>,
    /// How many entries the provider skipped, already SAID in the reader's
    /// language. Empty = none, or the provider does not keep count.
    ///
    /// Said on screen because this is the kind of failure that cannot be
    /// discovered by looking: what is missing is not there, and there is no
    /// row for the reader to stumble on it. An incomplete listing that says
    /// nothing lies by omission.
    ///
    /// Translated HERE, like the search's status sentence: a renderer does
    /// not translate, and "skipped one" and "skipped 3" are not said the
    /// same in every language.
    pub skipped_note: String,
    /// How many entries hiding is SETTING ASIDE, already said in the
    /// reader's language. Empty = none, or hiding is off.
    ///
    /// Permanent and not a status bar message: `pane.toggle-hidden`'s notice
    /// is overwritten by the next key, and then a listing showing less than
    /// what exists goes silent. Same discipline as [`Self::skipped_note`],
    /// and translated here for the same reason — "1 hidden" and "3 hidden"
    /// are not said the same in every language.
    pub hidden_note: String,
    /// Names are being REINTERPRETED with another encoding (#57). Empty =
    /// no.
    ///
    /// Same discipline as the two above, and that is why it is here and not
    /// in the status bar: what is painted is not the bytes on disk, and that
    /// must be knowable at the moment of deciding to copy or delete
    /// something. The toggle's message gets overwritten by the next key.
    ///
    /// `#[serde(default)]` does NOT promise compatibility with an older
    /// bridge —the renderer rejects any version that is not its own—: it is
    /// here so fixtures and round-trips do not have to enumerate fields
    /// that are almost always empty.
    #[serde(default)]
    pub names_note: String,
    /// The listing is STILL FILLING, and how many so far. Empty = whole.
    #[serde(default)]
    pub filling_note: String,
    /// Marks the last refresh dropped because their entry is no longer
    /// there. Empty = none dropped.
    #[serde(default)]
    pub pruned_note: String,
    /// How many entries are marked and how much they weigh, already said.
    /// Empty = no marks.
    #[serde(default)]
    pub marked_note: String,
    /// The path's BREADCRUMBS (bridge 65): the root (`⟨file⟩`, `⟨sftp⟩host`)
    /// and one segment per directory, each already masked. Clicking segment
    /// `depth` navigates to the directory with those `depth` segments
    /// (`breadcrumb_activate`). Empty = the path goes whole in
    /// `path_display`.
    #[serde(default)]
    pub path_segments: Vec<String>,
    /// How much of the volume is USED, in `0.0..=1.0` (bridge 65): the
    /// footer's space indicator. `None` = unknown (no volume, or a scheme
    /// that does not say).
    #[serde(default)]
    pub used_ratio: Option<f32>,
    /// The listing's footer (spec 2026-09-10): how many directories and
    /// files, how much they weigh, what is marked and the volume's free
    /// space, already worded. Empty = `[ui] pane_footer` off.
    #[serde(default)]
    pub footer: String,
    /// The configured columns' headers, in their order. Includes the name,
    /// which travels separately in the rows (`display_name`).
    pub columns: Vec<ColumnHeader>,
    /// What the slot is doing.
    pub state: SlotState,
    /// The incremental search, if open. While it is, text keys are ITS
    /// OWN: it is the listing's input context.
    pub quick: Option<QuickView>,
}

/// A listing's incremental search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickView {
    /// What was typed, already sanitized for painting.
    pub query: String,
    /// Filters the listing (`filter`) or jumps to the first match (`jump`).
    pub mode: String,
    /// How many rows match. With zero, the renderer says so: a search that
    /// finds nothing and does not show it looks broken.
    pub matches: u64,
}

/// What is happening to a listing right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SlotState {
    /// Complete listing, at rest.
    Ready,
    /// Requesting the first page, or filling the rest.
    ///
    /// Carries WHERE it is going, which is the missing half. The body keeps
    /// showing the PREVIOUS listing —on purpose: if the connection fails,
    /// the reader stays where they were— and without the destination that
    /// mix cannot be read: the screen shows one place while working on
    /// another, with no way to tell which. The terminal has put the
    /// destination in the header next to the spinner since #323.
    ///
    /// The THRESHOLD —nothing before 250 ms, because below that the
    /// operation finishes before the eye registers it and all you'd see is
    /// a flicker— is the renderer's business, and that is where it belongs:
    /// it is a purely visual delay, and in CSS it costs neither a timer nor
    /// a message.
    Loading {
        /// The VERB's Fluent key, from the shared CLOSED vocabulary
        /// (`norte_frontend::busy::BusyKind`): `busy-connecting`,
        /// `busy-listing`, `busy-opening`.
        ///
        /// From there and not a key of its own because that module exists
        /// since #323 precisely so both frontends do not say different
        /// things about the same wait — and this window used to say
        /// "loading…" even for a remote connection, the case that exposed
        /// it.
        #[serde(default)]
        verb_key: String,
        /// The path it is going to, already paintable and with the current
        /// reinterpretation. Empty = padding for the place already there,
        /// which goes nowhere.
        #[serde(default)]
        target_display: String,
        /// That path DIFFERS from the real bytes.
        #[serde(default)]
        target_hostile: bool,
    },
    /// The listing failed. The Fluent key says why; the detail already
    /// arrives sanitized and scoped.
    Error {
        /// Fluent key of the category.
        reason_key: String,
        /// Already sanitized detail, if any.
        detail: Option<String>,
    },
}

/// A row of the listing.
// Four bools, and each is an INDEPENDENT fact the renderer paints
// differently: the name differs from the real one, it is under the cursor,
// it is marked, its badge differs from the real one. It is not a state that
// can be folded — the lint targets parameters and state machines, not a
// wire row.
#[expect(
    clippy::struct_excessive_bools,
    reason = "wire row: independent badges, not a state machine"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowView {
    /// Opaque key, valid for this generation.
    pub key: RowKey,
    /// Name ready to paint.
    pub display_name: String,
    /// The painted name differs from the real one: lossy bytes or masked
    /// controls (spec §6). The renderer MUST mark it.
    pub hostile: bool,
    /// How far along the task working on THIS row is, 0–100 (bridge 69,
    /// spec 2026-09-15). `None` = no task is touching it.
    ///
    /// Resolved by the host with `processes::progress_for`, which matches
    /// by exact path: a copy INSIDE a directory does not paint the
    /// directory as half-done, because "half of this folder" is not what
    /// the number says.
    #[serde(default)]
    pub progress: Option<u8>,
    /// What it is.
    pub kind: RowKind,
    /// Under the cursor.
    pub selected: bool,
    /// Marked to operate on.
    pub marked: bool,
    /// Cells of the configured columns, in the header's order.
    pub cells: Vec<CellView>,
    /// The badge a plugin put on this row, already masked and scoped. Empty
    /// = none.
    ///
    /// Cosmetic by contract (ADR 0037): a decorator that does not answer,
    /// or a fallen catalogue, leave the row without a badge and the listing
    /// unaffected.
    pub badge: String,
    /// The badge is painted DIFFERENT from what it is. Written by a plugin
    /// and attached to a file name.
    pub badge_hostile: bool,
    /// The semantic role the plugin requested for the row (`warning`,
    /// `error`…), from `norte-theme`'s CLOSED vocabulary. Empty = none.
    ///
    /// A name not in the vocabulary arrives empty, not raw: the renderer
    /// uses it to pick a theme color, and a free string there would be a
    /// plugin choosing its own style.
    pub badge_role: String,
    /// The row's ICON (bridge 62, ADR 0105): what an `icon` slot decorator
    /// put, already masked and scoped. Empty = none. Painted to the LEFT of
    /// the name in a fixed-width column, which the renderer opens on every
    /// row of the slot as soon as one has it.
    pub icon: String,
    /// The icon is painted different from what it is. Same reason as the
    /// badge.
    pub icon_hostile: bool,
    /// The `#rrggbb` color the THEME paints this entry's name with, from
    /// `[files.ext]` (wins) or `[files.kind]`. Empty = the theme says
    /// nothing about it and the renderer uses the listing's normal color.
    ///
    /// Travels RESOLVED and not as a rule name because extensions are an
    /// OPEN set: a theme colors whichever it wants, so there is no list of
    /// classes the renderer could know in advance. The opposite of
    /// [`RowView::badge_role`], which is closed vocabulary.
    #[serde(default)]
    pub name_color: String,
    /// The name is in BOLD (a directory, an executable). Same style family
    /// as `name_color`.
    #[serde(default)]
    pub name_bold: bool,
    /// Dimmed. Retro presets dim compressed files this way, and without
    /// this field they came out dim in the terminal and at full brightness
    /// in the window.
    #[serde(default)]
    pub name_dim: bool,
    /// Italic.
    #[serde(default)]
    pub name_italic: bool,
    /// Underlined.
    #[serde(default)]
    pub name_underline: bool,
}

/// The header of ONE column.
///
/// The label arrives TRANSLATED and sanitized (`columns::header_label`, the
/// same one the TUI paints): a renderer does not translate, and a plugin's
/// header is foreign text that already arrives masked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnHeader {
    /// The column's stable id (`name`, `size`, `attr:posix.mode`…). It is
    /// what is sent back to sort: the renderer does not name columns by
    /// their position or their label.
    ///
    /// An IDENTITY, so it travels WHOLE or EMPTY: neither masked nor
    /// truncated. Both break it —masking is not injective and two
    /// configured columns could collapse into one, truncating left it not
    /// matching its own— and neither is needed: what paints it is `label`,
    /// and the renderer only puts the id into a `data-` attribute.
    pub id: String,
    /// Already translated label.
    pub label: String,
    /// `asc`/`desc` if the listing is sorted by THIS column; `None` if not.
    pub sort: Option<String>,
    /// The column sorts. One that does not is painted without a click
    /// affordance.
    pub sortable: bool,
    /// FIXED width in cells, if `[ui.columns] spec.width` sets it (bridge
    /// 64): the column's header and cells follow it, and dragging the
    /// header's edge changes it. `None` = whatever its content measures.
    /// For the `name` column it is its FLOOR (`columns::NAME_MIN`), not a
    /// width: the name grows, and below that the renderer drops columns.
    #[serde(default)]
    pub width: Option<u16>,
    /// `left` or `right`: the column's configured alignment, the same the
    /// terminal applies. Only has an effect with a fixed width.
    #[serde(default)]
    pub align: String,
}

/// An entry's kind, as far as painting cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    /// Directory.
    Dir,
    /// File.
    File,
    /// Symbolic link.
    Symlink,
    /// Anything else the provider reports.
    Other,
}

/// A column's value, already formatted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellView {
    /// Id of the column it belongs to.
    pub column: String,
    /// Already formatted and sanitized text. `None` = not known (yet).
    pub text: Option<String>,
}

/// The status bar.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatusView {
    /// Ephemeral message, already translated by the host.
    pub message: Option<String>,
    /// Persistent notices (degradation, journal, session), scoped.
    pub banners: Vec<BannerView>,
    /// Notices that expired without the reader opening the log (spec
    /// 2026-09-10, `[ui] notice_seconds`). The renderer paints a badge
    /// while there is one; clicking it opens the log panel via the panel
    /// bar's button. Opening it resets it to zero.
    #[serde(default)]
    pub notices_unread: u32,
    /// What is half-typed: a sequence, a counter, or both. ALWAYS painted
    /// while it exists — a pending prefix that is not visible is a prefix
    /// that cannot be cancelled.
    pub pending: Option<PendingView>,
}

/// The sync panel: the PLAN, before anything is written.
///
/// Windowed like the diff panel and for the same reason: a half-million-step
/// plan does not cross the whole bridge. And like the diff panel, steps are
/// named by their `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncView {
    /// The SOURCE root, already sanitized and with its flag.
    pub source: DialogLine,
    /// The DESTINATION root, already sanitized and with its flag.
    pub dest: DialogLine,
    /// The requested mode (`update` or `mirror`), by stable id.
    ///
    /// Painted BEFORE approving and is not decoration: a `mirror` DELETES
    /// at the destination and an `update` does not.
    pub mode: String,
    /// The steps window.
    pub steps: Vec<SyncStepView>,
    /// Index of the first step travelling.
    pub first_visible: u64,
    /// How many steps the plan has, INCLUDING the ones the list does not
    /// retain.
    ///
    /// The shared model bounds how many bodies it keeps and counts the
    /// dropped ones separately; adding them here is what keeps this number
    /// and the status line's from contradicting each other on a large plan.
    pub total: u64,
    /// The plan's SUMMARY, already said: how many irreversible, how many
    /// bytes, what could not be read, and whether the list hides steps.
    ///
    /// What a human needs before approving, and it does not fit in the
    /// status line: an approvable plan with three irreversible steps and an
    /// unreadable branch used to read as "5 steps, press approve."
    pub summary: Vec<String>,
    /// What PREVENTS syncing, already said, WITH its path. Empty = nothing
    /// prevents it.
    pub blockers: Vec<SyncBlockerView>,
    /// How many blockers there REALLY are.
    ///
    /// The wire truncates the list, and the total travels separately on
    /// purpose: a human needs to know there are forty thousand even if only
    /// two hundred fifty-six are shown.
    pub blockers_total: u64,
    /// The status, already said: planning, ready to approve, applying…
    pub status: String,
    /// What can be done now, already said (the footer's help line).
    pub hint: String,
    /// The SECOND question, already worded, when the plan is dangerous.
    ///
    /// `None` = approval has not been requested yet, or this plan does not
    /// need it (everything can be undone and it deletes no trees). Composed
    /// by the shared model, with a branch per undo perspective: a headline
    /// saying "some of this can be undone" over a confirmation saying
    /// "nothing" teaches skipping both.
    pub confirming: Option<String>,
    /// The steps that FAILED, once the sync finished.
    ///
    /// The count goes in the status line; this is the detail: which path
    /// and why. The Task's outcome says whether it ran, and what did not
    /// happen is counted only by the report.
    pub failures: Vec<SyncFailureView>,
    /// STOPPING what is running has already been requested.
    ///
    /// Travels because otherwise pressing `Escape` during writing changes
    /// not a single letter on screen: there is no way to distinguish "I
    /// heard you" from "this key does nothing," which is exactly what
    /// pushes someone to press it again.
    pub cancel_requested: bool,
    /// The plan can be approved NOW.
    ///
    /// Decided by the shared model: an unfinished plan, one with blockers,
    /// or one already submitted, is not approved — and a footer offering to
    /// approve what the model is going to reject is the broken screen this
    /// avoids.
    pub can_approve: bool,
    /// A Task is running (the plan's, or the application's).
    pub running: bool,
}

/// A step that failed while applying the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFailureView {
    /// Why it failed, already translated.
    pub cause: String,
    /// On which path, already sanitized.
    pub path: String,
    /// What is painted differs from the bytes.
    pub path_hostile: bool,
    /// Which root the path hangs from (`source`, `dest` or `either`), by
    /// stable id — for styling, not for reading.
    pub anchor: String,
    /// The same, already translated and meant to be PAINTED. Empty =
    /// nothing to say.
    ///
    /// Travels alongside the id because the id is not read: in a panel
    /// where an unqualified path means "from the source," staying silent
    /// about an `either` asserts the source, and a `data-` attribute no
    /// style looks at silences it just the same.
    pub anchor_label: String,
}

/// Something that prevents syncing, with where it happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncBlockerView {
    /// What it is, already translated.
    pub label: String,
    /// On which path, already sanitized. The ROOT is said as "the whole
    /// tree" and not as an empty string.
    pub path: String,
    /// What is painted differs from the bytes.
    pub path_hostile: bool,
}

/// A plan step, already ready to paint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStepView {
    /// Its id within the plan: the identity, never the position.
    pub id: u64,
    /// What it does, already translated.
    pub kind: String,
    /// Why, already translated.
    pub reason: String,
    /// Whether undo brings it back, already said.
    ///
    /// Never comes from `reversal` alone: that is half the answer, and the
    /// half that lies when the destination has no trash.
    pub undo: String,
    /// Which root the path hangs from (`source` or `dest`), by stable id.
    pub anchor: String,
    /// The same, already translated and meant for painting. Empty =
    /// nothing to say.
    pub anchor_label: String,
    /// The relative path, masked.
    pub path: String,
    /// What is painted differs from the bytes.
    pub path_hostile: bool,
    /// The DESTINATION's spelling, when its bytes differ from the source's.
    ///
    /// The write lands on THIS one. A field of its own and not a suffix on
    /// the name: two spellings in the same cell could be merged by a name.
    pub dest_path: Option<String>,
    /// What is painted for the destination differs from its bytes.
    pub dest_path_hostile: bool,
    /// The two spellings render THE SAME (an NFC/NFD pair), so the reader
    /// cannot see them as different and must be told.
    pub twins: bool,
}

/// The diff panel: two trees compared, row by row.
///
/// Windowed and not the whole list, for the same reason as a listing: the
/// engine emits one row per matched name across the WHOLE tree and nothing
/// bounds it —a cap would turn "are they the same?" into a half-answer—, so
/// half a million rows cannot cross the bridge. What travels is what is
/// visible, with its first row and the total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareView {
    /// The left root, already sanitized (the pane that launched the
    /// comparison).
    pub left: String,
    /// What is painted on the left differs from the real path.
    pub left_hostile: bool,
    /// The right root, already sanitized.
    pub right: String,
    /// What is painted on the right differs from the real path.
    pub right_hostile: bool,
    /// The window of VISIBLE rows (the ones a filter does not hide).
    pub rows: Vec<CompareRowView>,
    /// Index, among the visible ones, of the first row travelling.
    pub first_visible: u64,
    /// How many visible rows there are in total.
    pub total: u64,
    /// The selected row, by its id. Anchored to the id and not the index: a
    /// filter hides rows, never renumbers them.
    pub selected: Option<u64>,
    /// The category filters, in fixed order.
    pub filters: Vec<CompareFilterView>,
    /// The status, already said: how many so far and whether it is still
    /// walking.
    pub status: String,
    /// The comparison is still running.
    pub running: bool,
}

/// A category filter, with its count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareFilterView {
    /// Stable id of the category (`same`, `different`…), for the renderer.
    pub id: String,
    /// Its name, in the reader's language.
    pub label: String,
    /// How many rows fell into it, filters aside.
    pub count: u64,
    /// Is HIDING its category.
    pub hidden: bool,
}

/// A row of the diff panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareRowView {
    /// Its id within this comparison. It is the IDENTITY: selecting and
    /// marking go by it, never by position.
    pub id: u64,
    /// The verdict, already translated.
    pub verdict: String,
    /// Its category, by stable id (for painting the color).
    pub category: String,
    /// How confident the verdict is, already translated.
    pub confidence: String,
    /// Which rung decided it, already translated.
    pub criterion: String,
    /// The reason, already translated, when the verdict has one.
    pub reason: Option<String>,
    /// The left face, absent on a right-side orphan.
    pub left: Option<CompareFaceView>,
    /// The right face.
    pub right: Option<CompareFaceView>,
    /// Why this row shows TWO spellings, in an already translated sentence.
    ///
    /// A sentence and not a badge attached to the name, and that is not
    /// style: what is attached to a name can be forged by a name.
    pub paired_under: Option<String>,
}

/// A row's face: what is known about an entry, already sanitized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareFaceView {
    /// The name, already masked.
    pub name: String,
    /// What is painted differs from the bytes that are there.
    pub hostile: bool,
    /// The already formatted size, or empty if the provider does not know
    /// it.
    ///
    /// Empty and not a fabricated `0`: "I don't know" and "zero bytes" are
    /// two different answers, and an unhydrated orphan gives the first.
    pub size: String,
    /// The already formatted date, or empty if unknown.
    pub mtime: String,
    /// Is a directory.
    pub is_dir: bool,
}

/// A persistent status bar notice.
///
/// Not a bare string, and the two fields alongside it are for the same
/// reason as in a dialog. The flag: a notice about a plaintext session
/// paints a `host` that comes from the WIRE, gets masked, and without a
/// flag the absence of a badge reads as "this is faithful" — on the
/// indicator that matters most to an attacker. And the subject apart:
/// building `{scheme}://{host}` inside the sentence turns
/// `bank.example@evil.example` into something that reads as the userinfo of
/// a legitimate host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BannerView {
    /// The sentence, already translated and with nothing from outside
    /// inside it.
    pub text: String,
    /// Which connection it is about, if it is about one.
    pub subject: Option<BannerSubjectView>,
}

/// The connection a notice is about: each part in its own field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BannerSubjectView {
    /// Scheme, already masked and scoped.
    pub scheme: String,
    /// Host, already masked and scoped.
    pub host: String,
    /// WHY it is degraded, already translated (#279).
    ///
    /// A reason this binary does not know says "unknown reason" and does
    /// not inherit the sentence from one that does: a security notice
    /// cannot assert a cause nobody has stated.
    pub reason: String,
    /// The wire's human detail, masked and scoped, and only when the
    /// reason is unknown — which is when the proto's contract says to lean
    /// on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What is painted differs from what is there (in the scheme, the host
    /// or the detail).
    pub hostile: bool,
}

/// A half-typed sequence or counter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingView {
    /// The typed chords, already painted (`ctrl+x g`).
    pub chords: String,
    /// The accumulated counter, if the preset enables them and one is being
    /// typed.
    pub count: Option<u32>,
}

/// An open dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogView {
    /// Its identity: confirming the SAME id twice does nothing twice.
    pub id: ModalId,
    /// Fluent key of the title.
    pub title_key: String,
    /// WHERE what this dialog asks goes, if it goes anywhere.
    ///
    /// A field of its own and not the body's first line, and that is NOT
    /// style. A flat body can only distinguish "the destination" from "the
    /// sources" with a separator inside the text —an arrow, a colon—, and a
    /// directory name can contain that separator: `→` (U+2192) is
    /// legitimate, is not a terminal hazard and is therefore neither masked
    /// nor flagged. A directory named `docs → /home/DELETE` would produce a
    /// line that reads as two paths, and whoever confirms a move believes
    /// their files are going to the second one. The canonical corpus's
    /// `arrow_join_spoof` fixture says exactly this: label OUT OF BAND,
    /// never with a separator inside the text.
    pub destination: Option<DialogLine>,
    /// WHAT is being asked, when that is a nameable thing apart from the
    /// paths (an agent's op: `delete`, `copy`…).
    ///
    /// A field of its own for the same reason as [`Self::destination`]:
    /// mixed with the paths it was one more line, indistinguishable from a
    /// file name saying the same thing.
    pub subject: Option<DialogLine>,
    /// WHO is asking, if not whoever is in front of the screen: the agent
    /// session that requested the operation.
    ///
    /// It used to be dropped, and it is the first thing you need to know to
    /// decide: the title says "agent approval" and without this there is no
    /// way to know WHICH agent.
    pub asker: Option<DialogLine>,
    /// When the answer stops being accepted, already translated. `None` =
    /// no deadline, or it is not known.
    ///
    /// Outside the body, again for the same reason: among path lines, a
    /// file named "expires in 3600 s" is the only line that looks like a
    /// deadline when the REAL deadline is not known —a pending item
    /// reconstructed by `policy.pending`'s resync does not carry the
    /// remaining TTL—.
    pub deadline: Option<String>,
    /// WHEN it expires, in epoch-ms, so the renderer can count down (#279).
    ///
    /// [`Self::deadline`] is a sentence computed on OPEN, so it freezes: a
    /// modal that had been up for four minutes kept saying "expires in 300
    /// s." It does not lie dangerously —the dialog closes on its own when
    /// it expires— but it stops informing exactly when it matters most.
    ///
    /// The instant travels, not the remaining seconds, because what is
    /// needed is a FIXED reference: the seconds would need refreshing with
    /// another patch every second, which is exactly the work this avoids.
    /// Host and renderer share a machine, so they share a clock.
    ///
    /// `None` = there is no deadline or it is not known (a pending item
    /// reconstructed by `policy.pending`'s resync does not carry the
    /// remaining TTL). Then the renderer paints the sentence as is and
    /// counts nothing: counting down from a made-up deadline would be worse
    /// than not counting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<i64>,
    /// Body lines, already sanitized and scoped.
    ///
    /// When they are PATHS, the renderer numbers them by position: the
    /// label is structural and a file name cannot write it.
    pub body: Vec<DialogLine>,
    /// The body shows FEWER elements than the operation touches, and this
    /// says so, already translated. Empty = it shows them all.
    ///
    /// The body is bounded (a selection of a thousand files does not fit in
    /// a dialog), and a truncated list that does not say so describes an
    /// operation smaller than the one about to run: someone marks two
    /// hundred, sees sixteen, and confirms. This is the only place where it
    /// is still possible to say no.
    ///
    /// Translated HERE and in its own field, for the usual two reasons: a
    /// renderer does not translate or substitute numbers, and a notice
    /// tucked among the body's lines could be impersonated by a file name.
    pub overflow_note: String,
    /// Some of what is NOT shown would be painted altered.
    ///
    /// The hostile badge can only speak of what can be looked at, and what
    /// was truncated is not here to inspect — but that there is something
    /// with bidi or invisibles out there can still be said, and it is what
    /// decides whether it is worth expanding before approving. The
    /// terminal has always said this in its summary; this window did not,
    /// and they were the same paths.
    ///
    /// `#[serde(default)]`: absent = `false`, which is not flagging. The
    /// safe direction is the opposite of a VISIBLE path's badge —there,
    /// staying silent hides something being looked at— because here an
    /// extra badge over a truncation teaches ignoring it.
    #[serde(default)]
    pub overflow_hostile: bool,
    /// What point the DESTINATION check is at: whether it fits (#149) and
    /// whether it knows how to hold what gets written to it (#164).
    ///
    /// `#[serde(default)]`: a renderer from an older bridge does not send
    /// it, and its absence is [`DestCheckView::NotAsked`], which is what it
    /// used to be.
    #[serde(default)]
    pub dest_check: DestCheckView,
    /// What can be answered.
    pub choices: Vec<DialogChoice>,
    /// The dialog asks for free text, and this is what has been typed so
    /// far, ALREADY masked and scoped for painting. It is not the operand:
    /// what will be created are the bytes the user typed, kept separately
    /// by the host.
    pub input: Option<String>,
    /// What is typed is painted DIFFERENT from what it is (controls,
    /// direction marks). This is the only surface where approving a name is
    /// asked for, and showing it raw is how something else gets approved.
    pub input_hostile: bool,
    /// The field is a PASSWORD (#327).
    ///
    /// When `true`, [`Self::input`] carries DOTS —one per character— and
    /// not the text: what is typed stays on the host, in a buffer wiped
    /// with zeros when released (`norte_frontend::secret::TypedSecret`).
    /// The renderer paints the field as a password and **never reseeds it**
    /// with this value, which would turn what the user typed into a row of
    /// literal dots.
    ///
    /// A field of its own and not "guess it from the title" because this is
    /// the only difference that matters between painting a file name and
    /// painting a password, and leaving it implicit means the next dialog
    /// that asks for a secret inherits it wrong.
    ///
    /// `#[serde(default)]`: a renderer from an older bridge does not send
    /// it, and its absence means "not a secret," which is what it used to
    /// be.
    #[serde(default)]
    pub input_secret: bool,
    /// The FIELDS of a dialog that is a form (bridge 91).
    ///
    /// Empty —and then absent from the JSON— is the usual dialog: a
    /// question with at most one [`Self::input`]. Search is the first that
    /// needs seven fields and four toggles, but the list is GENERIC on
    /// purpose: any future form dialog wants it, and tailoring it to search
    /// would force redoing it for the second one.
    ///
    /// Coexists with `input` instead of replacing it: that one is the path
    /// for a single-field dialog and for the PASSWORD one, which does not
    /// travel here (#327) — a form stores what was typed on the host so it
    /// can be projected, and that is exactly what a secret does not do.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<DialogFieldView>,
}

/// A field of a form dialog (bridge 91).
///
/// The renderer paints what [`Self::kind`] says and decides nothing else:
/// the label is a Fluent KEY, the value arrives already masked and scoped,
/// and which values exist in a cycle is resolved by Rust before sending it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogFieldView {
    /// STABLE id of the field (`name`, `min-size`, `recursive`…).
    ///
    /// By id and not by index, for the same reason as the listing's rows: a
    /// field inserted in the middle would renumber every one below it, and
    /// what the renderer sends back would name a different one.
    pub id: String,
    /// Fluent key of the label.
    pub label_key: String,
    /// What is painted of the value: masked and scoped. Empty for the ones
    /// that are not text.
    pub value: String,
    /// What is painted DIFFERS from the real one (controls, direction
    /// marks). The renderer marks it; never hides it.
    pub hostile: bool,
    /// What kind of control it is.
    pub kind: DialogFieldKind,
}

/// What kind of control a form field is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DialogFieldKind {
    /// Free text: the renderer owns the caret and sends the WHOLE text.
    Text,
    /// A two-state toggle.
    Toggle {
        /// On.
        on: bool,
    },
    /// A cycle of closed values; the CURRENT value's label, already chosen
    /// in Rust.
    ///
    /// The key and not the index: how many values there are and in what
    /// order is a Rust decision, and a renderer that knew it could fall out
    /// of sync without anything turning red.
    Cycle {
        /// Fluent key of the current value.
        value_key: String,
    },
}

/// What is known about WHERE THE BYTES ARE GOING, while it is being asked.
///
/// For a transfer it is the destination directory; for a delete it is the
/// trash, or its absence — which is the same kind of fact and that is why
/// it shares a channel: "⚠ NO trash: this cannot be undone" answers the
/// same question as "doesn't fit" and "this destination doesn't confine."
/// One channel and not three also because the renderer paints them in a
/// block a file name cannot impersonate, and three blocks would be three
/// places to forget that property.
///
/// **Three states and not a list of warnings, because silence had to mean
/// exactly one thing.** The two questions —does it fit? does it know how to
/// confine?— are I/O, so the dialog is painted before they come back; with
/// a single empty `Vec`, "I haven't asked yet" and "I asked and there is
/// nothing to say" arrived identical, and the human could confirm in that
/// gap. The absence of #164's line MEANS "this destination holds its
/// writes," so leaving it ambiguous asserts it without knowing it.
///
/// The terminal does not have this problem: it asks in the turn's header,
/// before painting, so its modal is never seen without the answers set
/// (`norte_tui::turn`). Waiting for them here would leave F5 painting
/// nothing against a slow SFTP, which is worse: what the human has in front
/// of them meanwhile is the list of what is about to be copied, which is
/// what they came to read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum DestCheckView {
    /// This dialog has nothing to check about where the bytes are going,
    /// and it is the default value. The ones that do: transfer, drop and
    /// delete.
    #[default]
    NotAsked,
    /// It was asked and has not come back. The renderer SAYS SO and
    /// reserves the spot: a line that suddenly appears above the buttons
    /// moves them under the pointer of whoever was about to click.
    Checking,
    /// It came back. An empty list is the normal answer and is not
    /// painted: that it fits and that it confines are NOT announced,
    /// because a line on every copy is noise, and noise teaches skipping
    /// the line the day it says something.
    ///
    /// Already translated and without a single string a third party
    /// controls. They go here and not among the body's lines for that same
    /// reason: there a file name could impersonate them.
    Done {
        /// What needs to be known before saying yes. Empty = nothing.
        warnings: Vec<String>,
    },
}

/// A line of a dialog's body.
///
/// A structure and not a bare string because the line carries TWO things:
/// what is painted and whether what is painted differs from what is there.
/// A body of `Vec<String>` with a `Vec<bool>` alongside are two vectors that
/// can fall out of sync; a listing row ([`RowView`]) already solves the
/// same problem this way, and this is the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogLine {
    /// The text, masked and scoped.
    pub text: String,
    /// What is painted DIFFERS from the real one (non-UTF-8 bytes,
    /// controls, direction marks). The renderer marks it; never hides it.
    ///
    /// Matters here more than anywhere else: a dialog's body is what
    /// someone reads before approving that a file be deleted, copied or
    /// moved. A name painted different from what it is, with no badge, is a
    /// name that reads as faithful — and the approval is for SOMETHING
    /// ELSE.
    pub hostile: bool,
}

/// The rename plan a model proposed, under review.
///
/// A screen of its own and not a dialog, because of what it HAS to show:
/// pairs to browse, a core verdict that arrives AFTER it opens, and a
/// collision detail. A dialog is a question with answers; this is a
/// document read before approving it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenameView {
    /// The directory the plan was made for.
    pub dir: DialogLine,
    /// The window of pairs travelling, NOT the whole plan.
    pub pairs: Vec<AiRenamePairView>,
    /// The first pair of `pairs` within the plan.
    pub first_visible: u64,
    /// How many pairs the plan has.
    pub total: u64,
    /// How much is shown of how much there is, already translated. Empty =
    /// all of it is shown.
    ///
    /// Translated HERE and not in the renderer, and this time for a
    /// measured reason: the catalogue that crosses the bridge carries
    /// strings ALREADY formatted with no arguments, and Fluent writes a
    /// missing variable as `{$shown}` — no spaces. The renderer was
    /// substituting `{ $shown }`, which never matches, so the line saying
    /// how much of the plan is being looked at painted two raw identifiers
    /// on the screen where a batch is approved.
    pub more_note: String,
    /// OUTSIDE the window there is a name painted different from what it
    /// is.
    ///
    /// The window is five pairs out of up to 256, and every visible line
    /// carries its flag. Without this, the flag only exists for what is
    /// visible: putting the altered pair at position twelve is enough for a
    /// plan to get approved without a single badge ever having appeared.
    pub hidden_hostile: bool,
    /// The core's verdict, already translated: checking, applicable, not
    /// applicable, or not checked. The line that cannot be lost.
    pub status: String,
    /// The planner's machinery and the collisions, one per line and already
    /// translated, each saying whether what is painted differs from the
    /// real one.
    pub detail: Vec<DialogLine>,
    /// Approving can do something. Said by the CORE (`executable`), not a
    /// collision count: the field is normative, and a future verdict can
    /// stop a plan with no offending name to list.
    pub confirmable: bool,
    /// How many renames it will REALLY do, already said and translated. Not
    /// `total`: the planner drops null pairs, and promising the requested
    /// count would promise too much.
    ///
    /// Empty while there is no verdict: until the core answers it is not
    /// known, and a zero would read as "will do nothing."
    pub real_steps_note: String,
    /// The reader has walked through the WHOLE plan.
    ///
    /// Approving requires it. With 256 pairs allowed and five visible, pair
    /// two hundred used to run without anyone ever having seen it painted
    /// — and review is the entire defense there is against a plan a model
    /// wrote from names an attacker controls.
    pub seen_all: bool,
}

/// The ORGANIZE plan under review (phase 8).
///
/// The twin of [`AiRenameView`] with two differences, both coming from the
/// same thing: here what changes is the SHAPE of the directory.
///
/// - The body is a TREE, not a list of pairs. A list of forty
///   `a.pdf → invoices/2026/a.pdf` does not let you see how many folders
///   appear or what ends up inside each, which is exactly what is being
///   approved.
/// - There is no verdict to wait for. The plan's token travels WITH it, so
///   this screen is born approvable and does not go through a `Pending`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizeView {
    /// The directory the plan was made for.
    pub dir: DialogLine,
    /// The window of tree lines travelling, NOT the whole tree.
    pub lines: Vec<OrganizeLineView>,
    /// The first line of `lines` within the tree.
    pub first_visible: u64,
    /// How many lines the tree has.
    pub total: u64,
    /// How much is shown of how much there is, already translated. Empty =
    /// all of it is shown.
    pub more_note: String,
    /// OUTSIDE the window there is a name painted different from what it
    /// is. Without this, the flag only exists for what is visible.
    pub hidden_hostile: bool,
    /// "Creates N folders and moves M files," already translated: what is
    /// read to decide without counting lines. Goes BEFORE the tree.
    pub summary: String,
    /// The reader has walked through the WHOLE tree. Approving requires it.
    pub seen_all: bool,
}

/// A line of the organize tree (phase 8).
///
/// `kind` travels as DATA and not resolved to a color: the renderer decides
/// how a new folder looks, and a monochrome theme needs to be able to mark
/// it another way. Whether it is new or not is decided by
/// [`norte_frontend::organize::tree_lines`], shared with the terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizeLineView {
    /// How much it is indented: 0 is a direct child of the plan's
    /// directory.
    pub depth: u32,
    /// The name, masked and scoped, with its flag if it differs.
    pub text: DialogLine,
    /// What it is: a folder being CREATED, one that already existed, or a
    /// file being moved.
    pub kind: OrganizeLineKind,
}

/// What an organize tree line represents (phase 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganizeLineKind {
    /// A folder the plan is going to CREATE.
    NewDir,
    /// A folder that ALREADY exists and that the plan puts something into.
    ExistingDir,
    /// A file being moved there.
    Moved,
}

/// A plan's pair: from which name to which name.
///
/// Both names travel WHOLE and separately, never concatenated with an
/// arrow: the same reason as a transfer's destination
/// ([`DialogView::destination`]) — a name can contain the arrow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenamePairView {
    /// The current name.
    pub from: DialogLine,
    /// The one the model proposes.
    pub to: DialogLine,
}

/// A dialog's possible answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogChoice {
    /// Stable id of the answer (`confirm`, `cancel`, `overwrite`…).
    pub id: String,
    /// Fluent key of the label.
    pub label_key: String,
    /// This answer DESTROYS something: the renderer paints it as such.
    pub destructive: bool,
}

/// The splash screen (bridge 69, ADR 0115).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplashView {
    /// The art, one line per row. Comes from the shared model, so the
    /// compass is the same one the terminal paints.
    pub art: Vec<String>,
    /// Which build is running.
    pub version: String,
    /// And which revision it was built from.
    pub revision: String,
    /// Which core it talks to, already translated.
    pub daemon: String,
    /// The footer: how to dismiss it, and whether the numbers do anything.
    pub hint: String,
    /// The sections, already filtered: none arrives empty.
    pub sections: Vec<SplashSectionView>,
    /// How much longer it stays up, in milliseconds, or `None` if it stays
    /// until someone dismisses it.
    ///
    /// The deadline is decided by the host —the `brief` mode is its own and
    /// so is the clock—, but who enforces it is the renderer: there is no
    /// event loop here that wakes up on its own, as there is in the
    /// terminal, and a screen that promises to be brief and stays up until
    /// you press a key is worse than promising nothing. So the number
    /// CROSSES, instead of the renderer making up its own: two clocks with
    /// the same constant written twice is exactly the divergence ADR 0077
    /// chases down.
    pub close_after_ms: Option<u32>,
}

/// A splash screen section (bridge 69).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplashSectionView {
    /// The title, already translated and sanitized.
    pub title: String,
    /// Its rows, in the order they are painted.
    pub rows: Vec<SplashRowView>,
}

/// A splash screen row (bridge 69).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplashRowView {
    /// The number that opens it, or `0` if the row has no key.
    pub number: u8,
    /// What is read, already sanitized.
    pub label: String,
    /// The detail on the right (a path, a visit count).
    pub detail: String,
}

/// A task on the board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskView {
    /// The task's id in the daemon.
    pub task_id: u64,
    /// Kind (`copy`, `move`, `delete`, `sync`…).
    pub kind: String,
    /// What state it is in.
    pub state: TaskStateView,
    /// 0–100 percent if known.
    pub percent: Option<u8>,
    /// The rate, already written (`1.2 MiB/s`), or empty if unknown. Bridge
    /// 69.
    ///
    /// Written by the HOST and not a number: `human_rate` belongs to the
    /// shared crate, so the terminal and the window state the same speed
    /// with the same units, and the renderer does not choose roundings.
    #[serde(default)]
    pub rate: String,
    /// What remains, already written (`1m 20s`), or empty. Bridge 69.
    #[serde(default)]
    pub eta: String,
    /// Already sanitized short description (what is being moved).
    pub detail: Option<String>,
    /// [`Self::detail`] differs from the real path. Flagged for the same
    /// reason as [`DialogLine::hostile`]: a copy whose current file is
    /// painted with a masked name and no badge says that IS the name.
    pub detail_hostile: bool,
    /// The task belongs to ANOTHER client of the same session.
    pub foreign: bool,
}

/// A task's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStateView {
    /// Queued.
    Queued,
    /// Running.
    Running,
    /// Paused (ADR 0147, bridge 93): alive, stopped until resumed.
    Paused,
    /// Finished successfully.
    Done,
    /// Failed.
    Failed,
    /// Cancelled.
    Cancelled,
}

/// A change over the previous snapshot.
///
/// `base_sequence` is mandatory and not decorative: applying a patch over
/// another base is FORBIDDEN, and a renderer without that base requests a
/// snapshot instead of guessing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewPatch {
    /// The sequence this patch applies over.
    pub base_sequence: u64,
    /// What changes.
    pub changes: Vec<ViewChange>,
}

/// A specific change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "change")]
pub enum ViewChange {
    /// A slot's cursor moved (without resending the rows).
    Cursor {
        /// Slot.
        slot_id: u32,
        /// Generation the key is valid for.
        generation: u64,
        /// New row under the cursor.
        cursor: Option<RowKey>,
    },
    /// A slot's visible rows changed.
    Rows {
        /// Slot.
        slot_id: u32,
        /// Generation.
        generation: u64,
        /// First row travelling.
        first_visible: u64,
        /// The rows.
        rows: Vec<RowView>,
        /// The icon column is open (bridge 62): travels WITH the rows
        /// because it is with a rows patch that icons land, and a renderer
        /// that kept the value from the last snapshot would paint the
        /// first icon page without its column.
        icon_column: bool,
        /// How many rows the WHOLE listing has, not how many travel.
        ///
        /// Travels in the patch and not only in the snapshot because it is
        /// the HEIGHT of the renderer's scroll
        /// (`total * cell_height`, plus `aria-rowcount`), and paginated
        /// draining answers with patches —including the last batch—.
        /// Without this the renderer kept the first page's total forever: a
        /// directory of five thousand files hit a wall at row 100, and
        /// neither the wheel could scroll down nor the visible range could
        /// request the rest.
        total_rows: Option<u64>,
    },
    /// A listing's HEADER changed: its path and what is missing from it.
    ///
    /// Apart from [`ViewChange::Rows`] because it is another part of the
    /// screen —the renderer paints it in `paintHeader`—, and bundled with it
    /// because they move together: `pane.names-encoding` retranscribes the
    /// path just like the rows, and `pane.toggle-hidden` moves entries in
    /// and out of the listing, which changes how many are set aside.
    ///
    /// It used to travel only in the whole snapshot, so the rows would
    /// repaint and the title kept the old reading — mojibake at the top and
    /// the reader not knowing whether the command did anything (#57, #293).
    BrowserHeader {
        /// Slot.
        slot_id: u32,
        /// The path, already paintable and with the current
        /// reinterpretation.
        path_display: String,
        /// That path DIFFERS from the real bytes.
        path_hostile: bool,
        /// What the provider skipped, already said. Empty if nothing was
        /// skipped.
        skipped_note: String,
        /// What hiding sets aside. Empty if nothing is set aside.
        hidden_note: String,
        /// Names are being REINTERPRETED with another encoding (#57). Empty
        /// if not.
        ///
        /// Permanent for as long as it lasts, as in the terminal: what is
        /// painted is not the bytes on disk, and that must be knowable at
        /// the moment of deciding to copy or delete something — not only in
        /// the toggle's message, which the next key overwrites.
        names_note: String,
        /// The listing is STILL FILLING, and how many so far. Empty if
        /// already whole.
        ///
        /// An incomplete listing is never silent: without this the screen
        /// asserts that is all there is, which is precisely what is not yet
        /// known.
        filling_note: String,
        /// Marks the last refresh dropped because their entry is no longer
        /// there. Empty if none dropped.
        ///
        /// The most serious of this header's notices: with an empty
        /// selection the operand funnel falls back to the CURSOR, so
        /// staying silent redirects the next mass operation to something
        /// nobody marked.
        pruned_note: String,
        /// How many entries are marked and how much they weigh, already
        /// said. Empty with no marks: whoever does not mark gains no noise.
        marked_note: String,
        /// The listing's footer (spec 2026-09-10), already worded; travels
        /// with the header because it changes along with the same things it
        /// does: marking, hiding, filling. Empty = `[ui] pane_footer` off.
        #[serde(default)]
        footer: String,
        /// The path's breadcrumbs (bridge 65); see `BrowserSlotView`.
        #[serde(default)]
        path_segments: Vec<String>,
        /// How much of the volume is used (bridge 65); see
        /// `BrowserSlotView`.
        #[serde(default)]
        used_ratio: Option<f32>,
        /// How many entries are marked, raw.
        ///
        /// Still travels alongside [`Self::BrowserHeader::marked_note`] and
        /// is not a duplication: the sentence is for PAINTING and this
        /// number is for deciding (a renderer that wants to flag the slot,
        /// count, or enable something), and deriving a number from a
        /// translated sentence is what this DTO exists to spare everyone
        /// from doing.
        marks: u64,
        /// The mark ruler (bridge 89, ADR 0135): which spans of the listing
        /// —out of [`MARK_RULER_SPANS`] equal ones— carry a mark, in order.
        /// Empty with no marks.
        #[serde(default)]
        mark_ruler: Vec<u16>,
    },
    /// A slot's state changed (loading, error, ready).
    SlotState {
        /// Slot.
        slot_id: u32,
        /// New state.
        state: SlotState,
    },
    /// The status bar changed.
    Status(StatusView),
    /// The right half of the status bar's elements changed (ADR 0132). Like
    /// the panel bar: the host compares them with the last ones it sent
    /// when building each patch, because almost everything moves them —the
    /// cursor, a mark, the order, a task.
    StatusItems {
        /// The whole list.
        status_items: Vec<StatusItemView>,
    },
    /// A slot's thin line moves (ADR 0148, bridge 94).
    ///
    /// Apart from the listing on purpose: progress arrives at 30 Hz and
    /// resending the rows on every tick would pay for a whole listing for
    /// two pixels.
    SlotProgress {
        /// Which slot.
        slot_id: u32,
        /// 0–100, or nothing to paint.
        progress: Option<u8>,
    },
    /// The task board changed.
    ///
    /// A STRUCT variant and not a tuple one, and not by taste: an enum
    /// tagged internally (`tag = "change"`) cannot serialize a variant that
    /// wraps a sequence — serde has nowhere to put the tag. As a tuple, this
    /// used to compile and fail at runtime on the first renderer that asked
    /// for it as JSON.
    Tasks {
        /// The whole board.
        tasks: Vec<TaskView>,
        /// Which row of the processes panel is chosen, over these rows.
        ///
        /// Travels WITH the board and not in a `ViewChange` of its own, for
        /// the same reason `total_rows` travels with a listing's rows: it
        /// is an extension of what sits next to it, and both move at once.
        /// A task that expires after ten seconds removes a row and shifts
        /// the rest; without this, the cursor only travelled in the whole
        /// snapshot, so the panel kept highlighting row N —which is now
        /// another task, or none— while the cancel key acted on the one the
        /// host has bounded. Highlighting one and stopping another is the
        /// failure, not the delay.
        ///
        /// `None` with an empty board: an index with no row behind it
        /// highlights nothing.
        #[serde(default)]
        cursor: Option<u64>,
    },
    /// The open dialogs changed. A struct for the same reason as
    /// [`ViewChange::Tasks`].
    Dialogs {
        /// The open dialogs, in opening order.
        dialogs: Vec<DialogView>,
    },
    /// The connection changed state.
    Connection(ConnectionView),
    /// The layout changed: the window was resized, or focus (and with it
    /// the roles) moved to another slot.
    Layout(LayoutView),
    /// A listing's headers changed.
    ///
    /// Sorting moves the rows AND the sort mark. Without this change, after
    /// a click on the header the listing repainted in the new order and the
    /// `▲` kept describing the previous one: the screen contradicted
    /// itself, and a screen reader read `aria-sort` lying.
    Columns {
        /// Slot.
        slot_id: u32,
        /// The headers, in their order.
        columns: Vec<ColumnHeader>,
    },
    /// The profile picker opened, moved, or closed.
    Profiles {
        /// The picker, or `None` if it closed.
        profiles: Option<ProfilePickerView>,
    },
    /// The menu bar: one opened, the cursor moved, or it closed.
    ///
    /// Whole and not a delta, like help: the bar and the dropdown are one
    /// small whole, and sending it in pieces would mean inventing a
    /// protocol to save a few hundred bytes.
    Menu {
        /// The bar, always: the row of titles stays there with the
        /// dropdown closed.
        menu: MenuView,
    },
    /// The panel bar changed: a panel opened or closed, the keyboard moved,
    /// or something started having something to report.
    ///
    /// Not emitted by any one place in particular: the host compares it
    /// with the last one it sent every time it builds a patch, and adds it
    /// if it differs. It is what makes a panel opened by key, by menu, by
    /// palette or by the bar itself update the same — the TUI's "per
    /// frame," translated into a bridge that only speaks when something
    /// changes.
    PanelBar {
        /// The whole bar.
        panel_bar: PanelBarView,
    },
    /// The palette opened, filtered, moved, or closed.
    Palette {
        /// The palette, or `None` if it closed.
        palette: Option<PaletteView>,
    },
    /// "Go to anywhere" opened, filtered, moved, received a late section
    /// (connections, index) or closed (#357, bridge 77).
    Goto {
        /// The screen, or `None` if it closed.
        goto: Option<GotoView>,
    },
    /// The splash screen was shown or dismissed (bridge 69).
    Splash {
        /// The screen, or `None` if it was dismissed.
        splash: Option<SplashView>,
    },
    /// The first-launch wizard opened, moved, or closed.
    Wizard {
        /// The wizard, or `None` if it closed.
        wizard: Option<WizardView>,
    },
    /// The which-key panel appeared, changed, or went away.
    WhichKey {
        /// The continuations, or `None` if there is no longer a half-typed
        /// prefix.
        whichkey: Option<WhichKeyView>,
    },
    /// Help opened, changed page, moved the cursor, or closed.
    ///
    /// A whole patch and not a per-field delta: a page fits well within a
    /// message, and help's state is one whole — the sidebar, the body and
    /// what is runnable move together when the reader opens another one.
    Help {
        /// Help, or `None` if it closed.
        help: Option<HelpView>,
    },
    /// The theme opened or closed.
    Theme {
        /// The theme, or `None` if it closed.
        theme: Option<ThemeView>,
    },
    /// The search started, found something, finished, or closed.
    Search {
        /// The search, or `None` if it closed.
        search: Option<SearchView>,
    },
    /// The diff panel changed (opened, rows arrived, closed).
    Compare {
        /// The comparison, or `None` if it closed.
        compare: Option<CompareView>,
    },
    /// The sync panel changed (opened, steps arrived, closed).
    Sync {
        /// The plan, or `None` if it closed.
        sync: Option<SyncView>,
    },
    /// The layout picker opened, moved, or closed.
    Layouts {
        /// The picker, or `None` if it closed.
        layouts: Option<LayoutPickerView>,
    },
    /// The columns picker opened, moved, or closed.
    ColumnsPicker {
        /// The picker, or `None` if it closed.
        columns: Option<ColumnsPickerView>,
    },
    /// A picker opened, moved, or closed.
    Picker {
        /// The picker, or `None` if it closed.
        picker: Option<PickerView>,
    },
    /// Extensions opened, changed, or closed.
    Extensions {
        /// The manager, or `None` if it closed.
        extensions: Option<ExtensionsView>,
    },
    /// The agent sessions panel opened, moved, or closed.
    Agents {
        /// The panel, or `None` if it closed.
        agents: Option<AgentsView>,
    },
    /// An extension command's output was shown or closed.
    PluginOutput {
        /// What it printed, or `None` if it closed.
        output: Option<ExtensionOutputView>,
    },
    /// A program's output (#312) was shown or closed.
    ProgramOutput {
        /// What it printed, or `None` if it closed.
        output: Option<ProgramOutputView>,
    },
    /// Settings opened, moved the cursor, or closed.
    Settings {
        /// Settings, or `None` if it closed.
        settings: Option<SettingsView>,
    },
    /// The viewer changed (opened, scrolled, closed).
    ///
    /// A patch and not a snapshot: the viewer covers the screen, and
    /// sending the whole state on every scroll line would send the visible
    /// rows of EVERY listing underneath, which is the waste decision D7
    /// exists to avoid.
    /// A STRUCT variant, not a tuple one: an internally tagged enum also
    /// cannot serialize a variant wrapping an `Option`. It is the SAME trap
    /// that caught `Tasks` and `Dialogs`, and this time the corpus caught it
    /// before shipping.
    Viewer {
        /// The viewer, or `None` if it closed.
        viewer: Option<ViewerView>,
    },
    /// The rename plan under review changed: opened, the core's verdict
    /// arrived, was browsed, or closed.
    AiRename {
        /// The plan, or `None` if it closed.
        ai_rename: Option<AiRenameView>,
    },
    /// The ORGANIZE plan under review changed (phase 8): opened, was
    /// browsed, or closed. It has no third case like the rename one's
    /// "verdict arrived" because its token travels with the plan.
    Organize {
        /// The plan, or `None` if it closed.
        organize: Option<OrganizeView>,
    },
}

/// Something to say that is not a screen change.
///
/// Travels through the SAME sequence as everything else: it is not a
/// second channel with no ordering, because "the connection was lost" and
/// "this listing failed" have to arrive in the order they happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "notice")]
pub enum UiNotice {
    /// A normal notice, with its Fluent key.
    Message {
        /// Fluent key.
        key: String,
        /// Already-sanitized detail.
        detail: Option<String>,
    },
    /// The host is shutting down and this is the last thing it says.
    Shutdown {
        /// Work was left unfinished (a session not written, a live task).
        /// STATED, not kept quiet.
        incomplete: bool,
    },
    /// A failure of the host itself: the renderer can no longer trust its
    /// copy of the state. Carries no file names or session bodies.
    Fatal {
        /// Fluent key of the failure.
        key: String,
    },
}

/// What the host asks of the PROCESS hosting it, not the renderer.
///
/// A separate channel, and not one more `UiUpdate`, for two reasons that
/// point to the same place. The first is about audience: this carries
/// PATHS and programs, and the webview has no reason to see them —nor
/// permission to run them: its capabilities are listening for events and
/// nothing else (ADR 0066 D11). The second is about responsibility: the
/// host does not launch processes or touch the clipboard; it says WHAT
/// must be done, with operands that come from its own semantic state, and
/// whoever hosts it decides HOW, with one narrow door per thing. A
/// frontend that does not know how to do one simply does not do it, and
/// the host finds out because nobody answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeEffect {
    /// Put this on the clipboard.
    ///
    /// Already composed —one path per line, in its native form when it has
    /// one—: composing it is a presentation rule and lives where the rest
    /// of them do.
    CopyBytes {
        /// What is copied, in BYTES and undecoded.
        ///
        /// Bytes and not `String` because a file name is bytes (rule 1):
        /// running it through `from_utf8_lossy` would put the replacement
        /// character on the clipboard, and whatever gets pasted afterward
        /// would open a different file —or none. The system helper
        /// receives it over STDIN, which does not decode it either.
        bytes: Vec<u8>,
        /// How many paths it carries, to say so without counting them
        /// again.
        count: usize,
    },
    /// Opens THIS entry with whatever application the desktop picks.
    OpenPath {
        /// What is opened. It is a `VPath`: whoever hosts it converts it to
        /// a native path —or says it cannot, because an `sftp://` is not
        /// handed to `xdg-open`.
        path: norte_proto::VPath,
    },
    /// Sends a notice through the DESKTOP (#285).
    ///
    /// The text travels ALREADY COMPOSED, translated, masked and clamped: a
    /// notification leaves the process and can end up in a history or on
    /// the lock screen, so what it carries has to have gone through the
    /// same hands as what is painted on the bar. Whoever delivers it just
    /// delivers it.
    Notify {
        /// The first line: what happened, by category.
        title: String,
        /// The detail, with the file name when there is one.
        body: String,
    },
    /// Asks the DESKTOP for the reader to choose a directory (#284).
    ///
    /// Exists because with a single listing on screen there is no target
    /// pane to take the place from, and refusing the operation left
    /// whoever has not split the window unable to copy. The picker is
    /// painted by the system, not norte.
    ///
    /// **The path that comes back is renderer text and is treated as
    /// such**: the host validates it and, above all, SHOWS it in the
    /// confirmation before moving a byte. The operands —what is copied—
    /// still come from the host's state and not from the message, which
    /// is ADR 0069's rule.
    PickDirectory {
        /// Where to open the picker: the active pane's directory. It is a
        /// suggestion, not a restriction — the reader can go somewhere
        /// else.
        from: norte_proto::VPath,
    },
    /// Runs a PROGRAM with these arguments (#312): detached (a graphical
    /// comparer that opens its own window) or WAITED FOR and with its
    /// output captured, which comes back as `UiAction::ProgramFinished`
    /// and is shown.
    ///
    /// The argv arrives RESOLVED: the program is already an absolute path
    /// (ADR 0082, before it is given a `cwd`) and the files' paths are
    /// already interpolated with the shared rules (`[ui] diff`, `%F`).
    /// Whoever hosts it decides nothing: it launches. In BYTES, because a
    /// file name is bytes (rule 1) and an argument that was not UTF-8
    /// would open a different file, or none.
    RunProgram {
        /// Fluent key for what is being done, for the panel.
        title_key: String,
        /// Program (absolute path) and arguments, in bytes.
        argv: Vec<Vec<u8>>,
        /// Working directory, in native bytes, if there is one.
        cwd: Option<Vec<u8>>,
        /// `true` = launch and forget; `false` = wait and capture.
        detached: bool,
    },
    /// Opens a terminal sitting in THIS directory.
    OpenTerminal {
        /// Where it sits.
        dir: norte_proto::VPath,
    },
    /// The HANDOFF to the TERMINAL (phase 9): the screen is already
    /// written and the session, released; now launches `ntc --attach` and
    /// CLOSES this window.
    ///
    /// Goes through this channel and not through the view because
    /// launching a process and closing is the host's business: the host
    /// does not know how to open a terminal emulator, nor should it. What
    /// the host guarantees before emitting it is what makes the handoff
    /// safe — that the session is saved and free.
    ///
    /// If the launch fails, whoever hosts it says so and does NOT close:
    /// the session is released but the screen stays here, which is the
    /// cheap failure.
    HandoffToTerminal {
        /// `true` if this process talks to the daemon, so the terminal
        /// starts up the same way. Without it, it would go against its
        /// embedded core and would not find the session that was just
        /// released.
        daemon: bool,
    },
    /// The active theme is now this one: resolve whatever comes from it
    /// again.
    ///
    /// Goes through THIS channel and not the view's because the theme does
    /// not cross to the renderer as data: it crosses converted into
    /// whatever that renderer knows how to paint —CSS variables in the
    /// webview, something else in the next one— and that conversion
    /// belongs to whoever hosts it, not the host. The host says which
    /// theme there is; how it looks is the house's business.
    ///
    /// Exists because the host resolves the theme ONCE at startup. Without
    /// this, the window could not change theme on the fly: not from its
    /// own picker, nor on a profile change — which is half the point of a
    /// profile existing.
    ThemeChanged {
        /// The name of the theme to resolve.
        name: String,
    },
    /// Close yourself: the reader asked, and if there was something to ask
    /// about, it was already asked.
    ///
    /// Whoever hosts it flushes the session and destroys the window. Goes
    /// through here and not the window manager's own gesture because the
    /// question is decided by the host: `[ui] confirm_quit` is
    /// configuration, and a window that closes itself with a half-done
    /// copy is not a window that obeys.
    CloseWindow,
}

/// What the host sends to the renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "update")]
pub enum UiUpdate {
    /// Replaces the renderer's WHOLE state.
    ///
    /// Boxed: a whole snapshot is an order of magnitude bigger than a
    /// patch or a notice, and without the box that size is paid by EVERY
    /// message that crosses, most of which are cursor patches.
    Snapshot(Box<ViewSnapshot>),
    /// Changes what it says, on top of the base it says.
    Patch(ViewPatch),
    /// Something to say, in the same order as everything else.
    Notice(UiNotice),
}
