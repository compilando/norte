//! Pure TUI state (panes, cursor, name presentation) and the ERROR
//! presentation for the bar (#73): Fluent categories
//! ([`error_key`]/[`error_category`] and company) + detail sanitizing
//! ([`detail_for_bar`]). A testable machine with no terminal — the render
//! (`ui`) and the I/O (`main`) live apart; the Lua scripts (M4) consume the
//! STABLE key of [`error_key`] from here.

use norte_i18n::Lang;
use norte_proto::VPath;

#[cfg(test)]
pub(crate) mod testutil;

mod banners;
mod caps;
mod compare;
mod dialogs;
mod errors;
mod focus;
mod help_view;
mod layout;
mod modal;
mod nav;
mod nav_popup;
mod ops;
mod palette;
mod pane;
mod pickers;
mod plugins;
pub mod profile;
mod prompts;
mod session;
mod trail;

pub use dialogs::*;
pub use errors::*;
pub use help_view::*;
pub use modal::*;
pub use nav_popup::*;
pub use palette::*;
pub use pane::*;
pub use plugins::*;
pub use trail::*;

// Private in `app` before the split: the glob above only re-exports what is
// `pub`, so these three are named one by one.
use help_view::default_help_chords;

/// The format a name suggests lives in the SHARED crate: the TUI and the
/// window offer the same dialog (D14).
pub use norte_frontend::nav::format_by_name;

/// The size with a suffix is read by the SHARED crate: the same dialog asks
/// for it on both surfaces (D14).
pub use norte_frontend::nav::parse_size;

/// The run state (`CompareState`) and the open panel (`CompareView`) live in
/// [`norte_frontend::compare`] (#158): the GUI needs exactly this machine and
/// not a reimplemented one, which is how the CLI (phase A) and the MCP tool
/// (phase B) each went wrong on their own — both treated a response missing
/// batches as complete. See [`CompareState::Incomplete`] for why closing the
/// channel is not enough.
pub use norte_frontend::compare::{CompareState, CompareView};

/// The run state (`SyncRunState`) and the open panel (`SyncView`) live in
/// [`norte_frontend::sync`] (#161, the same argument that already took
/// [`CompareView`] there): the GUI needs exactly this wrapper around the run
/// and not a reimplemented one. C1 learned, at the cost of a branch review,
/// that moving the TYPE and leaving its decisions hand-written in each
/// frontend is worse than not moving it — so what travels with it is the
/// `TaskState` → [`SyncRunState`] mapping ([`SyncRunState::from_task_state`])
/// and the update bundle for "`sync.apply` was approved and started"
/// ([`SyncView::on_apply_started`]), not just the struct.
pub use norte_frontend::sync::{SyncRunState, SyncView};

// Name sanitizing ([`display_name`]/[`path_display`]/`must_mask`) and listing
// order ([`sort_entries`] + `nfc_key`/`name_bytes`) now live in
// `norte-frontend` (PURE presentation logic shared with the GUI). Re-exported
// here so the call sites at `crate::app::…`/`app::…` (main, ui, viewer) keep
// resolving unchanged.
pub use norte_frontend::{display_name, path_display, sort_entries};

/// External command that `pane.open` (F4) left resolved and the run loop will
/// launch (#28). Resolving is split from launching because the terminal's
/// owner is the run loop, not dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOpen {
    /// Binary to probe on the `PATH` before launching anything.
    pub program: String,
    /// The full argv, with the binary in `[0]` and byte-exact paths.
    pub argv: Vec<std::ffi::OsString>,
    /// `true` when it is the desktop launcher (`xdg-open`/`open`/
    /// `explorer.exe`): it hands the file to its associated program and
    /// returns right away, so the TUI is **not** suspended — doing so would
    /// paint a full-screen flicker for nothing. `false` is an opener declared
    /// in `ns.toml`, which can be `bat` or an editor and needs the whole
    /// terminal to itself.
    pub detached: bool,
    /// The directory of the focused pane, which the child receives as its
    /// cwd (#144).
    ///
    /// The three shell commands (#135) already passed it and the openers did
    /// not, so an editor opened on a file from the pane inherited norte's cwd
    /// and saved where nobody was looking. This was left as is on purpose in
    /// the shell wave —changing it changes behavior— and was decided on
    /// 2026-08-14: pass the pane's. `None` only when the path does not
    /// convert to native, where there is nothing better to inherit.
    pub cwd: Option<std::path::PathBuf>,
}

/// A SUSPENSION that dispatch resolved and the run loop will execute (#135).
///
/// Same split as [`PendingOpen`] and for the same reason: whoever owns the
/// terminal is the run loop, not dispatch. What changes is that here there is
/// no PRIOR probe to report in the bar — the argv comes from `$SHELL`, from
/// `$EDITOR` or from a line the user typed, and the program not existing is
/// reported through the launch error, not through a separate probe that
/// would guess the same thing.
///
/// What does happen at launch time is RESOLVING the program to an absolute
/// path (#302): the child is launched with [`Self::cwd`] set, and on unix
/// `current_dir` is applied before the program is resolved. See
/// [`crate::suspend::run_suspended`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingShell {
    /// The full argv, with the binary in `[0]`. EMPTY is legitimate and means
    /// "launch nothing": it is `app.toggle-panels`, which only shows the
    /// host terminal.
    pub argv: Vec<std::ffi::OsString>,
    /// The child's working directory. `None` = norte's own (which is what
    /// the openers of #28 do today).
    pub cwd: Option<std::path::PathBuf>,
    /// Wait for a key BEFORE repainting the panes. This is what makes a
    /// command's output legible: without it, the listing comes back over
    /// whatever was just printed.
    pub wait_for_key: bool,
    /// The path that has to still be a REGULAR file at the instant of the
    /// launch, or nothing gets launched (#303).
    ///
    /// Set by `pane.edit-new` and only by it: it is the gesture where norte
    /// ANNOUNCES a name by creating it and then hands it to another program.
    /// It travels in the suspension —and is not checked where the gesture is
    /// resolved— because between one thing and the other the two panes'
    /// re-listing runs: the check is only worth the gap it leaves behind, and
    /// here the gap is the `exec`.
    ///
    /// `None` = nothing to check, which is what the shell, the command line
    /// and `app.toggle-panels` carry.
    pub check_regular: Option<norte_proto::VPath>,
}

/// The key of the capability cache: the DIRECTORY, in wire form.
///
/// It used to be `(scheme, authority)` — one backend — and that stopped being
/// the right question when ADR 0054 made the daemon answer per LOCATION
/// (#215). Under one `file://` there are mounts: an exFAT stick that folds
/// case, an ext4 subtree in `+F`, a read-only bind. An answer cached for
/// `/home` was served for every one of them.
///
/// Owned because the map owns its keys, and the lookups are per help open and
/// per cd — not per frame.
type CapsKey = String;

/// The location `at` belongs to, as a cache key: the directory itself.
fn caps_key(at: &VPath) -> CapsKey {
    at.to_wire()
}

/// How many locations the capability cache keeps.
///
/// It was unbounded when the key was one per backend — there are seven schemes
/// — and a key per DIRECTORY is not: a session that walks a big tree would
/// grow it without end. Sixty-four is far more than the directories a reader
/// keeps coming back to, and the eviction is by insertion order, which for a
/// cache whose entries cost one round trip each is the honest cheap answer:
/// the oldest location is the one least likely to be the next cd.
const CAPS_CACHE_MAX: usize = 64;

/// What the run loop has to ask the DESTINATION before the human says yes:
/// whether it fits (#149) and whether it knows how to hold its writes (#164).
///
/// They travel together because they are the same question asked of the same
/// place at the same time, and splitting them would cost two I/O round trips
/// per dialog to paint two adjacent lines.
#[derive(Debug, Clone)]
pub struct DestCheck {
    /// The DESTINATION directory, which everything here is asked of.
    pub to: VPath,
    /// Bytes the transfer is going to write, if known.
    ///
    /// `None` = some item does not say how much it takes up (a directory does
    /// not carry it in the listing), and then there is NO space question:
    /// adding up only what is known would warn with a number smaller than the
    /// real one (computed by `App::transfer_total`, private). The confinement
    /// question is the same — it does not depend on size, and it is exactly
    /// the recursive case that needs it most.
    pub total: Option<u64>,
}

/// What the bar says about THIS session's journal.
///
/// An enum, not an `Option<NoJournal>` plus a bool: they are mutually
/// exclusive states of one thing —which sentence applies— and two fields
/// could disagree.
#[derive(Debug, Clone)]
enum JournalIndicator {
    /// Not recording, for this reason (#177/#178).
    NotRecorded(norte_core::embedded::NoJournal),
    /// And has been like this for minutes with no daemon to explain it (#203).
    Squatted,
}

/// Who holds the keyboard for the body of the screen.
///
/// This is NOT the focus. [`App::focus`] keeps pointing at the LISTING you
/// were on, and every operation —a copy, a delete, a `cd`— still goes there:
/// what this decides is only who the keys go to while an auxiliary panel is
/// in front, the same as help or the palette do.
///
/// It exists because `App::focus` is an index over the VISIBLE listings, so a
/// sidebar cannot have one without the refactor to `SlotId` that P6 deferred.
/// The day that refactor lands, this folds into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyOwner {
    /// The listings, which is the usual case.
    #[default]
    Panes,
    /// The places sidebar.
    Places,
    /// The docked viewer.
    Preview,
    /// The processes panel.
    Processes,
    /// The directory tree (#136).
    Tree,
    /// The log panel (#323).
    Log,
    /// The disk map (phase 4).
    ///
    /// No payload, like all of these: `KeyOwner` is compared for equality in
    /// eighty-six places, and the map declares `multi: false`, so there is at
    /// most one and the layout already knows which.
    DiskMap,
    /// The journal timeline (phase 7). No payload, for the same reason as the
    /// map: it declares `multi: false`, so there is at most one.
    Timeline,
    /// The terminal panel (#362). No payload, and that is the reason the kind
    /// declares `multi: false`: with several you would have to carry WHICH
    /// one has the keys, and this type is compared for equality in
    /// eighty-six places.
    ///
    /// It is the only owner that keeps the BYTES instead of the commands:
    /// while it is, everything typed goes to the shell except the bare chord
    /// that opened it (see [`crate::termpanel`]).
    Terminal,
    /// A panel contributed by a PLUGIN (phase 3, ADR 0115/0116).
    ///
    /// WITHOUT saying which, on purpose. `KeyOwner` is compared for equality
    /// in eighty-six places —`keys.rs`, `ui.rs`, `dispatch.rs`— and a variant
    /// with a payload would break all of them; and it is not needed: a
    /// plugin panel declares `multi: false`, so there is at most one visible
    /// and the layout already knows which one it is. Who paints it is asked
    /// of the tree, which is where that truth lives.
    Panel,
}

