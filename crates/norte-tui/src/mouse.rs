//! The TUI's mouse: terminal capture, painted geometry, hit testing and
//! translating crossterm events into the SHARED gestures of
//! [`norte_frontend::mouse`].
//!
//! No marking rule lives here: what a drag marks, when a gesture is a
//! transfer and when it is a rubber-band selection, and with which
//! modifiers, is decided by `norte-frontend` for both frontends at once
//! (rule 7). This module does the three things that ARE the terminal's:
//! asking the emulator to report the mouse, knowing which cell is which row,
//! and applying the resulting [`Effect`]s to the model.
//!
//! # Capture is not free
//!
//! With capture active the TERMINAL stops seeing the buttons it uses for its
//! own text selection: select-and-paste with the mouse stops working the way
//! the user has learned it. In almost every emulator holding Shift while
//! dragging restores the native selection, and `[ui] mouse = false` restores
//! it entirely. That is USER-facing information, not a comment: it lives in
//! help's `mouse` topic and in the `ui.mouse` setting's description.

use crate::app::Trail;
use crate::dispatch::dispatch;
use crate::keymap::Command;
use crate::navigate::{apply_cd, cd_in};
use crate::refresh::reap_search_run;
use crate::screens::drain_places_drives;
use std::io::Write;
use std::time::{Duration, Instant};

#[cfg(windows)]
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use norte_frontend::mouse::{Drag, Effect, Mods, Pending, Press, Spot};

use crate::app::{App, TransferKind};
use crate::ui::HOSTILE_BADGE;

/// Double-click window. crossterm does NOT report double clicks (no terminal
/// mouse protocol has them): this window counts them over the SAME row of
/// the SAME pane, which is also the rule that keeps two clicks on different
/// rows from reading as one double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Rows a wheel tick moves. Three is what almost every terminal and browser
/// uses; a single row makes the wheel useless on a long listing and a whole
/// page loses its place.
const WHEEL_ROWS: usize = 3;

/// A draggable border between two layout slots.
///
/// Carried by the LEFT slot (or the TOP one), which is the one
/// `Node::drag_border` knows how to name: the border is "its own with the
/// next one".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeBorder {
    /// The left or top slot.
    pub slot: norte_frontend::layout::SlotId,
    /// The right or bottom one: together the two locate the layout node
    /// where they are neighbors (`Node::border_pair`), which may not be
    /// `slot`'s.
    pub vecino: norte_frontend::layout::SlotId,
    /// Which direction the `Split` containing them lays out.
    pub dir: norte_frontend::layout::Dir,
    /// The border's column (or row).
    pub linea: u16,
    /// Where the border starts, on the other axis.
    pub desde: u16,
    /// End (exclusive) of the border's span.
    pub hasta: u16,
    /// Where the PAIR starts on the layout's axis.
    pub inicio: u16,
    /// How much the two together take up. This is what turns a pointer
    /// column into a fraction.
    pub largo: u16,
}

impl ResizeBorder {
    /// Does `(col, row)` fall on this border?
    ///
    /// The border is TWO columns, not one: in the TUI each slot paints its
    /// own frame, so between two neighbors there is one's right side and the
    /// other's left side. Grabbing only one of the two leaves half the line
    /// dead, and the one that dies is the one the eye sees first.
    #[must_use]
    pub const fn hit(&self, col: u16, row: u16) -> bool {
        let (axis, other) = match self.dir {
            norte_frontend::layout::Dir::Horizontal => (col, row),
            norte_frontend::layout::Dir::Vertical => (row, col),
        };
        (axis + 1 == self.linea || axis == self.linea) && other >= self.desde && other < self.hasta
    }
}

/// A layout slot and the rectangle it occupied, in cells.
///
/// Serves a single question —which panel is under the pointer?— and that is
/// why it stores neither the kind nor who would take the keyboard: that is
/// answered by [`App::focus_slot`] with the registry both frontends share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelSlot {
    /// The slot.
    pub slot: norte_frontend::layout::SlotId,
    /// Left column, border included.
    pub x: u16,
    /// Top row, border included.
    pub y: u16,
    /// Width, borders included.
    pub width: u16,
    /// Height, borders included.
    pub height: u16,
}

impl PanelSlot {
    /// Does `(col, row)` fall inside this slot?
    #[must_use]
    pub const fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.x
            && col < self.x.saturating_add(self.width)
            && row >= self.y
            && row < self.y.saturating_add(self.height)
    }
}

/// A pane's PAINTED geometry, in terminal cells.
///
/// Filled by [`crate::ui::pane_geometry`] after every frame and stored by
/// the model (#124): hit testing resolves against the last screen the user
/// actually saw, not against a hand-recalculated layout that may have
/// already changed.
///
/// Deliberately WITHOUT ratatui types: it is model state, and the model does
/// not know the render engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaneGeometry {
    /// The block's left column (border included).
    pub x: u16,
    /// The block's top row (border included).
    pub y: u16,
    /// The block's width, borders included.
    pub width: u16,
    /// The block's height, borders included.
    pub height: u16,
    /// First LISTING row: `y + 2` (top border + column header). Stored
    /// precomputed rather than derived in the hit test, so that the day the
    /// pane gains or loses a chrome row there is ONE place to change.
    pub first_list_row: u16,
    /// How many listing rows were painted. `0` = the pane has no room for
    /// any (a tiny terminal): then NO row resolves.
    pub list_rows: u16,
    /// First PAINTED index of the listing (the scroll). In painted
    /// coordinates: under a quick-search filter it is a position inside the
    /// visible subset, not an index into `entries`.
    pub offset: usize,
}

impl PaneGeometry {
    /// Does `(col, row)` fall inside this pane's block, borders included?
    #[must_use]
    pub const fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.x
            && col < self.x.saturating_add(self.width)
            && row >= self.y
            && row < self.y.saturating_add(self.height)
    }

    /// The PAINTED index under `(col, row)`, or `None` if there is no
    /// listing row there.
    ///
    /// `None` covers ALL the chrome, and each case is here on purpose because
    /// the natural failure would be to saturate toward a real row: the top
    /// border with its title (the pane's path), the column header, the
    /// bottom border (where the quick-search input is also painted), the two
    /// side-border columns, and the gap BELOW the last entry of a short
    /// listing. A click on the empty space of a half-full pane must not mark
    /// the last entry.
    #[must_use]
    pub fn painted_row_at(&self, col: u16, row: u16) -> Option<usize> {
        if col == self.x || col.saturating_add(1) == self.x.saturating_add(self.width) {
            return None; // side borders
        }
        let k = row.checked_sub(self.first_list_row)?; // top border + header
        if k >= self.list_rows {
            return None; // bottom border (and any row beyond it)
        }
        Some(self.offset.saturating_add(usize::from(k)))
    }
}

/// Where a click landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Pane under the pointer (0 = left).
    pub pane: usize,
    /// ABSOLUTE index into `entries` of the row that was clicked, or `None`
    /// if chrome or the empty space below the listing was clicked. `None`
    /// is still a hit: the wheel and focus want the pane even with no row.
    pub index: Option<usize>,
}

/// Everything that has to stay true for a gesture in flight to still mean
/// something: each pane's indices, that the two panes are still on the side
/// they were on, and that nothing has come in front.
///
/// A gesture only carries indices ([`Spot`]), and an index names a row of the
/// listing that was painted. When that listing moves —another directory, a
/// refill after a mutation, a page of a paginated fill, a re-sort— the index
/// starts naming a different file, and the gesture has stopped being the one
/// the user made.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Validity {
    /// [`crate::app::Pane::listing_epoch`] of each VISIBLE pane, in order.
    ///
    /// Variable length since P6: with splits there are more than two, and a
    /// vector that CHANGES LENGTH also invalidates the gesture — which is
    /// correct, because opening or closing a panel moves everything else
    /// around.
    epochs: Vec<u64>,
    /// [`crate::app::App::swap_seq`]. The epochs do NOT cover a `pane.swap`:
    /// they travel with their pane, so the swap merely exchanges the two
    /// values and, when they tie —the normal case right after startup— the
    /// per-side comparison sees nothing move. The gesture, however, stores a
    /// pane index, and after the swap that index names the other side's
    /// content.
    swap: u64,
    /// There was an overlay/modal in front when painting. A modal that opens
    /// mid-drag takes the gesture down with it: by the time it closes, the
    /// user has already moved on.
    overlay: bool,
}

