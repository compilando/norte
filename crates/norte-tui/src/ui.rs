//! Ratatui render of the state (`app`): zero business logic — it paints what
//! is there. Marking hostile names follows spec §6 (lossy and MARKED).
//! Colors come from the resolved theme (`app.theme`, ADR 0020): a frontend
//! with no theme sees M1's monochrome fallback.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::{App, display_name};
use crate::theme::TuiTheme;
use norte_i18n::{t, ta};

mod chrome;
mod compare;
mod geometry;
mod help;
mod modals;
mod overlays;
mod pane;
mod panels;
mod pickers;
mod status;
mod sync;
mod text;

// `tests/`, `mouse.rs` and `event_loop.rs` name all of this via `ui::..`, so
// it is this module's API and does not drop to `pub(crate)`.
pub use chrome::{
    KeyZone, MenuHit, MenuZone, PanelZone, TabAction, TabZone, key_zones, menu_zones, panel_zones,
    tab_zones,
};
pub use compare::draw_compare;
pub use geometry::{
    before_frame, pane_geometry, pane_list_rows, panel_slots, rect_del_visor, resize_borders,
    resolved_for, tab_strip_for,
};
pub use help::{
    HelpZones, draw_help, help_body_size, help_group_is_painted, help_layout, help_sidebar_width,
    help_zones,
};
pub use overlays::{
    ExtensionHit, ExtensionZone, draw_shortcuts, draw_which_key, extension_zones,
    plugin_description_line,
};
pub use pane::painted_len_and_selection;
pub(crate) use pane::pane_columns;
pub use panels::{PlaceZone, TreeZone, places_zones, tree_zones};
pub use pickers::draw_theme_picker;
pub use status::{SessionZone, StatusItemZone, session_zone, status_item_zones};
pub use text::fit_hint_groups;

pub(crate) use chrome::{TARGET_BADGE, TabStrip, draw_tab_strip};
// Only for tests, which is why it carries its own `cfg`: outside them this
// module paints the bar and nobody else needs to derive its buttons. It
// comes out since #329 because `app::layout`'s tests check that the BUTTON
// says the same thing the screen does, and that pair — the tree's state and
// what the bar derives from it — is exactly what used to drift out of sync.
#[cfg(test)]
pub(crate) use chrome::panel_buttons;
use chrome::{draw_key_bar, draw_menu, draw_panel_bar};
pub(crate) use geometry::{
    body_rect, centered, chrome_body, pane_cols, placed_of_kind, resolved_frame, slot_content,
    slot_rect, visor_split,
};
use modals::draw_modal;
#[cfg(test)]
pub(crate) use modals::report_text;
pub use modals::{ModalZone, modal_zones};
use overlays::{
    EXTENSIONS_WIDE_MIN, draw_extensions, draw_goto, draw_palette, draw_plugin_config_panel,
    draw_settings, draw_splash, draw_wizard,
};
use pane::draw_pane;
use panels::{
    draw_disk_map, draw_log, draw_metadata, draw_places, draw_plugin_panel, draw_preview,
    draw_processes, draw_tasks, draw_terminal, draw_timeline, draw_tree, draw_viewer,
};
use pickers::{
    draw_columns_picker, draw_connections_picker, draw_layout_picker, draw_profile_picker,
};
use status::draw_status;
use sync::draw_sync;

/// Hostile-name badge: a PREFIX in a fixed column (at the end it would die
/// in ratatui's width truncation and the name would paint "clean") and in
/// ASCII (`⚠` is ambiguous-width: 2 cells in many terminals). Styled (role
/// `hostile-badge`) — out of band: a file called "! x" cannot imitate it.
/// DOCUMENTED EXCEPTION: the navigation popups carry the badge in-band
/// inside the item's display (like modal titles); a favorite called "! x"
/// can imitate it — a read-only surface the user owns, accepted risk.
/// `pub` since S4 (#135): the binary (`main.rs`, another crate) composes the
/// `msg-shell-remote` line with the already-sanitized path, and a copied `"!"`
/// literal there would be a second badge that can drift out of sync with
/// this one.
pub const HOSTILE_BADGE: &str = "!";

/// The theme's BASE style: [`Role::Background`]'s background plus
/// [`Role::Regular`]'s foreground. This is what makes a span with NO
/// foreground of its own (`Span::raw`/`Line::raw`, or a bare
/// `Modifier::DIM`) inherit the THEME's foreground and not the TERMINAL's —
/// with a light theme on a dark terminal the latter paints text nearly the
/// color of the background. A theme with no `background` sets no base
/// foreground: it keeps the terminal's, which is the one that clashes.
fn base_style(theme: &TuiTheme) -> ratatui::style::Style {
    let base = theme.role(Role::Background);
    if base.bg.is_some() {
        base.patch(theme.role(Role::Regular))
    } else {
        base
    }
}

