//! What the renderer ASKS FOR.
//!
//! These are SEMANTIC actions, not backend methods: "move the cursor", not
//! "call `fs.list` with this cursor". The difference matters because what
//! is exposed is what a renderer can do, and a renderer must not be able to
//! request an arbitrary `rpc(method, params)` (ADR 0066, decision D11).
//!
//! No action names a path. Rows are acted on by their [`RowKey`], and every
//! action that names a row ALSO carries the generation in which the
//! renderer saw it. Without that pair the key says nothing: it is an index,
//! and an index from an earlier screen names a different file. The host
//! compares the generation with the listing's epoch and answers
//! [`crate::ActionAck::Stale`] when they do not match — which is what stops
//! a late click from acting on what occupied that row AFTERWARD.
//!
//! And no action accepts a path string, nor ever will: what the renderer can
//! name is what the host gave it.

use serde::{Deserialize, Serialize};

use crate::bridge::{ModalId, RowKey};
use crate::keys::KeyInput;

/// What a tab-bar button does (ADR 0133).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabVerb {
    /// Open a tab in the group (`pane.tab-new`).
    New,
    /// Close the tab (`pane.tab-close`).
    Close,
}

/// A request from the renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum UiAction {
    /// Moves the slot's cursor. `delta` in rows; negative is upward.
    ///
    /// It is the most-repeated action (a held key) and the host does NOT
    /// coalesce it: it applies them one by one and emits one cursor patch
    /// per each. With the reference renderer there is nothing to coalesce —
    /// it serializes its calls, so there is at most one in the mailbox — and
    /// coalescing without need complicates the point where acks are
    /// answered. A renderer that sends in batches will make it worth it;
    /// until then, this describes what happens, not what would be nice.
    MoveCursor {
        /// Slot.
        slot_id: u32,
        /// Rows to move.
        delta: i64,
    },
    /// Puts the cursor on a specific row (a click).
    SelectRow {
        /// Slot.
        slot_id: u32,
        /// Row.
        key: RowKey,
        /// The generation in which the renderer saw that row.
        generation: u64,
    },
    /// Marks or unmarks a row.
    ToggleMark {
        /// Slot.
        slot_id: u32,
        /// Row.
        key: RowKey,
        /// The generation in which the renderer saw that row.
        generation: u64,
    },
    /// Marks the WHOLE range between two rows, ends included.
    ///
    /// A mouse sweep (shift+click, drag) is ONE action, not a string of
    /// `ToggleMark`s: what belongs in a range — and what does not, like
    /// `..` — is a selection rule, and those live in `norte-frontend`, not
    /// in the renderer (ADR 0066, decision D14). The order of the ends does
    /// not matter.
    MarkRange {
        /// Slot.
        slot_id: u32,
        /// One end.
        from: RowKey,
        /// The other.
        to: RowKey,
        /// The generation in which the renderer saw those rows.
        generation: u64,
    },
    /// Opens whatever is under that row: enters the directory, or opens the
    /// file the usual way.
    Activate {
        /// Slot.
        slot_id: u32,
        /// Row.
        key: RowKey,
        /// The generation in which the renderer saw that row.
        generation: u64,
    },
    /// Goes up to the parent directory.
    Parent {
        /// Slot.
        slot_id: u32,
    },
    /// Clicks a breadcrumb (bridge 65): navigates to the directory with the
    /// current path's first `depth` segments. `0` is the root.
    ///
    /// By DEPTH and not by name: the segments already traveled masked, and
    /// a masked name is not a name again.
    BreadcrumbActivate {
        /// Slot.
        slot_id: u32,
        /// How many segments to keep.
        depth: u32,
        /// The generation of the listing that painted those breadcrumbs. If
        /// the slot already navigated elsewhere, the depth refers to a path
        /// that is no longer there: the breadcrumb is stale and is not
        /// reinterpreted over the new one.
        generation: u64,
    },
    /// Back and forward in the navigation trail.
    History {
        /// Slot.
        slot_id: u32,
        /// `true` = back.
        back: bool,
    },
    /// The visible window changed (scroll or resize).
    ///
    /// Arrives DEBOUNCED from the renderer: painting the scroll is its own,
    /// and the only thing that crosses is which rows are needed.
    SetVisibleRange {
        /// Slot.
        slot_id: u32,
        /// First visible row.
        first: u64,
        /// How many fit.
        count: u32,
    },
    /// Sorts the listing by a column (a click on its header).
    ///
    /// The column travels by its ID, not its position or its label: what
    /// sorting by it means — and whether it reverses or starts over — is
    /// decided by the shared rule (`SortSpec::after_click`), not the
    /// renderer.
    SortBy {
        /// Slot.
        slot_id: u32,
        /// The column's id, as it traveled in its header.
        column: String,
    },
    /// Sets a column's width (dragging its header's edge, bridge 64).
    ///
    /// The width belongs to the COLUMN, not the slot: `[ui.columns]
    /// spec.width` is global, so every slot that paints it changes at once,
    /// and the terminal reads the same value on its next load. `cells`
    /// arrives in grid cells; the host clamps it to what the configuration
    /// accepts.
    ResizeColumn {
        /// The slot where it was dragged.
        slot_id: u32,
        /// The column's id, as it traveled in its header.
        column: String,
        /// Requested width, in cells.
        cells: u16,
    },
    /// Changes which slot has keyboard focus.
    FocusSlot {
        /// Slot.
        slot_id: u32,
    },
    /// Answers a dialog.
    ///
    /// The `choice` is one of the ids the dialog itself published. An id
    /// not in the list is not interpreted: there are no implicit answers.
    Dialog {
        /// Dialog.
        id: ModalId,
        /// Chosen answer.
        choice: String,
        /// The password, and ONLY for a dialog that asks for one (#327).
        ///
        /// It travels here and deliberately not through
        /// [`Self::DialogInput`]. That one sends the WHOLE field on every
        /// keystroke, which is fine for a file name and for a password means
        /// `h`, `hu`, `hun`… cross the IPC and stay, each in its own bit of
        /// heap that nobody overwrites: a twenty-character password leaves
        /// twenty of its own prefixes along the way. With this it crosses
        /// ONCE, at the moment the reader decides to hand it over.
        ///
        /// The corollary is that **the host does not know what is being
        /// typed** until that moment, and does not need to: the field is
        /// masked by the renderer's own `input type=password`, so there are
        /// no keystrokes to count. What the host does not have cannot leak
        /// from it.
        ///
        /// `None` in every other dialog, and in a secret one means an empty
        /// field: confirming like that is INERT (see `responder_dialog`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret: Option<String>,
    },
    /// Re-lists ONE slot: the retry of one left in error.
    ///
    /// Per slot and not `pane.refresh`, which re-lists every visible one
    /// and acts on the focus: this is triggered by a click ON a specific
    /// slot's error, and refreshing the others along the way would be doing
    /// more than was asked.
    ///
    /// It is the gesture that turns a stalled pane into the matching
    /// question — a connection's password, typically: the host does not ask
    /// only at startup, because restoring a session is not requesting to
    /// connect.
    RefreshSlot {
        /// The slot being retried.
        slot_id: u32,
    },
    /// Show the log up to this level (#326).
    ///
    /// Raises the RING's level if needed and never lowers it: filtering on
    /// screen what was never logged is impossible, and stopping capture
    /// when lowering it would leave a hole the size of however long it was
    /// down.
    LogSetLevel {
        /// CLOSED vocabulary: `error`, `warn`, `info`, `debug`, `trace`. An
        /// unknown one is STATED, it does not fall back to `info`.
        level: String,
    },
    /// The log's text filter, over module and message.
    LogSetFilter {
        /// What was typed. Empty = everything.
        filter: String,
    },
    /// Scrolls up (`delta` negative) or down through the log, detaching
    /// from the end.
    LogScroll {
        /// Lines. The renderer sends whatever its wheel or key means.
        delta: i64,
    },
    /// A CELL of a plugin panel was clicked (phase 3).
    ///
    /// The cell travels, not a command: the host has the frame and resolves
    /// what zone it was and what command applies to it, with the same
    /// filter the terminal applies (`norte_frontend::frame::zone_can`).
    /// The renderer reports what happened; what it means is decided by
    /// whoever holds the state.
    PanelClick {
        /// Which slot.
        slot_id: u32,
        /// Row inside the frame, without the border.
        row: u16,
        /// Column inside the frame, without the border.
        col: u16,
    },
    /// Scrolls a slot's DOCKED viewer (#291): the wheel over it. Keys do
    /// not go through here — they go through the viewer's keymap when the
    /// slot has focus, like in the TUI.
    PreviewScroll {
        /// Which slot.
        slot_id: u32,
        /// Lines, negative is upward.
        delta: i64,
    },
    /// Scrolls the full-screen viewer: the WHEEL over it (bridge 59).
    ///
    /// Keys do not go through here — they go through the viewer's keymap —
    /// same as in [`UiAction::PreviewScroll`]. It exists because a wheel is
    /// not a key: the renderer knows how many lines a turn means on its
    /// platform, and manufacturing arrow keystrokes to express it would tie
    /// the gesture to nobody rebinding that arrow.
    ///
    /// Both axes in ONE action: the wheel with `shift` scrolls sideways, and
    /// splitting them would be two actions that are always sent by the same
    /// gesture.
    ViewerScroll {
        /// Lines, negative is upward.
        lines: i64,
        /// Columns, negative is toward the left.
        cols: i64,
    },
    /// Sticks the log back to the end and follows what arrives.
    LogFollow,
    /// Cycles the log's SOURCE: both → this window → the daemon (#328).
    ///
    /// ONE command and not three, and with no parameter: they are three
    /// states of the same question — "whose do I want to read?" — and a
    /// `set` with open vocabulary would force the host to validate a string
    /// the renderer has no reason to compose.
    ///
    /// Does nothing visible when there is no second source: then the
    /// renderer does not even paint the selector (`sources_available`).
    LogCycleSource,
    /// How many log rows fit in the last frame.
    ///
    /// Set by the renderer, like the listing's window: guessing it on the
    /// host is what made the TUI skip two lines on each page and four on the
    /// first, and what neither window showed could not be read at all.
    LogSetVisibleRange {
        /// Visible rows. Zero is treated as one.
        rows: u32,
    },
    /// The window gained or lost desktop focus (#285).
    ///
    /// The host needs this to avoid notifying about what is already being
    /// looked at from the outside: with the window in front, the bar and
    /// the board already report what a notification would report, and
    /// duplicating it is noise.
    ///
    /// Assumed FOCUSED until told otherwise: a renderer that does not send
    /// this behaves as before #285 — always notifies — instead of going
    /// silent, which would be losing notifications with nobody noticing.
    WindowFocus {
        /// `true` if the window is in front.
        focused: bool,
    },
    /// The reader picked a directory in the DESKTOP picker, or closed it
    /// without picking one (#284).
    ///
    /// The path comes from the renderer, so it is treated like everything
    /// from there: it is validated, and above all it is SHOWN in the
    /// confirmation before touching anything. What does NOT come in this
    /// message is what gets copied — that stays the host's state, per ADR
    /// 0069's rule.
    DirectoryPicked {
        /// The chosen NATIVE path, or `None` if the picker was closed. It is
        /// filesystem text, not a `VPath`: converting it is the host's job.
        path: Option<String>,
    },
    /// A program run and waited on (`NativeEffect::RunProgram`) finished
    /// (#312): what it printed, raw. The host masks it, splits it into
    /// lines and clamps it before showing it — it is another program's text
    /// about files anyone could have named.
    ProgramFinished {
        /// The title key that traveled in the effect.
        title_key: String,
        /// The argv that ran, already as text to display (lossy: it is for
        /// showing, not for running again).
        command: String,
        /// stdout and stderr, in that order, up to the host's cap.
        output: Vec<u8>,
        /// The host truncated the output.
        truncated: bool,
        /// It did not start, or it ran past the deadline.
        failed: bool,
    },
    /// The reader DROPPED files from the desktop onto the window (#283).
    ///
    /// Inbound only: dragging OUT is not offered, because that is
    /// publishing the marked items' paths to any application that accepts
    /// the drop, and that is a different design (ADR 0074).
    ///
    /// The paths come from ANOTHER process — the sender composes the list
    /// by hand if it wants to — so nothing is copied just by receiving them:
    /// they open the same confirmation as copying, with masked names. A
    /// drop is a gesture with no confirmation by nature, and this window
    /// asks before writing; the question is exactly what bounds the list
    /// being foreign.
    FilesDropped {
        /// NATIVE paths of this machine, exactly as the desktop sends them.
        /// Filesystem text, not `VPath`: converting them is the host's job,
        /// and one that fails to convert is dropped, saying so.
        paths: Vec<String>,
    },
    /// Types into the open dialog's text field.
    DialogInput {
        /// Dialog.
        id: ModalId,
        /// Full text after the edit (not a delta: the renderer owns the
        /// caret, and sending the whole text avoids rebuilding it in Rust).
        text: String,
    },
    /// Touches a field of a FORM dialog (bridge 91).
    ///
    /// Separate from [`Self::DialogInput`] and not an extension of it, for
    /// two reasons: that one names "the" field — there is no other — and it
    /// is the path a password does NOT travel by (#327), and a form has to
    /// say WHICH of its fields was touched. Mixing them would force the
    /// password dialog to carry a field id that means nothing.
    DialogField {
        /// Dialog.
        id: ModalId,
        /// The field's stable id, from the ones `DialogFieldView::id` sent.
        /// One the dialog does not have is dropped: the host decides the
        /// fields.
        field: String,
        /// What was done to it.
        value: DialogFieldValue,
    },
    /// Chooses a row of the differences pane, BY ITS ID.
    ///
    /// By id and not by index: a filter hides rows and would renumber them,
    /// and the selection has to keep naming the same one.
    CompareSelectRow {
        /// The id the row carried.
        id: u64,
    },
    /// Opens the chosen row: navigates to the ACTIVE side's directory.
    CompareActivateRow {
        /// The row's id.
        id: u64,
    },
    /// Shows or hides a whole category of the differences pane.
    CompareToggleFilter {
        /// The category's stable id (`same`, `different`…).
        category: String,
    },
    /// States which window of rows the renderer is painting.
    ///
    /// The comparison has no cap — a cap would turn "are they equal?" into
    /// a half-answer — so what crosses the bridge is a window, and this is
    /// what moves it.
    CompareSetVisibleRange {
        /// Index, among the VISIBLE ones, of the first painted row.
        first: u64,
        /// How many fit.
        count: u32,
    },
    /// Asks to cancel a task.
    CancelTask {
        /// The task's id.
        task_id: u64,
    },
    /// The window's size changed.
    ///
    /// In layout CELLS, not pixels: each pane's minimums are declared that
    /// way and shared with the TUI, so "this does not fit" means the same
    /// thing on both surfaces. Resizing redistributes again; it never
    /// rewrites the saved layout, which is the user's intent and not a
    /// function of their window's size.
    SetViewport {
        /// Width in cells.
        width: u16,
        /// Height in cells.
        height: u16,
    },
    /// The desktop asks for a light or dark scheme (`prefers-color-scheme`).
    ///
    /// Sent by the renderer at startup and every time it changes. The host
    /// needs it — and it is not enough for the renderer to just plug in the
    /// variant's CSS variables — because since bridge 66 an entry's color
    /// travels BAKED into its row: with `theme_dark = "vscode-dark"` and
    /// `theme_light = "vscode-light"`, switching the desktop to light
    /// repainted the whole screen with the light palette and left the NAMES
    /// with the dark theme's colors — blue #4daafc on white, 2.6:1, below
    /// the floor the presets themselves promise in their own header.
    SetColorScheme {
        /// `true` = the desktop asks for dark.
        dark: bool,
    },
    /// A key.
    ///
    /// The renderer sends the NORMALIZED key and nothing else: resolving a
    /// count, a half-typed prefix, or which command is bound is Rust's job,
    /// with the same resolver and the same presets as the TUI. Two keymaps
    /// would be two places to diverge without anyone noticing.
    Key(KeyInput),
    /// How many lines fit in the viewer.
    ///
    /// The host cannot know this: its grid is layout cells and the viewer's
    /// chrome is painted by the renderer. Guessing it did two things wrong
    /// at once — sending more lines than fit, which get clipped without
    /// saying so, and advancing a page by a number different from what is
    /// shown — so every page silently skipped whatever got clipped.
    SetViewerRows {
        /// Visible lines.
        rows: u32,
    },
    /// How many CELLS wide the viewer's body is, measured by whoever
    /// paints. It is what gets told to the previewer (proto 0.66.0) the
    /// next time it opens: the whole viewport counted the chrome, and an
    /// image shrunk to it spilled out on the right.
    SetViewerCols {
        /// Body width in cells.
        cols: u32,
    },
    /// Puts the help sidebar's cursor on that row and SHOWS whatever is
    /// there (a click).
    ///
    /// Shows, does not navigate, which is what the same arrow key does:
    /// walking the index must not leave the reader a step back that has to
    /// be undone with `⌫` before being able to close it. A group header and
    /// an out-of-range row do nothing.
    HelpSelectTopic {
        /// Sidebar row, as it traveled in `sidebar`'s order.
        row: u32,
    },
    /// Acts on an executable row of the help body (a click): runs the
    /// command, or opens the linked page.
    ///
    /// Goes through the SAME path as `enter`, and that through the same one
    /// as a key: help is another door into the catalogue, not a second
    /// dispatcher.
    HelpActivate {
        /// Index within `actions`.
        index: u32,
    },
    /// Puts the settings cursor on that row (a click).
    ///
    /// Only moves. Activating it is [`Self::SettingsActivate`].
    SettingsSelectRow {
        /// Row, counting ALL of them across all sections in order.
        row: u32,
    },
    /// Activates that settings row (a double click): whatever cycles,
    /// cycles; whatever is typed is asked for in a dialog. The same path as
    /// `enter` (bridge 60).
    SettingsActivate {
        /// Row, counting ALL of them across all sections in order.
        row: u32,
    },
    /// What is typed in the settings search box.
    ///
    /// The whole TEXT travels, not the key: this window's search box is a
    /// browser `<input>`, and printable keys do not reach the host — which
    /// is why this screen had no filter until now.
    SettingsQuery {
        /// The text exactly as it is in the box.
        text: String,
    },
    /// Takes the cursor to a section, by its STABLE key (`appearance`,
    /// `open-with`…).
    ///
    /// By the key and not the translated label: the index sends back what
    /// the host gave it, and a label traveling round-trip would tie the
    /// jump to the language.
    SettingsJumpSection {
        /// The section's stable key, from the view's `index`.
        section: String,
    },
    /// Resets that row: removes its key from the write layer.
    SettingsReset {
        /// Row, counting ALL of them across all sections in order.
        row: u32,
    },
    /// Sets a SPECIFIC value on a setting: what a toggle, a dropdown or a
    /// numeric field of the window sends.
    ///
    /// By the catalogue's ID, not by row: a control takes as long as the
    /// reader takes to release it, and the filter behind it may have
    /// changed which rows there are. A position does not name a row in a
    /// list that moves.
    ///
    /// And it is SETTING, not activating: `settings_activate` cycles, so
    /// choosing a dropdown's seventh theme would be seven trips and six
    /// writes to `norte.toml`. The value is validated by the shared editor,
    /// never by the renderer.
    SettingsSet {
        /// The catalogue's id (`ui.theme`).
        id: String,
        /// The value, as text. A boolean travels as `true`/`false`.
        value: String,
    },
    /// Brings this slot's tab to the front (a click).
    SelectTab {
        /// The slot inside the chosen tab.
        slot_id: u32,
    },

    /// Chooses an agent session by position (a click).
    AgentSelectRow {
        /// Row within the painted list.
        row: u32,
        /// The generation of the list the renderer was painting.
        ///
        /// The list changes WITHOUT a gesture — a permission request
        /// reorders it — so a click against the previous one chooses a
        /// different row. Out of generation is refused: here "this row" is
        /// whose work gets undone.
        generation: u64,
    },

    /// Chooses an extension in the manager (a click) and requests its
    /// detail card.
    ExtensionSelectRow {
        /// Row, in the order they traveled.
        row: u32,
    },
    /// Governs that row's extension from a BUTTON (bridge 61): selects it
    /// and does exactly what the keyboard verb would do to it —
    /// `dialog.add`, `dialog.toggle-enabled`, `dialog.remove` — with the
    /// same questions. A button is not a shortcut to skip the consent
    /// dialog or the removal one: it is another way to reach it.
    ///
    /// Carries the row AND its id: the catalogue is re-requested after
    /// every governing action and lands in the background, so a row deleted
    /// ABOVE the one pressed shifts every one below it, and an index alone
    /// would only name its neighbor. If that row's id is no longer this
    /// one, it is refused as stale.
    ExtensionGovern {
        /// Row, in the order they traveled.
        row: u32,
        /// The id the renderer saw in that row.
        id: String,
        /// What is changed.
        change: ExtensionChange,
    },
    /// Opens help on that row's extension page (bridge 61), what `app.help`
    /// does on the selected row in the terminal. Closes the manager, as it
    /// does there: help replaces it. Same row+id pair as
    /// [`Self::ExtensionGovern`], for the same reason.
    ExtensionHelp {
        /// Row, in the order they traveled.
        row: u32,
        /// The id the renderer saw in that row.
        id: String,
    },
    /// Puts a picker's cursor on that row (a click).
    PickerSelectRow {
        /// Row, in the order they traveled.
        row: u32,
        /// The generation with which that row was painted. The volume
        /// picker opens empty and fills afterward: same race.
        generation: u64,
    },
    /// Chooses a row of the places sidebar (a click) and ACTIVATES it:
    /// navigates to it, or folds its section if it is a header.
    ///
    /// Selects and activates at once, unlike the other lists: a sidebar
    /// exists to go places, and a click that only moves a cursor forces a
    /// follow-up with the keyboard.
    PlaceActivateRow {
        /// Row, in the order they traveled.
        row: u32,
        /// The generation with which that row was painted.
        ///
        /// Mandatory because this list CHANGES on its own: volumes arrive
        /// from a background task and get inserted before the favorites, so
        /// an index with no generation could name a row that is no longer
        /// the one clicked. One that does not match is rejected.
        generation: u64,
    },
    /// Chooses a tree branch (a click) and NAVIGATES to it: the focused
    /// listing goes to that directory, and the branch stays expanded.
    ///
    /// Expand *and* navigate, both: whoever clicks a branch wants to see
    /// what is inside, and seeing it in the listing is the complete answer.
    /// The tree stays where it is, which is what makes keeping it open
    /// useful.
    TreeActivateRow {
        /// Row, in the order they traveled.
        row: u32,
        /// The generation with which it was painted. Mandatory for the same
        /// reason as the places bar: expanding requests a listing, and that
        /// listing inserts rows IN THE MIDDLE when it arrives.
        generation: u64,
    },
    /// Folds or expands the branch, without navigating anywhere.
    TreeToggleRow {
        /// Row, in the order they traveled.
        row: u32,
        /// The generation with which it was painted.
        generation: u64,
    },
    /// Chooses a layout from the picker (a click) and APPLIES it.
    LayoutActivateRow {
        /// Row, in the order they traveled.
        row: u32,
    },
    /// Chooses a search result (a click) and GOES to it: the pane navigates
    /// to its directory and the cursor lands on it.
    ///
    /// The renderer sends an INDEX, never a path: the host has had the
    /// exact path since the daemon sent it, and reconstructing it from
    /// painted text is how you end up opening a different file.
    SearchActivateRow {
        /// Row, in the order they traveled.
        row: u32,
    },
    /// Answers the review of a rename plan: apply it or discard it.
    ///
    /// Exists alongside the keys because the review opens ON ITS OWN and
    /// keeps the keyboard: without it, the only way to answer was a key, and
    /// a reader using the mouse could not even get it out of the way. And
    /// unlike a key, a click on a button is a gesture AIMED at this screen —
    /// it cannot be a key meant for somewhere else.
    AiRenameDecide {
        /// `true` = apply. `false` = discard.
        approve: bool,
    },
    /// Answers the review of an ORGANIZE plan (phase 8), for the same
    /// reason and with the same contract as its twin above.
    OrganizeDecide {
        /// `true` = apply. `false` = discard.
        approve: bool,
    },
    /// The handoff to the terminal did NOT manage to open it (phase 9):
    /// the host process found no emulator, or the one it found did not
    /// start.
    ///
    /// Sent by the native-effects thread, like [`Self::DirectoryPicked`]: it
    /// is the one that knows whether the terminal opened. Without this the
    /// window kept saying "handing off the screen…" with the session
    /// already released, and the reader did not know they had to keep going
    /// here.
    ///
    /// Deliberately no free text: anyone talking to the host can send an
    /// action, and a reason written by the sender would be a message the
    /// host would paint without having written it. What can be said is two
    /// things, and a bool tells them apart.
    HandoffFailed {
        /// `true` = there is no terminal emulator at all on PATH; `false` =
        /// there was one and it did not start.
        no_terminal: bool,
    },
    /// Walks the organize tree without deciding anything (phase 8): it is a
    /// scrolling screen, and approving requires having reached the end —
    /// without a gesture to walk it, a reader using the mouse could never
    /// approve.
    OrganizeScroll {
        /// `true` = downward.
        down: bool,
    },
    /// Opens a menu-bar entry by its index, or closes the open one if it
    /// was already that one (a click on the open title folds it).
    MenuOpen {
        /// Which menu, in the order its titles traveled.
        menu: u32,
    },
    /// Moves the cursor within the open menu (the mouse hovering over it).
    MenuPointRow {
        /// Which entry, in the order they traveled.
        row: u32,
    },
    /// Runs an entry of the open menu (a click).
    ///
    /// Carries the row and not the command: what the renderer knows is
    /// where the reader clicked, and the host resolves the command against
    /// the menu it itself has open. A command id coming from the renderer
    /// would be a dispatcher running parallel to the keymap (ADR 0069).
    MenuActivateRow {
        /// Which entry, in the order they traveled.
        row: u32,
    },
    /// Closes the open menu without running anything (a click outside).
    MenuClose,
    /// Alt pressed and released ALONE, with no other key in between
    /// (bridge 68).
    ///
    /// It is the desktop gesture for going to the menu bar: it folds the
    /// menu if it is open and, if not, opens it like `app.menu`. It is not
    /// a key because a lone modifier is not a chord the keymap can name,
    /// and it carries no command because an id coming from the renderer
    /// would be a dispatcher running parallel to the keymap (ADR 0069). With
    /// a screen holding the keys in front — a dialog, help — it does
    /// nothing, same as F9 there.
    MenuToggle,
    /// Opens the first-run wizard (spec 2026-09-10, bridge 63). Sent by the
    /// renderer at startup when the catalogue says `first_run`: there is no
    /// user `norte.toml` yet.
    WizardOpen,
    /// Shows the splash screen, if the configuration wants it.
    ///
    /// Sent by the renderer at startup, like `wizard_open`: the host is the
    /// one that knows whether `[ui] splash` says `brief`, `home` or `off`,
    /// and which yields to the first-run wizard. The renderer does not
    /// decide, it only reports that this is startup.
    SplashOpen,
    /// Dismisses the splash screen (bridge 69, ADR 0115).
    ///
    /// Sent by the renderer on any key, any click, or when the deadline
    /// the screen itself brought expires (`close_after_ms`). Deliberately
    /// not a keymap command: no key is bound to dismiss it, it is dismissed
    /// with whichever one, which is what a person tries.
    SplashClose,
    /// Opens whatever a NUMBERED row of the splash screen says.
    ///
    /// The number is the one the row shows (1..=9), not its index: it is
    /// what the reader types, and counting it here from zero would be
    /// asking them to subtract.
    SplashActivateRow {
        /// The number painted on the row.
        number: u8,
    },
    /// Chooses a row of the wizard AND confirms it: what a click does.
    WizardActivateRow {
        /// Which row, in the order they traveled.
        row: u32,
    },
    /// Presses a panel-bar button (#324, bridge 51): opens the panel if
    /// closed and closes it if open.
    ///
    /// Carries the index and not the command, for the same reason as the
    /// menu: the host resolves the button against the bar it itself sent,
    /// and the panel opens through the SAME dispatch as its shortcut (ADR
    /// 0069, ADR 0077).
    PanelBarActivate {
        /// Which button, in the order they traveled.
        button: u32,
    },
    /// Presses a status-bar item (ADR 0132, bridge 85).
    ///
    /// By ID and not by position, like `settings_set`: the list changes
    /// with the cursor and the width, and it may have moved between the
    /// paint and the click. The host resolves the command with the same
    /// code as the TUI and runs it through the same dispatch as its
    /// shortcut.
    StatusItemActivate {
        /// The item's id (`sort`, `tasks`…).
        id: String,
    },
    /// Presses a layout button on the menu bar (ADR 0133, bridge 86), by
    /// ID.
    LayoutButtonActivate {
        /// The button's id (`split-h`, `pick`…).
        id: String,
    },
    /// A button on a group's tab bar (ADR 0133, bridge 86): open a tab in
    /// that group, or close the one at `slot_id`.
    ///
    /// The tab at `slot_id` is CHOSEN first — the pressed group gets focus,
    /// like in the TUI — and only then does the command run through its
    /// shortcut's dispatch. Pressing a group's `+` and having the tab born
    /// in the other one would be the opposite of what the finger said.
    TabAction {
        /// The tab being acted on.
        slot_id: u32,
        /// What to do. `verb` and not `action`: `action` is the enum's tag
        /// in the JSON.
        verb: TabVerb,
    },
    /// Drags the border between `slot_id` and the slot next to it.
    ///
    /// `cells` is WHERE the pointer is on the split's axis, in layout
    /// cells — not a size or a delta. The renderer knows how to convert
    /// pixels to cells because it already does so to declare its viewport;
    /// what that position means — which pair splits, how much each gets,
    /// what minimums apply — is decided by the host with the split it
    /// itself computed (ADR 0069).
    ResizeSlot {
        /// The slot to the LEFT of the border (or the one ABOVE).
        slot_id: u32,
        /// The pointer's position on the split's axis, in cells.
        cells: u16,
    },
    /// Drops slot `slot_id`, dragged by its title, onto `target` (bridge 90,
    /// ADR 0138): onto one of its sides, or in the center to join it as a
    /// tab. What happens to the tree is decided by the host
    /// (`Node::move_slot`); an id that is no longer there changes nothing.
    MoveSlot {
        /// The slot being dragged.
        slot_id: u32,
        /// The slot it is dropped onto.
        target: u32,
        /// Where, within `target`.
        zone: norte_frontend::layout::DropZone,
    },
    /// Chooses a row of the PROFILE picker and activates it (a click).
    ///
    /// Selects and activates at once, like the sidebar: a profile picker
    /// exists to switch profiles, and a click that only moves a cursor
    /// forces a follow-up with the keyboard.
    ProfileActivateRow {
        /// Row, in the order they traveled.
        row: u32,
        /// The generation with which it was painted. The list fills in from
        /// a background task: without this, an index names a different
        /// profile.
        generation: u64,
    },
    /// Asks for a full snapshot: the renderer lost track of the sequence.
    Resync,
    /// The reader wants to CLOSE the window.
    ///
    /// Does not close it: asks whether it should ask. With `[ui]
    /// confirm_quit` requesting it — always, or only if work remains — it
    /// opens the dialog and waits; if not, it answers with
    /// [`crate::dto::NativeEffect::CloseWindow`]. The host process does not
    /// decide this: it is configuration.
    RequestQuit,
}

