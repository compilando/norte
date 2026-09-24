//! The focused pane's status bar: its mark segments and the line that joins
//! them.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use super::HOSTILE_BADGE;
use super::text::cells;
use crate::app::{App, Pane};
use norte_i18n::{t, ta};

/// The status bar's pruned-marks notice (#103), with its indent. The
/// marking itself no longer lives here: it is the `marks` element of the
/// right half (ADR 0132); PRUNING is a notice and stays on the left.
pub(crate) fn pruned_segment(pane: &Pane) -> String {
    // The sentence is DRAFTED by the shared crate: the window puts the same
    // one in its header, and two draftings of the same fact is where half
    // of the parity audit came from (ADR 0077). What is left here is the
    // spacing, which does belong to this bar.
    let s = norte_frontend::notes::pruned_marks(pane.pruned_marks(), norte_i18n::active());
    if s.is_empty() { s } else { format!("  {s}") }
}

pub(crate) fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let c = compose(app, area);
    frame.render_widget(
        Paragraph::new(c.text).style(app.theme.role(Role::StatusBar)),
        area,
    );
}

/// A clickable item of the right half (ADR 0132): where it lands and what
/// command runs, through the same dispatch as its shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusItemZone {
    /// Row.
    pub row: u16,
    /// First column, inclusive.
    pub x0: u16,
    /// Last column, inclusive.
    pub x1: u16,
    /// The command, from the catalogue.
    pub command: &'static str,
}

/// The zones of the clickable items, for frame `area`.
#[must_use]
pub fn status_item_zones(app: &App, area: Rect) -> Vec<StatusItemZone> {
    let Some(status) = status_rect(app, area) else {
        return Vec::new();
    };
    compose(app, status)
        .items
        .into_iter()
        .map(|(x0, x1, command)| StatusItemZone {
            row: status.y,
            x0,
            x1,
            command,
        })
        .collect()
}

/// The facts the right half needs, from the focused pane.
fn status_input(app: &App) -> norte_frontend::statusbar::StatusInput {
    norte_frontend::statusbar::StatusInput::from_pane(
        app.focused().state(),
        app.strip.view(app.now_ms()),
        app.notices_unread,
    )
}

/// Cells between two adjacent items.
const SEP: usize = 2;

/// The status bar's rectangle for frame `area`, or `None` with an overlay in
/// front: then it is not clickable, by the same rule as the panel bar
/// (`panel_bar_visible`).
fn status_rect(app: &App, area: Rect) -> Option<Rect> {
    use super::geometry::{body_rect, chrome_body, resolved_frame, slot_rect};
    if crate::mouse::overlay_open(app) || app.menu.is_some() {
        return None;
    }
    let res = resolved_frame(app, area);
    let body = body_rect(&res, &app.layout).unwrap_or_else(|| chrome_body(app, area));
    Some(slot_rect(&res, crate::panel::SLOT_STATUS).unwrap_or(Rect {
        x: body.x,
        y: body.y.saturating_add(body.height).saturating_sub(1),
        width: body.width,
        height: 1,
    }))
}

/// What composes the bar: the text, and the columns of what is clickable.
struct Composed {
    text: String,
    session: Option<(u16, u16)>,
    /// The clickable items on the right: columns and command.
    items: Vec<(u16, u16, &'static str)>,
}

/// Where the loose-session indicator lands in the frame, if it is being
/// painted.
///
/// This is the status bar's clickable zone: a click on it opens help at the
/// page that explains what it means. It comes from the SAME composition
/// that paints the line (`compose`), so it only exists when the indicator
/// is really on screen — with a message, a wait or a live search in front,
/// the line is a different one and there is nothing to click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionZone {
    /// Row.
    pub row: u16,
    /// First column, inclusive.
    pub x0: u16,
    /// Last column, inclusive.
    pub x1: u16,
}