/// `Clear` + repaint the theme's base over `area`: ratatui's `Clear` widget
/// leaves the cells in the DEFAULT style (the terminal's foreground and
/// background), so an overlay that only does `Clear` loses the theme's
/// background AND foreground, and its text with no `fg` falls back to the
/// terminal's.
fn clear_themed(frame: &mut Frame<'_>, area: Rect, theme: &TuiTheme) {
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(Block::default().style(base_style(theme)), area);
}

/// Whether panel `i` carries the DESTINATION mark.
///
/// Two questions: who has the role, and whether with these panels the mark
/// says anything. The second is answered by the shared crate, which is
/// where the window used to answer it on its own and with a different
/// answer (ADR 0077): with two panels the destination is "the other one"
/// and a mark that always shows stops being read; from three on, a copy
/// toward whichever one the engine tiebreaks is silent data loss (ADR 0058
/// D7).
fn destination_mark(app: &App, i: usize) -> bool {
    norte_frontend::layout::target_worth_marking(app.panes.len()) && app.target_index() == Some(i)
}

/// A listing's footer (spec 2026-09-10), or `None` with `[ui] pane_footer`
/// off. It is drafted by the shared crate; here only the pane's counts are
/// joined with its volume's free space (from `App`'s cache).
fn pane_footer(app: &App, pane: &crate::app::Pane, width: u16) -> Option<String> {
    if !app.chrome.pane_footer() {
        return None;
    }
    let counts = norte_frontend::footer::counts(pane.entries(), pane.is_parent_row(0));
    let marked = norte_frontend::footer::Marked {
        n: pane.marks_len(),
        bytes: pane.marked_bytes(),
        dirs: pane.marked_dirs(),
    };
    let free = norte_frontend::space::free_for(pane.dir(), &app.volumes);
    // What the border leaves: the two corners and one space on each side.
    // Segments that do not fit drop by priority, not from the middle.
    let room = usize::from(width.saturating_sub(4));
    Some(norte_frontend::footer::fit(
        norte_frontend::footer::segments(counts, marked, free, norte_i18n::active()),
        room,
    ))
}

/// The frame's body: the two panes — or the panel that replaces them —, the
/// task strip and the status bar.
///
/// Separate from [`draw`] because a layout pass, two replacement branches
/// and three paint calls do not fit in a function that also assembles every
/// overlay. The body's two bottom strips — tasks and status — come from the
/// layout pass, with their fallback for when the tree does not place them.
/// The status fallback goes at the end of the BODY, not the frame: the key
/// bar reserves the last row (spec 2026-09-10).
fn bottom_strips(res: &norte_frontend::layout::Resolved, body: Rect) -> (Rect, Rect) {
    let tasks_area = slot_rect(res, crate::panel::SLOT_TASKS).unwrap_or(Rect {
        x: body.x,
        y: body.y.saturating_add(body.height),
        width: body.width,
        height: 0,
    });
    let status_area = slot_rect(res, crate::panel::SLOT_STATUS).unwrap_or(Rect {
        x: body.x,
        y: body.y.saturating_add(body.height).saturating_sub(1),
        width: body.width,
        height: 1,
    });
    (tasks_area, status_area)
}