/// The key bar cells of the three screens (spec 2026-09-10), in the language
/// in force when they were built.
#[derive(Debug, Clone, Default)]
pub struct KeyBars {
    /// With the listings or a side panel: the effective `browse`.
    pub browse: Vec<norte_frontend::keybar::KeyCell>,
    /// With the viewer full screen.
    pub viewer: Vec<norte_frontend::keybar::KeyCell>,
}

impl KeyBars {
    /// From the two effectives with function keys, in the active language.
    /// `dialog`'s does not enter: no preset binds an `F` there, and with a
    /// modal or an overlay in front the row goes blank (`App::key_bar_cells`).
    #[must_use]
    pub fn build(
        browse: &norte_frontend::keymap::Effective,
        viewer: &norte_frontend::keymap::Effective,
    ) -> Self {
        let lang = norte_i18n::active();
        Self {
            browse: norte_frontend::keybar::cells_in(browse, lang),
            viewer: norte_frontend::keybar::cells_in(viewer, lang),
        }
    }
}

impl App {
    /// Is the log panel in the layout? This is what zeroes the unread
    /// notices: if it is there, the reader has them in front.
    #[must_use]
    pub fn log_panel_open(&self) -> bool {
        self.layout.slot_ids().into_iter().any(|id| {
            self.layout
                .kind_of(id)
                .is_some_and(|k| k.as_str() == crate::logview::KIND)
        })
    }

    /// A one-second tick over the bar's notice (spec 2026-09-10,
    /// `[ui] notice_seconds`): past the cap, the message leaves the bar, goes
    /// to the log (through `tracing`, which is what the panel shows) and the
    /// `!n` badge counts one more. With `0` nothing expires: the message
    /// stays until the next key, as always. Persistent banners do not go
    /// through here: they are state, not a notice.
    pub fn tick_notices(&mut self) {
        if self.log_panel_open() {
            self.notices_unread = 0;
        }
        let Some(msg) = self.message.as_deref() else {
            self.message_ticks = 0;
            self.message_counted = None;
            return;
        };
        if self.message_counted.as_deref() == Some(msg) {
            self.message_ticks = self.message_ticks.saturating_add(1);
        } else {
            self.message_counted = Some(msg.to_owned());
            self.message_ticks = 1;
        }
        let cap = self.chrome.notice_seconds();
        if cap > 0 && self.message_ticks >= cap {
            let text = self.message.take().unwrap_or_default();
            self.message_ticks = 0;
            self.message_counted = None;
            self.notices_unread = self.notices_unread.saturating_add(1);
            // `info`, not `warn`: "copied 1 file" is not a warning, and the
            // level is what the log panel filters by.
            tracing::info!(target: "norte::notice", "{text}");
        }
    }

    /// The cells of the screen that has the keys RIGHT NOW: with a modal or
    /// an overlay in front, NONE —the row goes blank: no preset binds an `F`
    /// in `[dialog]`, and a cell announcing a verb the active modal refuses
    /// would be the lie `hints` exists so as not to tell—; the viewer full
    /// screen, its own; otherwise, the listings'. The viewer is asked BEFORE
    /// `overlay_open`, which includes it: it is the same order as
    /// `vista_barra_de_teclas` in the window (ADR 0077).
    #[must_use]
    pub fn key_bar_cells(&self) -> &[norte_frontend::keybar::KeyCell] {
        if self.modal.is_some() || self.help.is_some() || self.wizard.is_some() {
            &[]
        } else if self.viewer.is_some() {
            &self.key_bars.viewer
        } else if crate::mouse::overlay_open(self) {
            &[]
        } else {
            &self.key_bars.browse
        }
    }
}

/// What a click on a row of the places sidebar does (#226).
///
/// Whatever the model could do is already done by the time this returns;
/// this is what needs the backend, which [`App`] does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacesClick {
    /// The cursor moved and the keyboard came to the sidebar. Nothing else
    /// to do.
    Focused,
    /// A section was folded or unfolded: unfolding the drives is the moment
    /// to ask for them again, same as by keyboard.
    Folded,
    /// The listing has to be taken to wherever [`App::places_activate`] says.
    Activate,
}

/// The editor the configuration names (`[ui] editor`).
///
/// The template AS IS, with its field codes unexpanded: expanding them needs
/// the file and the directory, which are only known at the moment of the
/// gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorSpec {
    /// The template argv (`["zed", "%f"]`). The first token is the binary.
    pub command: Vec<String>,
    /// Opens its own window: the terminal is not suspended waiting for it.
    pub detached: bool,
}

/// Where a click landed inside a row of the tree (#136).
///
/// The MARK and the rest of the row do not do the same thing, and the name
/// says so: a `bool` in the call reads as "true" at the site where it
/// matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeSpot {
    /// On the `▾`/`▸`/`·`: folds or unfolds that branch.
    Mark,
    /// On any other cell of the row.
    Row,
}

/// What a click on a row of the tree does (#136).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeClick {
    /// The cursor moved (or the branch folded) and the keyboard came to the
    /// tree. Nothing else to do.
    Focused,
    /// The listing has to be taken to wherever [`App::tree_activate`] says.
    Activate,
}

/// What this process knows about the saved session (L2).
///
/// Grouped instead of five loose fields on [`App`]: they are one thing —the
/// screen that gets saved— and the three private ones only make sense among
/// themselves.
#[derive(Debug, Default)]
pub struct SessionUi {
    /// This window is NOT the owner: another one has it, so this one starts
    /// with the same screen and from then on goes its own way without
    /// writing anything. It is announced on open with a message and, for as
    /// long as it lasts, with a permanent mark in the status bar
    /// ([`App::session_banner`], #232).
    ///
    /// The window that finds a body from a newer version also becomes
    /// detached: it is not read, and above all it is not overwritten.
    pub detached: bool,
    /// This process comes from a HANDOFF (`--attach`, phase 9), so besides
    /// the screen it also claims the MARKS the other frontend left.
    ///
    /// Without this switch there is no way to tell a handoff apart from an
    /// ordinary start, and they are the same reading with two different
    /// correct answers: in a handoff seconds have passed and returning what
    /// was marked is returning the work that was in progress; in a start
    /// hours have passed, and it would be like putting an `F8` on what you
    /// marked yesterday.
    pub attach: bool,
    /// The revision this process holds as current, ONLY to start the
    /// session writer.
    ///
    /// From then on the real one is kept by the writer, which is the one
    /// that sees the core's responses; this one is only refreshed when it
    /// hears of a handoff. It is not compared against anything: reading it
    /// to decide something would be reading a stale number.
    pub revision: u64,
    /// Per-slot state that came in the session and that this layout does NOT
    /// have.
    ///
    /// Kept and written back as is: switching layout must not cost you the
    /// history of a panel you are going to return to. Trimmed by
    /// [`norte_frontend::session::SessionBody::prune`], which is the one that
    /// knows how many orphans fit.
    orphans: std::collections::BTreeMap<u32, norte_frontend::session::SlotState>,
    /// The layouts of the OTHER profiles, exactly as they arrived.
    ///
    /// Same treatment as the orphans and for the same reason: this session is
    /// the screen for SEVERAL profiles and this process only looks at one, so
    /// the others' data travels back intact. Writing only the active one
    /// would erase from the document the place where the others had left
    /// their panels (ADR 0079, D5).
    other_layouts: std::collections::BTreeMap<String, norte_frontend::layout::Node>,
    /// When each slot was last touched (epoch ms), for the age-based sweep.
    /// Stored instead of stamped at capture time because capturing is not
    /// touching: two captures of the same screen back to back have to
    /// produce the same document.
    touched: std::collections::HashMap<u32, u64>,
    /// The cursor the session carried, until the listing that can place it
    /// arrives: on an empty pane, putting the cursor on row 12 is putting it
    /// on row 0.
    cursors: std::collections::HashMap<u32, u64>,
    /// What a handoff (phase 9) had MARKED, until the listing arrives.
    ///
    /// Same treatment and same moment as [`Self::cursors`], and for a reason
    /// the pilot uncovered: the pane is born empty and `set_listing` clears
    /// the marks when the listing arrives —the right thing for a `cd`— so
    /// seeding them earlier would wipe them and the handoff would return the
    /// screen without what was marked.
    ///
    /// Only filled with `--attach`: an ordinary start is not a handoff.
    marks: std::collections::HashMap<u32, Vec<norte_proto::VPath>>,
    /// Which slots the saved session KNEW about, exactly as read from disk.
    ///
    /// Needed by `[profile.start]`, which only seeds the slot the session
    /// knows nothing about (ADR 0098). And it has to be what was READ, not
    /// `App::session_body()`, which is the screen NOW: that one names every
    /// live slot, so asking it would never seed the profile.
    read: std::collections::BTreeSet<u32>,
    /// The slots this process already seeded from `[profile.start]`.
    ///
    /// Seeding is a FIRST-time thing, and without this count a reader with no
    /// saved session —a fresh install— would return to the profile's start
    /// directory every time they entered and left it, which is ADR 0098's
    /// decision 2 turned backwards.
    seeded: std::collections::BTreeSet<u32>,
}

/// Rows `cursor.page-up/down` jumps (fixed until the pane's real height
/// travels with the command).
///
/// Lives here and not in the binary because EVERYTHING with a list paginates
/// with it: the panes, help, settings and the shortcut editor — and that last
/// one left the binary before the rest, which is when a shared constant stops
/// being able to live in the one that is leaving.
pub const PAGE: usize = 10;

