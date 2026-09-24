//! Where everything goes: resolving the layout for a frame and translating
//! it into `Rect`s.
//!
//! Nothing here paints. This is what `draw` consults before laying out the
//! frame, and also what mouse routing consults to know what is under the
//! cursor without having painted.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Block, Borders};

use super::chrome::TabStrip;
use super::compare::compare_layout;
use super::sync::sync_layout;
use crate::app::App;

/// Rows of the tasks panel in a frame (cap 6): part of [`draw`]'s layout,
/// pulled out so [`pane_list_rows`] counts the SAME thing that is painted.
pub(crate) fn tasks_rows(app: &App) -> u16 {
    u16::try_from(app.board.rows().len().min(6)).unwrap_or(6)
}

/// LISTING rows each pane paints in a frame of height `frame_height` (#124):
/// the frame's height minus the tasks panel and the status bar ([`super::draw`]'s
/// layout), minus the pane block's two borders and its column-header line
/// (`draw_pane`, private). The run loop feeds it back to the model
/// (`PaneState::set_viewport_rows`) so paging and the stat probe stop
/// guessing the viewport. A render test anchors it against the rows that
/// really appear in the buffer — if the layout changes, that test fails
/// here.
#[must_use]
pub fn pane_list_rows(app: &App, area: Rect) -> u16 {
    // With the viewer open no pane is painted: 0 visible rows.
    if app.viewer.is_some() {
        return 0;
    }
    // The height comes from the LAYOUT, not from manually subtracting the
    // tasks strip and the bar: those two are already gaps in the tree.
    // What is left here is the pane's own chrome, which the tree does not
    // know about.
    // Block borders (2) + column header (1). A pane this frame does not
    // paint (the `Split` collapsed) has no listing rows.
    pane_rects(app, area)
        .first()
        .map_or(0, |r| r.height)
        .saturating_sub(pane_chrome_rows(app, 0))
}