fn draw_body(frame: &mut Frame<'_>, app: &App) {
    // ONE layout pass per frame: the body, the two panes, the task strip and
    // the status bar all come out of it.
    let res = resolved_frame(app, frame.area());
    let body = body_rect(&res, &app.layout).unwrap_or_else(|| chrome_body(app, frame.area()));
    let (tasks_area, status_area) = bottom_strips(&res, body);
    let cols = pane_cols(&res, &app.layout);
    // #108 L5: relative-time cells' `now` — ONE read per frame; tests pin it
    // (`App::render_now_ms`) for stable snapshots.
    let now_ms = app.now_ms();

    // The differences panel takes the spot of BOTH panes: a row has two
    // sides and a verdict in between, so it does not fit in half the
    // screen. The tasks strip and the bar stay below, even though the
    // comparison does not go into the `TaskBoard` (same as a live search:
    // its progress is painted by the panel's own footer) — what is seen
    // below there are the OTHER tasks, still running.
    if let Some(view) = &app.sync {
        // Over the differences one, which stays alive behind it with its
        // marks: the plan is what has to be looked at while it is decided,
        // and going back to the rows closes the plan.
        draw_sync(frame, body, view, &app.theme);
    } else if let Some(view) = &app.compare {
        draw_compare(frame, body, view, &app.theme, &app.compare_size_hints);
    } else {
        for (i, pane) in app.panes.iter().enumerate() {
            // A pane the layout pass did not place is not painted: the
            // `Split` collapsed and the other one takes its whole spot.
            let Some(rect) = cols.get(i).copied() else {
                continue;
            };
            draw_pane(
                frame,
                rect,
                pane,
                app.focus() == i,
                &app.theme,
                now_ms,
                &app.columns,
                // #117 task 2: the pane's scheme's cached catalogue (hints
                // and headers); without it it paints with defaults, never
                // waits.
                app.attr_catalog(pane.dir().scheme()),
                tab_strip_for(app, i).as_ref(),
                // The count is decided by the shared crate (ADR 0077).
                destination_mark(app, i),
                // The wait, only if it is THIS panel's and already past the
                // threshold: a session job must not set a header spinning
                // over something that is not happening to it.
                app.busy.as_ref().filter(|b| b.visible() && b.affects(i)),
                pane_footer(app, pane, rect.width).as_deref(),
                app.chrome.dir_indicator(),
                app.chrome.row_stripes(),
            );
        }
    }
    draw_side_panels(frame, &res, app, tasks_area, status_area);
    // The panel-group strips (ADR 0134), over the row `placed_of_kind`
    // reserved for them.
    chrome::draw_pane_strips(frame, app);
    // Moving a panel (ADR 0138): the spot it would land on, marked with the
    // focus border, like the window's veil.
    if let Some(r) = app.mouse.move_target() {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Double)
                .border_style(app.theme.role(Role::BorderFocus)),
            crate::panel::to_ratatui(r),
        );
    }
}

/// The SIDE panels, which come out of the same layout pass as the listings.
///
/// Pulled out of [`draw_body`] once the disk map pushed it past a hundred
/// lines. They are a homogeneous group — each one asks the layout pass
/// whether its kind was placed and paints if so — so they come out together
/// and the order is kept: they go AFTER the listings and before the chrome
/// below.
fn draw_side_panels(
    frame: &mut Frame<'_>,
    res: &norte_frontend::layout::Resolved,
    app: &App,
    tasks_area: Rect,
    status_area: Rect,
) {
    // If the slot was not placed — closed, or collapsed for lack of room —
    // there is nothing to do here.
    if let Some((id, rect)) = placed_of_kind(res, &app.layout, "places")
        && let Some(state) = app.panes.places(id)
    {
        draw_places(
            frame,
            rect,
            state,
            app.key_owner() == crate::app::KeyOwner::Places,
            &app.theme,
        );
    }
    if let Some((id, rect)) = placed_of_kind(res, &app.layout, crate::preview::KIND)
        && let Some(p) = app.panes.preview(id)
    {
        draw_preview(
            frame,
            rect,
            p,
            app.key_owner() == crate::app::KeyOwner::Preview,
            app,
        );
    }
    if let Some((id, rect)) = placed_of_kind(res, &app.layout, crate::processes::KIND)
        && let Some(&p) = app.panes.processes(id)
    {
        draw_processes(
            frame,
            rect,
            p,
            app,
            app.key_owner() == crate::app::KeyOwner::Processes,
        );
    }
    // The log carries no state PER SLOT — there is one, and its level and
    // its filter belong to the session — so the rectangle it landed in is
    // enough.
    if let Some((_, rect)) = placed_of_kind(res, &app.layout, crate::logview::KIND) {
        draw_log(
            frame,
            rect,
            app,
            app.key_owner() == crate::app::KeyOwner::Log,
        );
    }
    // The terminal (#362): there is one and its state belongs to the
    // session, so the rectangle it landed in is enough, same as the log.
    if let Some((_, rect)) = placed_of_kind(res, &app.layout, crate::termpanel::KIND) {
        draw_terminal(
            frame,
            rect,
            app,
            app.key_owner() == crate::app::KeyOwner::Terminal,
        );
    }
    if let Some((id, rect)) = placed_of_kind(res, &app.layout, crate::diskmap::KIND)
        && let Some(m) = app.panes.disk_map(id)
    {
        draw_disk_map(
            frame,
            rect,
            m,
            app,
            app.key_owner() == crate::app::KeyOwner::DiskMap,
        );
    }
    if let Some((id, rect)) = placed_of_kind(res, &app.layout, crate::timeline::KIND)
        && let Some(tl) = app.panes.timeline(id)
    {
        draw_timeline(
            frame,
            rect,
            tl,
            app,
            app.key_owner() == crate::app::KeyOwner::Timeline,
        );
    }
    if let Some((id, rect)) = placed_of_kind(res, &app.layout, crate::tree::KIND)
        && let Some(t) = app.panes.tree(id)
    {
        draw_tree(
            frame,
            rect,
            t,
            app,
            app.key_owner() == crate::app::KeyOwner::Tree,
        );
    }
    if let Some((id, rect)) = placed_of_kind(res, &app.layout, crate::metadata::KIND)
        && let Some(e) = app.panes.metadata(id)
    {
        // NEVER a focus border: the sheet does not take the keyboard, and a
        // highlighted border over a panel that reads no key was the visible
        // half of the control that did not do what it said (#243).
        //
        // The title says WHICH LISTING it follows: with two open, a bare
        // "Details" does not say whose details they are.
        let follows = crate::metadata::follows(app, res);
        draw_metadata(frame, rect, e.as_ref(), follows.as_ref(), app, false);
    }
    // A PLUGIN's panel (phase 3) is resolved by PREFIX: its kind is not
    // known at compile time, so it does not go through `placed_of_kind`.
    if let Some(id) = app.panel_slot()
        && let Some(rect) = geometry::slot_rect(res, id)
    {
        let rect = geometry::slot_content(&app.layout, id, rect);
        let has_keyboard = app.key_owner() == crate::app::KeyOwner::Panel;
        draw_plugin_panel(frame, rect, app, id, has_keyboard);
    }
    draw_tasks(frame, tasks_area, app);
    draw_status(frame, status_area, app);
}

