//! The four side navigation panels and the popup that precedes them: tree,
//! processes, the places sidebar, and the history/hotlist/volumes popup.
//!
//! All four have the same shape — they resolve the key against the keymap's
//! `dialog` context, filter it by their overlay's ALLOWLIST, and return a
//! [`Cd`] because confirming means navigating by the normal `cd` path — and
//! all four used to live in the `ntc` binary's root, a crate DISTINCT from
//! this lib.
//!
//! `side_nav` and not `nav` because [`crate::nav`] already exists and is
//! something else (the popup's model); this is the one that reads its keys.
//!
//! The two rustdoc blocks of [`on_places_key`] and [`on_nav_popup_key`] were
//! STACKED on top of `on_tree_key` in `main.rs`, three doc comments in a row
//! in front of a single function: an earlier move left the other two's
//! documentation behind. Here each block goes back to its own function,
//! without a word changed.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::Error;

use crate::app::{
    ALLOW_NAV_POPUP, ALLOW_PLACES, App, NavPopup, NavPopupKind, PickerAction, Trail,
    detail_for_bar, error_category, error_message, io_error_category, volume_items,
};
use crate::config;
use crate::keymap::{Resolution, Resolver, chord_from_crossterm};
use crate::navigate::{Cd, cd, cd_in};

