//! The help screen: its sidebar + body layout, the scrollbar the two share,
//! and the footer that summarizes the live shortcuts.
//!
//! The sidebar's width depends on the LANGUAGE (`help_sidebar_desired`),
//! because translated labels do not measure the same.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};

use super::text::{cells, fit_hint_groups, right_ellipsis};
use super::{centered, clear_themed};
use crate::app::display_name;
use crate::theme::TuiTheme;
use norte_frontend::middle_ellipsis;
use norte_i18n::t;

/// Lower bound in CELLS of the help sidebar: the width it used to have,
/// unconditionally. Kept as a FLOOR so an 80-column frame never gets a
/// narrower list of topics than it had before the sidebar was sized to its
/// content.
pub(crate) const HELP_SIDEBAR_MIN: u16 = 24;

/// Upper bound of the sidebar, as a percentage of the FRAME's width. The
/// sidebar is a table of contents: past roughly a third of the screen it is
/// taking width from the prose it exists to point at.
pub(crate) const HELP_SIDEBAR_PCT: u16 = 35;

/// Cells between the sidebar and the body. Without it a title that fills the
/// sidebar sits against the first letter of the prose and the two columns
/// read as one broken line.
pub(crate) const HELP_GUTTER: u16 = 2;

/// Width of a scrollbar: one cell.
///
/// Help is the only screen with TWO lists scrolling at once — the index and
/// the page — and until now neither one said how far along it was or how
/// much was left. The footer's `N/M` indicator talks only about the body,
/// and only when it does not fit.
pub(crate) const HELP_SCROLLBAR: u16 = 1;

/// Cells of air between the prose and the body's scrollbar. Without it a line
/// wrapped to the full width ends against the bar (`under the cursor║`) and
/// the last letter reads as part of it.
pub(crate) const HELP_BODY_PAD: u16 = 1;

/// Typographic measure of the body in CELLS. Prose is read at 60–72 cells; at
/// 90 the eye loses the line on the return sweep, and the surplus is exactly
/// what the sidebar needs to stop truncating its titles.
pub(crate) const HELP_MEASURE: u16 = 72;

/// Cells the body keeps whatever the sidebar asks for. Only bites on frames
/// too narrow for the overlay to be useful at all, and only to keep the body
/// from being laid out at zero width.
pub(crate) const HELP_BODY_MIN: u16 = 20;

/// Cells a topic row is indented by in the sidebar, so that a title never
/// lines up with the group header above it.
pub(crate) const HELP_ROW_INDENT: usize = 2;

/// Cells the sidebar would need to paint every row of `lang` IN FULL: the
/// indent plus the widest title, and the widest group header.
///
/// Measured over the whole corpus and not over `HelpState::rows()`, which is
/// what the filter narrows: a sidebar sized to the rows that survive would
/// change width on every keystroke, and the body — pre-rendered at the width
/// left over — would re-wrap its prose under the reader while they type.
///
/// The synthetic `keys` GROUP is deliberately not measured: its header is not
/// painted (see [`draw_help`]).
pub(crate) fn help_sidebar_desired(lang: norte_help::Lang) -> u16 {
    let mut want = HELP_ROW_INDENT + cells(&t("help-topic-keys"));
    for topic in norte_help::topics(lang) {
        want = want.max(HELP_ROW_INDENT + cells(&topic.title));
        match topic.tags.first() {
            Some(tag) if !tag.is_empty() => {
                want = want.max(cells(&t(&format!("help-group-{tag}"))));
            }
            _ => {}
        }
    }
    u16::try_from(want).unwrap_or(u16::MAX)
}

/// Width in CELLS of the help sidebar over a frame of `base`, for the corpus
/// of `lang`.
///
/// Public for the test that pins the sizing decision: the sidebar grows with
/// its content, floors at the 24 cells it used to have fixed, and never takes
/// more than a 35% share of the frame. See `help_layout`, where that is
/// decided and where the two bounds are named.
#[must_use]
pub fn help_sidebar_width(base: Rect, lang: norte_help::Lang) -> u16 {
    let (_, sidebar, _, _) = help_layout(base, help_sidebar_desired(lang));
    sidebar.width
}