/// The footer the extension MANAGER paints with, or `None` when what is
/// seen is the narrow settings box.
///
/// With a tab (ADR 0104) the settings go INSIDE the manager, with the
/// settings panel's footer; on a narrow terminal the tab does not fit and
/// the settings have their own box, as before. The tab hosts the settings
/// only for the CHOSEN extension: a panel from another one — or from one no
/// longer in the list — has its own box.
///
/// One function because the decision is made by TWO: the painter and the
/// mouse, which measures its zones against what was painted.
pub(crate) fn extensions_footer<'a>(
    app: &'a App,
    mgr: &crate::app::ExtensionManager,
    frame_width: u16,
) -> Option<&'a str> {
    let Some(panel) = &mgr.config else {
        return Some(&app.dialog_hints.extensions);
    };
    let usable_width = frame_width
        .saturating_sub(6)
        .clamp(24, 120)
        .saturating_sub(2);
    let in_tab = usable_width >= EXTENSIONS_WIDE_MIN
        && mgr
            .plugins
            .get(mgr.cursor)
            .is_some_and(|p| panel.plugin_id == p.id);
    in_tab.then_some(app.dialog_hints.plugin_config.as_str())
}

/// Paints the whole frame: panes (or viewer) + tasks panel + status bar +
/// modal on top.
pub fn draw(frame: &mut Frame<'_>, app: &App) {
    // The theme's BASE background (ADR 0020): painted first; text styles
    // (fg only) keep it. No `background` in the theme = the terminal's
    // background.
    //
    // `Regular`'s FOREGROUND goes on the same base: a span with no
    // foreground of its own (`Span::raw`/`Line::raw`, or a bare
    // `Modifier::DIM` like the column cells' and the header's) inherits the
    // TERMINAL's default foreground, which need not match the THEME's
    // background — with a light theme on a dark terminal that produced text
    // nearly the color of the background (Size/Date/Type, the column header
    // and the overlays' bodies, invisible). Painting it here fixes it for
    // the WHOLE frame at once, with no need to touch every span: whoever
    // wants a different color still sets their own. A theme with no
    // `background` also paints no base foreground (it keeps the terminal's,
    // which is the one that matches).
    frame.render_widget(Block::default().style(base_style(&app.theme)), frame.area());
    // The viewer replaces the panes, NEVER the overlays: this arm used to
    // `return` and ANY overlay open with the viewer over it became
    // invisible even though the run loop had already given it the key (its
    // arm goes BEFORE the viewer in the chain) — help (F1), the theme
    // selector, settings, the palette and even an async approval modal ate
    // the keyboard without painting a single pixel: the viewer looked hung
    // and F1 "stopped working". The pixels must say who is in charge (same
    // criterion as the modal painted last, further below).
    if let Some(viewer) = &app.viewer {
        draw_viewer(frame, viewer, app);
    } else {
        draw_body(frame, app);
    }
    // The bar is painted if it is PINNED (even with the menu closed: that is
    // what it is for, so it shows there is a menu) or if the menu is open.
    if app.menu_bar || app.menu.is_some() {
        draw_menu(frame, app);
    }
    // #324: and the panel row below it. After the body for the same reason
    // as the menu: it is chrome, and the body has already been given the
    // spot left for it.
    draw_panel_bar(frame, app);
    if let Some(help) = &app.help {
        draw_help(
            frame,
            help,
            &app.theme,
            &app.dialog_hints.help,
            app.version_line,
        );
    }
    if let Some(picker) = &app.theme_picker {
        draw_theme_picker(frame, picker, &app.theme, &app.dialog_hints.picker);
    }
    if let Some(p) = &app.columns_picker {
        draw_columns_picker(frame, p, &app.theme, &app.dialog_hints.columns);
    }
    // Phase A: the layouts selector, with the same key allowlist as the
    // theme one (`ALLOW_PICKER`) and hence the same hint.
    if let Some(p) = &app.profile_picker {
        draw_profile_picker(frame, p, &app.theme, &app.dialog_hints.picker);
    }
    if let Some(p) = &app.layout_picker {
        draw_layout_picker(frame, p, &app.theme, &app.dialog_hints.picker, &app.kinds);
    }
    // #140: the connections selector, same allowlist and same hint as the
    // other two — it is a list with a cursor that mutates nothing.
    if let Some(p) = &app.connections_picker {
        draw_connections_picker(frame, p, &app.theme, &app.dialog_hints.picker);
    }
    if let Some(mgr) = &app.extensions {
        match (extensions_footer(app, mgr, frame.area().width), &mgr.config) {
            (Some(hint), _) => draw_extensions(frame, mgr, &app.theme, hint),
            (None, Some(panel)) => {
                draw_plugin_config_panel(frame, panel, &app.theme, &app.dialog_hints.plugin_config);
            }
            (None, None) => {}
        }
    }
    if let Some(popup) = &app.nav_popup {
        draw_nav_popup(frame, popup, &app.theme, &app.dialog_hints);
    }
    if let Some(dialog) = &app.search_dialog {
        draw_search_dialog(
            frame,
            dialog,
            app.focused().dir(),
            app.focused().name_encoding(),
            &app.theme,
        );
    }
    if let Some(palette) = &app.palette {
        draw_palette(frame, palette, &app.theme);
    }
    // "Go anywhere" (phase 6) goes with the palette, which is its sibling:
    // over the listing, under the wizard and a modal.
    if let Some(goto) = &app.goto {
        draw_goto(frame, goto, &app.theme);
    }
    // The first-run wizard (spec 2026-09-10): over the palette and the
    // settings, under a modal, like the rest of the overlays that are not a
    // security question.
    // The splash screen goes UNDER the wizard and over everything else: if
    // both were up, whichever one asks something rules. In practice they do
    // not coincide — the splash's gate yields to the wizard — but the order
    // is stated here rather than in an invariant somebody would have to
    // remember.
    if let Some(splash) = &app.splash {
        draw_splash(frame, splash, &app.theme);
    }
    if let Some(wizard) = &app.wizard {
        draw_wizard(frame, wizard, &app.theme);
    }
    if let Some(settings) = &app.settings {
        draw_settings(frame, settings, &app.theme);
    }
    // K3c: the shortcuts editor opens FROM settings and paints over it, with
    // the settings overlay open behind it — it is a screen of its own, not
    // a replacement, and closing it returns the reader where they were. It
    // also claims the keys before it does (`main`), so the pixels and the
    // routing say the same thing.
    if let Some(sc) = &app.shortcuts {
        draw_shortcuts(frame, sc, &app.theme);
    }
    // K3a: the which-key panel goes after the overlays and BEFORE the modal.
    // It is the only one that claims no key — the pane's resolver keeps them
    // while it is up — so it does not compete for the keyboard with anything
    // above; it is painted on top because it describes the sequence the
    // reader is typing RIGHT NOW, and covering it with an overlay opened
    // earlier would hide the answer to the question they just asked.
    //
    // WITH A MODAL OPEN it is NOT painted, and this is the exception that
    // proves the previous rule: a modal can open ALONE (a policy approval
    // arriving over the bus, a collision when a copy finishes) with nobody
    // having touched a key, and from then on keys go to the
    // `dialog_resolver`. A panel that kept saying "g → go to top" next to a
    // dialog that claims `g` is exactly the pixel lie the modal's comment
    // documents, further below. The sequence stays alive in the pane's
    // resolver (the modal does not cancel it, same as it does not cancel
    // the bar's `[g …]`): it is seen again once the dialog closes.
    if let Some(wk) = &app.which_key
        && app.modal.is_none()
    {
        draw_which_key(frame, wk, &app.theme);
    }
    // Review S, M3: the modal is painted LAST, over ANY other overlay — key
    // routing already treats it as AUTHORITATIVE in the presence of the
    // palette or the settings overlay (`modal_preempts_
    // palette`/`modal_preempts_settings`, `main.rs`: a modal in flight,
    // e.g. an async policy approval, ALWAYS wins the key). It used to be
    // painted right after the status bar, so any overlay later in this list
    // visually COVERED it — the pixels lied about who was in charge. This
    // closes the H1 MINOR-4 class (accepted back then only for the palette)
    // for BOTH overlays.
    if let Some(modal) = &app.modal {
        // H3c: with a help page OPEN OVER IT (`over_modal`), the help claims
        // the keys and the modal's verbs are INERT. The footer stops
        // offering them and says what is true (`with_modals_inert`): a
        // `[y] approve [n] deny` that does nothing is the same lie
        // `hints.rs` exists to keep a rebind from telling. The box and the
        // question are NOT touched — they keep being painted here, last,
        // over the page.
        let inert = app
            .help
            .as_ref()
            .is_some_and(|help| help.over_modal)
            .then(|| app.dialog_hints.with_modals_inert());
        draw_modal(
            frame,
            modal,
            &app.theme,
            app.focused().name_encoding(),
            inert.as_ref().unwrap_or(&app.dialog_hints),
        );
    }
    // The key bar (spec 2026-09-10) goes LAST: its row is outside the body,
    // so no overlay covers it. With a modal in front it goes blank
    // (`App::key_bar_cells`): no preset binds an `F` in `[dialog]`.
    draw_key_bar(frame, app);
}