/// What has to be done to the MODEL right before painting a frame of
/// `height` rows: getting each pane's window ready.
///
/// Lives here and not loose in the run loop because tests paint on their
/// own and have to go through the same thing — if this only lived in the
/// loop, a test would paint with a window nobody reconciled and check a
/// screen no user ever sees.
///
/// BEFORE the draw and not after: the cursor is already where the key left
/// it, so this decides which rows are visible and the draw paints them. The
/// other way around cost a frame of lag, and the delayed frame is exactly
/// the one the user is looking at when the cursor touches the edge.
pub fn before_frame(app: &mut App, area: Rect) {
    app.last_frame = Some(area);
    let res = resolved_frame(app, area);
    // Who is seen where: with tabs, each side's slot changes.
    let vis = visible_browsers(&res, &app.layout);
    let order: Vec<_> = vis.iter().map(|(id, _)| *id).collect();
    // Both together, always: two order lists that can go out of sync are a
    // bug that only shows up when switching tabs.
    app.panes.set_visible(&order);
    app.history.set_order(&order);
    let cols = pane_cols(&res, &app.layout);
    // The terminal panel (#362): the pty is told the size it really got,
    // and whatever the shell wrote is flushed.
    //
    // It goes HERE for the same reason as everything else in this function:
    // it is before the draw and with `&mut`, so the frame that gets painted
    // already carries the latest that arrived. Doing it after would cost a
    // frame of lag on every keystroke, which in a shell shows up as a slow
    // echo.
    //
    // The pty learns the size or a full-screen program keeps painting for
    // the previous one, and what shows is garbage. `resize` does
    // nothing if it did not change.
    if let Some((_, rect)) = placed_of_kind(&res, &app.layout, crate::termpanel::KIND)
        && let Some(t) = app.terminal.as_mut()
    {
        // The frame is subtracted: the shell paints INSIDE.
        t.resize((rect.width.saturating_sub(2), rect.height.saturating_sub(2)));
        t.pump();
    }
    // And if the shell left, the panel stops having a shell: it is released
    // so the slot says so instead of showing the last screen of a process
    // that no longer exists. The slot stays — closing it on its own would
    // move the reader's layout without them asking for it.
    if app
        .terminal
        .as_mut()
        .is_some_and(crate::termpanel::TermPanel::dead)
    {
        app.terminal = None;
        if app.key_owner() == crate::app::KeyOwner::Terminal {
            app.release_keyboard();
        }
    }
    // Focus cannot stay on a pane this frame does not paint: that would be
    // a keyboard moving a cursor nobody sees. With two sides this is
    // `position`; once there are N slots `layout::focus_next` will do it.
    // Focus cannot point at a position this frame does not paint.
    if app.focus() >= cols.len() && !cols.is_empty() {
        app.set_focus(0);
    }
    // And the roles are brought up to date with what is on screen: `active`
    // is the focus, `target` is the other one if it is still visible.
    let focus = app.panes.slot_of(app.focus());
    let (tree, kinds) = (app.layout.clone(), app.kinds.clone());
    app.roles.reconcile(&tree, &res, &kinds, focus);
    // One window PER PANE: the one that is not painted has no rows, and
    // reconciling its own against the other one's height would leave it a
    // window nobody saw.
    let viewer_open = app.viewer.is_some();
    for i in 0..app.panes.len() {
        let rows = if viewer_open {
            0
        } else {
            usize::from(
                cols.get(i)
                    .map_or(0, |r| r.height)
                    .saturating_sub(pane_chrome_rows(app, i)),
            )
        };
        app.panes[i].reconcile_viewport(rows);
    }
    // The OTHER two long lists (#210). The height comes from replicating
    // here the same truncation its `draw` does, for the same reason
    // `pane_geometry` replicates its own: if the layout changes, what
    // breaks is the test right next to it and not a silent scroll bug.
    let body = overlay_body(app, area);
    if let Some(view) = &mut app.sync {
        let (_, list, _) = sync_layout_rows(body, view);
        if let Some(plan) = view.state.plan_mut() {
            plan.reconcile_viewport(usize::from(list.height));
        }
    } else if let Some(view) = &mut app.compare {
        let (_, list, _, _) = compare_layout(block_inner(body));
        view.pane.reconcile_viewport(usize::from(list.height));
    }
    // The log (#323), for the same reason and with the same fix. It was
    // born with a GUESSED height — ten, whatever the slot brings on open —
    // while its `draw` used the real interior, which is eight: every page
    // skipped two lines, and the first one, four. Guessing the viewport
    // breaks scroll silently, which is exactly what this function exists to
    // not let happen.
    if let Some((_, rect)) = placed_of_kind(&res, &app.layout, crate::logview::KIND) {
        let inner = block_inner(rect);
        app.log_panel.set_viewport_rows(usize::from(inner.height));
    }
    // SETTINGS, with the same fix. It always painted from the top because
    // it fit on one screen; with ~30 it stopped fitting, and moving the
    // cursor down past the edge pushed it out of the box. It is reconciled
    // in LINES — section headers fall between rows — with the same plan and
    // the same height the drawing uses, so they cannot count differently.
    if let Some(settings) = &mut app.settings {
        let plan = super::overlays::settings_line_plan(settings);
        let cursor_line = plan
            .iter()
            .position(|l| *l == super::overlays::SettingsLine::Row(settings.cursor()))
            .unwrap_or(0);
        // The header that opens the cursor's section, if it does open one:
        // it enters the window WITH its row. Without this, the first row
        // (line 1, since 0 is "General") kept the scroll pinned at 1 and
        // the header never came back on scrolling up.
        let anchor = match cursor_line.checked_sub(1).and_then(|p| plan.get(p)) {
            Some(super::overlays::SettingsLine::Header(_)) => cursor_line - 1,
            _ => cursor_line,
        };
        let rows = super::overlays::settings_list_rows(area.height);
        settings.reconcile_viewport(cursor_line, anchor, plan.len(), rows);
    }
}