/// The session indicator's zone for frame `area`, if it is painted.
///
/// The bar's rectangle comes from the same layout as `draw_body`, and by the
/// same path: a click resolved against different geometry would land on the
/// cell next door.
#[must_use]
pub fn session_zone(app: &App, area: Rect) -> Option<SessionZone> {
    // With an overlay in front there is no zone (`status_rect`): the viewer
    // paints its own footer and not this bar, and with help or a modal on
    // top the bar is not clickable. `handle_at` already short-circuits
    // earlier via `overlay_open`, but that is an order of checks, not a
    // guarantee of this function.
    let status = status_rect(app, area)?;
    compose(app, status).session.map(|(x0, x1)| SessionZone {
        row: status.y,
        x0,
        x1,
    })
}

/// The status line, and the columns of what is clickable on it.
///
/// Two halves (ADR 0132). The RIGHT one is the `[ui] status_items` elements
/// that fit in half the width, dropped by priority (`statusbar::fit`); the
/// LEFT one is the usual string — wait, drag, message, search, path with its
/// notices — over whatever width is left, and it is the one that yields:
/// truncated, never pushed out.
fn compose(app: &App, area: Rect) -> Composed {
    // The plugins' first, to the left of the right half (ADR 0137); they
    // are the first to yield, so order gives them no room.
    let mut list = norte_frontend::statusbar::plugin_items(
        app.focused().state(),
        &app.status_plugins,
        norte_i18n::active(),
    );
    list.extend(norte_frontend::statusbar::items(
        &status_input(app),
        app.chrome.status_items(),
        norte_i18n::active(),
    ));
    let width = usize::from(area.width);
    // A persistent notice (loose session, journal) has to fit WHOLE on the
    // left: the right side is information and yields before a notice does.
    // The MINIMUM `compose_line` paints with it is ` {notice}{sequence}`:
    // the margin, the whole notice and whatever was typed halfway, which
    // also cannot be lost (ADR 0006). And the right side's budget deducts
    // its own two margins, which `fit` does not count.
    let notice_reserve = app.persistent_banner().map_or(0, |w| {
        let seq = if app.pending.is_empty() {
            0
        } else {
            cells(&format!("  [{} …]", app.pending))
        };
        1 + cells(&w) + seq
    });
    let budget = (width / 2)
        .min(width.saturating_sub(notice_reserve))
        .saturating_sub(2);
    let chosen = norte_frontend::statusbar::fit(&list, budget, SEP);
    let right: Vec<&norte_frontend::statusbar::StatusItemView> = chosen.iter().collect();
    if right.is_empty() {
        return compose_line(app, area);
    }
    // A space in front of the first one and one behind the last, like the
    // left side's margins.
    let right_w = right.iter().map(|v| v.cells()).sum::<usize>() + SEP * (right.len() - 1) + 2;
    let left_w = width.saturating_sub(right_w);
    let left = Rect {
        width: u16::try_from(left_w).unwrap_or(u16::MAX),
        ..area
    };
    let mut c = compose_line(app, left);
    let text = super::text::take_width(&c.text, left_w);
    let pad = left_w.saturating_sub(cells(&text));
    let mut line = format!("{text}{} ", " ".repeat(pad));
    let mut x = left_w + 1;
    for (n, v) in right.iter().enumerate() {
        if n > 0 {
            line.push_str(&" ".repeat(SEP));
            x += SEP;
        }
        line.push_str(&v.text);
        // The light bar (ADR 0146) goes behind the text, separated by a
        // space; `cells()` already counts it in the layout.
        if v.bar {
            line.push(' ');
            line.push_str(&norte_frontend::task_strip::bar_glyphs(
                v.progress.and_then(|p| p.percent),
                app.now_ms(),
            ));
        }
        let w = v.cells();
        if let Some(cmd) = v.command {
            let x0 = area.x.saturating_add(u16::try_from(x).unwrap_or(u16::MAX));
            let x1 = x0.saturating_add(u16::try_from(w).unwrap_or(u16::MAX).saturating_sub(1));
            c.items.push((x0, x1, cmd));
        }
        x += w;
    }
    line.push(' ');
    c.text = line;
    c
}