/// Complete TUI state: the panels and the focus.
// `struct_excessive_bools`: the lint looks for APIs whose boolean parameters
// get confused with each other at the call site. This is not an API: it is
// the TUI's complete state, and its flags are independent of each other, are
// read by name and never travel together as arguments. Grouping them into
// sub-structs just to count bools would hide what each painter looks at, for
// nothing in return.
#[expect(clippy::struct_excessive_bools, reason = "TUI state, not an API")]
pub struct App {
    /// The two panels (left, right), stored by slot.
    pub panes: crate::panel::PaneSlots,
    /// The current tree of slots. In L1a it is always `orthodox`.
    pub layout: norte_frontend::layout::Node,
    /// The kinds this binary knows how to paint.
    pub kinds: norte_frontend::layout::KindRegistry,
    /// The roles, reconciled after every layout pass.
    pub roles: norte_frontend::layout::Roles,
    /// Who has the keyboard for the body (L3). See [`KeyOwner`].
    key_owner: KeyOwner,
    /// The menu bar, if open. An overlay: it keeps ALL the keys while it is,
    /// like the rest.
    pub menu: Option<norte_frontend::menu::MenuState>,
    /// Which menu was opened last.
    ///
    /// Reopened from there ([`norte_frontend::menu::MenuState::reopen_at`]):
    /// always starting from the first would force walking the whole bar every
    /// time, and whoever uses two entries of the same menu would pay for it
    /// on every gesture.
    pub menu_ultimo: usize,
    /// The next `SlotId` to mint. Never decreases and never reused: a
    /// recycled id would make the orphaned state of a closed slot resurrect
    /// inside another one that has nothing to do with it.
    next_slot: u32,
    /// Resolved column config (#108 block 4): a set per scheme + sort. Seeded
    /// at startup from `[ui.columns]`; the render and the cd hooks read it.
    pub columns: norte_frontend::columns::ColumnsSettings,
    /// The user's themes (`<config>/themes/*.toml`) loaded by the config,
    /// already parsed. Seeded at startup and on reload; the selector, the
    /// wizard and settings list and preview them without touching disk.
    pub user_themes: Vec<norte_frontend::theme::UserTheme>,
    /// `now` for the RELATIVE time cells (#108 L5): `None` = the real clock;
    /// snapshot tests fix `Some(ms)` for a stable render.
    pub render_now_ms: Option<i64>,
    /// Attribute catalogue per SCHEME (#117): one `fs.capabilities` call per
    /// new scheme and per session; feeds hints and render headers and the
    /// picker's rows (task 4). Private: read through [`Self::attr_catalog`],
    /// written through [`Self::insert_attr_catalog`].
    attr_catalogs: std::collections::HashMap<String, norte_proto::AttrCatalog>,
    /// Capability flags per LOCATION, the other half of the same response.
    ///
    /// `fs.capabilities` answers with `capabilities` AND `attrs` in one
    /// message, and this crate was keeping only the attrs — so the honest
    /// answer to "does this location refuse writes" was already in the
    /// process, thrown away, and asking for it again meant a second round trip
    /// over a link that had just carried it. Filled from the SAME call as
    /// [`Self::attr_catalogs`] (`main::first_page`), so caching it costs
    /// nothing.
    ///
    /// Keyed by the DIRECTORY (`caps_key`), which the attr catalogue beside it
    /// is not, and the asymmetry is the point.
    ///
    /// It was keyed by `(scheme, authority)` — one backend — and that was the
    /// right shape until ADR 0054 made the daemon answer per LOCATION (#215).
    /// Under one `file://` there are mounts: an exFAT stick that folds case,
    /// an ext4 subtree in `+F`, a read-only bind. The answer cached for
    /// `/home` was served for all of them, and the only reason nothing had
    /// broken yet is that `pane_read_only` was the sole reader and no built-in
    /// provider varies `READ_ONLY` below its scheme. [`Self::caps`] is a
    /// general accessor to every flag, and the next flag read this way must
    /// not be the one that finds out.
    ///
    /// Bounded by [`CAPS_CACHE_MAX`]: a key per backend was bounded by the
    /// seven schemes that exist; a key per directory is not.
    ///
    /// Private: read through [`Self::caps`], written through
    /// [`Self::insert_caps`].
    caps: std::collections::HashMap<CapsKey, norte_proto::Capabilities>,
    /// Arrival order of [`Self::caps`]'s keys, to evict the oldest when it
    /// fills up ([`CAPS_CACHE_MAX`]).
    caps_order: std::collections::VecDeque<CapsKey>,
    /// Index of the focused pane (invariant 0|1: private, see
    /// [`Self::focus`]).
    focus: usize,
    /// `true` when the user asked to quit.
    pub quit: bool,
    /// SYNCHRONIZED navigation (`pane.sync-nav`): every `cd` of the focused
    /// panel is repeated by the other one.
    ///
    /// Run-time state, not configuration: it is a mode you switch on while
    /// doing one thing —comparing two trees by hand— and switch off
    /// afterward, as in Krusader. Storing it in the reader's `norte.toml`
    /// would turn a gesture into a preference.
    pub sync_nav: bool,
    /// The current `[ui] confirm_quit`, so quitting from INSIDE a side panel
    /// honors the same thing as quitting from a listing. The run loop keeps
    /// its own copy for `app.quit`'s named dispatch; both are set in the
    /// same two places (startup and hot reload).
    pub confirm_quit: crate::config::ConfirmQuit,
    /// Pending key sequence, already formatted (status bar). Written ONLY by
    /// [`App::show_pending`]/[`App::clear_pending`], which keep it in
    /// agreement with [`App::which_key`].
    pub pending: String,
    /// The which-key panel, open exactly while a chord sequence is pending
    /// (K3a). `None` = closed.
    ///
    /// It takes NO keys of its own — the pane (or viewer) resolver keeps the
    /// keyboard while it is up, which is the whole point: the reader carries
    /// on typing the sequence and watches the panel narrow. It is the one
    /// overlay of this crate for which that is true, so `keyboard_owner`
    /// counts it only so a dispatch that opens or closes it is noticed, never
    /// to route a key to it.
    ///
    /// Written ONLY by [`App::show_pending`]/[`App::clear_pending`], next to
    /// [`App::pending`]: the panel and the status-bar segment describe the
    /// same resolver state, and two fields that can be updated separately are
    /// two fields that will eventually disagree — a panel left open over a
    /// keymap that was hot-reloaded under it would teach keys nobody has.
    pub which_key: Option<norte_frontend::whichkey::WhichKeyRows>,
    /// Active modal dialog (blocks the keymap until resolved).
    pub modal: Option<Modal>,
    /// Last message for the bar (an error by category, or a result).
    pub message: Option<String>,
    /// How many one-second ticks [`Self::message`] has been on the bar (spec
    /// 2026-09-10). Counted in TICKS and not with an `Instant` so a test can
    /// advance it without sleeping; `[ui] notice_seconds` is the cap.
    pub message_ticks: u32,
    /// The text that was being counted: if it changes, the count goes back
    /// to zero.
    pub message_counted: Option<String>,
    /// Notices that expired without the reader opening the log. The bar
    /// paints `!n` on the right while there is any; opening the log panel
    /// zeroes it.
    pub notices_unread: u32,
    /// Everything this process knows about the saved session (L2).
    pub session: SessionUi,
    /// Live tasks panel.
    pub board: crate::tasks::TaskBoard,
    /// The lightweight progress bar of the `tasks` item (ADR 0146): tracks
    /// the board on every tick, with the render clock.
    pub strip: norte_frontend::task_strip::TaskStrip,
    /// Transfers that get launched go INTO THE QUEUE (ADR 0149): one at a
    /// time. Session state, not configuration: it is switched on for a while
    /// of moving things on a mechanical disk and switched off afterward.
    pub encolar: bool,
    /// Open viewer (F3); None = browsing.
    pub viewer: Option<crate::viewer::Viewer>,
    /// The thumbnail requested for [`Self::viewer`], if one was requested
    /// (phase 5 WOW, T3): `[ui] images` resolved to
    /// [`crate::viewer_open::Modo::Kitty`] AND the file is an image. `None`
    /// when it does not apply or the plugin did not know how — the viewer
    /// looks the same, with no pixels (ADR 0037). Lives here and not in
    /// [`crate::viewer::Viewer`] because that type belongs to `norte-frontend`
    /// and both frontends share it; the window paints images through its own
    /// path and does not need this field.
    pub viewer_imagen: Option<crate::viewer_open::ImagenColocada>,
    /// The file whose thumbnail arrived in a format kitty does not know how
    /// to place, if that happened (`Miniatura::FormatoAjeno`).
    ///
    /// The PATH travels, not a `bool`, for the same reason
    /// [`Self::viewer_imagen`] carries its own: the reader may be looking at
    /// another file while this is still standing, and a warning about the
    /// previous file describes something no longer on screen. The warning
    /// only fires when this names the file the viewer is showing.
    pub viewer_miniatura_ajena: Option<VPath>,
    /// The [`crate::viewer_open::Modo`] [`Self::viewer`] was opened with —
    /// resolved ONCE, on open (`viewer_open::open_viewer`), not recomputed on
    /// every frame.
    ///
    /// Branch review, finding 3: `[ui] images` reloads HOT
    /// (`applies_live` in `norte_frontend::settings`, the whole `app.chrome`
    /// is reassigned in `config_reload::reload_config`), and the
    /// warning/placement used to recompute the EFFECTIVE mode against the
    /// current config on every frame, not against what was actually
    /// requested on open. Two consequences, both reachable without closing
    /// the viewer: switching from `blocks` to `kitty` made the "thumbnail
    /// extension needs approval" warning appear over a file that was NEVER
    /// asked for one (the warning lies: what is needed is reopening, not
    /// approving anything); and switching from `kitty` to `off`/`blocks` left
    /// the pixels already placed on screen INDEFINITELY, violating what help
    /// promises about `off` ("leaves the viewer in hexview, nothing more").
    /// This field fixes the mode resolved on open for the warning and the
    /// placement; only `config_reload::reload_config` changes it afterward,
    /// and only in the Kitty→something-else direction (releasing
    /// [`Self::viewer_imagen`] at the same time) — the opposite direction is
    /// deliberately pinned, so as not to lie again about a file the new mode
    /// never asked anything of.
    pub viewer_modo: crate::viewer_open::Modo,
    /// Help overlay open (F1, H3b): the navigable view over the `norte-help`
    /// corpus — sidebar, body, filter and history — plus the generated
    /// keyboard page, which is still built from the EFFECTIVE keymap (preset
    /// and the user's layers included, never a hand-kept list).
    pub help: Option<HelpView>,
    /// Collisions waiting for a dialog: an open modal is NEVER overwritten
    /// (a key in flight would answer the wrong question); they are handled in
    /// order as the current modal closes.
    pub pending_collisions: std::collections::VecDeque<crate::tasks::RetrySpec>,
    /// Reports waiting for a dialog (from a rename batch or an undo), with
    /// the same discipline: whatever finishes while something else is being
    /// answered does not take over the screen, but is not lost either. Each
    /// one with its class.
    pub pending_reports:
        std::collections::VecDeque<(modal::ReportKind, Vec<norte_frontend::ReportLine>)>,
    /// Policy approvals waiting for a dialog (M3-3b T5): same discipline as
    /// the collisions (never overwrite an open modal), but with PRIORITY over
    /// them — an approval has a TTL in the daemon and a collision waits as
    /// long as it takes.
    pub pending_approvals: std::collections::VecDeque<norte_proto::methods::PolicyApprovalRequired>,
    /// Resolved theme + color depth (ADR 0020). The render reads from here;
    /// hot reload replaces it. Default = the `default` preset.
    pub theme: crate::theme::TuiTheme,
    /// Open theme selector (popup): None = closed.
    pub theme_picker: Option<ThemePicker>,
    /// Open layout selector (F9 → `layout.pick`): None = closed. The model
    /// lives in norte-frontend (rule 7); only stored here.
    pub layout_picker: Option<norte_frontend::layout_picker::LayoutPicker>,
    /// Open PROFILE selector (`profile.pick`): None = closed. Same pattern
    /// as the layout one, and for the same reason: the model lives in
    /// norte-frontend (rule 7) and is only stored here.
    pub profile_picker: Option<norte_frontend::profile_picker::ProfilePicker>,
    /// Whether the menu bar is PINNED to the top row (`[ui] menu_bar`).
    ///
    /// Pinned takes a row away from the body, and that subtraction happens in
    /// the frame's layout pass —the one place the paint, the click mapping
    /// and the loop's "which slot got placed" decisions all go through— so
    /// the three things line up on their own.
    ///
    /// Unpinned, the menu still opens with its key and paints OVER the first
    /// row, as always.
    pub menu_bar: bool,
    /// The command a click on the panel bar left requested (#324).
    ///
    /// Dispatched through the SAME path as its shortcut, not through one of
    /// its own: two paths to open the same panel diverge the moment one of
    /// the two grows a detail.
    pub pending_panel_command: Option<String>,
    /// The disk map's child to enter, if someone asked for it.
    ///
    /// Set by the key or the click and consumed by the LOOP, which is the one
    /// with the backend: entering a directory is an ordinary `cd`, with its
    /// filling and its refresh. A second navigation path is exactly what ADR
    /// 0077 exists to prevent.
    ///
    /// A [`norte_proto::Segment`] and not a path: the map names CHILDREN of
    /// the directory it shows, and whoever consumes them resolves them
    /// against it. A path here would be a second way of naming a file,
    /// skipping the one that already goes through the gate.
    pub pending_disk_map_enter: Option<norte_proto::Segment>,
    /// The disk map has to be measured again.
    ///
    /// Switched on by `r` inside the panel, by opening it, and by the watch's
    /// notice —which does not say WHAT changed, so the only honest thing is
    /// to measure again. Drained by the loop.
    pub disk_map_stale: bool,
    /// The timeline has to be (re)read (phase 7).
    ///
    /// Set by whoever OPENS it and by whoever inherits it from a saved
    /// layout: an adopted panel does not go through the toggle that would
    /// have filled it, and without this it kept saying "nothing has happened
    /// yet" about a journal it had not even looked at — which is the worst
    /// possible sentence on a history screen.
    pub timeline_stale: bool,
    /// The panel bar is pinned (`[ui] panel_bar`, #324).
    ///
    /// Side panels used to open by shortcut, by the menu or by the palette,
    /// and all three require KNOWING the panel exists: there was no surface
    /// that showed them. A permanent row costs one cell of height, so it is a
    /// choice — but by default it is on, because whoever does not know the
    /// panel exists also does not know the option to show it exists.
    pub panel_bar: bool,
    /// The configurable chrome (spec 2026-09-10): key bar, panel bar style,
    /// panel footer, date format, notice expiry and dialog buttons. Read
    /// every frame; `reload_config` copies it back in. A test `App` starts
    /// with the key bar and the footer OFF for the same reason as the panel
    /// bar: a row that appears on its own would shift the indices of eighty
    /// tests that are not about this.
    pub chrome: norte_config::UiChrome,
    /// `[ui] status_plugins` (ADR 0137): the plugin columns the status bar
    /// shows for the entry under the cursor. Empty in a test `App`, like the
    /// rest of the chrome.
    pub status_plugins: Vec<(String, String)>,
    /// The last frame's area (ADR 0138): moving or rotating a panel resolves
    /// the new tree against it before keeping it, so as not to hide a
    /// listing. `None` before the first one: not knowing is not the same as
    /// knowing there is none.
    pub ultimo_frame: Option<ratatui::layout::Rect>,
    /// The host's volumes, cached for each panel's footer (spec 2026-09-10).
    /// Requested by the loop when [`Self::volumes_stale`] says so —on landing
    /// a listing and on refresh— never on a frame: `host.volumes` mounts and
    /// queries space on every filesystem.
    pub volumes: Vec<norte_proto::methods::Volume>,
    /// [`Self::volumes`] has to be requested again.
    pub volumes_stale: bool,
    /// The `..` row is switched on (`[ui] parent_entry`).
    ///
    /// Stored here as well as in each pane because a NEW pane —a tab, a slot
    /// from a layout— has to be born with the same answer as the others.
    pub parent_row: bool,
    /// The active profile, or `None` if there is none.
    ///
    /// An in-memory mirror of `SessionBody.active`. Stored as an `OsString`
    /// —not the body's `String`— because that is what gets passed to the
    /// layer resolver, and there it is a directory name (D4).
    pub active_profile: Option<std::ffi::OsString>,
    /// A requested profile switch, not yet done.
    ///
    /// Set by `dispatch` and drained by the run loop, like the rest of what a
    /// command requests and cannot execute itself: switching profile reloads
    /// layers and re-lists panes, which is I/O, and `dispatch` already
    /// returns a [`crate::navigate::Cd`] — adding a second output channel to
    /// its signature would touch every caller to serve three arms.
    pub pending_profile: Option<std::ffi::OsString>,
    /// The sidebar needs the drives requested again.
    ///
    /// Same pattern as [`Self::pending_profile`] and for the same reason:
    /// `host.volumes` is I/O and `App` has no backend. Switched on by
    /// EVERYTHING that makes the section appear —opening the sidebar,
    /// unfolding it, mounting a layout that already carries it— and drained
    /// by the run loop once per turn.
    ///
    /// Before, every site requested the volumes on its own, and that is why
    /// they were missing exactly where nobody remembered: starting with
    /// `full`, switching profile, opening the sidebar from inside another
    /// panel. One flag and one drain is one place to get it wrong instead of
    /// five.
    pub places_wants_drives: bool,
    /// The connections selector (#140), if open.
    pub connections_picker: Option<norte_frontend::connections_picker::ConnectionsPicker>,
    /// The terminal panel's shell (#362), if one is alive.
    ///
    /// Lives HERE and not in the slot because the kind is `multi: false`:
    /// there is one, and it survives the panel being hidden and reopened.
    /// What kills it is closing the slot (`layout.close-slot`) or quitting
    /// norte.
    pub terminal: Option<crate::termpanel::TermPanel>,
    /// The log panel's state: which level is shown and what is filtered.
    pub log_panel: norte_frontend::logpanel::LogPanel,
    /// The log's text filter WHILE it is being typed.
    ///
    /// Separate from the already-applied filter (`log_panel.filter()`)
    /// because they are two things: what is being typed and what is
    /// filtering. Without the split, every letter would re-filter the list
    /// and the reader would see the screen jump under the cursor while
    /// typing.
    pub log_filter_input: Option<String>,
    /// The ring that panel reads from.
    ///
    /// `Option` because the subscriber is installed by `main`, and the tests
    /// build `App` without it: a panel with no ring paints empty saying there
    /// is no log installed, which is the truth, and does not crash.
    pub log_ring: Option<norte_config::logring::LogRing>,
    /// The REMOTE half of that panel: what the daemon has counted (#328).
    ///
    /// With `--socket` the ring above only has this terminal's own lines, and
    /// the providers, the journal, the policy and the reason a connection
    /// failed are in the other process. The in-flight request does not live
    /// here but in `InFlight`, which is the one that talks to the backend.
    pub log_remote: crate::logview::RegistroRemoto,
    /// What the reader is waiting on right now, if anything (#323).
    ///
    /// Set and cleared by whoever is waiting, and lasts only as long as the
    /// wait: a `Busy` that survives its own work is exactly the spinner that
    /// never advances. Not painted until it crosses
    /// [`norte_frontend::busy`]'s threshold, so a local navigation —the vast
    /// majority— never gets to show anything.
    pub busy: Option<norte_frontend::busy::Busy>,
    /// Columns picker overlay (#108 7a): same pattern as `theme_picker` — an
    /// Option on App, NOT a Modal variant (Modal is confirmation; this is a
    /// list with a cursor). The model lives in norte-frontend
    /// (`ColumnsPicker`, rule 7).
    pub columns_picker: Option<norte_frontend::columns_picker::ColumnsPicker>,
    /// Open extension manager (catalogue overlay, M4-P3): None = closed.
    pub extensions: Option<ExtensionManager>,
    /// Pending Lua TOFU (M4, [`Modal::TrustLuaInit`]): the project
    /// `init.lua`'s CANONICAL path + the BYTES read exactly once. Resolving
    /// the modal records the decision and, if approved, evaluates THESE
    /// bytes — disk is never re-read between the check and the eval
    /// (anti-TOCTOU).
    pub lua_pending_trust: Option<(std::path::PathBuf, Vec<u8>)>,
    /// Output of the active `init.lua`'s `norte.ui.statusbar` hook (M4 Lua),
    /// ALREADY sanitized by the host (`detail_for_bar`). `Some` replaces the
    /// focused pane's default bar line; `None` = the normal bar.
    pub lua_status: Option<String>,
    /// #44: remote sessions degraded to plaintext, BY SCHEME.
    ///
    /// Was a single pre-formatted `Option<String>`: `main` formatted the scheme
    /// and the host into a sentence and dropped the structured
    /// `ConnectionDegraded`, so "which connection degraded" had no answer, a
    /// second degradation silently overwrote the first, and the help had no
    /// fact to read. The value is kept whole and the banner
    /// ([`Self::connection_banner`]) is built from it on demand.
    ///
    /// One entry per scheme, so `sftp` and `ftp` coexist. Two HOSTS on one
    /// scheme still collapse into one entry, and the banner does not claim
    /// otherwise.
    ///
    /// A `VecDeque` in arrival order rather than a map, for two reasons that
    /// the map could not give: it is CAPPED at `DEGRADED_MAX` (a map keyed on
    /// a wire-supplied string grows as far as the sender wants), and the newest
    /// report is `back()`, which is the one the banner names when there are
    /// several. Lookup is a scan of at most 32 short strings, on the path that
    /// assembles the help's facts — not a per-frame one.
    ///
    /// NEVER CLEARED, deliberately. A degradation is not known to be resolved
    /// without a successful reconnect that reports the session encrypted, and
    /// the wire has no such notification: `connection.degraded` is only ever
    /// sent, never withdrawn. Any clearing rule this side could invent — a
    /// timeout, the next successful listing, leaving the pane — would say "the
    /// session is encrypted again" on evidence that does not support it, which
    /// is the one wrong answer for a security indicator. Follow-up work is a
    /// wire notification for the recovered case, not a heuristic here.
    ///
    /// Private: read through [`Self::degraded_for`] /
    /// [`Self::connection_banner`], written through [`Self::note_degraded`].
    degraded: norte_frontend::banners::DegradedSet,
    /// #177: this session is NOT recording its mutations to the journal.
    ///
    /// The embedded arm opens the state directory's journal on its first
    /// mutation, and if another process already has it (a live daemon,
    /// another session that already mutated) this one carries on WITHOUT
    /// recording: nothing that gets copied, moved or deleted from then on
    /// can be undone or audited.
    ///
    /// Persistent, for the same reason as `degraded`: it arrives ONCE, in the
    /// middle of an operation the user just launched from the keyboard, and
    /// `app.message` is cleared by the next key — there are 136 sites that
    /// write that field. A warning that lasts until the next `↓` is not a
    /// security indicator.
    ///
    /// Never cleared: this session's decision is made once and not revisited
    /// (see `norte_core::embedded`), so the sentence stays true for as long
    /// as the session lives. If reopening is ever retried, this needs the
    /// recovery event BEFORE the retry.
    ///
    /// The STRUCTURED value is kept rather than a `bool`, by the same
    /// criterion as `degraded` (H3d): the sentence is composed when painted,
    /// and the reason stays available to whoever needs it (a help page, a
    /// future detail in the bar).
    ///
    /// Private: read through [`Self::journal_banner`] and written through
    /// [`Self::note_no_journal`].
    no_journal: Option<JournalIndicator>,
    /// Per-pane directory history (spec 2026-07-18, `Alt+↓`): same index as
    /// `panes`. Lives on `App` and not on `Pane` (the history is not render
    /// state): every SUCCESSFUL cd pushes the previous dir (main.rs).
    pub history: crate::panel::Histories,
    /// Copy of `LoadedConfig`'s hotlist (cloned at startup and on every OK
    /// hot reload): the source for the `Ctrl+D` popup. Adds/removes only
    /// touch it after a successful persist (consistency with disk).
    pub hotlist: Vec<crate::config::HotlistItem>,
    /// Open navigation popup (history/hotlist): None = closed.
    pub nav_popup: Option<NavPopup>,
    /// "Go to anywhere" open (WOW program phase 6): `None` = closed. The
    /// model belongs to `norte-frontend`; where its rows come from and what
    /// confirming them means comes from [`crate::goto`].
    pub goto: Option<norte_frontend::goto::Goto>,
    /// Open live-search dialog (`Alt+F7`, liveSearch T6): None = closed.
    /// Captures printables like the nav popup's `name_input`.
    pub search_dialog: Option<SearchDialog>,
    /// Open compare panel (`Shift+F2`,
    /// 2026-08-11-directory-comparison.md): `None` = closed.
    ///
    /// An overlay (`Option` on `App`) and NOT a pane mode, unlike live
    /// search: a compare row has TWO sides and a verdict between them, so it
    /// does not fit in a pane's column nor is it an `Entry` `extend_listing`
    /// can swallow. It takes the place of both panes while open, which is
    /// what a diff is.
    pub compare: Option<CompareView>,
    /// Sizes hydrated on demand for the compare panel, by `VPath` (#157).
    ///
    /// Only the SELECTED row is probed, never a window: unlike the normal
    /// pane (row radius, #52), `list_offset` already puts the selected row
    /// inside the painted area as soon as the compare panel is what is being
    /// painted, so "on screen" is almost a tautology here — probing only that
    /// row covers exactly the case the issue points at: an orphan
    /// `OnlyLeft`/`OnlyRight` with no size is the row that needs it most,
    /// because it is the one that decides whether it gets copied.
    ///
    /// Lives in the TUI and not in `ComparePane` (`norte-frontend`) ON
    /// PURPOSE: it is a PRESENTATION cache, never travels over the wire and
    /// no other frontend needs it, and `ComparePane` today has no way to
    /// mutate a row that already arrived — its rows never change after
    /// `extend` (see its rustdoc: "Rows only ever grow"). Storing it here and
    /// painting it as an overlay in `ui::draw_compare` avoids needing that
    /// path.
    pub compare_size_hints: std::collections::HashMap<VPath, u64>,
    /// Paths ALREADY probed for [`Self::compare_size_hints`], hit or miss,
    /// so as not to retry a stat that failed on every frame — same criterion
    /// as `last_probed` for the normal pane. Cleared when `launch_compare`
    /// opens a new comparison, never during one: the rows of a comparison in
    /// progress do not change under your feet (see [`Self::compare_size_hints`]'s
    /// note).
    pub compare_size_probed: std::collections::HashSet<VPath>,
    /// Which comparison those two belong to
    /// ([`Self::begin_compare_generation`], #198). The probe lives in the run
    /// loop and `launch_compare` does not receive it, so a result in flight
    /// when another comparison starts would land in the NEXT one's
    /// just-cleared tables. What prevents that is the result carrying the
    /// generation it was requested with.
    compare_generation: u64,
    /// What has to be asked of the destination and the run loop has not
    /// asked yet (#149, #164): `open_transfer` knows WHAT is going to move,
    /// and asking about volumes and capabilities is I/O, which belongs to the
    /// run loop. Same split as `pending_compare`.
    pub pending_dest_check: Option<DestCheck>,
    /// `fs.compare` params dispatch resolved and the run loop has not
    /// launched yet (`Shift+F2`). Same split as [`Self::pending_open`] and
    /// [`Self::pending_shell`]: `dispatch` decides WHAT, the run loop —owner
    /// of the channel and the Task— does it.
    pub pending_compare: Option<norte_proto::methods::FsCompareParams>,
    /// Checksum batch dispatch resolved and the run loop has not launched
    /// yet (#311). Same split as [`Self::pending_compare`]: reading the
    /// checksum file and waiting for the report is I/O, and that belongs to
    /// the run loop.
    pub pending_checksum: Option<ChecksumRequest>,
    /// `true` when dispatch asked the model for an ORGANIZE plan (phase 8)
    /// and the run loop has not launched it yet. Same split as
    /// [`Self::pending_checksum`]: dispatch decides WHAT, the run loop —owner
    /// of the in-flight requests— asks for it.
    ///
    /// A boolean and not some params because there is nothing to choose: the
    /// operand is the focused directory, whole. The PLUGIN path does not go
    /// through here — the palette already dispatches with `work` in hand.
    pub pending_organize: bool,
    /// Open sync panel (`Ctrl+Y`, or `s`/`m` inside the compare panel):
    /// `None` = closed. Painted OVER the compare one, which stays alive
    /// behind it with its marks.
    pub sync: Option<SyncView>,
    /// Resolved `sync.plan` params, not launched yet. Same split as
    /// [`Self::pending_compare`].
    pub pending_sync: Option<norte_proto::methods::SyncPlanParams>,
    /// The `plan_hash` the reader approved and the run loop has not applied
    /// yet.
    ///
    /// It is the ONLY thing that travels: `sync.apply` carries no paths or
    /// mode, so there is no way to run something other than what was shown
    /// (ADR 0049). A `Box` because it is by far the largest of the
    /// `pending_*` fields and clippy measures the whole `App`.
    pub pending_sync_apply: Option<Box<norte_proto::methods::PlanHash>>,
    /// Where to take the panel that just disconnected (#140).
    ///
    /// The PATH and not a flag, for two reasons: the loop does not have to
    /// guess where —whoever disconnected decides it— and `App` does not
    /// grow its `bool` count, which is a lint in this repo and a sign that
    /// the state was turning into a bag of little flags.
    pub pending_disconnect_dest: Option<VPath>,
    /// The file `pane.edit-new` had created and the editor will open WHEN it
    /// exists (#290), with the id of the task creating it.
    ///
    /// Opening it as soon as it is queued would be opening something not yet
    /// on disk —and that may never get there: if policy denies the creation,
    /// the editor would create it itself, which is exactly what this key
    /// stopped doing. The id is what tells THIS creation apart from any other
    /// task that finishes in the meantime.
    pub pending_edit_open: Option<(norte_proto::TaskId, VPath)>,
    /// Name reinterpretation (#57) of the SOURCE side, frozen together with
    /// [`Self::pending_sync`] and not when the run loop opens the panel:
    /// between one thing and the other the reader may have pressed `Alt+E`,
    /// and a plan painted with a different reinterpretation than the one
    /// requested would show `????.txt` where the source had a CP1251 name.
    pub pending_sync_encoding: (
        Option<norte_encoding::NameEncoding>,
        Option<norte_encoding::NameEncoding>,
    ),
    /// This backend records its mutations to a journal and can therefore
    /// sync (`--daemon`).
    ///
    /// Fixed at startup, once, because the `Backend`'s arm does not change
    /// for the life of the process. It feeds
    /// [`norte_frontend::availability::Facts::journalled`]: without it the
    /// reference sheet would offer `Ctrl+Y` and the core would reject it in
    /// closed — a documented dead key, which is what #159 just cost once.
    pub backend_journalled: bool,
    /// This process talks to the DAEMON (phase 9). Same treatment and same
    /// reason as [`Self::backend_journalled`]: fixed at startup because the
    /// backend's arm does not change for the life of the process.
    ///
    /// It is a different question from `backend_journalled`, though today
    /// they almost coincide: that one says whether mutations get recorded,
    /// this one whether there is a daemon to SHARE the session with, which
    /// is what a handoff needs.
    pub backend_daemon: bool,
    /// There is a desktop to open a window on (phase 9). `false` over SSH.
    ///
    /// Checked once at startup: a desktop does not appear mid-session, and
    /// querying it on every draw would be asking the environment about
    /// something that does not move.
    pub has_desktop: bool,
    /// The HANDOFF is done: the screen is written and the session, released
    /// (phase 9). The loop launches the window and leaves.
    ///
    /// A flag and not a direct action because who finds out is the session
    /// writer's notice drain, and launching a process and quitting is not its
    /// business: the same split as `pending_open` or `pending_shell` — one
    /// decides WHAT, the loop does it.
    pub handoff_ready: bool,
    /// Dispatch requested a HANDOFF and the run loop has not launched it yet
    /// (phase 9). Same split as [`Self::pending_organize`]: dispatch decides
    /// WHAT, the loop —owner of the session writer— asks for it.
    pub pending_handoff: bool,
    /// The listings have to FORGET what the plugins said and ask again:
    /// raised by any governance or settings change of an extension (turning
    /// off the icon decorator left the icons up until the next `cd`), and
    /// drained by the event loop, which is the one with the in-flight
    /// batches. A flag and not a call because the manager cannot see the
    /// loop, same as the sidebar with whatever it needs the backend for.
    pub redecorate: bool,
    /// Merged declarative openers (#28): cloned at startup and on every OK
    /// hot reload. Source for `pane.open` (F4). Empty = no openers.
    pub openers: norte_frontend::openers::OpenersConfig,
    /// The `[ui] editor` editor, if the configuration names one.
    ///
    /// `None` = the usual one: `$VISUAL`, `$EDITOR`, and the POSIX fallback.
    /// Copied here at startup and on every reload, same as [`Self::openers`]:
    /// a gesture does not re-read configuration from disk.
    pub editor: Option<EditorSpec>,
    /// The `[ui] diff` comparator (#312), if there is one.
    ///
    /// `None` = `diff -u`, which POSIX guarantees. Same shape as
    /// [`Self::editor`] —a template argv and whether it opens a window—
    /// because it is the same deal: norte chooses the operand, the program
    /// chooses the format.
    pub diff: Option<EditorSpec>,
    /// External command resolved by `pane.open` and pending launch (#28).
    /// `dispatch` sets it after validating; the run loop —owner of the
    /// terminal— executes it.
    pub pending_open: Option<PendingOpen>,
    /// Suspension resolved by dispatch and pending execution (#135):
    /// `app.terminal`, `app.toggle-panels` and the Enter of
    /// [`Modal::CommandLine`]. Same split as [`Self::pending_open`], and
    /// drained in ONE single place in the run loop (at the very top of the
    /// turn, before the draw) so that none of the `continue`s that answer
    /// keys can leave it stuck.
    pub pending_shell: Option<PendingShell>,
    /// `app.toggle-panels` requested the SUBSHELL (#142).
    ///
    /// Same split as [`Self::pending_shell`] and for the same reason: the
    /// owner of the terminal —and of the long-lived shell— is the run loop,
    /// not dispatch. Kept apart because it is not a suspension: nothing gets
    /// launched, the terminal is handed to a process that ALREADY exists and
    /// stays alive on return.
    pub pending_subshell: bool,
    /// The chord that RESTORES the panels from the subshell, PRECOMPUTED
    /// from the effective keymap — same criterion as [`Self::dialog_hints`]
    /// and `palette_rows`, and for the same reason: the effective map moves
    /// into the shared `Resolver`, so whatever is derived from it is taken
    /// out beforehand.
    ///
    /// `None` = the preset does not bind `app.toggle-panels` to a bare
    /// chord, and then the terminal is not handed over: see
    /// [`norte_frontend::subshell::detach_chord`].
    pub subshell_chord: Option<norte_frontend::keymap::Chord>,
    /// The chord that TAKES the keyboard out of the terminal panel (#362),
    /// precomputed the same way as [`Self::subshell_chord`] and for the same
    /// reason.
    ///
    /// It is the same `layout.terminal` that opened it, and it is the ONLY
    /// one the panel does not pass to the shell. `None` = the preset does not
    /// bind it to a bare chord, and then the panel does not take the keys at
    /// all: a panel you cannot leave is worse than one you can only look at.
    pub terminal_chord: Option<norte_frontend::keymap::Chord>,
    /// Bytes that have to be written to the terminal EMULATOR, if any.
    ///
    /// Same split as [`Self::pending_shell`]: `dispatch` decides WHAT and
    /// the loop —owner of the output— writes it. Today only OSC 52 uses it,
    /// which is the only way to copy to the clipboard over SSH: whoever
    /// receives the sequence is the terminal the human is looking at, not the
    /// machine norte runs on (#286).
    pub pending_osc52: Option<Vec<u8>>,
    /// Footer hints for the dialog overlays (H1 T3, #24), PRECOMPUTED from
    /// the current effective `dialog` map — same as `help_lines` in
    /// `main.rs`, rebuilt at startup and on every OK hot reload
    /// (`main::build_keymaps` + `DialogHints::build`), BEFORE the effective
    /// map moves into the shared `Resolver`. `ui::draw_*` reads them instead
    /// of a static Fluent key.
    pub dialog_hints: crate::hints::DialogHints,
    /// The per-screen key bar cells (spec 2026-09-10), PRECOMPUTED from the
    /// three effective maps like `dialog_hints`: at startup and on every OK
    /// hot reload, before they move into the `Resolver`. Every frame picks
    /// which one to paint based on which screen has the keys.
    pub key_bars: KeyBars,
    /// The chord the CURRENT preset binds to `layout.split-h`, precomputed
    /// like the bars and for the same reason: closing a panel has to say
    /// what it is re-split with, and the resolver does not reach that far.
    /// `None` = the preset does not bind it and the menu is named instead.
    pub chord_split_h: Option<String>,
    /// A key the mouse asked to synthesize: a click on the key bar IS
    /// pressing the key, and the loop dispatches it through `on_key`, which
    /// is the only path with all three resolvers at hand.
    pub pending_key: Option<crossterm::event::KeyEvent>,
    /// Binary version and revision (`norte_frontend::version::VERSION_LINE`),
    /// painted in the help frame. Empty = not painted: what the tests
    /// receive, whose snapshots cannot depend on the commit.
    pub version_line: &'static str,
    /// Resolver of the help's live marks (H3b): rebuilt with the effective
    /// keymaps on every hot reload, exactly like `dialog_hints` and
    /// `help_lines` — a rebind must change the prose, and it does because the
    /// page is drawn through this.
    pub help_chords: std::sync::Arc<crate::help::TuiChords>,
    /// Open command palette (`Ctrl+P`/vim `:`, H1 T4): `None` = closed.
    pub palette: Option<Palette>,
    /// The last commands launched from the palette, most recent first (spec
    /// 2026-09-10). Live in the UI session: read on restore and written back
    /// with it.
    pub palette_recent: Vec<String>,
    /// The directories visited most, for the whole session (spec 2026-09-15
    /// D6). Like `palette_recent`: read on session restore and written back
    /// with it.
    pub popular: norte_frontend::history::Popular,
    /// Palette rows PRECOMPUTED from the current keymap
    /// ([`crate::palette::build_rows`]) — same criterion as `help_lines`/
    /// `dialog_hints`: rebuilt at startup and on every OK hot reload, BEFORE
    /// the effective maps move into the `Resolver`. Opening the palette
    /// (`dispatch`, `app.palette` arm) only clones this snapshot.
    pub palette_rows: Vec<crate::palette::Row>,
    /// The first-run wizard (spec 2026-09-10), while open. Just another
    /// overlay: it keeps the keys, and the model is the one shared with the
    /// window.
    pub wizard: Option<norte_frontend::wizard::Wizard>,
    /// The splash screen (spec 2026-09-15, phase 2), while up. It is a
    /// LAYER, not an overlay with its own keys: any key removes it, and the
    /// wizard wins over it —if both wanted to leave, the one asking something
    /// leaves—.
    pub splash: Option<norte_frontend::splash::SplashView>,
    // (the deadline constant lives outside the struct: see `SPLASH_BRIEF_MS`)
    /// When the `brief` splash stops covering, in the render clock
    /// ([`App::SPLASH_BRIEF_MS`] since it was set). `None` = it does not
    /// expire on its own (`home`), or there is no splash.
    pub splash_until_ms: Option<i64>,
    /// The processes panel was opened by the AUTOMATIC setting (`[ui]
    /// processes_panel = "auto"`), so the automatic setting can close it. A
    /// panel the reader opened does not close on its own: they opened it to
    /// look at it.
    pub processes_auto: bool,
    /// Which plugin panel has the keyboard, when [`KeyOwner::Panel`] says so
    /// (phase 3).
    ///
    /// `KeyOwner::Panel` does not carry the slot inside —carrying it would
    /// break the 86 `==` comparisons against the other owners— and nobody
    /// enforces `multi: false`: two plugins can each contribute a panel and a
    /// saved layout can place both. Without this field, "the panel" was the
    /// FIRST one visible, so `layout.grow` would enlarge one and the focus
    /// border would paint on another.
    ///
    /// `None` = the first one visible, which is correct when the keyboard
    /// arrived through the ring and not by pointing at a specific slot.
    pub panel_focus: Option<norte_frontend::layout::SlotId>,
    /// What each plugin panel has alive: its frame, its opaque state and what
    /// it requested (phase 3).
    ///
    /// Per SLOT and not a single field, even though today there can only be
    /// one plugin panel visible: the guest's state belongs to its slot, and
    /// with tabs there are more live slots than visible ones — same as the
    /// histories.
    pub paneles: norte_frontend::layout::BySlot<crate::panelplugin::PanelRuntime>,
    /// The splash row the reader just picked with its number, until the loop
    /// dispatches it. Like the rest of the pending intents: the key decides,
    /// and whoever has the backend in front executes.
    pub pending_splash_row: Option<(String, Option<String>)>,
    /// Mouse state (capture is separate, belonging to the terminal): the
    /// last frame's PAINTED geometry, the armed gesture and the last click.
    /// The geometry is returned by the run loop after every `draw` (#124):
    /// without it no click resolves.
    pub mouse: crate::mouse::MouseState,
    /// Open settings overlay (`app.settings`, S3): `None` = closed. Its rows
    /// are rebuilt from the CURRENT `cfg` on every OK hot reload
    /// (`main::reload_config`, `Settings::refresh`) — unlike `palette`/`help`,
    /// which get CLOSED, this overlay stays open and refreshes in place (see
    /// `Settings::refresh`'s doc).
    pub settings: Option<Settings>,
    /// Shortcut editor open (K3c, `Ctrl+K` from the settings overlay):
    /// `None` = closed.
    ///
    /// Sits IN FRONT of `settings`, which stays open behind it — the editor is
    /// a screen of Settings, not a replacement for it, and closing it returns
    /// the reader where they were.
    ///
    /// Like `settings` it is REFRESHED and not closed on a hot reload
    /// (`main::reload_config`), because that reload is usually its own write
    /// coming back through the watcher: an editor that closed on the write it
    /// just made would be unusable for a second rebind. Its capture, unlike a
    /// settings edit buffer, does NOT survive the refresh — see
    /// `ShortcutsState::refresh`, whose verdict belongs to the map that was
    /// just replaced.
    pub shortcuts: Option<Shortcuts>,
    /// How many times the two panes have been exchanged (`pane.swap`).
    ///
    /// It exists because nothing else in the model records that a swap
    /// happened: everything indexed by pane TRAVELS with the pane, so a swap
    /// merely exchanges two values and anything comparing them per side sees
    /// nothing move. Read through [`Self::swap_seq`] by the mouse, whose
    /// armed gesture carries pane indices that the swap has just
    /// re-attributed to the other side's content.
    ///
    /// Only ever compared for EQUALITY, so it wraps rather than saturating —
    /// saturating would eventually stop changing, which is the one thing it
    /// must never do.
    swap_seq: u64,
    /// `--pick` (S2): true for the lifetime of the process once the flag was
    /// passed. Read by the run loop's Enter/Ctrl+Enter override (design §B)
    /// and by `Command::AppPickAccept`'s dispatch arm, which is a no-op
    /// without it — the command exists in the catalogue unconditionally
    /// (help, palette, rebind checks), but only ever FIRES under `--pick`.
    /// Set once in `main`, right after construction; never toggled at
    /// runtime.
    pub pick: bool,
    /// The picker's answer, written by `Command::AppPickAccept` and read by
    /// `main` after the run loop returns (`app.quit` is set alongside it, so
    /// this is always read exactly once). `None` after a normal quit means
    /// the pick was CANCELLED, not that nothing happened — `main` tells the
    /// two apart with `Self::pick`, per the exit-code table in the design
    /// (0 accepted, 1 cancelled, 2 error).
    pub picked: Option<Vec<VPath>>,
}