/// This frame's layout, with the `Auto`s already substituted.
///
/// The saved tree (`app.layout`) keeps its `Auto`s; the frame's does not.
/// Substituting here and not inside `resolve` is what keeps the engine pure
/// and free of closures in its signature.
/// The BORDERS that can be dragged in `area`'s frame.
///
/// A border is the gap between two ADJACENT slots of the layout: the left
/// one (or the top one) is the one that carries it, because it is the one
/// `Node::drag_border` knows how to name. What is stored is where the pair
/// begins and how much room it takes together, which is what turns a
/// pointer column into a fraction.
///
/// It comes from the SAME layout that paints, not a second count: two
/// calculations of where a border is are a border grabbed in one spot and
/// moved from another.
#[must_use]
pub fn resize_borders(app: &App, area: Rect) -> Vec<crate::mouse::ResizeBorder> {
    use norte_frontend::layout::Dir;
    let res = resolved_frame(app, area);
    // Only between PANELS. The status bar and the tasks strip are also
    // layout slots and also have borders, but they measure a fixed row and
    // dragging them means nothing — and offering them ate the last row of
    // the panel above, which IS theirs. "Panel" is what the shared registry
    // calls focusable.
    let panel = |id| {
        app.layout
            .kind_of(id)
            .and_then(|k| app.kinds.get(k))
            .is_some_and(|d| d.focusable)
    };
    // The pair is measured WHOLE, in the layout where the two are
    // neighbors: between the second listing and the details, the border
    // separates the body from the details, and measuring only the listing
    // gave the fraction of a different pair.
    let pair = |a, b, dir| {
        let (left, right) = app.layout.border_pair(a, b)?;
        norte_frontend::layout::border_span(&res, &left, &right, dir)
    };
    let mut out = Vec::new();
    for (a, ra) in &res.placements {
        if !panel(*a) {
            continue;
        }
        for (b, rb) in &res.placements {
            if !panel(*b) {
                continue;
            }
            // Vertical: `b` starts exactly where `a` ends, and they overlap
            // in rows. The `+ 1` is the border column, which in the TUI is
            // the frame both paint.
            if rb.x == ra.x + ra.width
                && overlaps(ra.y, ra.height, rb.y, rb.height)
                && let Some((start, len)) = pair(*a, *b, Dir::Horizontal)
            {
                out.push(crate::mouse::ResizeBorder {
                    slot: *a,
                    neighbor: *b,
                    dir: Dir::Horizontal,
                    line: ra.x + ra.width,
                    from: ra.y.max(rb.y),
                    until: (ra.y + ra.height).min(rb.y + rb.height),
                    start,
                    long: len,
                });
            }
            if rb.y == ra.y + ra.height
                && overlaps(ra.x, ra.width, rb.x, rb.width)
                && let Some((start, len)) = pair(*a, *b, Dir::Vertical)
            {
                out.push(crate::mouse::ResizeBorder {
                    slot: *a,
                    neighbor: *b,
                    dir: Dir::Vertical,
                    line: ra.y + ra.height,
                    from: ra.x.max(rb.x),
                    until: (ra.x + ra.width).min(rb.x + rb.width),
                    start,
                    long: len,
                });
            }
        }
    }
    out
}

/// The SLOTS placed in `area`'s frame, with their rectangle.
///
/// This is what turns a click into "which panel the pointer pointed at."
/// It comes from the SAME layout that paints, for the same reason as the
/// borders: a second count of where each panel is is a click that focuses
/// the one next door.
///
/// EVERY placed slot goes in, including the ones that do not take keys: who
/// listens is decided by `App::focus_slot` with the shared registry, not a
/// second table written here.
#[must_use]
pub fn panel_slots(app: &App, area: Rect) -> Vec<crate::mouse::PanelSlot> {
    resolved_frame(app, area)
        .placements
        .into_iter()
        .map(|(slot, r)| {
            // Without a panel group's strip: that row belongs to its zones.
            let r = slot_content(&app.layout, slot, crate::panel::to_ratatui(r));
            crate::mouse::PanelSlot {
                slot,
                x: r.x,
                y: r.y,
                width: r.width,
                height: r.height,
            }
        })
        .collect()
}

/// Do two spans `[a, a+la)` and `[b, b+lb)` overlap?
const fn overlaps(a: u16, la: u16, b: u16, lb: u16) -> bool {
    a < b + lb && b < a + la
}

pub(crate) fn resolved_frame(app: &App, area: Rect) -> norte_frontend::layout::Resolved {
    let tree = app.layout.substitute_auto(&|id| natural(app, id));
    norte_frontend::layout::resolve(
        crate::panel::from_ratatui(body_area(app, area)),
        &tree,
        &app.kinds,
    )
}