/// What [`UiAction::ExtensionGovern`] changes about an extension (bridge
/// 61).
///
/// The manager's three verbs, with their own name and not the keyboard's:
/// a renderer paints buttons, and "add" on an already-approved extension
/// means revoke — the host resolves it by looking at its state, same as
/// with the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionChange {
    /// Grant its capabilities if it does not have them; withdraw them if it
    /// does. Granting ASKS, enumerating them.
    Approval,
    /// Turn it on if it is off; turn it off if it is on. Turning on an
    /// unapproved one is refused, and said.
    Enabled,
    /// Uninstall it: delete its files and withdraw its consent. ASKS,
    /// because it is irreversible.
    Uninstall,
}

/// What was done to a FORM dialog's field (bridge 91).
///
/// A toggle and a cycle carry no value: what the renderer says is that they
/// were TOUCHED, and which state they go to is decided by Rust. Sending the
/// target state would let two quick presses step on each other — the second
/// born from an earlier snapshot — and the renderer does not own that
/// state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "set")]
pub enum DialogFieldValue {
    /// Full text after the edit, for the same reason as
    /// [`UiAction::DialogInput`]: the caret belongs to the renderer.
    Text {
        /// What is typed in the field.
        text: String,
    },
    /// The toggle was pressed.
    Toggled,
    /// It moved to the cycle's next value.
    Cycled,
}