// `PendingWrite`/`SettingsEditError`/`Settings`/`cycle` (S3 overlay editor)
// hoisted to `norte_frontend::settings` in S4 (GUI settings view): the code
// had ZERO TUI-specific coupling (no ratatui/crossterm, pure state +
// `norte_frontend::nav::fold`) — re-exported here under their historical
// names so the rest of this crate (and integration tests referencing
// `norte_tui::app::{Settings, PendingWrite, SettingsEditError}`) keep
// resolving unchanged. See `norte_frontend::settings` module doc.
pub use norte_frontend::settings::{PendingWrite, SettingsEditError, SettingsState as Settings};

// K3c: the shortcut editor's state machine is shared with the GUI for the same
// reason as the settings one — it is pure (rows, a filter, a cursor and a
// capture with its verdict), and two frontends deciding separately what a
// collision is would be two answers to one question.
pub use norte_frontend::shortcuts::ShortcutsState as Shortcuts;

impl App {
    /// An App with focus on the left pane.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "one field per line: the App's defaults"
    )]
    pub fn new(left: Pane, right: Pane) -> Self {
        Self {
            panes: crate::panel::PaneSlots::new(left, right),
            layout: crate::panel::orthodox(),
            kinds: norte_frontend::layout::KindRegistry::builtin(),
            roles: norte_frontend::layout::Roles::con_active(crate::panel::SLOT_LEFT),
            key_owner: KeyOwner::Panes,
            sync_nav: false,
            // Lazy, like the subshell: a shell per session that nobody is
            // going to use is a process, a pty and someone's `.bashrc`
            // running just in case.
            terminal: None,
            log_panel: norte_frontend::logpanel::LogPanel::default(),
            log_filter_input: None,
            log_ring: None,
            log_remote: crate::logview::RegistroRemoto::default(),
            busy: None,
            menu: None,
            menu_ultimo: 0,
            // Los cuatro primeros son los del preset `orthodox`.
            next_slot: 5,
            render_now_ms: None,
            attr_catalogs: std::collections::HashMap::new(),
            caps: std::collections::HashMap::new(),
            caps_order: std::collections::VecDeque::new(),
            columns: norte_frontend::columns::ColumnsSettings::default(),
            user_themes: Vec::new(),
            focus: 0,
            quit: false,
            confirm_quit: crate::config::ConfirmQuit::default(),
            pending: String::new(),
            which_key: None,
            modal: None,
            message: None,
            message_ticks: 0,
            message_counted: None,
            notices_unread: 0,
            session: SessionUi::default(),
            board: crate::tasks::TaskBoard::default(),
            strip: norte_frontend::task_strip::TaskStrip::default(),
            encolar: false,
            viewer: None,
            viewer_imagen: None,
            viewer_miniatura_ajena: None,
            viewer_modo: crate::viewer_open::Modo::Nada,
            help: None,
            pending_collisions: std::collections::VecDeque::new(),
            pending_reports: std::collections::VecDeque::new(),
            pending_approvals: std::collections::VecDeque::new(),
            theme: crate::theme::TuiTheme::default(),
            theme_picker: None,
            layout_picker: None,
            profile_picker: None,
            menu_bar: true,
            panel_bar: true,
            chrome: norte_config::UiChrome {
                key_bar: Some(false),
                pane_footer: Some(false),
                ..Default::default()
            },
            status_plugins: Vec::new(),
            ultimo_frame: None,
            volumes: Vec::new(),
            volumes_stale: true,
            pending_panel_command: None,
            pending_disk_map_enter: None,
            disk_map_stale: false,
            timeline_stale: false,
            // Off until startup says otherwise: a test `App` does not read
            // configuration, and a row that appears on its own would shift
            // the indices of eighty tests that are not about this.
            parent_row: false,
            active_profile: None,
            pending_profile: None,
            places_wants_drives: false,
            connections_picker: None,
            columns_picker: None,
            extensions: None,
            lua_pending_trust: None,
            lua_status: None,
            degraded: norte_frontend::banners::DegradedSet::default(),
            no_journal: None,
            history: crate::panel::Histories::new(),
            hotlist: Vec::new(),
            nav_popup: None,
            goto: None,
            search_dialog: None,
            compare: None,
            compare_size_hints: std::collections::HashMap::new(),
            compare_size_probed: std::collections::HashSet::new(),
            compare_generation: 0,
            pending_compare: None,
            pending_checksum: None,
            pending_organize: false,
            pending_dest_check: None,
            sync: None,
            pending_sync: None,
            pending_sync_apply: None,
            pending_disconnect_dest: None,
            pending_edit_open: None,
            pending_sync_encoding: (None, None),
            // Fail-CLOSED: a test `App` has no backend, and offering sync by
            // default would turn every test into a grant.
            // `main` switches it on when the backend is remote.
            backend_journalled: false,
            backend_daemon: false,
            has_desktop: false,
            handoff_ready: false,
            pending_handoff: false,
            redecorate: false,
            openers: norte_frontend::openers::OpenersConfig::empty(),
            editor: None,
            diff: None,
            pending_open: None,
            pending_shell: None,
            pending_subshell: false,
            subshell_chord: None,
            terminal_chord: None,
            pending_osc52: None,
            dialog_hints: crate::hints::DialogHints::default(),
            key_bars: KeyBars::default(),
            chord_split_h: None,
            pending_key: None,
            version_line: "",
            help_chords: default_help_chords(),
            palette: None,
            palette_recent: Vec::new(),
            popular: norte_frontend::history::Popular::default(),
            palette_rows: Vec::new(),
            wizard: None,
            splash: None,
            splash_until_ms: None,
            processes_auto: false,
            panel_focus: None,
            paneles: norte_frontend::layout::BySlot::new(),
            pending_splash_row: None,
            mouse: crate::mouse::MouseState::default(),
            settings: None,
            shortcuts: None,
            swap_seq: 0,
            pick: false,
            picked: None,
        }
    }

    /// A new listing, already with this session's configuration applied.
    ///
    /// Slots are born in four places —a tab, a split, a layout slot, a
    /// restored session— and forgetting the `..` row would be one half of the
    /// screen behaving differently from the other.
    #[must_use]
    pub fn nuevo_pane(&self, dir: VPath, entries: Vec<norte_proto::Entry>) -> Pane {
        let mut pane = Pane::new(dir, entries);
        pane.set_parent_row(self.parent_row);
        pane
    }

    /// A new pane with pane `i`'s listing: what splitting a panel and
    /// opening a tab need.
    ///
    /// Inherits the already-listed entries instead of requesting a listing
    /// —it is the SAME directory, so the new panel appears full right away
    /// and does not flicker empty while something reads the same thing
    /// again— and inherits the REAL ones: [`norte_frontend::PaneState::real_entries`]
    /// leaves out the `..` row, which the new pane sets for itself. Copying
    /// `entries()` instead left the inherited `..` as a normal, markable
    /// entry in the middle of the listing, with the parent directory's name —
    /// one more per split.
    ///
    /// ONE gate for both, not three repeated lines in each: the third one
    /// that showed up would repeat them wrong.
    #[must_use]
    pub fn fork_pane(&self, i: usize) -> Pane {
        let p = &self.panes[i];
        self.nuevo_pane(p.dir().clone(), p.real_entries().to_vec())
    }

    /// Puts into `id` a listing that was born OUTSIDE [`Self::nuevo_pane`]
    /// and applies this session's configuration to it.
    ///
    /// The three that are born outside belong to the SESSION: the one
    /// `apply_session` builds over the saved path, the one startup lists for
    /// it, and the one `pin_start_dir` sets when the command line names a
    /// directory. All three restored the sort and the hidden flag and none
    /// restored the `..` row, so `[ui] parent_entry = true` would switch
    /// itself off from the first saved session onward — and the reader read
    /// it as the TUI having no up row while the window does.
    ///
    /// Stamps the CONFIG side (the `..` row) and restores the SESSION side
    /// (the sort and the hidden flag), in that order and in one place.
    ///
    /// The three callers used to do it on their own and in different orders,
    /// which is how one of them left the row out; the field added tomorrow
    /// would be left out by two. `None` in `sort`/`hidden` means "this
    /// caller has nothing to restore", not "use the factory default".
    pub fn adoptar_pane(
        &mut self,
        id: norte_frontend::layout::SlotId,
        mut pane: Pane,
        sort: Option<norte_frontend::SortSpec>,
        hidden: Option<bool>,
    ) {
        pane.set_parent_row(self.parent_row);
        if let Some(s) = sort {
            pane.set_sort(s);
        }
        if let Some(h) = hidden {
            pane.set_show_hidden(h);
        }
        self.panes.insert_browser(id, pane);
    }

    /// Enciende o apaga la fila `..` en TODOS los panes (`[ui] parent_entry`).
    ///
    /// In all of them and not only the visible ones: a pane behind a tab is
    /// painted again exactly as it was left, and one half of the screen with
    /// the row and the other without it would be the same configuration
    /// saying two things.
    pub fn set_parent_row(&mut self, on: bool) {
        self.parent_row = on;
        for pane in self.panes.browsers_mut() {
            pane.set_parent_row(on);
        }
    }

    /// Closes the menu bar, recording where it was.
    ///
    /// ONE gate, and not out of taste: the menu closes from five places —the
    /// key, `Esc`, choosing an entry, clicking outside and clicking on the
    /// bar— and whichever one forgot to record it would be the one that
    /// makes the next opening start from the first with no apparent reason.
    pub fn close_menu(&mut self) {
        if let Some(m) = &self.menu {
            self.menu_ultimo = m.menu();
        }
        self.menu = None;
    }

    /// The highlighted command in the menu, and the menu closed: what
    /// `Enter` and clicking an entry do, through the same gate.
    pub fn take_menu_choice(&mut self) -> Option<String> {
        let chosen = self
            .menu
            .as_ref()
            .and_then(norte_frontend::menu::MenuState::selected)
            .map(str::to_string);
        self.close_menu();
        chosen
    }

    /// Opens the menu bar, or closes it if it was already open: the same key
    /// does both things, like the rest of the overlays.
    ///
    /// Reopens from where it was —always starting from the first would force
    /// walking the whole bar on every gesture— and that is why it goes
    /// through [`Self::close_menu`], which is the one that records it.
    pub fn toggle_menu(&mut self) {
        if self.menu.is_some() {
            self.close_menu();
        } else {
            self.menu = Some(norte_frontend::menu::MenuState::reopen_at(self.menu_ultimo));
        }
    }

    /// What a side panel with the keyboard does NOT decide: the application's
    /// chrome. `true` = handled here and the panel does not have to look at
    /// it.
    ///
    /// The menu bar does not belong to the listings, it belongs to the whole
    /// application, and while the keyboard was inside the tree, the sidebar
    /// or the processes panel its key died: it was not in any of their
    /// allowlists, so the panel swallowed it and the screen stayed the same.
    /// It is the same lesson `layout.places` already brought to those
    /// allowlists, and that is why it lives in ONE place: three panels with
    /// their own copy are three places to forget the fourth.
    pub fn panel_chrome_command(&mut self, cmd: &str) -> bool {
        match cmd {
            "app.menu" => {
                self.toggle_menu();
                true
            }
            // Quitting is not the panel's either. Without this, `F10` and
            // `q` died with the keyboard inside the tree or the sidebar
            // —only `Ctrl+C` got out— and the reader closed the terminal
            // window believing they had quit: `ntc` was still alive holding
            // the session lock, and every next `ntc` started up detached
            // without saving anything.
            "app.quit" => {
                self.request_quit();
                true
            }
            _ => false,
        }
    }

    /// `app.quit` honoring `[ui] confirm_quit`: asks if it should, and if
    /// not, quits. The same three-way decision as the named dispatch
    /// ([`quit_needs_confirm`]); `Ctrl+C`'s `app.quit = true` remains the
    /// emergency exit, immediate and without asking.
    pub fn request_quit(&mut self) {
        if quit_needs_confirm(self.confirm_quit, self.board.has_active()) {
            self.modal = Some(Modal::ConfirmQuit);
        } else {
            self.quit = true;
        }
    }

    /// The interface's clock, in epoch milliseconds.
    ///
    /// ONE single source: [`Self::render_now_ms`] when it is fixed (the
    /// tests fix it so a snapshot does not depend on the time), and the real
    /// clock otherwise. Used by the relative time cells and the expiry of the
    /// task board's terminal rows — two places that have to agree, because a
    /// test that fixes the clock for the first and not the second would have
    /// a panel that changes on its own.
    /// How long the `brief` splash covers, AT MOST.
    ///
    /// It is not one of the timers ADR 0006 forbids —that one is about
    /// resolving KEYS, and here no key depends on the clock: any of them
    /// removes the splash first—. It is the deadline that stops a cover
    /// screen from staying up when the first listing is slow: 1.2s reads as
    /// instantaneous and does not feel like a slow start.
    ///
    /// The number is the SHARED one: two different deadlines would be two
    /// different starts, and whichever took longer would read as the
    /// terminal being slower than the window.
    pub const SPLASH_BRIEF_MS: i64 = norte_frontend::splash::BRIEF_MS;

    /// Shows the board to the lightweight progress bar (ADR 0146), with the
    /// render clock.
    pub fn note_strip(&mut self) {
        let ahora = self.now_ms();
        self.strip.update(
            ahora,
            self.board
                .rows()
                .iter()
                .map(|r| norte_frontend::task_strip::StripTask {
                    progress: &r.last,
                    operand: r.operand.as_ref(),
                    bps: r.rate.bps(),
                }),
        );
    }

    /// El reloj del pintado.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.render_now_ms.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        })
    }

    /// Publishes a resolver's in-flight state to the screen: the status-bar
    /// segment ([`Self::pending`]) and the which-key panel
    /// ([`Self::which_key`]), which must never disagree about it.
    ///
    /// The ONE place that decides whether the panel is open, and it decides it
    /// from the resolver rather than from the [`Resolution`] variant that got
    /// us here:
    ///
    /// - a pending SEQUENCE opens it — immediately, with no delay of any kind.
    ///   ADR 0006's resolution is timing-free and a panel that waited 400 ms
    ///   would put timing back into what the reader sees;
    /// - a bare COUNT does not. Its pending sequence is empty, so there are no
    ///   rows: the continuation of a count is any key at all, and the panel
    ///   would be the whole keymap. K2a already paints the count in the bar,
    ///   and a count typed BEHIND a prefix still shows — in the panel's title.
    ///
    /// [`Resolution`]: norte_frontend::keymap::Resolution
    pub fn show_pending(&mut self, resolver: &norte_frontend::keymap::Resolver, lang: Lang) {
        self.pending = crate::keymap::pending_display(resolver);
        self.which_key = (!resolver.pending().is_empty()).then(|| {
            norte_frontend::whichkey::WhichKeyRows::build(
                resolver.effective(),
                resolver.pending(),
                resolver.count(),
                lang,
            )
        });
    }

    /// Nothing is pending any more: the bar segment and the panel go together.
    ///
    /// Called by every arm that ENDS a pending state — a command ran, the key
    /// was unavailable, `Esc` reset it, a key the frontend does not model
    /// arrived — and by the hot reload, which replaces the resolvers whole
    /// (ADR 0007): a panel built from the old effective map would survive its
    /// keymap and list keys the new one does not have.
    pub fn clear_pending(&mut self) {
        self.pending.clear();
        self.which_key = None;
    }

    /// The reader is no longer typing a sequence, and it was not a key of that
    /// sequence that ended it: a gesture (a double click), or a key SWALLOWED
    /// before the resolver ever saw it (the `Esc` that cancels a Lua command,
    /// an AI rename or a semantic search in flight).
    ///
    /// Resets the resolver too, which [`Self::clear_pending`] alone does not:
    /// the pending chords live in the resolver, and clearing only the screen
    /// would leave `g` armed inside it, so the NEXT key would complete a
    /// sequence the reader had already abandoned. It is the same treatment the
    /// run loop gives a key the frontend does not model.
    ///
    /// The visible symptom this exists for: with a Lua command running and `g`
    /// pending, `Esc` is consumed by the cancellation and the panel used to
    /// stay on screen — the one key that means "never mind" appearing to do
    /// nothing at all.
    pub fn abandon_pending(&mut self, resolver: &mut norte_frontend::keymap::Resolver) {
        resolver.reset();
        self.clear_pending();
    }

    /// Lays the open help overlay out for a body of `width`×`height` cells,
    /// through the resolver and theme in force. No-op with the overlay closed.
    ///
    /// The split-borrow wrapper exists because [`HelpView::refresh`] needs
    /// three fields of `App` at once ([`Self::help`] mutably,
    /// [`Self::help_chords`] and [`Self::theme`] shared) and a caller outside
    /// this module cannot name them disjointly. The run loop calls it with
    /// [`crate::ui::help_body_size`] of the frame it is about to paint, so
    /// the geometry the model clamps against is the geometry on screen.
    pub fn refresh_help(&mut self, width: usize, height: usize) {
        if let Some(help) = &mut self.help {
            help.refresh(&self.help_chords, width, height, &self.theme);
        }
    }

    /// Opens the live-search dialog (`Alt+F7`, liveSearch T6) empty. The
    /// walk's root is resolved at launch time (the focused pane's cwd).
    pub fn open_search_dialog(&mut self) {
        self.search_dialog = Some(SearchDialog::new());
    }
}