/// Fetches `Backend::volumes` for `pane`'s side and opens/refreshes the
/// volumes popup (design §D). Opening from `pane.select-drive*` and
/// re-opening after the in-popup unfiltered toggle are the SAME operation —
/// a fresh frozen snapshot for the requested mode — so both call this. A
/// fetch error surfaces as the usual status message and leaves whatever
/// popup was already open alone, same pattern as `Command::AppExtensions`
/// on a failed `plugins_list`.
pub async fn open_drive_popup(app: &mut App, backend: &Backend, pane: usize, include_pseudo: bool) {
    match backend.volumes(include_pseudo).await {
        Ok(volumes) => {
            let enc = app.panes[pane].name_encoding();
            let items = volume_items(&volumes, enc);
            app.open_volumes_popup(pane, include_pseudo, items);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// Tree keys (#136): same layout and same allowlist as the sidebar.
///
/// `⏎` over a branch expands or collapses it; `dialog.confirm` with the
/// branch already open SENDS the listing there, which is what a tree is
/// opened for. `Esc` and `Tab` release the keyboard and leave the panel
/// open — closing it is `pane.tree`, the same SECOND keystroke as the
/// sidebar: opening either of the two already gives them the keyboard.
pub async fn on_tree_key(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Cd {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return Cd::Cancelled;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled;
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    if !ALLOW_PLACES.contains(&cmd.as_str()) {
        return Cd::Cancelled;
    }
    // The application's chrome comes before anything else: it is not this
    // panel's, and that is why this panel does not decide it
    // (`App::panel_chrome_command`, one for all three).
    if app.panel_chrome_command(&cmd) {
        return Cd::Cancelled;
    }
    match cmd.as_str() {
        "dialog.up" => {
            if let Some(t) = app.tree_mut() {
                t.up();
            }
        }
        "dialog.down" => {
            if let Some(t) = app.tree_mut() {
                t.down();
            }
        }
        "dialog.toggle-enabled" => {
            if let Some(t) = app.tree_mut() {
                t.toggle();
            }
        }
        // `Esc` releases the keyboard, and so does `Tab`: the same rule as
        // the sidebar and the processes panel. Neither one CLOSES the
        // tree — that is `pane.tree` — and opening a side column cannot
        // cost you the key that switches panes throughout the whole app.
        "dialog.cancel" | "dialog.pane" | "pane.switch" => app.return_keys_to_panes(),
        // The ring moves to the panel NEXT TO it, which is what `Tab` does
        // not do: the key that cycles the screen also has to work from
        // inside the panel you want to leave.
        "layout.focus-next" => app.layout_focus(1),
        "layout.focus-prev" => app.layout_focus(-1),
        // The tree's width, for the same reason as the sidebar's (#244 M1).
        "layout.grow" => app.layout_resize(1),
        "layout.shrink" => app.layout_resize(-1),
        "pane.tree" => app.toggle_tree(),
        // And the other panels' keys, same as in the sidebar.
        "layout.places" => app.toggle_places(),
        "layout.preview" => app.toggle_preview(),
        "layout.processes" => app.toggle_processes(),
        "layout.metadata" => app.toggle_metadata(),
        "layout.log" => app.toggle_log(),
        "layout.disk-map" => app.toggle_disk_map(),
        "dialog.confirm" => {
            let dest = app.tree().and_then(crate::tree::Tree::selected);
            if let Some(dir) = dest {
                // Expand AND navigate: whoever presses Enter on a branch
                // wants to see what is inside, and seeing it in the listing
                // is the complete answer.
                if let Some(t) = app.tree_mut() {
                    t.expand();
                }
                return cd(app, backend, events, dir).await;
            }
        }
        _ => {}
    }
    Cd::Cancelled
}

/// Processes panel keys (#243): resolves through the keymap (`dialog`
/// screen) and filters by [`crate::app::ALLOW_PROCESSES`] — same
/// single-source discipline as the rest of the panels with a keyboard
/// (#24).
///
/// Synchronous and backend-free: cancelling is releasing the token on a task
/// this process already observes, not a call.
pub fn on_processes_key(app: &mut App, resolver: &mut Resolver, mods: KeyModifiers, code: KeyCode) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    // The dispatch lives in `App` (library) so a test can feed it a real key
    // directly; what stays here is the resolution, which this binary has
    // and `App` does not.
    if let Some(msg) = app.processes_command(&cmd) {
        app.message = Some(msg);
    }
}

/// Journal timeline keys (phase 7).
///
/// Four things: moving, requesting more history on reaching the bottom,
/// returning the keyboard to the listings, and asking whether to undo up to
/// the marked row.
///
/// It is `async` because two of them need the backend, and for the same
/// reason the map does NOT navigate from its handler: requesting a page is
/// I/O, and here it is fine to await it because this handler already lives
/// in the loop.
///
/// **Enter does not undo: it asks.** And the question carries the COUNT,
/// computed over what is loaded — everything after the cursor is, by
/// definition: it pages backward from the newest. Undoing without saying how
/// much would be the worst key in this program.
pub async fn on_timeline_key(
    app: &mut App,
    backend: &Backend,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return;
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    let Some(slot) = app.timeline_slot() else {
        return;
    };
    match cmd.as_str() {
        "dialog.up" => {
            if let Some(t) = app.panes.timeline_mut(slot) {
                t.up();
            }
        }
        "dialog.down" => {
            let (at_bottom, cursor) = app.panes.timeline_mut(slot).map_or((false, None), |t| {
                t.down();
                (t.cursor() + 1 >= t.len(), t.next_before_seq())
            });
            // Reaching the bottom requests the next page. It is the only
            // moment more is asked for: a list loaded whole on open would
            // pull in months of journal to show twelve rows.
            if at_bottom && let Some(from) = cursor {
                crate::dispatch::load_timeline(app, backend, Some(from)).await;
            }
        }
        "dialog.cancel" | "dialog.pane" | "pane.switch" => app.return_keys_to_panes(),
        "dialog.confirm" => {
            let Some(tl) = app.panes.timeline(slot) else {
                return;
            };
            let (Some(seq), summary) = (tl.cutoff(), tl.summary()) else {
                return;
            };
            // A cut that carries nothing away does NOT open a dialog: asking
            // "are you sure?" about something that will not happen teaches
            // people to say yes without reading, which is how the next
            // question — one that DOES matter — also goes unread.
            if summary.no_does_nothing() {
                app.message = Some(t("timeline-undo-nothing"));
                return;
            }
            app.modal = Some(crate::app::Modal::ConfirmUndoAfter {
                seq,
                to_undo: summary.to_undo,
                irreversible: summary.irreversible,
                foreign: summary.foreign,
                // Frozen NOW, with the count that is about to be shown.
                techo: tl.techo(),
            });
        }
        _ => {}
    }
}

/// Disk map keys (phase 4).
///
/// Two layers, as in the log panel and for the same reason. First the map's
/// OWN keys ([`crate::diskmap::key`]): arrows, pages, ends, `Enter`, `r` and
/// `Esc` are its own while it has the keyboard, and they do not go through
/// the keymap because outside here they mean nothing — putting them there
/// would force all seven presets to declare useless shortcuts. Whatever it
/// does not claim goes to the `dialog` screen's resolver, filtered by
/// [`crate::app::ALLOW_DISK_MAP`].
///
/// **`Enter` does not navigate here.** It leaves the chosen child in
/// `pending_disk_map_enter` and the loop consumes it, since that is where
/// the backend is: entering a directory is a `cd` like any other, with its
/// padding and its refresh, and doing it halfway from a synchronous function
/// would be the second navigation path ADR 0077 exists to prevent.
pub fn on_disk_map_key(app: &mut App, resolver: &mut Resolver, mods: KeyModifiers, code: KeyCode) {
    use crate::diskmap::MapAction;

    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    if let Some(action) = crate::diskmap::key(code, mods) {
        let Some(slot) = app.disk_map_slot() else {
            return;
        };
        match action {
            MapAction::Mover(n) => {
                if let Some(m) = app.panes.disk_map_mut(slot) {
                    m.mover(n);
                }
            }
            MapAction::Enter => {
                let chosen = app
                    .panes
                    .disk_map(slot)
                    .and_then(|m| m.chosen().map(|c| c.name.clone()));
                // Only a DIRECTORY opens: entering a file is not navigating,
                // and the map shows both.
                let is_dir = app
                    .panes
                    .disk_map(slot)
                    .and_then(|m| m.chosen().map(|c| c.kind == norte_proto::EntryKind::Dir));
                if let (Some(name), Some(true)) = (chosen, is_dir) {
                    app.pending_disk_map_enter = Some(name);
                }
            }
            MapAction::Remeasure => app.disk_map_stale = true,
            MapAction::Leave => app.return_keys_to_panes(),
        }
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    app.disk_map_command(&cmd);
}

/// Keys of a panel CONTRIBUTED by a plugin (phase 3), resolved through the
/// same path as the processes panel's and filtered by `App::panel_command`.
///
/// Without this arm the panel took the focus border and its keys kept going
/// to the `browse` resolver: the reader believed the keyboard was in the
/// panel and `F8` opened the delete dialog over the listing's selection
/// behind it.
pub fn on_panel_key(app: &mut App, resolver: &mut Resolver, mods: KeyModifiers, code: KeyCode) {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    app.panel_command(&cmd);
}

/// Places sidebar keys (L3), resolved by the `dialog` context.
///
/// The sidebar does not navigate on its own: Enter returns a path and the
/// `cd` goes to the FOCUSED listing, by the same path as any other. That is
/// what keeps opening it from changing where operations go.
pub async fn on_places_key(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Cd {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return Cd::Cancelled;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    if !ALLOW_PLACES.contains(&cmd.as_str()) {
        return Cd::Cancelled; // outside this panel's allowlist: inert
    }
    // The application's chrome, before this panel's own: same funnel as the
    // tree and the processes panel.
    if app.panel_chrome_command(&cmd) {
        return Cd::Cancelled;
    }
    match cmd.as_str() {
        "dialog.up" => app.places_up(),
        "dialog.down" => app.places_down(),
        // `places_toggle_fold` leaves the drives requested if the section
        // ended up expanded; the loop serves them.
        "dialog.toggle-enabled" => app.places_toggle_fold(),
        // `Esc` releases the keyboard, and so does `Tab`. Neither one
        // CLOSES the panel: closing it is `layout.places`.
        //
        // `Tab`'s behavior is not symmetry for its own sake: without it,
        // opening the sidebar left dead the key that switches panes
        // throughout the whole app. `pane.switch` is not in the `dialog.*`
        // vocabulary and the panel swallows whatever is not in its
        // allowlist. It exits to the listings without changing pane, so the
        // NEXT `Tab` does what it always does and the key means only one
        // thing: "to the next region," with the sidebar counting as a
        // region.
        "dialog.cancel" | "dialog.pane" | "pane.switch" => app.return_keys_to_panes(),
        // The ring moves to the panel NEXT TO it, which is what `Tab` does
        // not do: the key that cycles the screen also has to work from
        // inside the panel you want to leave.
        "layout.focus-next" => app.layout_focus(1),
        "layout.focus-prev" => app.layout_focus(-1),
        // The sidebar's width, which is the ONLY path that can change it:
        // `layout_resize`'s caller always passes a visible listing, so the
        // `Size::Fixed` arm was unreachable by anyone (#244 M1).
        "layout.grow" => app.layout_resize(1),
        "layout.shrink" => app.layout_resize(-1),
        // And `layout.places` with the keyboard INSIDE closes: it is the
        // SECOND keystroke, because opening this panel already gives it the
        // keyboard.
        "layout.places" => app.toggle_places(),
        // The other panels' keys keep opening theirs: being in a side
        // column cannot cancel the key that opens the one next to it.
        "layout.preview" => app.toggle_preview(),
        "layout.processes" => app.toggle_processes(),
        "layout.metadata" => app.toggle_metadata(),
        "layout.log" => app.toggle_log(),
        "layout.disk-map" => app.toggle_disk_map(),
        "pane.tree" => app.toggle_tree(),
        // `⏎` over a HEADER folds or unfolds its section, as in the tree
        // next to it. Before, it did nothing: `activate()` returns `None`
        // for a header, so Enter over "Drives" was inert and folding was
        // Space and only Space. Enter is the gesture people try first on
        // something that opens, and the two side panels must answer it the
        // same way.
        //
        // Over a drive or a favorite it still NAVIGATES, which is what Enter
        // means over a leaf.
        "dialog.confirm" => {
            if app.places_cursor_on_header() {
                app.places_toggle_fold();
            } else if let Some(path) = app.places_activate() {
                let pane = app.focus();
                return cd_in(app, backend, events, pane, path, Trail::Record).await;
            }
        }
        _ => {}
    }
    Cd::Cancelled
}

/// Serves the pending drives request, if there is one.
///
/// The ONLY consumer of [`App::places_wants_drives`]: the run loop drains it
/// once per turn and startup once before entering it, so the first frame
/// already comes out with the list set.
///
/// It exists because `host.volumes` is I/O and whatever sets the flag —
/// opening the sidebar, expanding its section, loading a layout that
/// already carries it — does not always have a backend in front of it. When
/// each of those places requested the volumes on its own, they were missing
/// exactly where nobody remembered to.
pub async fn drain_places_drives(app: &mut App, backend: &Backend) {
    if std::mem::take(&mut app.places_wants_drives) {
        refresh_places_drives(app, backend).await;
    }
}

/// Requests the volumes from the host and leaves them in the sidebar.
///
/// Called by [`drain_places_drives`] and nobody else: a sidebar with a clock
/// would break ADR 0058's suspension rule from the first frame, and
/// `host.volumes` is not free (it mounts and queries space on every
/// filesystem).
///
/// A failure does NOT empty whatever list there was: what was showing stays
/// as the last thing the host said, and the error goes out through the bar
/// like any other.
pub async fn refresh_places_drives(app: &mut App, backend: &Backend) {
    let Some(id) = app.places_slot() else {
        return;
    };
    match backend.volumes(false).await {
        Ok(res) => {
            if let Some(state) = app.panes.places_mut(id) {
                state.set_drives(&res);
            }
        }
        Err(e) => {
            app.message = Some(ta(
                "gui-msg-volumes-failed",
                &[("error", &error_category(&e))],
            ));
        }
    }
}

/// Navigation popup keys (history `Alt+↓` / hotlist `Ctrl+D` / volumes
/// `Alt+F1`/`Alt+F2`, design §D); `ctrl+c` keeps its global quit, hardcoded
/// BEFORE anything else. With `name_input` active (hotlist's `a` opens a
/// field for the favorite's name) printables/backspace are captured as a RAW
/// text editor — H1 T2 decision: it is NOT a `dialog.*` command, it is free
/// input, it stays hardcoded. Outside `name_input`, the key resolves against
/// the keymap's `dialog` context (H1 T2, issue #24); `add`/`remove` are
/// filtered by this overlay's ALLOWLIST to `kind == Hotlist` (history has
/// nothing to name or delete — same criterion as before H1) and
/// `toggle-enabled` to `kind == Volumes` (design §D's "show all" toggle).
/// Enter on a valid item NAVIGATES through the normal cd flow, against
/// [`crate::app::NavPopup::target_pane`] and not `app.focus()` — history and
/// hotlist freeze the focus there, but `-left`/`-right` freeze a fixed SIDE
/// (design §D); if the cd from HISTORY fails with `NotFound`, the entry is
/// removed (spec 2026-07-18) — hotlist's and volumes' are NOT (hotlist is
/// user config and a volume is not removed because a one-off cd failed).
pub async fn on_nav_popup_key(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Cd {
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return Cd::Cancelled;
    }
    let Some(popup) = &app.nav_popup else {
        return Cd::Cancelled;
    };
    let kind = popup.kind;
    // SHIFT passes through (uppercase arrives as Char+SHIFT); ctrl/alt do
    // not type.
    let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
    if name_input_key(app, code, plain).await || filter_key(app, code, plain) {
        return Cd::Cancelled;
    }
    let Some(chord) = chord_from_crossterm(mods, code) else {
        return Cd::Cancelled; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // Sequence in progress, or a key bound to something this build does
        // not run (K1 T4): ignore and reset the resolution state.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return Cd::Cancelled;
        }
        Resolution::Reset => return Cd::Cancelled,
    };
    // H1 T3: the SAME allowlist each generated hint consumes
    // (`hints::DialogHints::build`, `nav_list`/`nav_volumes` fields) — a
    // single source for dispatch, even though the PRINTED hint is narrower
    // per kind. Covers all three kinds (History is a subset: `add`/`remove`
    // are filtered by the `kind == Hotlist` guard below, `toggle-enabled` by
    // the `kind == Volumes` guard).
    if !ALLOW_NAV_POPUP.contains(&cmd.as_str()) {
        return Cd::Cancelled; // outside this overlay's allowlist: inert
    }
    match cmd.as_str() {
        "dialog.up" => {
            app.nav_popup_input(PickerAction::Up);
        }
        "dialog.down" => {
            app.nav_popup_input(PickerAction::Down);
        }
        "dialog.cancel" => {
            app.nav_popup_input(PickerAction::Cancel);
        }
        // In favorites it creates one for the panel; in history or popular,
        // for the cursor row (spec 2026-09-15 D2).
        "dialog.add" if kind != NavPopupKind::Volumes => {
            app.nav_popup_open_name_input();
        }
        "dialog.remove" if kind == NavPopupKind::Hotlist => {
            if let Some(name) = app.nav_popup_selected_hotlist_name() {
                hotlist_remove(app, &name).await;
            }
        }
        // Spec 2026-09-15 D2: history and popular are also editable.
        "dialog.remove" if matches!(kind, NavPopupKind::History | NavPopupKind::Popular) => {
            app.nav_popup_remove_selected();
        }
        "dialog.filter" if matches!(kind, NavPopupKind::History | NavPopupKind::Popular) => {
            app.nav_popup_set_filter(Some(String::new()));
        }
        "dialog.clear" if matches!(kind, NavPopupKind::History | NavPopupKind::Popular) => {
            app.nav_popup_clear();
        }
        // The chosen item goes to the OTHER pane and the focus stays where
        // it is. Holds for every navigating list: a history path, a
        // favorite, or a volume all open on the other side the same way.
        "dialog.confirm-other" => {
            let Some(other) = app.nav_popup_other_pane() else {
                app.message = Some(t("host-no-other-slot"));
                return Cd::Cancelled;
            };
            return confirm_nav_popup(app, backend, events, kind, other).await;
        }
        // design §D: the in-popup unfiltered toggle. Same operation as
        // opening the popup, just with the flag flipped and the SAME target
        // pane — `open_drive_popup` re-fetches and replaces the snapshot.
        "dialog.toggle-enabled" if kind == NavPopupKind::Volumes => {
            let refresh = app
                .nav_popup
                .as_ref()
                .map(|p| (p.target_pane(), !p.include_pseudo()));
            if let Some((pane, want)) = refresh {
                open_drive_popup(app, backend, pane, want).await;
            }
        }
        "dialog.confirm" => {
            // The target pane is frozen on the popup, not `app.focus()`:
            // history/hotlist froze it AT the focus (so this is the same
            // value), but `-left`/`-right` froze a fixed SIDE (design §D).
            // Read it BEFORE `nav_popup_input` may close the popup below.
            let pane = app
                .nav_popup
                .as_ref()
                .map_or_else(|| app.focus(), NavPopup::target_pane);
            return confirm_nav_popup(app, backend, events, kind, pane).await;
        }
        _ => {} // outside this overlay's (or kind's) allowlist: inert
    }
    Cd::Cancelled
}

/// Navigates the popup's choice to pane `to` and closes the popup.
///
/// `to` is the popup's pane for `dialog.confirm` and the OTHER one for
/// `dialog.confirm-other` (spec 2026-09-15 D2); the rest is identical, and
/// that is why this is a single function. Confirm on an invalid item or an
/// empty list is a no-op: the popup stays open. If the destination came from
/// HISTORY and no longer exists, it is removed from that list's history
/// (spec 2026-07-18) — `from`'s, which need not be `to`'s —; the bar already
/// shows the normal error for a failed cd.
async fn confirm_nav_popup(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    kind: NavPopupKind,
    to: usize,
) -> Cd {
    let from = app
        .nav_popup
        .as_ref()
        .map_or_else(|| app.focus(), NavPopup::target_pane);
    let Some(path) = app.nav_popup_input(PickerAction::Confirm) else {
        return Cd::Cancelled;
    };
    let outcome = cd_in(app, backend, events, to, path.clone(), Trail::Record).await;
    if matches!(&outcome, Cd::Failed(Error::NotFound)) {
        // A directory that no longer exists leaves the list it came from:
        // `from`'s history, or popular (rust-reviewer, phase 1).
        match kind {
            NavPopupKind::History => app.history[from].remove(&path),
            NavPopupKind::Popular => app.popular.remove(&path),
            NavPopupKind::Hotlist | NavPopupKind::Volumes => {}
        }
    }
    outcome
}

/// TEXT keys while filtering a history or popular list (spec 2026-09-15
/// D2): printables and backspace write the filter, `Esc` removes it, and the
/// rest — Enter, arrows, `Del` — keeps going to the keymap as if there were
/// no filter. `true` if the key belonged to the filter.
fn filter_key(app: &mut App, code: KeyCode, plain: bool) -> bool {
    let Some(mut filter) = app.nav_popup.as_ref().and_then(|p| p.filter.clone()) else {
        return false;
    };
    match code {
        KeyCode::Char(c) if plain => filter.push(c),
        KeyCode::Backspace if plain => {
            filter.pop();
        }
        KeyCode::Esc => {
            app.nav_popup_set_filter(None);
            return true;
        }
        _ => return false,
    }
    app.nav_popup_set_filter(Some(filter));
    true
}

/// The keys while a favorite's name field is open (`a`): a RAW text editor,
/// not `dialog.*` commands (H1 T2). Enter saves with the popup's target;
/// Esc closes the field without closing the popup. `true` if the field was
/// open: while it is, every key is its own.
async fn name_input_key(app: &mut App, code: KeyCode, plain: bool) -> bool {
    let Some(input) = app.nav_popup.as_mut().and_then(|p| p.name_input.as_mut()) else {
        return false;
    };
    match code {
        KeyCode::Char(c) if plain => input.push(c),
        KeyCode::Backspace if plain => {
            input.pop();
        }
        KeyCode::Esc => {
            if let Some(p) = &mut app.nav_popup {
                p.name_input = None;
            }
        }
        KeyCode::Enter => {
            let name = app
                .nav_popup
                .as_mut()
                .and_then(|p| p.name_input.take())
                .unwrap_or_default();
            // Empty input = cancel (plan T5): there is no nameless favorite.
            if !name.is_empty() {
                hotlist_add(app, &name).await;
            }
        }
        _ => {}
    }
    true
}

/// `config::user_config_dir()` or the SAME `NotFound` io that
/// `persist_ui_theme` fabricates with no environment (bare CI): the bar
/// paints it as `err-not-found` through the category (#73), an existing and
/// reasonable key.
fn user_config_dir_io() -> std::io::Result<std::path::PathBuf> {
    config::user_config_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no user config directory")
    })
}

/// Persists the favorite `name` = the popup's target
/// ([`App::nav_popup_add_target`]: the focused pane's cwd in favorites, the
/// cursor row in a history) to the USER's `norte.toml` (`spawn_blocking`,
/// rule 2 — `persist_hotlist_add` is blocking by contract). Only if the disk
/// write succeeded is the copy in `App` refreshed (consistency with disk)
/// and `msg-hotlist-saved` shown; an io failure goes out by category and the
/// copy is left UNTOUCHED.
async fn hotlist_add(app: &mut App, name: &str) {
    let Some(target) = app.nav_popup_add_target() else {
        return;
    };
    let wire = target.to_wire();
    let n = name.to_owned();
    // To the active PROFILE if there is one: favorites belong to a
    // workspace, and writing them to the user's layer while a profile also
    // sets them leaves them covered (ADR 0079).
    let dest = app.config_write_dir();
    let res = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let dir = dest.map_or_else(user_config_dir_io, Ok)?;
        config::persist_hotlist_add(&dir, &n, &wire)?;
        Ok(())
    })
    .await;
    match res {
        Ok(Ok(())) => {
            app.hotlist_apply_saved(name, target);
            // The user typed the name, but a PASTE can smuggle in
            // bidi/control characters: through `detail_for_bar` like any
            // other detail (#73).
            app.message = Some(ta("msg-hotlist-saved", &[("name", &detail_for_bar(name))]));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-hotlist-persist-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        // A panic while persisting is OUR bug: let it blow up visibly
        // (binary's criterion, same as `config::load_async`).
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Removes the favorite `name` from the `norte.toml` of whichever layer is
/// being edited — the active PROFILE if there is one, otherwise the user's
/// — (`spawn_blocking`, rule 2). Same consistency contract as
/// [`hotlist_add`].
async fn hotlist_remove(app: &mut App, name: &str) {
    let n = name.to_owned();
    let dest = app.config_write_dir();
    let res = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let dir = dest.map_or_else(user_config_dir_io, Ok)?;
        config::persist_hotlist_remove(&dir, &n)?;
        Ok(())
    })
    .await;
    match res {
        Ok(Ok(())) => {
            app.hotlist_apply_removed(name);
            app.message = Some(ta(
                "msg-hotlist-removed",
                &[("name", &detail_for_bar(name))],
            ));
        }
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-hotlist-persist-failed",
                &[("error", &io_error_category(&e))],
            ));
        }
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}