/// The area left for the BODY: the frame's minus the menu bar, if it is set.
///
/// The subtraction happens HERE and nowhere else. This is the only point
/// painting, mouse click mapping and the event loop's "which slot got
/// placed" decisions all go through, so subtracting once makes the three
/// line up on their own — and subtracting in the painter, the mouse would
/// have kept believing row 0 belongs to the panel above and every click
/// would have landed one row below where the reader gave it.
///
/// A one-row terminal runs out of body before it runs out of bar, and that
/// is why the subtraction saturates: a degraded screen is preferable to a
/// layout over a rectangle of negative height.
#[must_use]
pub(crate) fn body_area(app: &App, area: Rect) -> Rect {
    // Two possible chrome rows on top, each optional on its own: the menu's
    // and the panel bar's (#324). Whichever are present are subtracted, and
    // saturating — a degraded screen is preferable to a layout over
    // negative height.
    let rows = u16::from(app.menu_bar) + panel_bar_row(app);
    // And the key bar row BELOW (spec 2026-09-10): subtracted from the
    // height, not the origin. Same criterion as the two above: one
    // subtraction, here.
    let bottom = u16::from(key_bar_area(app, area).is_some());
    // And the panel bar in a COLUMN (spec 2026-09-21) is subtracted from
    // the width, from the origin. The question goes to `panel_bar_area`,
    // which already knows whether it fits: a rail that is not painted
    // cannot eat three columns.
    let left = if bar_in_column(app) {
        panel_bar_area(app, area).map_or(0, |r| r.width)
    } else {
        0
    };
    if rows + bottom + left == 0 || area.height == 0 {
        return area;
    }
    Rect {
        x: area.x.saturating_add(left),
        y: area.y.saturating_add(rows),
        width: area.width.saturating_sub(left),
        height: area.height.saturating_sub(rows).saturating_sub(bottom),
    }
}

/// Width of the panel bar in a column: `" S·"`, a button's cell in letters
/// (`panelbar::button_cell`), the same in a row or a column.
pub(crate) const RAIL_W: u16 = 3;

/// Does the panel bar go in a COLUMN? `[ui] panel_bar_position`, with the
/// terminal's answer for `auto`: on top, because width is short here.
#[must_use]
pub(crate) fn bar_in_column(app: &App) -> bool {
    app.panel_bar && app.chrome.panel_bar_position().vertical(false)
}

/// How many top rows the panel bar eats: one in a row, none in a column.
fn panel_bar_row(app: &App) -> u16 {
    u16::from(app.panel_bar && !bar_in_column(app))
}

/// The row where the key bar goes (spec 2026-09-10), if there is one: the
/// LAST one of the frame, as in mc, far and norton, with the status one
/// staying above it. The row is RESERVED even with an overlay in front —
/// opening a modal does not relayout the screen behind it, same as with the
/// panel bar —; what gets painted on it is decided by
/// `App::key_bar_cells`, and with a modal that is nothing.
#[must_use]
pub(crate) fn key_bar_area(app: &App, area: Rect) -> Option<Rect> {
    // With all three bars on a three-row terminal there is no body left;
    // the key one is the one that yields: `<=` so it does not fall outside
    // the buffer.
    let top = u16::from(app.menu_bar) + panel_bar_row(app);
    if !app.chrome.key_bar() || area.height <= top.saturating_add(1) {
        return None;
    }
    Some(Rect {
        y: area.y.saturating_add(area.height).saturating_sub(1),
        height: 1,
        ..area
    })
}

/// Where the panel bar goes, if there is one: a row, or a column.
///
/// Below the menu one when both are on: the menu names what can be done and
/// the bar shows where it is, so top-to-bottom order goes from the general
/// to the specific. In a column (`[ui] panel_bar_position = "left"`) it goes
/// on the left edge, from below the menu to above the key bar.
#[must_use]
pub(crate) fn panel_bar_area(app: &App, area: Rect) -> Option<Rect> {
    // `<=` and not `== 0`: with both bars on in a one-row terminal, the
    // panel one would fall OUTSIDE the buffer. Ratatui truncates and does
    // not crash, but the clickable zones would be published over a row
    // that does not exist.
    if !app.panel_bar || area.height <= u16::from(app.menu_bar) {
        return None;
    }
    if bar_in_column(app) {
        // A terminal that leaves no body next to the rail loses the rail:
        // better a listing with no buttons than buttons with no listing.
        let bottom = u16::from(key_bar_area(app, area).is_some());
        let height = area
            .height
            .saturating_sub(u16::from(app.menu_bar))
            .saturating_sub(bottom);
        if area.width <= RAIL_W || height == 0 {
            return None;
        }
        return Some(Rect {
            x: area.x,
            y: area.y.saturating_add(u16::from(app.menu_bar)),
            width: RAIL_W,
            height,
        });
    }
    Some(Rect {
        y: area.y.saturating_add(u16::from(app.menu_bar)),
        height: 1,
        ..area
    })
}