/// Is there anything painted OVER the viewer this frame?
///
/// Review, IMPORTANT 5: kitty's pixels are painted OUTSIDE ratatui and at
/// `z=0` (in front of the text), so they survive any cell repaint `draw`
/// does AFTER the viewer — opening F1 or the palette over a viewer with an
/// image left it covered by the thumbnail, exactly the bug class the
/// comment on [`draw`] (above) says was fixed for the viewer itself. The run
/// loop (T4) consults it before placing pixels: with something over it, it
/// does not place them (and erases them if something was placed).
///
/// Repeats, ON PURPOSE, the list of overlays [`draw`] paints AFTER the
/// viewer — it IS the same question, "what is over it", looked at from the
/// run loop instead of from the painter. There is no single source the two
/// can come from without building an overlay registry this phase does not
/// ask for; if you touch the chain of `if let Some(x) = &app.x` above, touch
/// this list too.
///
/// Review, round 2: `app.menu` was missing. The dropdown is painted inside
/// the BODY (`chrome::draw_menu`, `y = area.y + 1`, over the viewer's
/// interior when it is open), and `f9`/`alt+m` are in `[global]` — merged
/// into EVERY screen, so opening the menu with the viewer in front is
/// reachable. `app.menu_bar` (the PINNED bar) is not needed: it lives
/// outside `body_area`, never competing for the viewer's spot.
#[must_use]
pub fn something_above_the_viewer(app: &App) -> bool {
    app.menu.is_some()
        || app.help.is_some()
        || app.theme_picker.is_some()
        || app.columns_picker.is_some()
        || app.profile_picker.is_some()
        || app.layout_picker.is_some()
        || app.connections_picker.is_some()
        || app.extensions.is_some()
        || app.nav_popup.is_some()
        || app.search_dialog.is_some()
        || app.palette.is_some()
        || app.goto.is_some()
        || app.splash.is_some()
        || app.wizard.is_some()
        || app.settings.is_some()
        || app.shortcuts.is_some()
        || app.which_key.is_some()
        || app.modal.is_some()
}