/// Mouse state that lives in the model: the last frame's geometry, the armed
/// gesture, the instant of the last click (for the double) and all of that's
/// validity.
#[derive(Debug, Default)]
pub struct MouseState {
    /// `None` = the last frame painted no panes (viewer open) or there has
    /// been no frame yet. With no geometry NOTHING resolves: a click against
    /// a screen that does not exist is worse than an ignored click.
    geometry: Option<Vec<PaneGeometry>>,
    /// The last frame's clickable menu-bar zones.
    menu_zones: Vec<crate::ui::MenuZone>,
    panel_zones: Vec<crate::ui::PanelZone>,
    /// The last frame's clickable key-bar cells (spec 2026-09-10). Empty =
    /// bar off, or no room to paint it.
    key_zones: Vec<crate::ui::KeyZone>,
    /// The last frame's active modal buttons (spec 2026-09-10). Empty = no
    /// modal, or its key line painted as a hint.
    modal_zones: Vec<crate::ui::ModalZone>,
    /// The last frame's clickable tab-bar zones.
    ///
    /// Empty = no panel has tabs, which is the usual case.
    tab_zones: Vec<crate::ui::TabZone>,
    /// The last frame's clickable places-sidebar rows (#226). Empty =
    /// sidebar closed, or no room to paint it.
    places_zones: Vec<crate::ui::PlaceZone>,
    /// The last frame's clickable tree rows, for the same reason. Empty =
    /// tree closed, or no room to paint it.
    tree_zones: Vec<crate::ui::TreeZone>,
    /// The last frame's extension-manager rows and buttons. Empty = manager
    /// closed.
    extension_zones: Vec<crate::ui::ExtensionZone>,
    /// The last frame's help sidebar, body and clickable pages. `None` =
    /// help closed.
    help_zones: Option<crate::ui::HelpZones>,
    /// The last frame's status-bar detached-session indicator. `None` = the
    /// window is the owner, or the bar was saying something else.
    session_zone: Option<crate::ui::SessionZone>,
    /// The last frame's clickable status-bar items (ADR 0132).
    status_item_zones: Vec<crate::ui::StatusItemZone>,
    /// The last frame's draggable borders.
    borders: Vec<ResizeBorder>,
    /// The slots placed in the last frame, to know which panel is under a
    /// click.
    slots: Vec<PanelSlot>,
    /// The border being dragged RIGHT NOW, if there is one.
    ///
    /// Frozen on grab and not looked up again for the duration of the
    /// gesture: the layout changes under the pointer on every move —that is
    /// the point of a drag— and asking again "what border is here" would end
    /// up grabbing the neighboring one the moment it crossed over the other.
    resizing: Option<ResizeBorder>,
    /// The column whose width is being dragged RIGHT NOW, frozen on grab for
    /// the same reason as `resizing`.
    columna: Option<ColumnDrag>,
    /// What the last column drag left on release —`(id, cells)`— for the
    /// run loop to store. [`After`] is `Copy` and cannot carry it inside.
    ancho_soltado: Option<(String, u16)>,
    /// The shared gesture machine (`norte-frontend`).
    drag: Drag,
    /// `(when, where)` of the last left click, for the double.
    last_click: Option<(Instant, Spot)>,
    /// The previous frame's [`Validity`], to detect the change.
    validity: Validity,
    /// The panel being MOVED by its title (ADR 0138), if there is one.
    moviendo: Option<MoveDrag>,
    /// The LAST mouse event's modifiers, so [`drop_hint`] can ask
    /// [`Drag::pending`] what releasing RIGHT NOW would do.
    ///
    /// Remembered because a terminal does not report the keyboard while the
    /// button is held: crossterm carries the modifiers INSIDE each mouse
    /// event, so pressing Shift without moving the pointer does not arrive
    /// until the next cell is crossed. The hint updates then, not before —
    /// it is a limit of the protocol, not a choice, and that is why the hint
    /// names both outcomes ("with Shift, move") instead of trusting that the
    /// modifier will be reflected instantly.
    last_mods: Mods,
}

/// A panel being dragged by its title row (ADR 0138).
#[derive(Debug, Clone, Copy)]
struct MoveDrag {
    /// The slot being dragged.
    slot: norte_frontend::layout::SlotId,
    /// Where it was grabbed.
    x0: u16,
    /// Where it was grabbed.
    y0: u16,
    /// Past the threshold: no longer a click.
    activo: bool,
    /// Where it would land if released now, and the part that gets
    /// highlighted.
    destino: Option<(
        norte_frontend::layout::SlotId,
        norte_frontend::layout::DropZone,
        norte_frontend::layout::Rect,
    )>,
}

impl MouseState {
    /// The part of the panel where the one being moved would land, to
    /// highlight it; `None` if nothing is moving or it lands nowhere.
    #[must_use]
    pub fn move_target(&self) -> Option<norte_frontend::layout::Rect> {
        self.moviendo
            .filter(|m| m.activo)
            .and_then(|m| m.destino)
            .map(|(_, _, r)| r)
    }

    /// The last frame's geometry, one `PaneGeometry` per visible panel.
    #[must_use]
    pub fn geometry(&self) -> Option<&[PaneGeometry]> {
        self.geometry.as_deref()
    }

    /// The width the last column drag left, `(id, cells)`, to store it.
    /// Consumed on read: a width is written once.
    pub fn take_column_width(&mut self) -> Option<(String, u16)> {
        self.ancho_soltado.take()
    }

    /// The rectangle slot `id` was painted with in the last frame, if it was
    /// painted.
    ///
    /// Not only the mouse's business: whoever is about to SPLIT a slot also
    /// asks this, needing to know whether what is there has room for two.
    /// Only the frame knows the real size —the layout depends on the
    /// terminal, the chrome and the weights— and this is where the frame
    /// left it recorded.
    #[must_use]
    pub fn slot_rect(
        &self,
        id: norte_frontend::layout::SlotId,
    ) -> Option<norte_frontend::layout::Rect> {
        self.slots
            .iter()
            .find(|s| s.slot == id)
            .map(|s| norte_frontend::layout::Rect {
                x: s.x,
                y: s.y,
                width: s.width,
                height: s.height,
            })
    }

    /// Releases the armed gesture and the half-paired click.
    ///
    /// Marks a rubber-band selection already applied STAY: releasing the
    /// gesture is not undoing it (contract of [`Drag::cancel`]).
    fn invalidate(&mut self) {
        self.drag.cancel();
        self.last_click = None;
        // The COLUMN drag is not released here: its borders do not depend on
        // which rows exist, and a listing that fills by pages or a watcher
        // refresh used to cut it halfway — the width showed on screen and
        // was never saved.
    }
}

/// Everything CLICKABLE the just-painted frame left on the screen.
///
/// A struct and not six loose arguments: five of the six fields are a `Vec`
/// and a call that crossed them would compile — the mouse would resolve the
/// tabs against the sidebar's rows without a word. Same reason [`Press`] is
/// a struct.
#[derive(Debug, Default)]
pub struct FrameZones {
    /// The tab-bar zones.
    pub tabs: Vec<crate::ui::TabZone>,
    /// The menu-bar zones.
    pub menus: Vec<crate::ui::MenuZone>,
    /// The panel-bar boxes (#324).
    pub panels: Vec<crate::ui::PanelZone>,
    /// The key-bar cells (spec 2026-09-10).
    pub keys: Vec<crate::ui::KeyZone>,
    /// The active modal's buttons (spec 2026-09-10).
    pub modal: Vec<crate::ui::ModalZone>,
    /// The places-sidebar rows (#226).
    pub places: Vec<crate::ui::PlaceZone>,
    /// The tree's rows (#136).
    pub tree: Vec<crate::ui::TreeZone>,
    /// The extension manager's rows and buttons, if open.
    pub extensions: Vec<crate::ui::ExtensionZone>,
    /// Help's zones, if open.
    pub help: Option<crate::ui::HelpZones>,
    /// The status bar's detached-session indicator, if painted.
    pub session: Option<crate::ui::SessionZone>,
    /// The status bar's clickable items (ADR 0132).
    pub status_items: Vec<crate::ui::StatusItemZone>,
    /// The draggable borders.
    pub borders: Vec<ResizeBorder>,
    /// The placed slots, to know which panel is under a click.
    pub slots: Vec<PanelSlot>,
}

/// Closes the frame: returns the just-painted geometry to the model (#124)
/// and releases the gesture in flight if it has stopped meaning anything.
///
/// **This is the ONLY place a gesture expires**, and it lives here because
/// the run loop passes through here after EVERY frame, before handling any
/// event.
///
/// The alternative was patching the places that swallow mouse events: the
/// cd's internal `select!`, `on_tick`, `refresh_panes`, the viewer's pump…
/// all of them filter `Event::Key` and drop the rest, so a release landing
/// there never arrives. The gesture stays ARMED and the next motion
/// continues a rubber-band selection the user finished a while ago; and a
/// click from before a cd pairs with one from after into a double click that
/// enters a directory nobody asked for. But those pumps are four today and
/// will be five tomorrow, and the fifth has no reason to remember. What IS
/// invariant is that a gesture lives on indices and the listing is what
/// moves the indices: checking it here covers all four, and the fifth for
/// free.
pub fn after_frame(app: &mut App, geometry: Option<Vec<PaneGeometry>>, zones: FrameZones) {
    let FrameZones {
        tabs: tab_zones,
        menus: menu_zones,
        panels: panel_zones,
        keys: key_zones,
        modal: modal_zones,
        places: places_zones,
        tree: tree_zones,
        extensions: extension_zones,
        help: help_zones,
        session: session_zone,
        status_items: status_item_zones,
        borders,
        slots,
    } = zones;
    let validity = Validity {
        epochs: app
            .panes
            .iter()
            .map(crate::app::Pane::listing_epoch)
            .collect(),
        swap: app.swap_seq(),
        overlay: overlay_open(app),
    };
    // With no panes painted (viewer open) there is also nowhere to drop.
    if validity != app.mouse.validity || geometry.is_none() {
        app.mouse.invalidate();
    }
    app.mouse.validity = validity;
    app.mouse.geometry = geometry;
    app.mouse.tab_zones = tab_zones;
    app.mouse.menu_zones = menu_zones;
    app.mouse.panel_zones = panel_zones;
    app.mouse.key_zones = key_zones;
    app.mouse.modal_zones = modal_zones;
    app.mouse.places_zones = places_zones;
    app.mouse.tree_zones = tree_zones;
    app.mouse.extension_zones = extension_zones;
    app.mouse.help_zones = help_zones;
    app.mouse.session_zone = session_zone;
    app.mouse.status_item_zones = status_item_zones;
    app.mouse.borders = borders;
    app.mouse.slots = slots;
}

/// The extension manager's button commands the LAST frame painted, in the
/// order they were painted.
///
/// `tab`'s ring walks THIS list, the same one a click resolves against, and
/// not the one `extension_buttons` would build: the card is not painted with
/// a narrow box, and a button that did not fit the width is not there
/// either. Taking the keyboard's stops from what was PAINTED is the same
/// rule that made the manager clickable, and the one that stops `tab` from
/// moving focus to a place with nothing in it.
#[must_use]
pub fn painted_extension_buttons(app: &App) -> Vec<&'static str> {
    app.mouse
        .extension_zones
        .iter()
        .filter_map(|z| match z.hit {
            crate::ui::ExtensionHit::Button(cmd) => Some(cmd),
            crate::ui::ExtensionHit::Row(_) => None,
        })
        .collect()
}

/// The command of the status-bar item under `ev` in the last frame
/// (ADR 0132), if there is a clickable one there.
fn elemento_de_estado_en(app: &App, ev: MouseEvent) -> Option<&'static str> {
    app.mouse
        .status_item_zones
        .iter()
        .find(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
        .map(|z| z.command)
}