/// Is the panel bar visible RIGHT NOW?
///
/// Different from [`panel_bar_area`], which is geometry: the row's gap is
/// subtracted from the body no matter what is in front — otherwise opening
/// a modal would relayout the whole screen behind it — but with an overlay
/// in front the bar is neither painted nor clickable.
///
/// Exists because not having it was a BLOCKER: the bar was painted before
/// the overlays and its zones stayed active underneath, so with help open a
/// click on help's title bar — row 1 — landed on a button and opened or
/// closed an invisible panel. Painted and clickable have to be the same
/// thing, and the way to guarantee it is for both to ask here.
#[must_use]
pub(crate) fn panel_bar_visible(app: &App, area: Rect) -> Option<Rect> {
    if crate::mouse::overlay_open(app) || app.menu.is_some() {
        return None;
    }
    panel_bar_area(app, area)
}

/// This frame's layout, for whoever does not paint.
///
/// `pub` because the run loop needs to know which slots got PLACED to
/// decide what to request: a preview that was not placed does not read
/// (spec rule 2), and only the layout knows that.
#[must_use]
pub fn resolved_for(app: &App, area: Rect) -> norte_frontend::layout::Resolved {
    resolved_frame(app, area)
}

/// The size a slot asks for by its CONTENT.
///
/// Only the tasks strip has one: `min(tasks, 6)` rows, and zero at rest. It
/// is the only thing on screen the tree cannot know on its own.
pub(crate) fn natural(app: &App, id: norte_frontend::layout::SlotId) -> (u16, u16) {
    if id == crate::panel::SLOT_TASKS {
        (0, tasks_rows(app))
    } else {
        (0, 0)
    }
}

/// Where a slot landed in this layout.
pub(crate) fn slot_rect(
    res: &norte_frontend::layout::Resolved,
    id: norte_frontend::layout::SlotId,
) -> Option<Rect> {
    res.placements
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, r)| crate::panel::to_ratatui(*r))
}

/// The first PLACED slot with that kind, and where it landed.
///
/// From the LAYOUT and not the tree: whoever paints can only paint what got
/// placed, and a slot behind a tab or inside a collapsed `Split` did not get
/// placed. That is where a hidden slot's suspension stops being a written
/// rule and becomes the only thing the code can do.
pub(crate) fn placed_of_kind(
    res: &norte_frontend::layout::Resolved,
    tree: &norte_frontend::layout::Node,
    kind: &str,
) -> Option<(norte_frontend::layout::SlotId, Rect)> {
    res.placements
        .iter()
        .find(|(id, _)| tree.kind_of(*id).is_some_and(|k| k.as_str() == kind))
        .map(|(id, r)| (*id, slot_content(tree, *id, crate::panel::to_ratatui(*r))))
}

/// Where the CONTENT of a slot placed in `rect` lands.
///
/// In a panel group (ADR 0134) the first row is the tab STRIP, and the
/// content starts one row below. ONE count for the painter and for the
/// mouse: with two, a click on the disk map picked the child next door.
pub(crate) fn slot_content(
    tree: &norte_frontend::layout::Node,
    id: norte_frontend::layout::SlotId,
    mut rect: Rect,
) -> Rect {
    if panel_group(tree, id).is_some() && rect.height > 1 {
        rect.y = rect.y.saturating_add(1);
        rect.height -= 1;
    }
    rect
}

/// `id`'s PANEL group (ADR 0134): its slots and which one is in front, if
/// `id` lives in a tab alongside another panel. A group with a listing
/// inside is a listing's own tab strip, and that one has its own strip.
#[must_use]
pub(crate) fn panel_group(
    tree: &norte_frontend::layout::Node,
    id: norte_frontend::layout::SlotId,
) -> Option<(Vec<norte_frontend::layout::SlotId>, usize)> {
    let (slots, active) = tree.tabs_of(id)?;
    (slots.len() >= 2
        && slots
            .iter()
            .all(|s| tree.kind_of(*s).is_some_and(|k| k.as_str() != "browser")))
    .then_some((slots, active))
}