/// Geometry of the help overlay: `(box, sidebar, body, footer)`.
///
/// A single function because the painter and the PRE-RENDER
/// ([`crate::app::App::refresh_help`]) have to measure the same thing: the
/// model bounds `body_scroll` against the number of lines laid out for a
/// given width, and laying out for a width different from the painted one
/// leaves the scroll outside the body right at the edges (the bug the
/// pre-render avoids).
///
/// `sidebar_desired` is what the sidebar would need to paint its rows in
/// full (`help_sidebar_desired`); it arrives as a parameter so this stays a
/// function of numbers, measurable at any size with no corpus.
#[must_use]
pub fn help_layout(base: Rect, sidebar_desired: u16) -> (Rect, Rect, Rect, Rect) {
    let area = centered(
        base,
        base.width.saturating_sub(4).max(20),
        base.height.saturating_sub(2).max(6),
    );
    let inner = Block::default().borders(Borders::ALL).inner(area);
    // The VERTICAL cut goes FIRST (review MAJOR): the box reserves its last
    // line for the footer (the filter or the generated hint), the way
    // `draw_settings` reserves its own for the description — the border's
    // footer (`title_bottom`) would not fit with the sidebar in front.
    // Cutting the horizontal one first left the footer with the BODY's width
    // (50 cells in an 80-wide frame) and `fit_hint_groups` dropped the group
    // that opens the body, `[tab]`, which is the only entry to the half
    // where `Enter` touches the filesystem. This way the footer takes the
    // FULL width (74 cells in that same frame).
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    // The HORIZONTAL cut: sidebar, gutter and body. The sidebar asks for
    // what its content measures, floored at what it always had and capped
    // at a share of the frame; the body keeps the rest, capped to its
    // MEASURE. Whatever is left over — a very wide terminal — is simply not
    // used: 90 cells of prose read worse than 72, not better.
    let avail = rows[0].width;
    let pct = u16::try_from(u32::from(base.width) * u32::from(HELP_SIDEBAR_PCT) / 100)
        .unwrap_or(u16::MAX);
    let ceiling = pct
        .max(HELP_SIDEBAR_MIN)
        .min(avail.saturating_sub(HELP_GUTTER + HELP_BODY_MIN));
    // `max` AFTER `min`: in a frame too narrow for the floor, the ceiling
    // wins — a sidebar wider than the box would leave the body at zero
    // cells, and a `clamp` with the range inverted panics.
    let side = sidebar_desired
        .max(HELP_SIDEBAR_MIN.min(ceiling))
        .min(ceiling);
    let gutter = HELP_GUTTER.min(avail.saturating_sub(side));
    let body = avail.saturating_sub(side + gutter).min(HELP_MEASURE);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(side),
            Constraint::Length(gutter),
            Constraint::Length(body),
            Constraint::Min(0),
        ])
        .split(rows[0]);
    // The BODY keeps exactly the same height as before (`inner` minus the
    // footer row): `help_body_size` publishes it and the pre-render bounds
    // against it. What changes is the sidebar, which now also yields that
    // row — the footer belongs to the box, not to a column.
    (area, cols[0], cols[2], rows[1])
}

/// Width and height IN CELLS of the help overlay's body over a frame of
/// `base`, so the run loop can lay out the page with
/// [`App::refresh_help`](crate::app::App::refresh_help) right before
/// painting it. See `help_layout`, where it comes from.
///
/// `lang` is the corpus locale the overlay was opened with
/// (`HelpState::lang`): the sidebar is sized to the titles it has to paint,
/// so the width left for the body depends on it. The HEIGHT does not.
#[must_use]
pub fn help_body_size(base: Rect, lang: norte_help::Lang) -> (usize, usize) {
    let (_, _, body, _) = help_layout(base, help_sidebar_desired(lang));
    // The body's last column is its scrollbar, so the prose wraps to one
    // cell less. This comes from here and not from painting because the run
    // loop is what lays out the page, and a width that does not match the
    // painted one splits lines in the wrong place.
    (
        usize::from(body.width.saturating_sub(HELP_SCROLLBAR + HELP_BODY_PAD)),
        usize::from(body.height),
    )
}