/// The slot (interior, WITHOUT borders) where [`App::viewer_imagen`]'s
/// thumbnail must be placed this frame, or `None` if it must not be seen —
/// neither its pixels nor the blank slot that makes room for them.
///
/// Branch review, finding 2: `panels::draw_viewer` (the painter, which
/// blanks the slot) and the run loop (T4, which places the real pixels,
/// `event_loop.rs`) each did this count on their own side — the painter
/// only looked at `path`, the run loop added [`something_above_the_viewer`] and
/// that the rect was not empty. With an overlay that does NOT cover the
/// whole screen (the menu, which-key, a small modal, the nav popup) the
/// painter blanked the slot JUST AS ALWAYS while the run loop refused to
/// place pixels: neither image nor hexview, an empty viewer. With both
/// questions resolved by the SAME function, diverging like that stops being
/// possible (memory `funcion-compartida-no-basta`).
#[must_use]
pub fn image_to_place(app: &App, area: Rect) -> Option<crate::viewer_open::Placement> {
    let viewer = app.viewer.as_ref()?;
    let image = app.viewer_imagen.as_ref()?;
    if image.path != viewer.path || something_above_the_viewer(app) {
        return None;
    }
    let rect = rect_del_visor(app, area);
    if rect.is_empty() {
        return None;
    }
    // Zoom (spec 2026-09-20). The pan is the keys that move the viewer,
    // which with an image have nothing else to move: `scroll` goes down it
    // and `hscroll` travels across it.
    Some(crate::viewer_open::placement(
        viewer.zoom_pct(),
        rect,
        image.width,
        image.height,
        viewer.hscroll(),
        viewer.scroll,
    ))
}