/// A tab in a panel group's strip: where it lands, which slot it carries
/// and whether it is the one in front.
pub(crate) struct PanelTab {
    /// The label, with a space on each side.
    pub text: String,
    /// First column.
    pub x0: u16,
    /// Last column, inclusive.
    pub x1: u16,
    /// The slot inside.
    pub slot: norte_frontend::layout::SlotId,
    /// Is the one shown.
    pub active: bool,
}

/// The frame's panel groups' tab strips (ADR 0134): the row and its tabs.
/// ONE measurement for painting and for the mouse.
#[must_use]
pub(crate) fn panel_tab_strips(app: &App, area: Rect) -> Vec<(Rect, Vec<PanelTab>)> {
    let res = resolved_frame(app, area);
    let lang = norte_i18n::active();
    let mut out = Vec::new();
    for (id, r) in &res.placements {
        let Some((slots, active)) = panel_group(&app.layout, *id) else {
            continue;
        };
        let rect = crate::panel::to_ratatui(*r);
        // A slot with no height has no row of its own: painting it would
        // step on the neighbor's.
        if rect.height == 0 {
            continue;
        }
        let row = Rect { height: 1, ..rect };
        let ceiling = rect.x.saturating_add(rect.width);
        let labels: Vec<(String, u16)> = slots
            .iter()
            .map(|s| {
                let kind = app
                    .layout
                    .kind_of(*s)
                    .map_or("", norte_frontend::layout::KindId::as_str);
                let name =
                    norte_frontend::panelbar::label_in(lang, kind, &format!("layout.{kind}"));
                let text = format!(" {} ", norte_frontend::display_name(name.as_bytes()).0);
                let w = u16::try_from(super::text::cells(&text)).unwrap_or(u16::MAX);
                (text, w)
            })
            .collect();
        // The one in front is reserved before anything else: a narrow strip
        // that eats exactly that one does not say which panel is being
        // shown.
        let active_w = labels.get(active).map_or(0, |(_, w)| *w);
        let mut x = rect.x;
        let mut tabs = Vec::new();
        for (i, ((text, w), s)) in labels.into_iter().zip(&slots).enumerate() {
            let reserve = if i < active { active_w } else { 0 };
            // A tab that does not fit whole is neither painted nor
            // clickable.
            if x.saturating_add(w).saturating_add(reserve) > ceiling {
                continue;
            }
            tabs.push(PanelTab {
                text,
                x0: x,
                x1: x.saturating_add(w).saturating_sub(1),
                slot: *s,
                active: i == active,
            });
            x = x.saturating_add(w);
        }
        out.push((row, tabs));
    }
    out
}

/// The BODY: the bounding box of the placed `browser`s.
///
/// With only one placed — the `Split` collapsed — the box is that same one,
/// which is exactly the spot a viewer or a differences panel must occupy.
pub(crate) fn body_rect(
    res: &norte_frontend::layout::Resolved,
    tree: &norte_frontend::layout::Node,
) -> Option<Rect> {
    let mut bbox: Option<Rect> = None;
    for (_, r) in visible_browsers(res, tree) {
        bbox = Some(match bbox {
            None => r,
            Some(c) => {
                let x = c.x.min(r.x);
                let y = c.y.min(r.y);
                Rect {
                    x,
                    y,
                    width: (c.x + c.width).max(r.x + r.width) - x,
                    height: (c.y + c.height).max(r.y + r.height) - y,
                }
            }
        });
    }
    bbox
}

/// The body computed by hand, for when the layout places no pane at all.
///
/// Does not happen with the `orthodox` preset; it exists because a layout
/// with no `browser` cannot leave an open viewer with nowhere to go.
///
/// It builds on [`body_area`] and not the frame: without that, the fallback
/// body started below the panel rail (and the chrome rows) instead of
/// beside it.
pub(crate) fn chrome_body(app: &App, area: Rect) -> Rect {
    let area = body_area(app, area);
    let height = area
        .height
        .saturating_sub(tasks_rows(app))
        .saturating_sub(1);
    Rect { height, ..area }
}