/// Whether `ev` is the left button landing on the last frame's
/// detached-session indicator.
fn pulsa_indicador_de_sesion(app: &App, ev: MouseEvent) -> bool {
    matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && app
            .mouse
            .session_zone
            .is_some_and(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
}

/// The help page that explains the detached-session indicator: the profiles
/// and session one, which is where each panel's whereabouts get told. Lived
/// in `panes` until that page got split.
///
/// A corpus id and not a context: the indicator is not a screen the reader
/// is on, it is a fact about this window, and its page is fixed. The mouse
/// test ties the id to a real page in both languages, and to that page
/// TALKING about the session: existing was not enough —when `panes` got
/// split, the page still existed with no line about the session—.
pub const SESSION_HELP_TOPIC: &str = "profiles";

/// What the run loop must do after a mouse event. Everything that can be
/// done on the model is already done by the time this returns; this is only
/// what needs the backend or the terminal, which this module does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum After {
    /// Nothing: the event was resolved entirely here.
    #[default]
    Nothing,
    /// A panel-bar button was pressed (#324): the run loop dispatches
    /// `App::pending_panel_command` through the SAME path as its shortcut.
    /// Same as the menu, and for the same reason: dispatching is async and
    /// this module does not have the backend.
    PanelBar,
    /// A menu item was pressed: the run loop must run it, through the same
    /// path as `Enter`. This module cannot: dispatching is async and needs
    /// the backend.
    MenuAccept,
    /// Double click on a row: dispatches `nav.enter`, THE SAME command as
    /// the keyboard's (never a second path that enters directories on its
    /// own).
    Enter,
    /// A places-sidebar section was folded or unfolded (#226): unfolding the
    /// drives is the moment to request them again, and it is the SAME path
    /// the key takes.
    PlacesFolded,
    /// A sidebar row was activated: the listing has to be taken to wherever
    /// `App::places_activate` says, through the usual `cd` flow.
    PlacesActivate,
    /// A tree branch was activated (#136): same treatment as the sidebar
    /// row, and `App::tree_activate` says the destination.
    TreeActivate,
    /// The status bar's detached-session indicator was pressed: the run loop
    /// opens help on the page that explains it. Cannot be done here: help
    /// opens with the key sheet and the language, which belong to the run
    /// loop.
    SessionHelp,
    /// A button on the extension manager's card was pressed, or the row
    /// already chosen: the run loop dispatches this command through the SAME
    /// path as its key (`on_extensions_click`). Cannot be done here:
    /// enabling, approving or uninstalling talk to the backend.
    Extension(&'static str),
    /// A key-bar cell or a modal's button was pressed (spec 2026-09-10): the
    /// key is left in `App::pending_key` and the run loop dispatches it
    /// through `on_key`, which is the ONLY path with all three resolvers at
    /// hand. A click there IS pressing the key; there is no second dispatch
    /// that could diverge.
    SynthKey,
    /// A column's border was released after dragging it: the width is
    /// already applied and painted, and the run loop stores it with what it
    /// left in [`MouseState::take_column_width`]. Cannot be done here:
    /// writing `norte.toml` is disk work and stays outside the loop (rule 2).
    ColumnWidth,
}

/// The ABSOLUTE index into `entries` of a PAINTED position of the pane.
///
/// Under a quick-search filter what is painted is the visible subset, so the
/// position is translated through it; with no filter, what is painted IS
/// `entries`. Out of range (a listing shorter than the window, or a listing
/// that changed between the frame and the click) returns `None` instead of
/// saturating.
fn absolute_index(pane: &crate::app::Pane, painted: usize) -> Option<usize> {
    match pane.quick_visible() {
        Some(vis) => vis.get(painted).copied(),
        None => (painted < pane.entries().len()).then_some(painted),
    }
}

/// Resolves `(col, row)` against the last frame's geometry.
///
/// `None` = outside both panes (tasks panel, status bar) or no geometry
/// (viewer open).
#[must_use]
pub fn hit_test(app: &App, col: u16, row: u16) -> Option<Hit> {
    let geometry = app.mouse.geometry()?;
    let (pane, geom) = geometry
        .iter()
        .enumerate()
        .find(|(_, g)| g.contains(col, row))?;
    Some(Hit {
        pane,
        index: geom
            .painted_row_at(col, row)
            .and_then(|painted| absolute_index(&app.panes[pane], painted)),
    })
}

/// What the status bar says about a drag IN FLIGHT: how many items would
/// travel, to which directory, and whether releasing now COPIES or MOVES.
/// `None` = no drop pending (no gesture, marking in progress, or the pointer
/// is still at home — dropping there is an explicit no-op and promising a
/// copy that will not happen is worse than promising nothing).
///
/// The hint is NOT computed separately: it comes from [`Drag::pending`], the
/// same source and the same rules [`Drag::release`] reads, and counts the
/// items with the same reading as [`App::open_transfer`] (the marks, or the
/// promoted row). A hint derived on its own would end up promising a copy
/// while the drop moves, or "3 items" while one travels.
///
/// Twin of `drop_hint` in the GUI, down to the Fluent key.
#[must_use]
pub fn drop_hint(app: &App) -> Option<String> {
    let Some(Pending::Drop {
        from_pane,
        to_pane,
        move_files,
        promoted,
    }) = app.mouse.drag.pending(app.mouse.last_mods)
    else {
        return None;
    };
    let n = match promoted {
        Some(idx) => usize::from(app.panes[from_pane].entries().get(idx).is_some()),
        None => app.panes[from_pane].marked_paths().len(),
    };
    if n == 0 {
        return None;
    }
    // The destination dir, with the SAME sanitizing as the pane's header
    // (rule 1: display is always lossy, and marked if hostile).
    let (to_txt, hostile) = norte_frontend::path_display_with(
        app.panes[to_pane].dir(),
        app.panes[to_pane].name_encoding(),
    );
    let to_txt = if hostile {
        format!("{HOSTILE_BADGE} {to_txt}")
    } else {
        to_txt
    };
    let key = if move_files { "drag-move" } else { "drag-copy" };
    Some(norte_i18n::ta(
        key,
        &[("n", &n.to_string()), ("to", &to_txt)],
    ))
}

/// The two modifiers marking understands. The rest (alt, super) is the
/// keymap's business, not these gestures'.
fn mods(m: KeyModifiers) -> Mods {
    Mods::new(
        m.contains(KeyModifiers::CONTROL),
        m.contains(KeyModifiers::SHIFT),
    )
}

/// Is there an overlay swallowing the interaction? With one open the panes
/// are still painted UNDERNEATH, so the geometry is still valid and a click
/// would resolve a row perfectly — and would move the cursor of a listing
/// the user is not looking at, under a modal that is asking them something.
/// The keyboard already routes this way (`modal_wins` and the run loop's
/// overlay chain); the mouse does the same, as one piece.
pub(crate) fn overlay_open(app: &App) -> bool {
    app.modal.is_some()
        || app.viewer.is_some()
        || app.help.is_some()
        || app.palette.is_some()
        || app.wizard.is_some()
        || app.settings.is_some()
        // K3c: the shortcut editor. Today it is always behind `settings`,
        // which is already in this list, but that is a property of HOW it
        // opens and not of the type — and the GUI (c4) will open it on its
        // own.
        || app.shortcuts.is_some()
        || app.theme_picker.is_some()
        || app.columns_picker.is_some()
        || app.extensions.is_some()
        || app.nav_popup.is_some()
        || app.search_dialog.is_some()
        // The compare panel REPLACES both panes on screen, so a click there
        // used to land on a listing no longer visible — and a double click
        // would do a real `cd` on an invisible pane, leaving the panel open
        // over roots that no longer describe anything (review MAJOR-1).
        // `keyboard_owner` already declares it the keyboard's owner; this is
        // the other half of the same piece.
        || app.compare.is_some()
}

/// A click with the menu bar open.
///
/// Outside every zone it CLOSES it: that is what any menu does, and leaving
/// it open after clicking elsewhere would turn one extra click into a menu
/// stuck on the screen.
///
/// Pressing an item does NOT run it here: it only highlights it and returns
/// [`After::MenuAccept`], because running a command is async and this module
/// does not have the backend. The run loop finishes it through the same path
/// as `Enter`.
fn menu_click(app: &mut App, col: u16, row: u16) -> After {
    let zone = app
        .mouse
        .menu_zones
        .iter()
        .find(|z| z.row == row && col >= z.x0 && col <= z.x1)
        .copied();
    match zone.map(|z| z.hit) {
        Some(crate::ui::MenuHit::Title(i)) => {
            // With the menu CLOSED this opens it: it is the click that makes
            // the pinned bar usable. Before, it only moved the already-open
            // menu from one title to another, so the bar was visible and
            // could not be clicked.
            if let Some(m) = &mut app.menu {
                m.open(i);
            } else {
                let mut m = norte_frontend::menu::MenuState::new();
                m.open(i);
                app.menu = Some(m);
            }
            After::Nothing
        }
        Some(crate::ui::MenuHit::Item(i)) => {
            if let Some(m) = &mut app.menu {
                m.point_at(i);
            }
            After::MenuAccept
        }
        None => {
            app.close_menu();
            After::Nothing
        }
    }
}

/// The mouse inside the extension manager.
///
/// The wheel moves the list's cursor. A click on a row selects it; on the row
/// ALREADY selected it opens its settings, which is what its footer promises
/// ("press Enter, or the row"). A click on a card button fires that button's
/// command — the SAME one as its key, never a second path. Outside every
/// zone nothing happens: the manager is modal and a stray click does not
/// close it, same as a key not in its allowlist.
///
/// Choosing another row with the previous one's settings open CLOSES them:
/// without this the card would show one plugin and another's settings, and
/// with the narrow box the settings panel would cover the list that was just
/// clicked.
fn extensions_mouse(app: &mut App, ev: MouseEvent) -> After {
    let Some(mgr) = &mut app.extensions else {
        return After::Nothing;
    };
    match ev.kind {
        MouseEventKind::ScrollUp => mgr.up(),
        MouseEventKind::ScrollDown => mgr.down(),
        MouseEventKind::Down(MouseButton::Left) => {
            let zone = app
                .mouse
                .extension_zones
                .iter()
                .find(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
                .copied();
            match zone.map(|z| z.hit) {
                Some(crate::ui::ExtensionHit::Row(i)) if i == mgr.cursor => {
                    return After::Extension("dialog.confirm");
                }
                Some(crate::ui::ExtensionHit::Row(i)) => {
                    if i < mgr.plugins.len() + mgr.errors.len() {
                        mgr.cursor = i;
                        // Choosing a row returns focus to the list, same as
                        // the arrows do: the buttons belong to the chosen
                        // plugin.
                        mgr.foco = crate::app::ExtFoco::Lista;
                        let other = mgr.config.as_ref().is_some_and(|c| {
                            mgr.plugins.get(i).is_none_or(|p| p.id != c.plugin_id)
                        });
                        if other {
                            mgr.config = None;
                        }
                    }
                }
                Some(crate::ui::ExtensionHit::Button(cmd)) => return After::Extension(cmd),
                None => {}
            }
        }
        _ => {}
    }
    After::Nothing
}

/// The tab-bar zone under `(col, row)`, if there is one.
fn tab_zone_at(app: &App, col: u16, row: u16) -> Option<crate::ui::TabZone> {
    app.mouse
        .tab_zones
        .iter()
        .find(|z| z.row == row && col >= z.x0 && col <= z.x1)
        .copied()
}

/// The places-sidebar row under `(col, row)`, if there is one (#226).
fn place_zone_at(app: &App, col: u16, row: u16) -> Option<crate::ui::PlaceZone> {
    app.mouse
        .places_zones
        .iter()
        .find(|z| z.row == row && col >= z.x0 && col <= z.x1)
        .copied()
}

/// The tree row under `(col, row)`, if there is one (#136).
fn tree_zone_at(app: &App, col: u16, row: u16) -> Option<crate::ui::TreeZone> {
    app.mouse
        .tree_zones
        .iter()
        .find(|z| z.row == row && col >= z.x0 && col <= z.x1)
        .copied()
}

/// Applies what clicking a tab-bar zone does.
fn apply_tab_zone(app: &mut App, z: crate::ui::TabZone) {
    // The panel of the bar that was clicked gets the focus: clicking a tab
    // on the other side and having this one receive the command would be the
    // opposite of what the finger said.
    app.set_focus(z.pane);
    match z.action {
        crate::ui::TabAction::Goto(i) => app.tab_goto(i + 1),
        crate::ui::TabAction::New => app.tab_new(),
        crate::ui::TabAction::Close => app.tab_close(),
    }
}

/// The resize gesture: grabbing a border, moving it and releasing it.
///
/// `None` = this event is not the gesture's and goes on its way. The three
/// stages are here together on purpose: a drag is a three-state machine, and
/// splitting it across the dispatcher is how one ends up dragging with the
/// button released.
///
/// The size is written to the TREE, which is what the session saves: that is
/// why a moved border stays where it was left on the next open, with nothing
/// more needed.
fn resize_gesture(app: &mut App, ev: MouseEvent) -> Option<After> {
    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let border = *app
                .mouse
                .borders
                .iter()
                .find(|b| b.hit(ev.column, ev.row))?;
            // Grabbing a border is not a click on anything: whatever was
            // armed gets cancelled, or releasing would read as a selection.
            app.mouse.drag.cancel();
            app.mouse.last_click = None;
            app.mouse.resizing = Some(border);
            Some(After::Nothing)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let border = app.mouse.resizing?;
            let axis = match border.dir {
                norte_frontend::layout::Dir::Horizontal => ev.column,
                norte_frontend::layout::Dir::Vertical => ev.row,
            };
            if border.largo == 0 {
                return Some(After::Nothing);
            }
            let inside = f32::from(axis.saturating_sub(border.inicio));
            let frac = inside / f32::from(border.largo);
            app.layout =
                app.layout
                    .drag_border_between(border.slot, border.vecino, frac, border.largo);
            Some(After::Nothing)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // Only swallows the event if there really was a drag: any other
            // `Up` has its own owners further down.
            app.mouse.resizing.take().map(|_| After::Nothing)
        }
        _ => None,
    }
}