/// Live search dialog (`Alt+F7`, liveSearch T6): two text fields
/// (name/content) with a `_` on the active one, the two regex/case toggles
/// and the walk's root (the pane's `cwd`, not editable) — all sanitized,
/// never raw bidi/controls (the fields go through [`display_name`], the root
/// through [`path_display`]; a hostile paste does not paint invisibles on
/// the border).
fn draw_search_dialog(
    frame: &mut Frame<'_>,
    dialog: &crate::app::SearchDialog,
    root: &norte_proto::VPath,
    enc: Option<norte_encoding::NameEncoding>,
    theme: &TuiTheme,
) {
    use crate::app::SearchField;
    let on_txt = |b: bool| if b { t("on-yes") } else { t("on-no") };
    let field = |label: &str, value: &str, active: bool| {
        let (masked, _) = display_name(value.as_bytes());
        let cursor = if active { "_" } else { "" };
        format!("{label} {masked}{cursor}")
    };
    // #98/F4: the walk's root is a decision surface — it follows the pane's
    // reinterpretation (the bar below paints the same dir this way).
    let (root_txt, root_hostile) = norte_frontend::path_display_with(root, enc);
    let root_line = if root_hostile {
        format!("{HOSTILE_BADGE} {root_txt}")
    } else {
        root_txt
    };
    // The seven fields in the order Tab walks them, then the four toggles.
    // They are generated from the SAME `ORDEN` as Tab: two hand-written
    // lists drift apart the moment a field is added, and then the cursor
    // jumps to a line that is not painted.
    let mut lines: Vec<String> = SearchField::ORDEN
        .iter()
        .map(|f| field(&t(f.key()), dialog.text(*f), dialog.field == *f))
        .collect();
    lines.push(ta("search-regex", &[("on", &on_txt(dialog.regex))]));
    lines.push(ta("search-case", &[("on", &on_txt(dialog.case))]));
    lines.push(ta(
        "search-whole-word",
        &[("on", &on_txt(dialog.whole_word))],
    ));
    lines.push(ta("search-recursive", &[("on", &on_txt(dialog.recursive))]));
    lines.push(ta("search-kinds", &[("what", &t(dialog.kinds.key()))]));
    lines.push(middle_ellipsis(&root_line, 56));
    lines.push(t("search-hint"));
    let body = lines.join("\n");
    // Height = the lines plus the frame, but WITHOUT going past the screen:
    // with eleven fields and toggles the dialog measures 18 rows, and on a
    // 24-row terminal that ate the menu bar above and the status bar below.
    // When trimmed, the last lines — the path and the cheat sheet — are lost
    // before the frame, which is what keeps the dialog a dialog.
    let height = u16::try_from(lines.len().saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(frame.area().height);
    let area = centered(frame.area(), 60, height);
    clear_themed(frame, area, theme);
    frame.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", t("search-title")))
                .title_style(theme.role(Role::Title))
                .border_style(theme.role(Role::ModalBorder)),
        ),
        area,
    );
}