/// The area the panes — or whatever panel replaces them — occupy in `area`.
pub(crate) fn overlay_body(app: &App, area: Rect) -> Rect {
    let res = resolved_frame(app, area);
    body_rect(&res, &app.layout).unwrap_or_else(|| chrome_body(app, area))
}

/// The `browser`s this layout DOES paint, left to right.
///
/// With tabs there are more than two live listings and only two visible, so
/// "the left pane" stops being a fixed id and becomes a POSITION: the
/// leftmost placed browser. Sorting by `(x, y)` is exactly what the user
/// sees, and it is what keeps `app.panes[0]`'s meaning intact.
pub(crate) fn visible_browsers(
    res: &norte_frontend::layout::Resolved,
    tree: &norte_frontend::layout::Node,
) -> Vec<(norte_frontend::layout::SlotId, Rect)> {
    let mut v: Vec<_> = res
        .placements
        .iter()
        .filter(|(id, _)| {
            tree.kind_of(*id)
                .is_some_and(|k| *k == norte_frontend::layout::KindId::browser())
        })
        .map(|(id, r)| (*id, crate::panel::to_ratatui(*r)))
        .collect();
    v.sort_by_key(|(_, r)| (r.x, r.y));
    v
}

/// Where each pane lands, or `None` if this frame does not paint it.
///
/// A `None` is not an error: the `Split` collapsed because the body does
/// not give room for twice the `browser`'s minimum, and the other one
/// paints at full width. Whoever had focus there loses it in
/// [`before_frame`].
pub(crate) fn pane_cols(
    res: &norte_frontend::layout::Resolved,
    tree: &norte_frontend::layout::Node,
) -> Vec<Rect> {
    visible_browsers(res, tree)
        .into_iter()
        .map(|(_, r)| r)
        .collect()
}

/// Like [`pane_cols`], resolving the frame on its own.
pub(crate) fn pane_rects(app: &App, area: Rect) -> Vec<Rect> {
    pane_cols(&resolved_frame(app, area), &app.layout)
}

/// The tabs of the pane on side `side`, if it is in a group.
///
/// `pub` because the mouse needs the same titles to measure the zones.
///
/// Each one's title is its slot's directory name, sanitized by
/// `display_name`: a directory with a hostile name inside a tab is as
/// hostile as inside a listing (rule 1).
#[must_use]
pub fn tab_strip_for(app: &App, side: usize) -> Option<TabStrip> {
    let slot = app.panes.slot_of(side);
    let (slots, active) = app.layout.tabs_of(slot)?;
    let titles = slots
        .iter()
        .map(|id| {
            app.panes
                .browser(*id)
                .map(|p| {
                    let bytes = p
                        .dir()
                        .file_name()
                        .map_or_else(Vec::new, |s| s.as_bytes().to_vec());
                    if bytes.is_empty() {
                        p.dir().scheme().to_owned()
                    } else {
                        norte_frontend::display_name_with(&bytes, p.name_encoding()).0
                    }
                })
                .unwrap_or_default()
        })
        .collect();
    Some(TabStrip { titles, active })
}

/// How many of a pane's rows are CHROME: the two borders, the column header
/// and, if it is in a group, the tab bar.
pub(crate) fn pane_chrome_rows(app: &App, side: usize) -> u16 {
    3 + u16::from(tab_strip_for(app, side).is_some())
}

/// The interior of a block bordered on all four sides.
pub(crate) fn block_inner(area: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(area)
}

/// The full-screen viewer's layout in its TWO rows: the content frame (with
/// its borders — what `draw_viewer`'s `Block` receives) and the one-row
/// status bar below it.
///
/// The only function that does this count: [`rect_del_visor`] is its first
/// half, and `draw_viewer` takes both from here instead of repeating the
/// `Layout::split` by hand — two counts of the same slot drift apart
/// silently (memory `funcion-compartida-no-basta`).
pub(crate) fn visor_split(app: &App, area: Rect) -> (Rect, Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(body_area(app, area));
    (rows[0], rows[1])
}