/// Cells that have to move before pressing the title counts as a drag: a
/// click on the title still has to focus the panel, and that cannot fail to
/// happen.
const UMBRAL_MOVER: u16 = 2;

/// Is `slot` chrome (status bar, tasks)? It neither moves nor receives.
fn es_cromo(app: &App, slot: norte_frontend::layout::SlotId) -> bool {
    app.layout
        .kind_of(slot)
        .is_some_and(|k| matches!(k.as_str(), "status" | "tasks"))
}

/// The gesture to MOVE a panel (ADR 0138): grabbed by its title row, dragged
/// over another and released on one of its sides or in the center, like in
/// the window. `None` = the event does not belong to the gesture.
///
/// Pressing does not swallow the event: the click on the title still focuses
/// the panel. Only past [`UMBRAL_MOVER`] cells does the gesture claim it, and
/// releasing without crossing it is the usual click.
fn move_gesture(app: &mut App, ev: MouseEvent) -> Option<After> {
    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // A gesture that never saw its `Up` —released outside the
            // terminal— cannot stay armed for the next click.
            app.mouse.moviendo = None;
            let s = app
                .mouse
                .slots
                .iter()
                .find(|s| s.y == ev.row && ev.column >= s.x && ev.column < s.x + s.width)?;
            if es_cromo(app, s.slot) {
                return None;
            }
            app.mouse.moviendo = Some(MoveDrag {
                slot: s.slot,
                x0: ev.column,
                y0: ev.row,
                activo: false,
                destino: None,
            });
            None
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let mut m = app.mouse.moviendo?;
            if !m.activo {
                if ev.column.abs_diff(m.x0) < UMBRAL_MOVER && ev.row.abs_diff(m.y0) < UMBRAL_MOVER {
                    return Some(After::Nothing);
                }
                m.activo = true;
                app.mouse.drag.cancel();
                app.mouse.last_click = None;
            }
            m.destino = app
                .mouse
                .slots
                .iter()
                .find(|s| {
                    ev.column >= s.x
                        && ev.column < s.x + s.width
                        && ev.row >= s.y
                        && ev.row < s.y + s.height
                })
                .filter(|s| s.slot != m.slot && !es_cromo(app, s.slot))
                .map(|s| {
                    let r = norte_frontend::layout::Rect {
                        x: s.x,
                        y: s.y,
                        width: s.width,
                        height: s.height,
                    };
                    let zone = norte_frontend::layout::DropZone::at(ev.column, ev.row, r);
                    (s.slot, zone, zone.part_of(r))
                });
            app.mouse.moviendo = Some(m);
            Some(After::Nothing)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let m = app.mouse.moviendo.take()?;
            if !m.activo {
                return None;
            }
            if let Some((target, zone, _)) = m.destino {
                app.layout_move(m.slot, target, zone);
            }
            Some(After::Nothing)
        }
        _ => None,
    }
}

/// A column drag in flight.
#[derive(Debug, Clone)]
struct ColumnDrag {
    /// The column's id, in the form `persist_column_width` writes and the
    /// window uses (`ColumnId` as text).
    column: String,
    /// The width it was grabbed with.
    inicio: u16,
    /// The cell it was grabbed at. The column's end does not move during the
    /// gesture —the name, which is the part that grows, absorbs the
    /// difference— so the width is `inicio` plus however far the pointer has
    /// moved left from HERE. Measuring against the border instead of the
    /// grab point made the width jump a cell on the first move for whoever
    /// grabbed the cell before the separator.
    agarre: u16,
    /// There was movement. Without it, releasing is not a new width but a
    /// click.
    movido: bool,
}

impl ColumnDrag {
    /// The width with the pointer on cell `col`.
    const fn ancho_en(&self, col: u16) -> u16 {
        self.inicio.saturating_add(self.agarre).saturating_sub(col)
    }
}

/// The column border under `(col, row)`, if there is one.
///
/// Grabs the border that OPENS each column except the name —the separator
/// cell and the one before it, two like the panel borders— and not the one
/// that closes it: the name grows to the right and the last column ends at
/// the panel's frame, which is already the border that divides panels.
///
/// The widths come from `column_widths` with the PAINTED geometry's inner
/// width, the same call as the header: two different layouts would make the
/// border the mouse grabs be another column's.
fn column_border_at(app: &App, col: u16, row: u16) -> Option<ColumnDrag> {
    let geometry = app.mouse.geometry()?;
    for (i, g) in geometry.iter().enumerate() {
        let header = g.first_list_row.checked_sub(1);
        if g.list_rows == 0
            || header != Some(row)
            || col < g.x
            || col >= g.x.saturating_add(g.width)
        {
            continue;
        }
        let pane = app.panes.get(i)?;
        // The SAME catalogue the painting uses: two different answers here
        // would make the border the mouse grabs belong to a different
        // column.
        let anchos = crate::ui::pane_columns(
            &app.columns,
            pane,
            g.width.saturating_sub(2),
            app.attr_catalog(pane.dir().scheme()),
        );
        let mut x = g.x.saturating_add(1);
        for (k, f) in anchos.iter().enumerate() {
            if k > 0 && (col == x || col.saturating_add(1) == x) {
                return Some(ColumnDrag {
                    column: f.id.to_string(),
                    inicio: f.width,
                    agarre: col,
                    movido: false,
                });
            }
            x = x.saturating_add(f.width);
        }
    }
    None
}

/// The drag of a column's border, in the gesture's three stages, like
/// [`resize_gesture`].
///
/// While it lasts, the width is applied IN MEMORY on every move —the header
/// and the rows paint it on the next frame— and only on release is it saved:
/// writing `norte.toml` on every cell the pointer crosses would be the file
/// rewritten forty times per gesture.
fn column_gesture(app: &mut App, ev: MouseEvent) -> Option<After> {
    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let grab = column_border_at(app, ev.column, ev.row)?;
            app.mouse.drag.cancel();
            app.mouse.last_click = None;
            // Pressing the header used to focus the panel before the border
            // was grabbable; being a border now does not take that away.
            enfocar_lo_pulsado(app, ev.column, ev.row);
            app.mouse.columna = Some(grab);
            Some(After::Nothing)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let grab = app.mouse.columna.as_mut()?;
            grab.movido = true;
            let cells = grab.ancho_en(ev.column);
            let column = grab.column.clone();
            app.columns.apply_width(&column, cells);
            Some(After::Nothing)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let grab = app.mouse.columna.take()?;
            if !grab.movido {
                return Some(After::Nothing);
            }
            let cells = app
                .columns
                .apply_width(&grab.column, grab.ancho_en(ev.column));
            app.mouse.ancho_soltado = Some((grab.column, cells));
            Some(After::ColumnWidth)
        }
        _ => None,
    }
}