/// Help overlay (H3b), (almost) full-screen and above everything: a topics
/// sidebar on the left, the open topic's body on the right and a one-line
/// footer under the body.
///
/// **Lays out nothing**: the body arrives ALREADY rendered in
/// [`crate::app::HelpView`] (see its doc — the model needs to know how many
/// lines came out to bound its scroll, and a `draw_*` only receives `&App`).
/// Here it is only truncated by scroll and highlighted, nothing more.
///
/// Masked: the corpus's titles come from the binary (built-in) or are
/// already masked by `norte_help::parse_untrusted` (plugin), and the body's
/// lines were produced by [`crate::help_render`] over that same input — this
/// draw does not filter them again, same as `draw_palette` with its rows.
/// The ONLY free input is the filter the user typed, which goes through the
/// same double filter as the quick search bar (`filter_display` — never
/// `filter_raw` — plus [`display_name`]).
pub fn draw_help(
    frame: &mut Frame<'_>,
    help: &crate::app::HelpView,
    theme: &TuiTheme,
    hint: &str,
    version_line: &str,
) {
    use norte_frontend::help::Focus;

    let (area, sidebar, body_area, footer_area) =
        help_layout(frame.area(), help_sidebar_desired(help.state.lang()));
    clear_themed(frame, area, theme);
    let mut frame_block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("help-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(Role::ModalBorder));
    // Which binary this is, top right: version and tree revision. Not
    // interface text but an identifier, so it does not go through Fluent;
    // and empty it is not painted (tests build `App` that way).
    if !version_line.is_empty() {
        frame_block = frame_block.title_top(Line::raw(format!(" {version_line} ")).right_aligned());
    }
    frame.render_widget(frame_block, area);

    let state = &help.state;
    // The index's bar lives in the gutter's FIRST cell: glued to the
    // sidebar and without taking a single column from the titles, which is
    // what the gutter was there to give.
    let sidebar_scrollbar = Rect {
        x: sidebar.x.saturating_add(sidebar.width),
        y: sidebar.y,
        width: HELP_SCROLLBAR.min(frame.area().width.saturating_sub(sidebar.x + sidebar.width)),
        height: sidebar.height,
    };
    // The gutter is its own layout COLUMN, so the sidebar can spend its
    // whole width on the title.
    let side_w = usize::from(sidebar.width);
    let (rows_ui, painted) = painted_sidebar(state.rows());
    let items: Vec<ListItem<'_>> = rows_ui
        .into_iter()
        .map(|row| match row {
            SidebarRowUi::Blank => ListItem::new(Line::default()),
            // The model hands over TAGS, not text: translation is the
            // frontend's business (the same row is called differently in
            // the TUI and the GUI). A tag with no Fluent entry would paint
            // its own key, which the i18n suite prevents.
            SidebarRowUi::Group(tag) => ListItem::new(Line::styled(
                right_ellipsis(&t(&format!("help-group-{tag}")), side_w),
                theme.role(Role::Title),
            )),
            // The synthetic `keys` entry's title is the label
            // `HelpView::new` gave the model (`help-topic-keys`), so there
            // is no special case here: the sidebar paints the same thing
            // the filter searches for.
            SidebarRowUi::Topic(title) => ListItem::new(Line::raw(right_ellipsis(
                &format!("{}{title}", " ".repeat(HELP_ROW_INDENT)),
                side_w,
            ))),
        })
        .collect();
    // How many rows the PAINTED index has (with its separators): it is the
    // total its bar is sized against, and it has to be read before the
    // widget takes ownership of the list.
    let rows_index = items.len();
    let mut list_state = ListState::default();
    // `HelpState` guarantees the cursor ALWAYS rests on a selectable row
    // (never on a header); with the filter returning nothing there is no
    // row at all to highlight.
    list_state.select(painted.get(state.cursor()).copied());
    // Which of the two halves gets the keys, said the way the listing's two
    // panes say it (spec 2026-09-10): the cursor of the one WITHOUT focus
    // stays dimmed. Before, both highlighted equally bright and the screen
    // did not say where the arrows would go.
    //
    // It is the role and not a border because help is ONE frame: splitting
    // it in two would eat a column from the titles, which is what makes an
    // index readable. And with no theme the dimmed one still shows, because
    // `SelectionUnfocused`'s `fallback` is `reverse().dim()`.
    let (index_role, body_role) = if state.focus() == Focus::Topics {
        (Role::Selection, Role::SelectionUnfocused)
    } else {
        (Role::SelectionUnfocused, Role::Selection)
    };
    frame.render_stateful_widget(
        List::new(items).highlight_style(theme.role(index_role)),
        sidebar,
        &mut list_state,
    );

    let (lines, action_lines) = help.body();
    // The action line under the body's cursor. Highlighted WHENEVER it
    // exists, whether focus is here or not — what changes is the role.
    // Before, it disappeared when focus left, and then the unfocused half
    // was not "a dimmed cursor" but "no cursor at all": coming back with
    // Tab did not say which line you were returning to.
    let focused = action_lines.get(state.action_cursor()).copied();
    let body: Vec<Line<'_>> = lines
        .iter()
        .enumerate()
        .skip(state.body_scroll())
        .take(usize::from(body_area.height))
        .map(|(i, line)| {
            if Some(i) == focused {
                line.clone().style(theme.role(body_role))
            } else {
                line.clone()
            }
        })
        .collect();
    // The body's last column is its bar: the prose already comes wrapped to
    // one cell less (`help_body_size`), so here it is only split.
    let (text_area, body_bar) = split_body(body_area);
    frame.render_widget(Paragraph::new(body), text_area);
    // BOTH columns say how far along they are. Until now neither one did:
    // the footer's `N/M` talks only about the body and only when it does
    // not fit, so the index had NOTHING saying rows were left below.
    render_scrollbar(
        frame,
        body_bar,
        theme,
        lines.len(),
        state.body_scroll(),
        usize::from(body_area.height),
    );
    render_scrollbar(
        frame,
        sidebar_scrollbar,
        theme,
        rows_index,
        list_state.offset(),
        usize::from(sidebar.height),
    );

    draw_help_footer(
        frame,
        footer_area,
        theme,
        state,
        hint,
        lines.len(),
        body_area.height,
    );
}

/// The help overlay's footer: the hint (or the filter) on the left and
/// where the reader is on the right.
pub(crate) fn draw_help_footer(
    frame: &mut Frame<'_>,
    footer_area: Rect,
    theme: &TuiTheme,
    state: &norte_frontend::help::HelpState,
    hint: &str,
    total: usize,
    body_height: u16,
) {
    // Where the reader is inside the page, in the SAME language as the
    // viewer (`{row}/{total}`, `draw_viewer`). Only when the page does NOT
    // fit: a `1/9` over nine visible lines is noise. It matters more here
    // than in the viewer because the runnable rows — the chord column and
    // the `Enter` this overlay exists for — are painted BEHIND all the
    // prose, so on a long page they are not visible on the first render and
    // without this nothing says they are there.
    //
    // As PERCENT READ, down to the bottom of the window, and not in lines:
    // an `11/663` tells nobody how much is left, because 663 is not a
    // number the reader carries in their head. "17%" does; "100%" means
    // they have already seen the end. Same as what `less` shows.
    let height = usize::from(body_height);
    let pos = (total > height).then(|| {
        let read = state.body_scroll().saturating_add(height).min(total);
        format!(" {} % ", read.saturating_mul(100) / total.max(1))
    });
    let pos = pos.unwrap_or_default();
    // The indicator takes its slice of the footer BEFORE the hint is
    // truncated: on the right it never disputes the left border with the
    // hint, and the hint never eats into it (`fit_hint_groups` drops whole
    // groups, not loose cells).
    let width = usize::from(footer_area.width);
    let left_max = width.saturating_sub(cells(&pos));
    let left = if state.filtering() {
        let (query, _) = display_name(state.filter_display().as_bytes());
        middle_ellipsis(&format!(" /{query}"), left_max)
    } else {
        // NEVER `middle_ellipsis` on a generated hint: see
        // [`fit_hint_groups`]. One footer cell belongs to the left margin.
        format!(" {}", fit_hint_groups(hint, left_max.saturating_sub(1)))
    };
    let slot = width.saturating_sub(cells(&left) + cells(&pos));
    let footer = Line::from(vec![
        Span::raw(left),
        Span::raw(" ".repeat(slot)),
        Span::raw(pos),
    ]);
    frame.render_widget(
        Paragraph::new(footer).style(theme.role(Role::BorderUnfocused)),
        footer_area,
    );
}

/// A PAINTED row of the help sidebar.
enum SidebarRowUi<'a> {
    /// The air between two groups.
    Blank,
    /// A group header, by its tag.
    Group(&'a str),
    /// A page, by its title.
    Topic(&'a str),
}

/// What the sidebar paints and, for each row of the MODEL, which painted row
/// it lands on.
///
/// The painted rows are NOT the model's: a blank line goes between one group
/// and the next. This lives here and not in `HelpState::rows`, which is the
/// NAVIGABLE list — its indices are what `cursor()` addresses, and putting
/// separators there would break the cursor and, along with it, the sibling
/// GUI. That is why it carries the row→item map: it translates the model's
/// cursor to the widget's index, and a click back. Used by the painter and
/// [`help_zones`]: what is clickable comes from what is painted.
fn painted_sidebar(
    rows: &[norte_frontend::help::SidebarRow],
) -> (Vec<SidebarRowUi<'_>>, Vec<usize>) {
    use norte_frontend::help::SidebarRow;
    let mut rows_ui: Vec<SidebarRowUi<'_>> = Vec::with_capacity(rows.len() + 4);
    let mut painted: Vec<usize> = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        match row {
            SidebarRow::Group { tag } => {
                if keys_only_group(rows, i) {
                    // A header named the same as its only entry conveys
                    // nothing and costs a row. The item that is coming is
                    // noted — the cursor never rests on a header, so the
                    // map only has to stay well-formed.
                    painted.push(rows_ui.len());
                    continue;
                }
                // Air between groups, except before the first one: a blank
                // right at the top reads as a misaligned sidebar.
                if !rows_ui.is_empty() {
                    rows_ui.push(SidebarRowUi::Blank);
                }
                painted.push(rows_ui.len());
                rows_ui.push(SidebarRowUi::Group(tag));
            }
            SidebarRow::Topic { title, .. } => {
                painted.push(rows_ui.len());
                rows_ui.push(SidebarRowUi::Topic(title));
            }
        }
    }
    (rows_ui, painted)
}

/// Where each thing of help's lands in the painted frame, for the mouse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelpZones {
    /// The topics sidebar.
    pub sidebar: Rect,
    /// The body, with its scrollbar.
    pub body: Rect,
    /// Every VISIBLE page of the sidebar: `(screen row, model row)`. Headers
    /// and air are not there: they are not clickable.
    pub rows: Vec<(u16, usize)>,
}