/// A path shorter than this many cells does not say where you are: with a
/// persistent notice that shortened it further, the notice keeps the whole
/// line, as it always did.
const LEGIBLE_PATH: usize = 12;

/// The session indicator's columns when the persistent notice ends at cell
/// `end` (exclusive, relative to the bar): the indicator closes the notice
/// (`persistent_banner`), so it is its last cells. `None` if there is no
/// indicator or it does not fit WHOLE: half a word is not an indicator.
fn session_zone_at(app: &App, area: Rect, end: usize) -> Option<(u16, u16)> {
    let badge = app.session_banner()?;
    let width = cells(&badge);
    let x0 = area
        .x
        .saturating_add(u16::try_from(end.saturating_sub(width)).unwrap_or(u16::MAX));
    let x1 = area
        .x
        .saturating_add(u16::try_from(end.saturating_sub(1)).unwrap_or(u16::MAX));
    (x1 < area.x.saturating_add(area.width)).then_some((x0, x1))
}

/// The status line without the badge, and the session indicator's columns
/// if it is in it.
fn compose_line(app: &App, area: Rect) -> Composed {
    let mut session = None;
    let pane = app.focused();
    // Position and marking have been right-half elements since ADR 0132
    // (`position`, `marks`); what is left here are the path and the
    // NOTICES.
    let pruned = pruned_segment(pane);
    let (dir_text, dir_hostile) =
        norte_frontend::path_display_with(pane.dir(), pane.name_encoding());
    let mark = if dir_hostile { HOSTILE_BADGE } else { "" };
    // No key cheat sheet: it would lie depending on the preset. The pending
    // sequence IS painted (ADR 0006), and since K3a the which-key panel
    // ([`draw_which_key`]) paints over this bar what can FOLLOW that
    // sequence. This segment does not disappear with it: it is the only
    // line that survives a panel truncated on a short terminal.
    let seq = if app.pending.is_empty() {
        String::new()
    } else {
        format!("  [{} …]", app.pending)
    };
    // A pending message (error by category, result) displaces the rest of
    // the bar until the next key (issue #20). With no message: a pane with
    // a live search (liveSearch T6) paints `search-status-*` (hits =
    // `entries.len()`); otherwise, the statusbar Lua hook (M4, already
    // sanitized by the host) replaces the focused pane's default line.
    // A drag IN FLIGHT overrides everything else while it lasts. It is the
    // only thing on this line that announces a MUTATION about to be
    // proposed, and the gesture needs the decision (copy or move) BEFORE
    // the button comes up: without this row the user drops blind. It lasts
    // as long as the button is held and consumes nothing — whatever
    // message it covers is still there once released.
    // A wait IN PROGRESS overrides everything else: while it lasts, anything
    // else on this line — the previous operation's message, the counter —
    // describes a state that is no longer current, and the reader is
    // looking at it precisely because they want to know if the program is
    // still alive. It leaves on its own once the wait ends (`App::busy` is
    // cleared by whoever waited), so it does not consume or cover anything
    // permanently. Before the threshold it does not enter here: `visible()`
    // decides for every surface.
    let text = if let Some(busy) = app.busy.as_ref().filter(|b| b.visible()) {
        format!(
            " {} {}  {}",
            busy.frame(),
            t(busy.kind.key()),
            t("busy-cancel")
        )
    } else if let Some(drag) = crate::mouse::drop_hint(app) {
        format!(" {drag}")
    } else if let Some(msg) = &app.message {
        format!(" {msg}")
    } else if pane.virtual_search {
        use crate::app::SearchState;
        // `Failed` is PERSISTENT (review MINOR-2): after `app.message` is
        // cleared, the pane keeps painting `search-status-failed` with the
        // error's category (kept in `search_error`) — a failure never
        // degrades to "done" on the next key.
        if pane.search_state == SearchState::Failed {
            format!(
                " {}{seq}",
                ta(
                    "search-status-failed",
                    &[("error", pane.search_error.as_deref().unwrap_or(""))],
                )
            )
        } else {
            let key = match pane.search_state {
                SearchState::Running => "search-status-running",
                SearchState::Truncated => "search-status-truncated",
                SearchState::Cancelled => "search-status-cancelled",
                // `Failed` was already handled above; `Done` is the rest.
                SearchState::Done | SearchState::Failed => "search-status-done",
            };
            // #81: context for the content match of the hit UNDER THE
            // CURSOR (line + preview — sanitized at the source by the core;
            // passed through detail_for_bar as a belt, same criterion as
            // errors).
            let hit = pane
                .entries()
                .get(pane.cursor())
                .and_then(|e| pane.search_matches.get(&e.path))
                .map_or_else(String::new, |m| {
                    let line = m.line.map_or_else(String::new, |l| format!(":{l}"));
                    let preview = m.preview.as_deref().map_or_else(String::new, |p| {
                        format!(" {}", crate::app::detail_for_bar(p))
                    });
                    format!("  [{line}{preview}]")
                });
            format!(
                " {}{hit}{seq}",
                ta(key, &[("n", &pane.entries().len().to_string())])
            )
        }
    } else if let Some(lua) = &app.lua_status {
        format!(" {lua}{seq}")
    } else {
        // #44: remote session degraded to plain text, and #177: a session
        // that mutates without being recorded in the journal. PERSISTENT
        // (like `search-status-failed`): they survive keystrokes — with no
        // `message`, no live search and no Lua hook they keep warning every
        // frame. H3d: the sentence is COMPOSED here from the structured
        // value (one connection: names it; several: how many), instead of
        // being stored already written.
        //
        // Since 2026-09-11 the notice does NOT replace the line: it goes to
        // the RIGHT of the path and the counter, which stay put. Replacing
        // it left whoever had a loose session all day without "file x/x."
        // Only if it fits with a legible path; otherwise, the notice alone,
        // as before.
        let warn = app.persistent_banner();
        let reserve = warn.as_ref().map_or(0, |w| cells(w) + 2);
        // #93: the container skipped entries from its index — the listing
        // shown is NOT everything the archive contains. Persistent while
        // the pane stays inside it (parallel to the hostile badge, never
        // silent).
        let indented = |s: String| if s.is_empty() { s } else { format!("  {s}") };
        let lang = norte_i18n::active();
        let skipped = indented(norte_frontend::notes::skipped(pane.skipped(), lang));
        // #57: active reinterpretation mode — PERSISTENT while it lasts
        // (the painted names are not the bytes; the user must know it at
        // all times, not just in the toggle's message).
        let names = indented(norte_frontend::notes::names_encoding(
            pane.name_encoding(),
            lang,
        ));
        // #107: active hiding with entries set aside — same discipline as
        // `skipped`: a listing that shows less than there is is never
        // silent. It stays quiet with 0 set aside (a dir with no dotfiles)
        // and with hiding turned off. It goes AFTER `pruned` in the line
        // (#107 review MINOR-3): hiding with marks produces both, and the
        // pruning notice is the one that cannot be truncated first.
        let hidden = indented(norte_frontend::notes::hidden(pane.hidden_count(), lang));
        // Review MAJOR M3: the NOTICES (`skipped` — incomplete listing,
        // "never silent" — and `names` — the reinterpretation badge, "the
        // user must know it at all times") go BEFORE the informational
        // marks counter. The line has no width budget and ratatui
        // truncates the tail: with `marked`/`pruned` first (25+ cells,
        // easily) a long path at 80 columns pushed the encoding badge out
        // of the cut. Real debt (#103): a width budget that ellipsizes
        // `dir_text` so that NO later field is ever truncated, instead of
        // just reordering by priority.
        // The PATH yields, and it alone yields: everything else on this
        // line is a notice or a counter, and truncating the tail — which
        // is what ratatui used to do — took away whatever said how many
        // entries there are or that the listing is incomplete. With a long
        // path, what was seen of `pos/total` was a lone digit.
        //
        // `middle_ellipsis` truncates through the MIDDLE: the start of a
        // path says where you are and the end says which folder it is, and
        // losing either end is losing half the useful information.
        let tail = format!("{skipped}{names}{pruned}{hidden}{seq}");
        let width = usize::from(area.width);
        let room = width
            .saturating_sub(cells(&tail))
            .saturating_sub(cells(mark))
            .saturating_sub(1) // the left margin
            .saturating_sub(reserve);
        match warn {
            Some(warn) if reserve > 0 && room < LEGIBLE_PATH => {
                // The session one closes the line (`persistent_banner`), so
                // its columns are the notice's last ones: it is what the
                // mouse clicks. Only if it fits WHOLE: half a word is not
                // an indicator.
                session = session_zone_at(app, area, 1 + cells(&warn));
                format!(" {warn}{seq}")
            }
            Some(warn) => {
                let dir_text = norte_frontend::middle_ellipsis(&dir_text, room);
                let base = format!(" {mark}{dir_text}{tail}");
                let pad = width.saturating_sub(cells(&base) + cells(&warn) + 1);
                session = session_zone_at(app, area, cells(&base) + pad + cells(&warn));
                format!("{base}{}{warn} ", " ".repeat(pad))
            }
            None => {
                let dir_text = norte_frontend::middle_ellipsis(&dir_text, room);
                format!(" {mark}{dir_text}{tail}")
            }
        }
    };
    Composed {
        text,
        session,
        items: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::draw_status;
    use crate::app::testutil::app_two_panes;
    use norte_frontend::busy::{Busy, BusyKind, THRESHOLD};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn bar(app: &crate::app::App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(70, 1)).expect("test terminal");
        terminal
            .draw(|f| draw_status(f, f.area(), app))
            .expect("draw");
        terminal.backend().to_string()
    }

    /// #323: while waiting, the bar says WHAT is being waited for and that
    /// Esc cancels, and that WINS over the previous message.
    ///
    /// The order is half the fix. The previous operation's message
    /// describes a state that is no longer current, and the reader is
    /// looking at that line precisely because they want to know if the
    /// program is still alive: leaving the old text on top answers a
    /// different question.
    #[test]
    fn waiting_overrides_the_previous_message() {
        let mut app = app_two_panes();
        app.message = Some("copied 1 file".to_string());
        assert!(bar(&app).contains("copied 1 file"));

        let mut busy = Busy::new(BusyKind::Connecting, None, Some(0));
        busy.elapsed = THRESHOLD;
        let frame = busy.frame();
        app.busy = Some(busy);
        let line = bar(&app);
        assert!(line.contains(frame), "no spinner: {line}");
        assert!(
            !line.contains("copied 1 file"),
            "the old message covers the wait: {line}"
        );
    }

    /// The items (ADR 0132) go on the RIGHT, in the configured order, and
    /// the one that is clicked says where it lands and what runs.
    /// Contrasted against the painted text.
    #[test]
    fn the_items_go_right_and_are_clickable() {
        let mut app = app_two_panes();
        app.notices_unread = 3;
        app.chrome.status_items =
            Some(norte_config::StatusItems::parse(&["position", "notices"]).expect("valid"));
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        let c = super::compose(&app, area);
        let line = bar(&app);
        assert!(c.text.trim_end().ends_with("!3"), "{:?}", c.text);
        assert!(c.text.contains('/'), "the position: {:?}", c.text);
        assert_eq!(super::cells(&c.text), 70, "the line fills the width");
        let (x0, x1, cmd) = c.items[0];
        assert_eq!(cmd, "layout.log");
        let painted: String = line
            .chars()
            .skip(1 + usize::from(x0))
            .take(usize::from(x1 - x0) + 1)
            .collect();
        assert_eq!(painted, "!3", "{line}");

        // With no items, the line is the usual one and there is nothing to
        // click.
        app.chrome.status_items = Some(norte_config::StatusItems::parse::<&str>(&[]).unwrap());
        assert!(super::compose(&app, area).items.is_empty());
    }

    /// ADR 0137: a plugin's item is its column's value for the entry under
    /// the cursor, goes to the left of the right half and is not clickable.
    #[test]
    fn a_plugins_item_shows_its_column_and_is_not_clickable() {
        let mut app = app_two_panes();
        app.chrome.status_items =
            Some(norte_config::StatusItems::parse(&["position"]).expect("valid"));
        app.status_plugins = vec![("git".to_owned(), "branch".to_owned())];
        let under_cursor = app
            .focused()
            .selected()
            .expect("there are entries")
            .path
            .clone();
        let mut values = std::collections::HashMap::new();
        values.insert(under_cursor, "main".to_owned());
        let mut columns = std::collections::HashMap::new();
        columns.insert("plugin:git/branch".to_owned(), values);
        app.focused_mut().set_plugin_columns(columns);
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        let c = super::compose(&app, area);
        let branch = c.text.find("main").expect("the branch is painted");
        let position = c.text.rfind('/').expect("and the position");
        assert!(branch < position, "the plugin's goes first: {:?}", c.text);
        assert!(
            c.items.is_empty(),
            "neither one is clickable: {:?}",
            c.items
        );
    }

    /// A loose window carries its indicator in the bar, and the bar knows
    /// which columns it painted it in: that is what the mouse clicks to ask
    /// for the explanation. Contrasted against the PAINTED text, not
    /// against parallel arithmetic.
    #[test]
    fn the_session_indicator_says_where_it_lands() {
        use super::compose;
        use crate::ui::text::cells;
        let mut app = app_two_panes();
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        assert!(
            compose(&app, area).session.is_none(),
            "the owner has no indicator"
        );

        app.session.detached = true;
        let c = compose(&app, area);
        let (line, span) = (c.text, c.session);
        let badge = app.session_banner().expect("there is an indicator");
        let (x0, x1) = span.expect("and the bar knows where");
        let byte = line.find(&badge).expect("the indicator is in the line");
        assert_eq!(
            usize::from(x0),
            cells(&line[..byte]),
            "starts where it is painted"
        );
        assert_eq!(
            usize::from(x1),
            cells(&line[..byte]) + cells(&badge) - 1,
            "and ends at its last cell"
        );
        assert!(bar(&app).contains(&badge), "and it shows: {}", bar(&app));

        // With a message in front, the line is a different one and there
        // is nothing to click.
        app.message = Some("copied 1 file".to_string());
        assert!(compose(&app, area).session.is_none());
    }

    /// A notice expires after `notice_seconds` ticks (spec 2026-09-10): it
    /// leaves the bar, the `!n` badge counts one more to the right and is
    /// clickable; with `0` it never expires; a NEW message resets the
    /// count; and opening the log panel zeroes the badge.
    #[test]
    fn a_notice_expires_and_leaves_a_clickable_badge() {
        use super::compose;
        let mut app = app_two_panes();
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        app.chrome.notice_seconds = Some(2);
        app.message = Some("copied 1 file".to_string());
        app.tick_notices();
        assert!(app.message.is_some(), "one tick: still there");
        app.message = Some("other".to_string());
        app.tick_notices();
        assert!(app.message.is_some(), "a new message resets the count");
        app.tick_notices();
        assert!(app.message.is_none(), "two ticks: expired");
        assert_eq!(app.notices_unread, 1);
        let c = compose(&app, area);
        assert!(
            c.text.ends_with("!1 "),
            "the badge on the right: {:?}",
            c.text
        );
        let zone = |c: &super::Composed| {
            c.items
                .iter()
                .find(|(_, _, cmd)| *cmd == "layout.log")
                .map(|(x0, x1, _)| (*x0, *x1))
        };
        assert_eq!(zone(&c), Some((67, 68)), "clickable");
        assert!(bar(&app).contains("!1"));

        // Since ADR 0132 the badge is a right-side ELEMENT, and a message
        // on the left no longer covers it: they are different halves.
        app.message = Some("new".to_string());
        assert_eq!(zone(&compose(&app, area)), Some((67, 68)));
        // With `0`, nothing expires.
        app.chrome.notice_seconds = Some(0);
        for _ in 0..5 {
            app.tick_notices();
        }
        assert!(app.message.is_some());
        // Opening the log leaves the badge at zero.
        app.message = None;
        assert_eq!(app.notices_unread, 1);
        app.toggle_log();
        app.tick_notices();
        assert_eq!(app.notices_unread, 0);
    }

    /// With a loose session the bar still says the path and the `x/x`, and
    /// the notice goes to the right (2026-09-11: it used to replace the
    /// whole line and whoever had a loose session all day lost the
    /// counter). On a narrow bar, the notice alone, as before.
    #[test]
    fn the_persistent_notice_does_not_cover_the_path_or_the_counter() {
        use super::compose;
        use crate::ui::text::cells;
        let mut app = app_two_panes();
        app.session.detached = true;
        let badge = app.session_banner().expect("there is an indicator");
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        let c = compose(&app, area);
        // The counter is now the right side's `position` element (ADR
        // 0132), and the notice closes the LEFT half.
        assert!(
            c.text.contains("1/"),
            "the counter is still there: {:?}",
            c.text
        );
        assert!(c.text.contains(&badge), "the notice is there: {:?}", c.text);
        let (x0, x1) = c.session.expect("clickable");
        let byte = c.text.find(&badge).expect("it is there");
        assert_eq!(
            usize::from(x0),
            cells(&c.text[..byte]),
            "the zone starts where the indicator does"
        );
        assert_eq!(usize::from(x1), cells(&c.text[..byte]) + cells(&badge) - 1);
        // With no room for a legible path: the notice alone.
        let narrow =
            ratatui::layout::Rect::new(0, 0, u16::try_from(cells(&badge) + 8).expect("fits"), 1);
        let c = compose(&app, narrow);
        assert!(c.text.contains(&badge), "{:?}", c.text);
        assert!(c.session.is_some(), "the items yield to the notice");
    }

    /// REGRESSION (ADR 0132 review): with items that fill EXACTLY their
    /// budget, the right half's margin ate the notice's last cell. The
    /// notice has to fit whole, with the pending sequence behind it if
    /// there is one.
    #[test]
    fn the_items_do_not_truncate_a_persistent_notice() {
        use super::compose;
        use crate::ui::text::cells;
        let mut app = app_two_panes();
        app.session.detached = true;
        let badge = app.session_banner().expect("there is an indicator");
        for pending in ["", "g"] {
            app.pending = pending.to_owned();
            // `1/1` (3 cells) fits exactly in what the notice leaves.
            for extra in 3..12 {
                let width = u16::try_from(cells(&badge) + extra).expect("fits");
                let c = compose(&app, ratatui::layout::Rect::new(0, 0, width, 1));
                assert!(
                    c.text.contains(&badge),
                    "width {width}, pending {pending:?}: {:?}",
                    c.text
                );
                assert!(c.session.is_some(), "clickable at {width}");
            }
        }
    }

    /// On a narrow terminal the indicator is truncated, and an indicator
    /// that does not fit whole is not clickable: half a word is not an
    /// indicator.
    #[test]
    fn a_truncated_indicator_is_not_clickable() {
        use super::compose;
        let mut app = app_two_panes();
        app.session.detached = true;
        let badge = app.session_banner().expect("there is an indicator");
        let width = crate::ui::text::cells(&badge);
        // Exactly what it takes with its margin: it fits.
        let exact = ratatui::layout::Rect::new(0, 0, u16::try_from(width + 1).expect("fits"), 1);
        assert!(
            compose(&app, exact).session.is_some(),
            "fits whole and can be clicked"
        );
        // One cell less: not anymore.
        let narrow = ratatui::layout::Rect::new(0, 0, u16::try_from(width).expect("fits"), 1);
        assert!(
            compose(&app, narrow).session.is_none(),
            "truncated, no zone"
        );
    }

    /// With an overlay in front there is no zone, even if the window is
    /// still loose: the viewer paints its own footer, and over help the bar
    /// is not clickable.
    #[test]
    fn with_an_overlay_in_front_there_is_no_zone() {
        use super::session_zone;
        let mut app = app_two_panes();
        app.session.detached = true;
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        assert!(session_zone(&app, area).is_some(), "with no overlay, yes");
        app.help = Some(crate::app::HelpView::new(norte_i18n::Lang::Es, Vec::new()));
        assert!(session_zone(&app, area).is_none(), "with help in front, no");
    }

    /// Below the threshold the bar does not react: a flicker on every local
    /// `cd` is exactly the noise that makes nobody look at the indicator.
    #[test]
    fn below_the_threshold_the_bar_does_not_notice() {
        let mut app = app_two_panes();
        app.message = Some("copied 1 file".to_string());
        let mut busy = Busy::new(BusyKind::Connecting, None, Some(0));
        busy.elapsed = THRESHOLD
            .checked_sub(std::time::Duration::from_millis(1))
            .expect("the threshold is greater than 1 ms");
        let frame = busy.frame();
        app.busy = Some(busy);
        let line = bar(&app);
        assert!(!line.contains(frame), "spinner ahead of time: {line}");
        assert!(line.contains("copied 1 file"), "{line}");
    }

    /// ADR 0146: with work already running, the tasks item carries its bar
    /// behind it, and the clickable zone covers it whole: a click on the
    /// bar opens processes just like a click on the text.
    #[test]
    fn the_tasks_item_paints_its_bar_and_is_clickable_whole() {
        use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
        let mut app = app_two_panes();
        app.chrome.status_items =
            Some(norte_config::StatusItems::parse(&["tasks"]).expect("valid"));
        let p = TaskProgress {
            task_id: TaskId::new(1),
            kind: TaskKind::Copy,
            state: TaskState::Running,
            bytes_done: 50,
            bytes_total: Some(100),
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let t = norte_frontend::task_strip::StripTask {
            progress: &p,
            operand: None,
            bps: None,
        };
        app.strip.update(0, [t]);
        app.strip.update(norte_frontend::task_strip::UMBRAL_MS, [t]);
        app.render_now_ms = Some(norte_frontend::task_strip::UMBRAL_MS);
        let area = ratatui::layout::Rect::new(0, 0, 70, 1);
        let c = super::compose(&app, area);
        assert_eq!(super::cells(&c.text), 70, "the line fills the width");
        let bar = norte_frontend::task_strip::bar_glyphs(Some(50), 0);
        assert!(c.text.contains(&bar), "{:?}", c.text);
        let (x0, x1, cmd) = c.items[0];
        assert_eq!(cmd, "layout.processes");
        let zone: String = c
            .text
            .chars()
            .skip(usize::from(x0))
            .take(usize::from(x1 - x0) + 1)
            .collect();
        assert!(zone.starts_with('⟳') && zone.ends_with('▏'), "{zone:?}");
    }
}