/// What is handled BEFORE the panels: the menu, the viewer, the extension
/// manager and the lock on the other overlays. `Some` = the event already
/// has an owner and the listings do not see it.
fn por_encima_de_los_paneles(app: &mut App, ev: MouseEvent) -> Option<After> {
    let click = matches!(ev.kind, MouseEventKind::Down(MouseButton::Left));
    // The menu bar is handled BEFORE everything: it is an overlay, so while
    // it is open nothing behind it should receive a click, and its own zones
    // have to be clickable.
    if app.menu.is_some() {
        return Some(if click {
            menu_click(app, ev.column, ev.row)
        } else {
            After::Nothing
        });
    }
    // With the menu CLOSED but the bar pinned, a click on the bar's row opens
    // it. Goes here and not further down because that row does not belong to
    // any panel: without this arm the click would land in the listings' hit
    // test, which returns `None` for it, and nothing happened.
    // Except on a layout button (ADR 0133), which lives on that same row and
    // runs its order through its shortcut's dispatch, like the panel bar.
    if app.menu_bar && ev.row == 0 && click {
        if let Some(cmd) = app
            .mouse
            .panel_zones
            .iter()
            .find(|z| z.row == 0 && ev.column >= z.x0 && ev.column <= z.x1)
            .map(|z| z.command.clone())
        {
            app.mouse.drag.cancel();
            app.mouse.last_click = None;
            app.pending_panel_command = Some(cmd);
            return Some(After::PanelBar);
        }
        return Some(menu_click(app, ev.column, ev.row));
    }
    // The key bar (spec 2026-09-10): a pressed cell is the pressed key, and
    // gets dispatched as such. BEFORE the overlay lock for the same reason
    // as a modal's buttons below: the zones already come empty when there is
    // nothing to press (`key_zones`), and this order does not depend on
    // anyone remembering. Its row does not belong to any panel.
    if click
        && let Some(key) = app
            .mouse
            .key_zones
            .iter()
            .find(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
            .map(|z| z.key)
    {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        app.pending_key = Some(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::F(key),
            KeyModifiers::NONE,
        ));
        return Some(After::SynthKey);
    }
    // A modal's button (spec 2026-09-10), through the same path: the painted
    // chord is synthesized and `on_key` resolves it against the dialog, the
    // same as if the terminal had delivered it. A chord the TUI cannot
    // deliver (none in a preset) is ignored.
    if click
        && let Some(chord) = app
            .mouse
            .modal_zones
            .iter()
            .find(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
            .map(|z| z.chord.clone())
        // The inverse of `paint_chord`, not a `to_lowercase`: `Alt+Shift+C`
        // goes back to `alt+C` and a bare `K` is still `K`, which the keymap
        // tells apart from `k` (review M2).
        && let Ok(chord) = norte_frontend::keymap::parse_chord(
            &norte_frontend::keymap::unpaint_chord(&chord),
        )
        && let Some((mods, code)) = crate::keymap::crossterm_from_chord(chord)
    {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        app.pending_key = Some(crossterm::event::KeyEvent::new(code, mods));
        return Some(After::SynthKey);
    }
    // Help BEFORE the viewer: it paints over everything, viewer included, so
    // whatever is under the pointer is help.
    if raton_en_la_ayuda(app, ev) {
        return Some(After::Nothing);
    }
    if rueda_en_el_visor(app, ev) {
        return Some(After::Nothing);
    }
    // The extension manager BEFORE the overlay lock: it is an overlay, and
    // until now that meant "the mouse does not exist". Its rows and its
    // buttons are painted; they are clickable.
    if app.extensions.is_some() {
        return Some(extensions_mouse(app, ev));
    }
    overlay_open(app).then_some(After::Nothing)
}

/// A crossterm mouse event, with the real clock.
pub fn handle(app: &mut App, ev: MouseEvent) -> After {
    handle_at(app, ev, Instant::now())
}

/// Like [`handle`] with the instant injected: the double click is a time
/// window, and a test that depended on the machine's clock would be a test
/// that fails in CI on a Tuesday.
pub fn handle_at(app: &mut App, ev: MouseEvent, now: Instant) -> After {
    if let Some(after) = por_encima_de_los_paneles(app, ev) {
        return after;
    }
    // #324: the panel bar, for the same reason as the menu bar's — that row
    // does not belong to any panel, so without this arm the click would land
    // in the listings' hit test and nothing happened. Side panels were once
    // born mute to the mouse (#290) and that does not repeat.
    //
    // BELOW `overlay_open`, and that was a review BLOCKER: the bar paints
    // before the overlays, so with help open a click on its title bar —row
    // 1— landed on a button and opened or closed a panel the reader was not
    // looking at. Now the zones also empty out with an overlay in front
    // (`panel_bar_visible`), so this is the second belt on the same
    // invariant: painted and clickable are the same thing.
    if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(cmd) = app
            .mouse
            .panel_zones
            .iter()
            .find(|z| z.row == ev.row && ev.column >= z.x0 && ev.column <= z.x1)
            .map(|z| z.command.clone())
    {
        // And the gesture in flight is released, as the sidebar and the tree
        // do: without this, a click on a row, another on the bar and another
        // on the same row inside the double-click window used to read as a
        // double click, and norte entered a directory the reader had only
        // pointed at.
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        app.pending_panel_command = Some(cmd);
        return After::PanelBar;
    }
    // The detached-session indicator, in the status bar: pressing it asks
    // for the explanation, which is the help page that has it. A discreet
    // indicator is only that if there is an equally discreet way to know
    // what it means. Behind `overlay_open` for the same reason as the panel
    // bar: with help in front, the bar is not clickable.
    if pulsa_indicador_de_sesion(app, ev) {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        return After::SessionHelp;
    }
    // A status-bar item (ADR 0132): runs its command through the SAME
    // dispatch as its shortcut and as a panel-bar button. The notices badge
    // is one of them (`notices` → `layout.log`).
    if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(cmd) = elemento_de_estado_en(app, ev)
    {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        app.pending_panel_command = Some(cmd.to_owned());
        return After::PanelBar;
    }
    // Dragging a BORDER comes before everything about the listing, in all
    // three stages of the gesture: while it lasts, the pointer leaves the
    // border and does not thereby stop dragging it.
    if let Some(after) = resize_gesture(app, ev) {
        return after;
    }
    // And a COLUMN border's, for the same reason: the header is chrome to
    // the listing, and the pointer leaves the border's cell on the first
    // move.
    if let Some(after) = column_gesture(app, ev) {
        return after;
    }
    // MOVING a panel by its title (ADR 0138), after the borders: a stacked
    // panel's top border is also its title, and grabbing it is resizing, as
    // it always was.
    if let Some(after) = move_gesture(app, ev) {
        return after;
    }
    if let Some(after) = pulsar_panel(app, ev) {
        return after;
    }
    // Tab bars are handled BEFORE: their cells are chrome to the listing's
    // hit test, so a click there would land on "this panel, no row" and the
    // button would do nothing.
    if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(z) = tab_zone_at(app, ev.column, ev.row)
    {
        apply_tab_zone(app, z);
        return After::Nothing;
    }
    // The places sidebar, for the same reason: its cells belong to no
    // listing, so a click there would land on "outside the panes" and do
    // nothing — the panel painted and could not be touched (#226). The drag
    // is cancelled: nothing gets dragged from here.
    if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(z) = place_zone_at(app, ev.column, ev.row)
    {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        return match app.places_click(z.index) {
            crate::app::PlacesClick::Focused => After::Nothing,
            crate::app::PlacesClick::Folded => After::PlacesFolded,
            crate::app::PlacesClick::Activate => After::PlacesActivate,
        };
    }
    // And the tree, for the same reason: its cells are not part of any
    // listing either, so the click would land on "outside the panes" and the
    // panel painted with no way to touch it (#136). Pressing the MARK folds
    // or unfolds; the rest of the row selects, and the second press
    // activates.
    if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(z) = tree_zone_at(app, ev.column, ev.row)
    {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        let spot = if ev.column == z.mark_x {
            crate::app::TreeSpot::Mark
        } else {
            crate::app::TreeSpot::Row
        };
        return match app.tree_click(z.index, spot) {
            crate::app::TreeClick::Focused => After::Nothing,
            crate::app::TreeClick::Activate => After::TreeActivate,
        };
    }
    let hit = hit_test(app, ev.column, ev.row);
    let m = mods(ev.modifiers);
    app.mouse.last_mods = m;
    match ev.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let abajo = matches!(ev.kind, MouseEventKind::ScrollDown);
            // The DOCKED viewer first: its slot is not a listing, so the
            // hit test returns `None` and the wheel was getting lost. A
            // panel that paints and cannot be scrolled is the same failure
            // as a panel that cannot be clicked (#226, #290).
            if !rueda_en_preview(app, ev.column, ev.row, abajo) {
                scroll(app, hit, abajo);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => return press(app, hit, m, now),
        MouseEventKind::Drag(MouseButton::Left) => {
            // A motion outside every row is NOT reported: passing over the
            // header mid-sweep must not cancel it (that is what
            // `Drag::motion`'s contract says).
            if let Some(spot) = spot(hit) {
                let fx = app.mouse.drag.motion(spot);
                apply(app, &fx);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let fx = app.mouse.drag.release(spot(hit), m);
            apply(app, &fx);
            for pane in &mut app.panes {
                pane.end_sweep();
            }
        }
        // Right button: NOTHING yet. The context menu is task 4 of the plan;
        // inventing a second menu here would guarantee the two frontends end
        // up with different menus.
        _ => {}
    }
    After::Nothing
}

/// Pressing a panel: gives it the KEYBOARD and, if it landed on a plugin
/// panel's zone, runs its command (phase 3).
///
/// Focus comes BEFORE every specialized path in `handle_at`: who owns the
/// keyboard is decided by the slot under the pointer, not by what each path
/// knows to do afterward with the click. Placed in each path instead, the
/// panel none of them handle —the docked viewer, the tree, the sheet— would
/// be left unable to receive it.
///
/// The zone goes through the SAME path as a panel-bar button, which is the
/// same as its keyboard shortcut: a plugin does not run anything on its own
/// —it names a catalogue command and norte dispatches it— so a click here
/// cannot do anything a key could not. Policy stays intact (hard rule 9).
fn pulsar_panel(app: &mut App, ev: MouseEvent) -> Option<After> {
    if !matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
        return None;
    }
    enfocar_lo_pulsado(app, ev.column, ev.row);
    // The disk map first: its rectangle names a CHILD, not a command, so it
    // cannot go through the plugin zones' path. Pressing one both chooses it
    // AND enters it, which is the whole gesture — in a map, pointing and
    // opening are the same act, like a double click on a listing.
    if let Some(arg) = hijo_del_mapa_en(app, ev.column, ev.row) {
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        if let Ok(seg) = norte_proto::Segment::parse_wire(&arg) {
            let slot = app.disk_map_slot();
            let is_dir = slot
                .and_then(|s| app.panes.disk_map(s))
                .and_then(|m| {
                    m.informe()
                        .children
                        .iter()
                        .find(|c| c.name == seg)
                        .map(|c| c.kind == norte_proto::EntryKind::Dir)
                })
                .unwrap_or(false);
            if let Some(s) = slot
                && let Some(m) = app.panes.disk_map_mut(s)
            {
                m.elegir(&seg);
            }
            // Enter ONLY a directory: the map shows both kinds, and
            // "entering" a file is not navigating.
            if is_dir {
                app.pending_disk_map_enter = Some(seg);
            }
        }
        return Some(After::PanelBar);
    }
    let cmd = zona_de_panel_en(app, ev.column, ev.row)?;
    // And the gesture in flight is released, like the bar: without this, a
    // click on a row and another on the zone inside the double-click window
    // used to read as one.
    app.mouse.drag.cancel();
    app.mouse.last_click = None;
    app.pending_panel_command = Some(cmd);
    Some(After::PanelBar)
}

/// The command of the plugin panel's clickable zone at `(col, row)`, if
/// there is one (phase 3).
///
/// The frame's coordinates are INSIDE the border: the guest describes its
/// content and does not know where its slot landed, so the arithmetic
/// —subtracting the slot's origin and the border— is done by whoever
/// painted it, which is this side.
///
/// The `Hit`'s argument does not travel yet: the terminal's command
/// catalogue takes no parameters, so a zone just runs its command and
/// nothing more. When a command with an operand exists, it enters through
/// here.
fn zona_de_panel_en(app: &App, col: u16, row: u16) -> Option<String> {
    let slot = app.panel_slot()?;
    let rect = app.mouse.slots.iter().find(|s| s.slot == slot)?;
    if !rect.contains(col, row) {
        return None;
    }
    // INSIDE the border on all four sides. `checked_sub` only guards the
    // top-left: without bounding the other side, a click on the right border
    // gave the column right after the last inside one, and a zone spanning
    // the whole width —the normal case for a clickable label— would fire on
    // pressing the frame itself, for instance while dragging it.
    let inside_x = col
        .checked_sub(rect.x.saturating_add(1))
        .filter(|x| *x < rect.width.saturating_sub(2))?;
    let inside_y = row
        .checked_sub(rect.y.saturating_add(1))
        .filter(|y| *y < rect.height.saturating_sub(2))?;
    let frame = app.paneles.get(slot)?.frame.as_ref()?;
    frame
        .hit_at(inside_y, inside_x)
        .map(|h| h.command.clone())
        // The PLUGIN chooses the command, same as the label, and nothing
        // ties them together: a zone labeled "Update" could name
        // `pane.unpack`. The filter keeps the click within the same scope as
        // a focused panel's key (`ALLOW_PANEL`), which is what this side
        // promises.
        .filter(|c| norte_frontend::frame::zona_puede(c))
}

/// The NAME of the child whose rectangle is at `(col, row)` of the disk map.
///
/// Twin of [`zona_de_panel_en`] with two differences that matter, both
/// because the frame is OURS and not a third party's:
///
/// 1. **Returns the `arg`, not the command.** In a plugin panel the argument
///    is discarded —the terminal's catalogue takes no parameters— and the
///    zone only runs its command. Here the argument IS the answer: which
///    child gets entered. Returned in its WIRE form, which is the reversible
///    one; what gets painted is masked and names no file.
/// 2. **Does not go through `zona_puede`.** That filter exists because in a
///    plugin panel a third party chooses the label and the command and
///    nothing ties them together. The rectangles here are laid out by
///    `squarify`, so filtering them would be guarding against ourselves —
///    and would leave the map with no gesture at all.
fn hijo_del_mapa_en(app: &App, col: u16, row: u16) -> Option<String> {
    let slot = app.disk_map_slot()?;
    let rect = app.mouse.slots.iter().find(|s| s.slot == slot)?;
    if !rect.contains(col, row) {
        return None;
    }
    // INSIDE the border on all four sides, with the same arithmetic —and the
    // same reason— as the plugin panel.
    let inside_x = col
        .checked_sub(rect.x.saturating_add(1))
        .filter(|x| *x < rect.width.saturating_sub(2))?;
    let inside_y = row
        .checked_sub(rect.y.saturating_add(1))
        .filter(|y| *y < rect.height.saturating_sub(2))?;
    let map = app.panes.disk_map(slot)?;
    let frame = norte_frontend::treemap::squarify(
        &map.informe().children,
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    );
    frame.hit_at(inside_y, inside_x).and_then(|h| h.arg.clone())
}

/// Gives the keyboard to the panel under `(col, row)`.
///
/// Only with the LEFT button down: the wheel moves the listing under the
/// pointer without stealing anyone's focus (see [`scroll`]), and a drag that
/// crosses the neighboring panel must not carry the keyboard off mid-task.
///
/// A slot that does not take keys changes nothing, and a click outside every
/// slot —there is none: the layout covers the whole screen— does not either.
fn enfocar_lo_pulsado(app: &mut App, col: u16, row: u16) {
    let Some(slot) = app
        .mouse
        .slots
        .iter()
        .find(|s| s.contains(col, row))
        .map(|s| s.slot)
    else {
        return;
    };
    app.focus_slot(slot);
}

/// The wheel over the full-screen VIEWER: scrolls it and says yes.
///
/// Handled BEFORE the overlay cut-off because the viewer is one of them, so
/// until now scrolling over an open file did absolutely nothing. It is the
/// most obvious gesture a viewer has, and the only thing underneath is a
/// listing that is not visible — scrolling THAT would have been worse.
///
/// Both AXES, as in the window: the viewer does not wrap, so width needs it
/// as much as height. `shift+wheel` is the usual gesture for the horizontal
/// axis, and some terminals also send their own horizontal wheel.
///
/// `true` also when the viewer is open and the event is not a wheel: with a
/// file in front, no other mouse gesture has an owner.
fn rueda_en_el_visor(app: &mut App, ev: MouseEvent) -> bool {
    let shift = mods(ev.modifiers).shift;
    let Some(v) = app.viewer.as_mut() else {
        return false;
    };
    match ev.kind {
        MouseEventKind::ScrollUp if shift => v.scroll_left(WHEEL_ROWS),
        MouseEventKind::ScrollDown if shift => v.scroll_right(WHEEL_ROWS),
        MouseEventKind::ScrollUp => v.scroll_up(WHEEL_ROWS),
        MouseEventKind::ScrollDown => v.scroll_down(WHEEL_ROWS),
        MouseEventKind::ScrollLeft => v.scroll_left(WHEEL_ROWS),
        MouseEventKind::ScrollRight => v.scroll_right(WHEEL_ROWS),
        _ => {}
    }
    true
}

/// The mouse with help open: the wheel scrolls whatever is under the
/// pointer —the sidebar flips pages, the body scrolls down— and a click
/// chooses a page or an action, same as in the window.
///
/// Until now help was just another overlay for [`overlay_open`]'s lock, so
/// with it open the mouse did not exist: not even the wheel scrolled through
/// the text. `true` whenever help is open, because with it in front no other
/// gesture has an owner.
///
/// Clicking an action SELECTS it rather than running it: running a
/// file-touching command from a click on text that is being read is too easy
/// to do by accident. `Enter` runs it, same as with the keyboard.
fn raton_en_la_ayuda(app: &mut App, ev: MouseEvent) -> bool {
    if app.help.is_none() {
        return false;
    }
    let zones = app.mouse.help_zones.clone();
    let (Some(help), Some(z)) = (app.help.as_mut(), zones) else {
        return true;
    };
    let inside = |r: ratatui::layout::Rect| {
        ev.column >= r.x
            && ev.column < r.x.saturating_add(r.width)
            && ev.row >= r.y
            && ev.row < r.y.saturating_add(r.height)
    };
    match ev.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let down = matches!(ev.kind, MouseEventKind::ScrollDown);
            if inside(z.sidebar) {
                if down {
                    help.state.down();
                } else {
                    help.state.up();
                }
            } else if inside(z.body) {
                let rows = isize::try_from(WHEEL_ROWS).unwrap_or(1);
                help.state.scroll_body(if down { rows } else { -rows });
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if inside(z.sidebar) {
                if let Some(&(_, row)) = z.rows.iter().find(|(y, _)| *y == ev.row) {
                    help.state.click_row(row);
                }
            } else if inside(z.body) {
                let line = help.state.body_scroll() + usize::from(ev.row - z.body.y);
                if let Some(i) = help.body().1.iter().position(|&l| l == line) {
                    help.state.click_action(i);
                }
            }
        }
        _ => {}
    }
    true
}