/// Help's zones in `area`'s frame, or nothing if it is not open.
///
/// The TUI's help was born deaf to the mouse — with it open, the wheel and
/// clicks were dropped whole — and the way to keep that from happening again
/// is the one from [`super::extension_zones`]: the zones come from the SAME
/// layout that paints ([`help_layout`] and the sidebar rows' layout,
/// `painted_sidebar`).
///
/// The list's offset is the one `ratatui` applies to a freshly made
/// `ListState`, which is what the painter uses: zero while the cursor fits,
/// and otherwise whatever leaves the cursor on the last row.
#[must_use]
pub fn help_zones(app: &crate::app::App, area: Rect) -> Option<HelpZones> {
    let help = app.help.as_ref()?;
    let (_, sidebar, body, _) = help_layout(area, help_sidebar_desired(help.state.lang()));
    let rows = help.state.rows();
    let (_, painted) = painted_sidebar(rows);
    let height = usize::from(sidebar.height);
    let offset = painted
        .get(help.state.cursor())
        .map_or(0, |&sel| (sel + 1).saturating_sub(height));
    let visible = painted
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            matches!(
                rows.get(*i),
                Some(norte_frontend::help::SidebarRow::Topic { .. })
            )
        })
        .filter_map(|(i, &item)| {
            let row = item.checked_sub(offset)?;
            let row = u16::try_from(row).ok().filter(|f| *f < sidebar.height)?;
            Some((sidebar.y.saturating_add(row), i))
        })
        .collect();
    Some(HelpZones {
        sidebar,
        body,
        rows: visible,
    })
}