/// Semantic hits visible at once in [`Modal::SemanticHits`] (scroll window)
/// — the constant lives in `norte-frontend` (shared with the GUI, same
/// criterion as [`AI_RENAME_PAIR_LIMIT`]); re-exported for the render (`ui`),
/// the modal's height and [`App::semantic_cursor`]'s clamp.
pub use norte_frontend::SEMANTIC_HIT_LIMIT;

/// AI plan pairs visible at once in [`Modal::AiRenamePlan`] (scroll window,
/// audit MAJOR-3) — the constant lives in `norte-frontend` (shared with the
/// GUI, quality review 78eb243 MAJOR-1); re-exported for the render (`ui`),
/// the modal's height and [`App::ai_plan_scroll`]'s clamp.
pub use norte_frontend::AI_RENAME_PAIR_LIMIT;

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    /// Search dialog (liveSearch T6): Tab cycles the active field and
    /// printables/backspace land in the focused field.
    #[test]
    fn search_dialog_tab_and_field_edit() {
        let mut d = SearchDialog::new();
        assert_eq!(d.field, SearchField::Name);
        d.push_char('*');
        d.push_char('x');
        assert_eq!(d.name, "*x");
        assert_eq!(d.content, "");
        d.toggle_field();
        assert_eq!(d.field, SearchField::Content);
        d.push_char('a');
        d.push_char('b');
        d.backspace();
        assert_eq!(d.content, "a");
        assert_eq!(d.name, "*x", "backspace only touched the active field");
        // Since 0.81.0 Tab cycles through SEVEN fields, not two: the full
        // round trip is checked by `tab_da_la_vuelta_entera` in `app::pane`.
        d.toggle_field();
        assert_eq!(d.field, SearchField::Exclude);
    }

    /// The toggles (F2 regex / F3 case) flip their flags independently.
    #[test]
    fn search_dialog_toggles_regex_and_case() {
        let mut d = SearchDialog::new();
        assert!(!d.regex && !d.case);
        d.toggle_regex();
        assert!(d.regex && !d.case);
        d.toggle_case();
        assert!(d.regex && d.case);
        d.toggle_regex();
        assert!(!d.regex && d.case);
    }

    /// Criteria validation: with no field there is no search; one (name OR
    /// content) is enough for there to be one.
    #[test]
    fn search_dialog_criteria_not_empty() {
        let mut d = SearchDialog::new();
        assert!(!d.has_criteria(), "both empty: does not launch");
        d.push_char('*');
        assert!(d.has_criteria(), "name alone is enough");
        let mut d = SearchDialog::new();
        d.toggle_field();
        d.push_char('a');
        assert!(d.has_criteria(), "content alone is enough");
    }

    /// `begin_search` marks the pane as virtual, empties the entries and
    /// resets the state to `Running`; `extend_listing` feeds the hits
    /// WITHOUT turning off virtual mode (the hits are still from a search).
    #[test]
    fn begin_search_marks_virtual_and_extend_keeps_it() {
        let mut p = pane_con(&["basura"]);
        p.begin_search(root());
        assert!(p.virtual_search);
        assert_eq!(p.search_state, SearchState::Running);
        assert!(p.entries().is_empty(), "the hits start empty");
        p.extend_listing(vec![file("hit1"), file("hit2")]);
        assert!(p.virtual_search, "extend does not turn off virtual mode");
        assert_eq!(names(&p), vec!["hit1", "hit2"]);
    }

    /// A NORMAL listing (cd/refresh) turns off search's virtual mode.
    #[test]
    fn normal_listings_turn_off_virtual_mode() {
        let mut p = pane_con(&[]);
        p.begin_search(root());
        assert!(p.virtual_search);
        p.begin_listing(root(), vec![file("a")], false, None);
        assert!(!p.virtual_search, "begin_listing turns off virtual");

        p.begin_search(root());
        p.set_listing(root(), vec![file("a")]);
        assert!(!p.virtual_search, "set_listing turns off virtual");

        p.begin_search(root());
        p.refresh_listing(vec![file("a")]);
        assert!(!p.virtual_search, "refresh_listing turns off virtual");
    }
}