/// The wheel over a DOCKED viewer slot: scrolls it and says yes.
///
/// `false` when there is none under the pointer, and then the wheel goes on
/// its normal way to the listing.
fn rueda_en_preview(app: &mut App, col: u16, row: u16, abajo: bool) -> bool {
    let Some(slot) = app
        .mouse
        .slots
        .iter()
        .find(|s| s.contains(col, row))
        .map(|s| s.slot)
    else {
        return false;
    };
    let Some(v) = app
        .panes
        .preview_mut(slot)
        .and_then(crate::preview::Preview::viewer_mut)
    else {
        return false;
    };
    if abajo {
        v.scroll_down(WHEEL_ROWS);
    } else {
        v.scroll_up(WHEEL_ROWS);
    }
    true
}

/// The `Spot` of a hit that landed on a real row.
fn spot(hit: Option<Hit>) -> Option<Spot> {
    let hit = hit?;
    Some(Spot::new(hit.pane, hit.index?))
}

/// Wheel: scrolls the listing UNDER THE POINTER, focused or not — looking at
/// one thing and scrolling over another is the normal gesture with two
/// panels, and stealing the active pane's focus by passing the mouse over it
/// would be worse than not scrolling at all.
///
/// "Scrolling" here means moving that pane's cursor: the TUI keeps no
/// independent scroll (see `ui::list_offset`), the painted window comes from
/// the cursor. With a quick-search filter active it moves the FILTER's
/// selection, which is what is painted.
fn scroll(app: &mut App, hit: Option<Hit>, down: bool) {
    let Some(hit) = hit else { return };
    let pane = &mut app.panes[hit.pane];
    if pane.quick().is_some() {
        for _ in 0..WHEEL_ROWS {
            if down {
                pane.quick_down();
            } else {
                pane.quick_up();
            }
        }
    } else if down {
        pane.move_down(WHEEL_ROWS);
    } else {
        pane.move_up(WHEEL_ROWS);
    }
}