/// The help body in (text, bar): [`split_scrollbar`] plus the margin between
/// the prose and the bar ([`HELP_BODY_PAD`]). It comes from the text area,
/// not the bar: the prose already comes wrapped to that width
/// (`help_body_size`).
fn split_body(area: Rect) -> (Rect, Rect) {
    let (text, bar) = split_scrollbar(area);
    let text = Rect {
        width: text.width.saturating_sub(HELP_BODY_PAD),
        ..text
    };
    (text, bar)
}

/// Splits an area into (content, scrollbar): the LAST column is the bar.
/// With less than two cells there is no bar to paint and the whole area is
/// returned — a bar that eats the text is worse than not having one.
pub(crate) fn split_scrollbar(area: Rect) -> (Rect, Rect) {
    if area.width < 2 {
        return (area, Rect::new(area.x, area.y, 0, area.height));
    }
    let text = Rect {
        width: area.width - HELP_SCROLLBAR,
        ..area
    };
    let bar = Rect {
        x: area.x + area.width - HELP_SCROLLBAR,
        width: HELP_SCROLLBAR,
        ..area
    };
    (text, bar)
}

/// Paints a vertical scrollbar in `area` for content of `total` rows of
/// which `visible` are shown from `offset`.
///
/// Paints nothing when everything fits: a bar full top to bottom informs of
/// nothing and on top of that invites dragging it.
pub(crate) fn render_scrollbar(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &TuiTheme,
    total: usize,
    offset: usize,
    visible: usize,
) {
    if area.width == 0 || area.height == 0 || total <= visible {
        return;
    }
    let mut state = ScrollbarState::new(total.saturating_sub(visible)).position(offset);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .style(theme.role(Role::BorderUnfocused)),
        area,
        &mut state,
    );
}