/// Navigation popup (spec 2026-07-18): history `Alt+↓` / hotlist `Ctrl+D` /
/// volumes `Alt+F1`/`Alt+F2` (design 2026-08-10 §D), tracing
/// [`draw_theme_picker`]. Items arrive ALREADY sanitized from
/// [`crate::app::App::open_nav_popup`]/[`crate::app::App::open_volumes_popup`]
/// — here they are only painted. The key footer only applies to hotlist
/// (`a`/`d`) and volumes (the "show all" toggle, plus the current mode);
/// with the name input active it is replaced by the `name: …` line (the
/// input goes through the SAME mask as the quick search query: a hostile
/// paste does not paint raw bidi). `hints` carries each kind's GENERATED
/// hint (`app.dialog_hints.nav_list`/`.nav_volumes`, H1 T3/#24 and design
/// §D) — history paints no footer, same as before H1.
fn draw_nav_popup(
    frame: &mut Frame<'_>,
    popup: &crate::app::NavPopup,
    theme: &TuiTheme,
    hints: &crate::hints::DialogHints,
) {
    use crate::app::NavPopupKind;
    let title = match popup.kind {
        NavPopupKind::History => match popup.side {
            Some(0) => t("history-title-left"),
            Some(_) => t("history-title-right"),
            None => t("history-title"),
        },
        NavPopupKind::Popular => t("popular-title"),
        NavPopupKind::Hotlist => t("hotlist-title"),
        NavPopupKind::Volumes => t("volumes-title"),
    };
    // The footer is built FIRST to size the popup with its REAL width
    // (unicode cells via `Line::width`, not bytes): 64 at minimum — the
    // hotlist key footer in ES is 60 cells and at 60 the border would
    // truncate it ("cerra…") — and it grows if the footer (e.g. a long name
    // in the input) needs it to.
    let footer: Option<Line<'_>> = if let Some(input) = &popup.name_input {
        let (masked, _) = display_name(input.as_bytes());
        Some(Line::raw(format!(
            " {} {masked}_ ",
            t("hotlist-name-prompt")
        )))
    } else if let Some(filter) = &popup.filter {
        // What is being filtered IS seen, with the same mask as a
        // favorite's name: a hostile paste does not paint raw bidi.
        let (masked, _) = display_name(filter.as_bytes());
        Some(Line::raw(format!(" /{masked}_ ")))
    } else if popup.kind == NavPopupKind::Hotlist {
        Some(Line::raw(format!(" {} ", hints.nav_list)))
    } else if matches!(popup.kind, NavPopupKind::History | NavPopupKind::Popular) {
        // Spec 2026-09-15 D2: history is already editable (remove, clear,
        // open in the other panel), and a list that is edited says how.
        Some(Line::raw(format!(" {} ", hints.nav_history)))
    } else if popup.kind == NavPopupKind::Volumes {
        // design §D: the footer says what MODE the list is in, not just
        // which keys there are — a toggle with no indicator leaves the
        // reader guessing whether they already pressed it.
        let mode = if popup.include_pseudo() {
            t("volumes-mode-all")
        } else {
            t("volumes-mode-filtered")
        };
        Some(Line::raw(format!(" {mode} — {} ", hints.nav_volumes)))
    } else {
        None
    };
    let footer_w = footer.as_ref().map_or(0, Line::width);
    let width = u16::try_from(footer_w.saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(64);
    let rows = u16::try_from(popup.items().len().max(1)).unwrap_or(8) + 2;
    let area = centered(frame.area(), width, rows.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
    // Long items: MIDDLE ellipsis (head + tail, like the path modals) at the
    // inner width — ratatui's right truncation would make two paths with a
    // common prefix indistinguishable (BAJA-3).
    let inner = usize::from(area.width.saturating_sub(3));
    let (items, selected): (Vec<ListItem<'_>>, Option<usize>) = if popup.items().is_empty() {
        let empty = match popup.kind {
            NavPopupKind::History => t("history-empty"),
            NavPopupKind::Popular => t("popular-empty"),
            NavPopupKind::Hotlist => t("hotlist-empty"),
            NavPopupKind::Volumes => t("volumes-empty"),
        };
        (vec![ListItem::new(Line::raw(format!(" {empty}")))], None)
    } else {
        (
            popup
                .items()
                .iter()
                .map(|it| {
                    // A history row's mark goes in its OWN span and with a
                    // different style: stuck to the text a directory with
                    // that name could imitate it. The trim is the path's,
                    // not the mark's.
                    let mark = it.mark.as_deref().map(|m| {
                        ratatui::text::Span::styled(format!(" · {m}"), theme.role(Role::Info))
                    });
                    let mark_w = mark.as_ref().map_or(0, ratatui::text::Span::width);
                    let path = ratatui::text::Span::raw(format!(
                        " {}",
                        middle_ellipsis(&it.display, inner.saturating_sub(mark_w))
                    ));
                    ListItem::new(Line::from(
                        std::iter::once(path).chain(mark).collect::<Vec<_>>(),
                    ))
                })
                .collect(),
            Some(popup.cursor()),
        )
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(Role::ModalBorder));
    if let Some(footer) = footer {
        block = block.title_bottom(footer);
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

/// MIDDLE ellipsis by cell width: NOW lives in `norte-frontend` (encoding
/// audit M4-IA-2 H1) — the invariant "a mile-long path never expels the
/// field that follows it" is not particular to a terminal, the GUI needed it
/// just the same. Local re-import so the whole module (and its tests) call
/// it by its short name, with no change to a single render output.
use norte_frontend::middle_ellipsis;