/// Left button down.
fn press(app: &mut App, hit: Option<Hit>, m: Mods, now: Instant) -> After {
    let Some(hit) = hit else {
        // Outside the panes (tasks panel, status bar): the armed gesture
        // dies; no dragging from there.
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        return After::Nothing;
    };
    let Some(index) = hit.index else {
        // Pane chrome (borders, header, gap below the last entry): focuses
        // that pane and that is it. Still a useful action —the title with
        // the path is a big target— and touches neither cursor nor marks.
        app.mouse.drag.cancel();
        app.mouse.last_click = None;
        app.set_focus(hit.pane);
        return After::Nothing;
    };
    let at = Spot::new(hit.pane, index);
    // Double click BEFORE the gesture machine: entering a directory is not a
    // marking gesture, and with ctrl/shift held what the user is asking for
    // is marking, not navigating.
    if m == Mods::NONE
        && app
            .mouse
            .last_click
            .is_some_and(|(when, prev)| prev == at && now.duration_since(when) <= DOUBLE_CLICK)
    {
        app.mouse.last_click = None;
        app.mouse.drag.cancel();
        app.set_focus(hit.pane);
        app.panes[hit.pane].set_cursor(index);
        return After::Enter;
    }
    // Only a CLEAN click can be the first half of a double. A ctrl+click is
    // a discrete, complete gesture; reading it as a first half would make
    // marking a row and immediately pressing it again —to drag it, which is
    // exactly what happens after marking— enter the directory instead of
    // starting the drag.
    app.mouse.last_click = (m == Mods::NONE).then_some((now, at));
    let marked = app.panes[hit.pane]
        .entries()
        .get(index)
        .is_some_and(|e| app.panes[hit.pane].is_marked(e));
    // A shift+click's anchor is what the user SEES highlighted, not the real
    // cursor: under a quick search in filter mode the highlight comes from
    // the filter's selection and the real cursor can be anywhere in the full
    // listing, so taking it as the anchor would mark a range starting on a
    // row nobody is looking at.
    let cursor = painted_anchor(&app.panes[hit.pane]);
    let fx = app.mouse.drag.press(Press {
        at,
        marked,
        cursor,
        mods: m,
    });
    apply(app, &fx);
    // A CLEAN click closes the pressed pane's quick search, and only that
    // one.
    //
    // The order matters, and so does the exception. With the filter on, a
    // SUBSET is painted: the highlight comes from the filter's selection, so
    // moving the real cursor would not move anything visible and the next
    // operation would act on the filter's row instead of the pressed one.
    // Closing it fixes that — the index is ABSOLUTE and survives the whole
    // listing coming back.
    //
    // But closing it BEFORE marking would be much worse than not closing it:
    // `mark_range`/`set_mark`/`apply_sweep` consult the filter so as not to
    // reach what it hides (see their rustdoc), and with no filter a
    // shift+click would mark EVERY index in between — hidden ones included —
    // which is exactly the silent widening of the next copy or delete those
    // guards exist to prevent. That is why it comes AFTER `apply`, and why
    // only for the gesture that marks nothing: a clean press arms the sweep
    // but does not mark (contract of `Drag::press`), and by the time the
    // first motion arrives the whole listing will already have repainted.
    if m == Mods::NONE {
        app.panes[hit.pane].quick_cancel();
    }
    After::Nothing
}

/// The ABSOLUTE index of a pane's HIGHLIGHTED row: the quick search's
/// selection while it filters (which is what gets painted,
/// `ui::painted_len_and_selection`), the real cursor otherwise.
fn painted_anchor(pane: &crate::app::Pane) -> usize {
    pane.quick()
        .and_then(crate::nav::QuickSearch::selected_entry_index)
        .unwrap_or_else(|| pane.cursor())
}

/// Applies the effects the shared machine returns. Each one maps onto ONE
/// operation that already existed on `PaneState`: this module invents none.
fn apply(app: &mut App, effects: &[Effect]) {
    for effect in effects {
        match *effect {
            // Never touches the quick search: marking with the filter on is
            // what keeps marking from reaching what the filter hides. `press`
            // is the one that closes it, and only for a clean click, AFTER
            // applying the effects (see its comment).
            Effect::MoveCursor { pane, index } => {
                app.set_focus(pane);
                app.panes[pane].set_cursor(index);
            }
            Effect::SetMark {
                pane,
                index,
                marked,
            } => app.panes[pane].set_mark(index, marked),
            Effect::MarkRange { pane, from, to } => {
                app.panes[pane].mark_range(from, to);
            }
            Effect::BeginSweep { pane } => app.panes[pane].begin_sweep(),
            Effect::SweepRange { pane, from, to } => {
                app.panes[pane].apply_sweep(from, to);
            }
            // The sweep crossed into the other panel and the machine
            // promoted it to a transfer: reverts whatever it had marked. A
            // promotion changes what the gesture DOES, not what is selected.
            Effect::RevertSweep { pane } => app.panes[pane].revert_sweep(),
            // The drop. Opens EXACTLY the same modal as the copy or move key
            // (`App::open_transfer`, single source): same confirmation, same
            // collision dialog, same journal entry, same undo, same policy
            // gate. A drop is a mutation and has no quieter path than the
            // rest.
            //
            // No overlay guard needed: `handle_at` already returns before
            // touching anything if one is in front, and `after_frame`
            // expires the gesture as soon as one appears.
            Effect::Transfer {
                from_pane,
                to_pane,
                move_files,
                promoted,
            } => {
                let kind = if move_files {
                    TransferKind::Move
                } else {
                    TransferKind::Copy
                };
                // The batch is consumed from the FOCUSED pane
                // (`consume_marks` after sending), and a drop's source is the
                // pane where the button went down. Focus is ALREADY there
                // —the press put it there— but stating it turns an
                // accidental invariant into a written one: if a motion over
                // the other panel ever moved focus, the marks would be
                // consumed from the wrong pane silently.
                app.set_focus(from_pane);
                app.open_transfer(kind, from_pane, to_pane, promoted);
            }
        }
    }
}

/// The terminal's mouse capture, with its state.
///
/// A type and not a loose `bool` because the enable and disable sequences
/// have to stay paired with what the terminal believes: asking for enable
/// twice is harmless, but LEAVING it on when quitting (or when handing the
/// terminal to another program) leaves the user with an emulator spitting
/// escape garbage the moment they move the mouse.
#[derive(Debug, Default)]
pub struct Capture {
    active: bool,
}

impl Capture {
    /// Capture off (the state of a freshly taken terminal).
    #[must_use]
    pub const fn new() -> Self {
        Self { active: false }
    }

    /// Is it currently requested?
    #[must_use]
    pub const fn active(&self) -> bool {
        self.active
    }

    /// Requests (or withdraws) capture if needed. Idempotent: what lets
    /// `[ui] mouse`'s hot reload call this on every reload without sending
    /// the terminal sequences that change nothing.
    ///
    /// # Errors
    /// Whatever writing to `out` returns.
    pub fn set(&mut self, want: bool, out: &mut impl Write) -> std::io::Result<()> {
        if want == self.active {
            return Ok(());
        }
        write_capture(want, out)?;
        self.active = want;
        Ok(())
    }
}

/// The mouse modes that get requested, and `crossterm::event::EnableMouseCapture`
/// is NOT used to request them.
///
/// That command adds `?1003h` (*any-event tracking*): the terminal reports an
/// event for EVERY cell the pointer crosses, with all buttons released. This
/// module discards those events ([`handle_at`], the `_` arm), but by then
/// they have already woken the run loop, which repaints the whole frame on
/// every turn — and the frame costs whatever the listing costs (`draw_pane`
/// builds one `ListItem` per entry, not per visible row). Moving the mouse
/// over the window without pressing anything turns into hundreds of
/// repaints: measured, ~1 ms per frame with 100 entries and ~38 ms with
/// 20,000. And whoever never touches the mouse would pay for it too.
///
/// So only the three modes this module CONSUMES are requested: normal
/// (`?1000`, press and release), button-event (`?1002`, motion ONLY with a
/// button held — that is where the `Drag`s come from) and SGR (`?1006`,
/// coordinates past column 223; without it a wide terminal reports garbage).
/// `?1015` (rxvt mode) is left out because `?1006` replaces it and crossterm
/// understands both.
#[cfg(not(windows))]
const CAPTURE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";

/// The same modes, withdrawn in reverse order (see [`CAPTURE_ON`]).
#[cfg(not(windows))]
const CAPTURE_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";

/// Writes the capture request (or withdrawal).
///
/// On Windows this still goes through crossterm: there `EnableMouseCapture`
/// NEVER sends ANSI (its `is_ansi_code_supported` always returns `false`),
/// but a console call instead — writing escapes by hand would be a silent
/// no-op on a legacy console.
fn write_capture(want: bool, out: &mut impl Write) -> std::io::Result<()> {
    #[cfg(not(windows))]
    {
        out.write_all(if want { CAPTURE_ON } else { CAPTURE_OFF }.as_bytes())?;
        out.flush()
    }
    #[cfg(windows)]
    {
        if want {
            crossterm::execute!(out, EnableMouseCapture)
        } else {
            crossterm::execute!(out, DisableMouseCapture)
        }
    }
}