/// Paints a HORIZONTAL scrollbar in `area` for content of `total` columns of
/// which `visible` are shown from `offset`.
///
/// Twin of [`render_scrollbar`] and with the same rule: nothing when
/// everything fits. It exists because the viewer does not wrap — a line can
/// keep going to the right — and without a bar to say so, a truncated file
/// reads as a short one.
pub(crate) fn render_hscrollbar(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &TuiTheme,
    total: usize,
    offset: usize,
    visible: usize,
) {
    if area.width == 0 || area.height == 0 || total <= visible {
        return;
    }
    let mut state = ScrollbarState::new(total.saturating_sub(visible)).position(offset);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
            .begin_symbol(None)
            .end_symbol(None)
            .style(theme.role(Role::BorderUnfocused)),
        area,
        &mut state,
    );
}

/// Whether the sidebar paints a header for the group row at `header`.
///
/// Public because it is also what says which `help-group-{tag}` lookups the
/// painter can make, and the i18n sweep over those lookups
/// (`norte-tui/tests/keymap.rs`) must ask rather than re-derive: a tag whose
/// header is never painted needs no Fluent entry, and one that is painted
/// needs one in every locale.
#[must_use]
pub fn help_group_is_painted(rows: &[norte_frontend::help::SidebarRow], header: usize) -> bool {
    !keys_only_group(rows, header)
}