/// The slot where the full-screen viewer really paints its CONTENT: the
/// frame's INTERIOR, with NO borders — where `draw_viewer` leaves the lines
/// blank when an image is placed.
///
/// `pub` because the run loop (T4, phase 5 WOW) needs it after
/// `terminal.draw` to know where to place the pixels. Review, CRITICAL 2:
/// the first version returned the frame WITH borders (`visor_split(...).0`
/// alone) — two cells too many per axis, right over the border and the
/// scrollbars `viewer_scrollbars` paints there — so now it applies
/// `block_inner` (private, not exported) itself: matching the slot
/// `draw_viewer` leaves empty is STRUCTURAL, not something to remember at
/// every call site. It comes from `visor_split` (also private), the SAME
/// count that paints the frame — not a copy.
#[must_use]
pub fn rect_del_visor(app: &App, area: Rect) -> Rect {
    block_inner(visor_split(app, area).0)
}

/// Like [`sync_layout`], from the panel's EXTERNAL area (the one
/// `draw_sync` receives): subtracts the border before laying out.
pub(crate) fn sync_layout_rows(
    area: Rect,
    view: &crate::app::SyncView,
) -> (Option<Rect>, Rect, Option<Rect>) {
    sync_layout(block_inner(area), view)
}

/// The PAINTED geometry of the two panes in an `area` frame, or `None` when
/// this frame paints no panes (viewer open).
///
/// The layout is NO LONGER computed here: it comes from `pane_rects`, the
/// same call `draw` uses. What still lives here is the CHROME — the block's
/// borders and the column header — which is what turns a pane rectangle
/// into listing rows.
///
/// Same treatment as [`pane_list_rows`] (#124): the draw is the one that
/// knows where everything landed, so the run loop feeds this back to the
/// model ([`crate::mouse::after_frame`]) after every frame and the mouse
/// resolves its clicks against the LAST screen the user saw, not one
/// recalculated by eye. It is computed here, next to the layout it
/// replicates, so that changing it breaks the geometry test right next to
/// it and not the mouse silently.
///
/// A pane's rows, top to bottom: top border (1), column header (1), the
/// listing, bottom border (1). Columns: left border (1), content, right
/// border (1). Everything that is not listing is CHROME, and a click there
/// resolves to "this pane, no row."
#[must_use]
pub fn pane_geometry(app: &App, area: Rect) -> Option<Vec<crate::mouse::PaneGeometry>> {
    // Not with the viewer nor the differences panel: both replace the
    // panes, and a geometry for something not painted is a click resolved
    // against a row the reader cannot see.
    if app.viewer.is_some() || app.compare.is_some() {
        return None;
    }
    let cols = pane_rects(app, area);
    // One `PaneGeometry` per PAINTED panel. Its length varies with the
    // layout, and the hit test resolves against the last frame's — which is
    // what the reader had in front of them.
    let mut out = vec![crate::mouse::PaneGeometry::default(); cols.len()];
    for (i, pane) in app.panes.iter().enumerate() {
        let Some(block) = cols.get(i).copied() else {
            continue;
        };
        // Interior of the block with `Borders::ALL`, without building the
        // block: a margin of 1 per side. `title_bottom` (the quick search
        // input) does NOT consume rows — it is painted over the bottom
        // border.
        let inner_w = block.width.saturating_sub(2);
        let inner_h = block.height.saturating_sub(2);
        // The column header eats the interior's first row, and the tab
        // bar — if the pane is in a group — another one above it.
        let chrome = pane_chrome_rows(app, i).saturating_sub(2);
        let list_rows = inner_h.saturating_sub(chrome);
        out[i] = crate::mouse::PaneGeometry {
            x: block.x,
            y: block.y,
            width: block.width,
            height: block.height,
            first_list_row: block.y.saturating_add(1).saturating_add(chrome),
            list_rows: if inner_w == 0 || inner_h == 0 {
                0
            } else {
                list_rows
            },
            // The window is decided by the MODEL (sticky), and the hit test
            // reads exactly the one that was painted: deriving it again
            // here is how a click ends up resolved against the row next
            // door.
            offset: pane.viewport_offset(),
        };
    }
    Some(out)
}

pub(crate) fn centered(base: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(base.width);
    let h = h.min(base.height);
    Rect {
        x: base.x + (base.width - w) / 2,
        y: base.y + (base.height - h) / 2,
        width: w,
        height: h,
    }
}