/// Releases capture before handing the terminal to an external program
/// (`run_opener`), and returns whether it was on so it can be restored.
///
/// Without this the launched program inherits a terminal in mouse mode it
/// never asked for: `less` or an editor would receive every movement's
/// sequences as if they were keys, and on exit the user would have a
/// terminal nobody is listening to any more.
///
/// # Errors
/// Whatever writing to `out` returns.
pub fn release_for_suspend(cap: &mut Capture, out: &mut impl Write) -> std::io::Result<bool> {
    let was = cap.active();
    cap.set(false, out)?;
    Ok(was)
}

/// Restores capture on return from the external program, if it was on.
///
/// # Errors
/// La de escribir en `out`.
pub fn restore_after_suspend(
    cap: &mut Capture,
    was: bool,
    out: &mut impl Write,
) -> std::io::Result<()> {
    cap.set(was, out)
}

/// Dispatches by name the command a CLICK chose, through the same path as
/// its key.
///
/// One for the menu and for the panel bar (#324): both do the same thing
/// from a different origin, and having it twice is how the menu and the bar
/// end up opening a panel two ways that drift apart the moment one grows a
/// detail. It is ADR 0077's lesson applied inside a single frontend.
#[expect(clippy::too_many_arguments, reason = "loop wiring, not an API")]
/// Dispatches a NAMED command the mouse requested, and finishes off
/// everything that command left pending.
///
/// "Everything" is three things and all three go HERE, not in each mouse
/// arm: the `cd`'s outcome, the live search's harvest and **the opener the
/// command left armed**. The last one was missing in the double-click arm,
/// so a double click on a `.jpg` ran `nav.enter`, which resolved the desktop
/// program into `pending_open`… and nobody launched it: the gesture did
/// absolutely nothing, and did not say why. [`on_mouse`]'s rustdoc promised
/// exactly that from the start ("launch the opener a double click left
/// resolved"), and the wiring was not there.
///
/// Sharing the exit path is the fix, not adding the missing line: two arms
/// that finish off by hand are two places to forget the third.
async fn despachar_clic(
    app: &mut crate::app::App,
    backend: &norte_core::backend::Backend,
    events: &mut crate::console::Console<'_>,
    capture: &mut Capture,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    lang: norte_i18n::Lang,
    quick_mode: crate::nav::Mode,
    confirm_quit: crate::config::ConfirmQuit,
    cfg: &crate::config::LoadedConfig,
    work: &mut crate::jobs::InFlight,
    id: &str,
) {
    let Some(cmd) = Command::parse(id) else {
        return;
    };
    let outcome = dispatch(
        app,
        backend,
        events,
        help_lines,
        lang,
        quick_mode,
        confirm_quit,
        cfg,
        cmd,
    )
    .await;
    apply_cd(
        &app.panes,
        &mut work.fill,
        &mut work.decorate,
        &mut work.probed,
        &mut work.search,
        outcome,
    );
    reap_search_run(app, &mut work.search);
    crate::event_loop::launch_pending(app, events, capture).await;
}

/// Writes to `[ui.columns]` the width the last drag left.
///
/// Off the loop (`spawn_blocking`, rule 2) and to the active PROFILE if there
/// is one, like the column picker: writing it to the user layer with a
/// profile that also fixes the width would leave it saved and without
/// effect. Only failure gets reported; a width that saves fine is already
/// being seen on screen.
async fn guardar_ancho_de_columna(app: &mut crate::app::App) {
    let Some((column, cells)) = app.mouse.take_column_width() else {
        return;
    };
    let Some(dir) = app.config_write_dir() else {
        app.message = Some(norte_i18n::t("msg-settings-no-config-dir"));
        return;
    };
    let res = tokio::task::spawn_blocking(move || {
        crate::config::persist_column_width(&dir, &column, cells)
    })
    .await;
    match res {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            app.message = Some(norte_i18n::ta(
                "msg-settings-save-failed",
                &[("error", &crate::app::io_error_category(&e))],
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, "the task saving the column width did not finish");
            app.message = Some(norte_i18n::t("msg-settings-save-crashed"));
        }
    }
}

/// Applies a mouse event and finishes off whatever the gesture leaves
/// pending.
///
/// The gesture's semantics —what marks, what sweeps, what transfers— live in
/// `norte-frontend` (rule 7) and [`handle`] resolves them; what is left here
/// is what only the loop can do: dispatching a menu item's command,
/// navigating, or launching the opener a double click left resolved.
///
/// Twin of [`crate::keys::on_key`]: a gesture is ANOTHER kind of input, and
/// takes exactly the same paths as the equivalent key — which is what keeps
/// the mouse and the keyboard from diverging.
#[expect(clippy::too_many_arguments, reason = "loop wiring, not an API")]
pub async fn on_mouse(
    app: &mut crate::app::App,
    backend: &norte_core::backend::Backend,
    capture: &mut Capture,
    // The terminal travels inside (`events.terminal()`): a click can also
    // open a long navigation, and the terminal has a single owner.
    events: &mut crate::console::Console<'_>,
    resolver: &mut crate::keymap::Resolver,
    help_lines: &mut Vec<ratatui::text::Line<'static>>,
    lang: norte_i18n::Lang,
    quick_mode: crate::nav::Mode,
    confirm_quit: crate::config::ConfirmQuit,
    cfg: &crate::config::LoadedConfig,
    work: &mut crate::jobs::InFlight,
    me: crossterm::event::MouseEvent,
) {
    match self::handle(app, me) {
        // `SynthKey`: the synthesized key is dispatched by the loop through
        // `on_key`, right after this gesture — the three resolvers are not
        // here. Nothing to do, same as `Nothing`.
        self::After::Nothing | self::After::SynthKey => {}
        // Releasing a column's border: the width is already in memory and
        // painted; only saving it with the SAME function the window uses is
        // left.
        self::After::ColumnWidth => guardar_ancho_de_columna(app).await,
        // The detached-session indicator: the explanation is in help, and it
        // opens through the SAME constructor as `F1` over a palette row — a
        // page in hand, not a context to resolve.
        self::After::SessionHelp => {
            crate::overlays::open_help_topic(app, lang, help_lines, SESSION_HELP_TOPIC);
        }
        // A button on the extension manager goes through the SAME dispatch
        // as its key: enabling, approving, uninstalling and opening settings
        // are the manager's decisions, and the mouse only points at them.
        self::After::Extension(cmd) => {
            crate::screens::on_extensions_click(app, backend, lang, help_lines, cmd).await;
        }
        // #324: a panel-bar button goes through the SAME dispatch as its
        // shortcut. Two paths to open the same panel drift apart the moment
        // one of the two grows a detail — it is ADR 0077's lesson applied
        // inside a single frontend.
        self::After::PanelBar => {
            if let Some(id) = app.pending_panel_command.take() {
                despachar_clic(
                    app,
                    backend,
                    events,
                    capture,
                    help_lines,
                    lang,
                    quick_mode,
                    confirm_quit,
                    cfg,
                    work,
                    &id,
                )
                .await;
            }
        }
        // Pressing a menu item: the mouse already
        // left the cursor on it; running it is
        // async and needs the backend, so it is
        // finished off here — the SAME path as `Enter`,
        // which is what keeps a menu and a key from
        // being able to diverge.
        self::After::MenuAccept => {
            if let Some(id) = app.take_menu_choice() {
                despachar_clic(
                    app,
                    backend,
                    events,
                    capture,
                    help_lines,
                    lang,
                    quick_mode,
                    confirm_quit,
                    cfg,
                    work,
                    &id,
                )
                .await;
            }
        }
        // Double click = `nav.enter`, through the SAME `dispatch`
        // as the key: same cd, same paginated fill,
        // same live-search harvest. A second
        // path to enter a directory would be a
        // second place to fix every cd bug.
        //
        // And through the same finish as the menu (`despachar_clic`),
        // which is what was missing: over a FILE, `nav.enter`
        // resolves the desktop program and leaves it
        // armed, so without launching it a double click on a
        // `.jpg` did nothing.
        self::After::Enter => {
            // K3a: a gesture is ANOTHER kind of input. Whatever
            // sequence the reader was typing is abandoned along with
            // its panel — the mouse does not complete it, and leaving
            // it armed would make the next key fire a
            // command requested before changing directory.
            app.abandon_pending(resolver);
            despachar_clic(
                app,
                backend,
                events,
                capture,
                help_lines,
                lang,
                quick_mode,
                confirm_quit,
                cfg,
                work,
                "nav.enter",
            )
            .await;
        }
        // #226: the sidebar with the mouse takes the SAME
        // paths as its keyboard. Unfolding the drives
        // is the moment to request them again —and folding,
        // not to request them— so the mouse cannot be
        // a fourth refresh trigger: it is this one.
        self::After::PlacesFolded => drain_places_drives(app, backend).await,
        // And activating a row takes the listing through the
        // usual `cd` flow, same as `Enter`
        // inside the sidebar.
        self::After::PlacesActivate => {
            app.abandon_pending(resolver);
            if let Some(path) = app.places_activate() {
                let pane = app.focus();
                let outcome = cd_in(app, backend, events, pane, path, Trail::Record).await;
                apply_cd(
                    &app.panes,
                    &mut work.fill,
                    &mut work.decorate,
                    &mut work.probed,
                    &mut work.search,
                    outcome,
                );
            }
        }
        // And the tree branch, through the SAME `cd` flow as its `Enter`
        // (#136): the tree unfolds the branch and sends the focused listing
        // there.
        self::After::TreeActivate => {
            app.abandon_pending(resolver);
            if let Some(path) = app.tree_activate() {
                let pane = app.focus();
                let outcome = cd_in(app, backend, events, pane, path, Trail::Record).await;
                apply_cd(
                    &app.panes,
                    &mut work.fill,
                    &mut work.decorate,
                    &mut work.probed,
                    &mut work.search,
                    outcome,
                );
            }
        }
    }
}