/// Whether the group header at `header` heads a group whose only member is
/// the synthetic keyboard entry.
///
/// Keyed off [`norte_frontend::help::KEYS_ID`] and never off the STRING: the
/// header and the row are both painted from Fluent, and in every locale so far
/// they are the same word — but that is a fact about the catalogue, not
/// something to branch on.
pub(crate) fn keys_only_group(rows: &[norte_frontend::help::SidebarRow], header: usize) -> bool {
    use norte_frontend::help::{KEYS_ID, SidebarRow};

    let mut members = rows
        .get(header.saturating_add(1)..)
        .unwrap_or_default()
        .iter()
        .take_while(|row| matches!(row, SidebarRow::Topic { .. }));
    let only = matches!(
        members.next(),
        Some(SidebarRow::Topic { id, .. }) if id.as_str() == KEYS_ID
    );
    only && members.next().is_none()
}

#[cfg(test)]
mod help_footer_tests {
    use crate::app::ALLOW_HELP;
    use crate::hints::{dialog_hints, without_navigation};
    use crate::keymap::{COMMANDS, DIALOG_COMMANDS, Effective, Screen, presets};
    use crate::ui::text::{cells, fit_hint_groups, hint_groups};

    /// The `dialog` effective of the shipped default preset — the very one
    /// the help overlay's footer is generated from at runtime.
    fn dialog_eff() -> Effective {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("orthodox preset");
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        Effective::build_for(&preset, &[], &known, Screen::Dialog).expect("preset effective")
    }

    /// FIX 1: the footer must never paint a `[` that does not open a WHOLE
    /// `[chord] label` group.
    ///
    /// `middle_ellipsis` cut inside a group and left the brackets balanced —
    /// `[esc]…kspace]` at 80 columns, a chord for a key the app invented. A
    /// group is data (the effective keymap × the Fluent label); half of one
    /// is a fabrication. Swept over EVERY budget from one cell to the full
    /// hint, so no width has a special case hiding in it.
    #[test]
    fn help_footer_never_splits_a_group() {
        let eff = dialog_eff();
        let hint = dialog_hints(&without_navigation(ALLOW_HELP), &eff);
        let groups = hint_groups(&hint);
        assert!(groups.len() > 2, "help's hint has several groups: {hint:?}");
        for max in 1..=cells(&hint) {
            let out = fit_hint_groups(&hint, max);
            assert!(
                cells(&out) <= max,
                "max={max}: {} cells in {out:?}",
                cells(&out)
            );
            // What is left after removing the truncation mark has to be a
            // sequence of WHOLE groups from the real hint.
            let body = out
                .strip_suffix('…')
                .map_or(out.as_str(), str::trim_end)
                .to_owned();
            for (i, _) in body.match_indices('[') {
                assert!(
                    groups.iter().any(|g| body[i..].starts_with(g)),
                    "max={max}: a `[` that does not open a whole group: {out:?}"
                );
            }
            if body != hint {
                assert!(
                    out.ends_with('…'),
                    "max={max}: something was dropped without marking it: {out:?}"
                );
            }
        }
    }

    /// And what gets dropped is dropped from the TAIL: the footer is a real
    /// prefix of the hint, never a chunk from the middle (which is where
    /// H3b's new verbs used to fall — `[tab] the other pane` disappeared
    /// whole).
    #[test]
    fn help_footer_is_a_prefix_of_the_hint() {
        let eff = dialog_eff();
        let hint = dialog_hints(&without_navigation(ALLOW_HELP), &eff);
        for max in 1..=cells(&hint) {
            let out = fit_hint_groups(&hint, max);
            let body = out.strip_suffix('…').map_or(out.as_str(), str::trim_end);
            assert!(
                hint.starts_with(body),
                "max={max}: {body:?} is not a prefix of {hint:?}"
            );
        }
    }
}
