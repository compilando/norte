//! Painting a pane: the row per entry, the column header and the width
//! layout the two share.
//!
//! `entry_item` is the hottest function in the render — it is called once
//! per visible row and per frame — and that is why it receives everything
//! by parameter instead of looking at `App`: grouping its arguments into a
//! single-use struct would only move the list somewhere else.

use norte_proto::EntryKind;
use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::text::{clamp_spans, take_width};
use super::{HOSTILE_BADGE, TARGET_BADGE, TabStrip, draw_tab_strip};
use crate::app::{Pane, display_name};
use crate::theme::TuiTheme;
use norte_frontend::middle_ellipsis;
use norte_i18n::t;

/// Cells of the icon column (ADR 0105): two for the glyph — an emoji
/// measures two — and one for separation. It opens on every row of a
/// listing as soon as one has an icon, and the header shifts by the same
/// amount.
pub(crate) const ICON_GUTTER: usize = 3;

/// ONE row's icon-column cell (ADR 0105): two cells and a space, BEFORE the
/// name. A one-cell icon (`$`) is padded to two; one that does not fit in
/// two (an emoji with a modifier) is clipped, because the column is what
/// aligns the names and a wide icon would break it. With no icon on this
/// row, the slot stays empty: the row stays aligned.
///
/// It is painted with the entry's color: it says what it IS, not what state
/// it is in, and the name already carries that color.
fn icon_span<'a>(
    decoration: Option<&norte_frontend::Decoration>,
    theme: &TuiTheme,
    name: &[u8],
    kind: EntryKind,
) -> Span<'a> {
    let glyph = decoration.and_then(|d| d.icon.as_deref()).unwrap_or("");
    let glyph = take_width(glyph, ICON_GUTTER - 1);
    let relleno = (ICON_GUTTER - 1).saturating_sub(glyph.width());
    Span::styled(
        format!("{glyph}{} ", " ".repeat(relleno)),
        theme.entry(name, kind),
    )
}

/// How many items a pane paints and which is highlighted, IN PAINTED
/// COORDINATES (position within the filter when quick search is in filter
/// mode, absolute index otherwise). Shared by `draw_pane` and
/// [`super::geometry::pane_geometry`] so the scroll comes from the same
/// calculation.
pub fn painted_len_and_selection(pane: &Pane) -> (usize, Option<usize>) {
    match pane.quick_visible() {
        Some(vis) => (
            vis.len(),
            pane.quick()
                .and_then(crate::nav::QuickSearch::selected_entry_index)
                .and_then(|s| vis.iter().position(|&i| i == s)),
        ),
        None => (
            pane.entries().len(),
            (!pane.entries().is_empty()).then_some(pane.cursor()),
        ),
    }
}

/// The indices the painter actually formats: THE WINDOW, not the listing.
///
/// `offset` is the MODEL's window (`PaneState::reconcile_viewport`,
/// sticky) and `alto` the rows the frame leaves for the listing. The range
/// is clipped to `total`, so the end of the listing paints what is left and
/// an `offset` that ended up past it (the listing shrank under a stale
/// window) gives an EMPTY range instead of blowing up.
///
/// It exists because formatting one row per ENTRY to show thirty-three used
/// to cost ~50ms of CPU per frame in a 3794-entry directory, with the loop
/// repainting eleven times a second: almost a whole core burned while idle,
/// and the cost grew with the directory's size.
///
/// ONE calculation and not a second one next to [`Pane::viewport_offset`]:
/// the mouse's hit test resolves against that same window, and two
/// calculations of the same slot diverge silently — that is where a click
/// ends up marking the file next door (`funcion-compartida-no-basta`
/// memory).
pub(crate) fn painted_rows(offset: usize, total: usize, alto: usize) -> std::ops::Range<usize> {
    let from = offset.min(total);
    let until = from.saturating_add(alto).min(total);
    from..until
}

/// The state ratatui paints the list with, for an already-clipped `window`.
///
/// Two things, and both follow from `items` being ONLY the window:
///
/// - the selection is in PAINTED coordinates, so it is rebased by
///   subtracting the window's start; a cursor outside it highlights no row,
///   which is what already happened when ratatui did the clipping;
/// - the `offset` is ZERO on purpose. Putting the model's there would skip
///   rows twice and paint the slot empty.
///
/// The MODEL's window (`PaneState::reconcile_viewport`, sticky) still lives
/// in `Pane::viewport_offset`, and it is the one `pane_geometry` declares to
/// the mouse: both come from the same place, which is what keeps a click
/// from resolving to the row next door.
fn list_state(selected: Option<usize>, window: &std::ops::Range<usize>) -> ListState {
    let mut state = ListState::default();
    state.select(
        selected
            .and_then(|s| s.checked_sub(window.start))
            .filter(|k| *k < window.len()),
    );
    *state.offset_mut() = 0;
    state
}

/// The window this frame paints of `pane`, resolved BEFORE formatting any
/// row.
///
/// The height comes from the same calculation [`draw_tab_strip`] does when
/// laying out the block's interior — tab strip if there is one, column
/// header, and the listing below; here it is only anticipated, the
/// painting stays in its place. Anticipating it is what allows formatting
/// the WINDOW instead of the whole listing, which is where the cost came
/// from.
fn pane_window(
    pane: &Pane,
    area: Rect,
    with_tabs: bool,
    painted_len: usize,
) -> std::ops::Range<usize> {
    let inner = super::geometry::block_inner(area);
    let bar = u16::from(with_tabs);
    let alto = usize::from(inner.height.saturating_sub(bar).saturating_sub(1));
    painted_rows(pane.viewport_offset(), painted_len, alto)
}

#[cfg(test)]
mod painted_rows_tests {
    use super::painted_rows;

    /// The case that was hard to find: a large directory must NOT format
    /// one row per entry to show the window.
    ///
    /// Measured before the fix: `/usr/share/man/man1` (3794 entries) cost
    /// ~550ms of CPU per second in the painting, 11 frames per second, i.e.
    /// ~50ms per frame to show 33 rows. The cost was linear in the
    /// listing's size, which is exactly what this range prevents.
    #[test]
    fn a_huge_listing_paints_only_the_window() {
        let r = painted_rows(0, 3794, 33);
        assert_eq!(r.len(), 33, "33 window rows, not 3794");
        let r = painted_rows(3000, 100_000, 40);
        assert_eq!(r.len(), 40, "the listing's size does not change the cost");
        assert_eq!(r.start, 3000, "starts where the model's window says");
    }

    /// The end of the listing is clipped to the total: asking for more rows
    /// than remain does not invent entries nor overflow.
    #[test]
    fn the_last_stretch_is_clipped_to_the_total() {
        let r = painted_rows(95, 100, 33);
        assert_eq!(r, 95..100, "only five are left");
    }

    /// An offset past the end gives an EMPTY range, not a panic nor an
    /// inverted range. It really happens: the listing shrinks (a refresh, a
    /// filter) while the model's window stays where it was.
    #[test]
    fn an_out_of_range_offset_does_not_blow_up() {
        assert!(painted_rows(500, 100, 33).is_empty());
        assert!(painted_rows(0, 0, 33).is_empty(), "empty listing");
        assert!(
            painted_rows(0, 100, 0).is_empty(),
            "no height means no rows"
        );
    }
}

/// The header line (#108 L5): Fluent labels (or the spec's custom `header`,
/// #108 7b — ALREADY sanitized and capped when resolved, here only the
/// width clip), the active sort's with `▲`/`▼`. Width faithful to the
/// rows' cells; the style's `align` picks the padding side on the
/// non-name ones, in step with their cells.
pub(crate) fn column_header_line(
    cols: &[(
        norte_frontend::columns::ColumnId,
        u16,
        norte_frontend::columns::ColumnStyle,
    )],
    sort: &norte_frontend::SortSpec,
    catalog: Option<&norte_proto::AttrCatalog>,
    // The cells the icon column (ADR 0105) takes from the name block: 0
    // with no icons, [`ICON_GUTTER`] with them. The "Name" header shifts by
    // the same amount as the names, or stops sitting above them.
    icon_gutter: usize,
) -> String {
    use norte_frontend::SortDir;
    use norte_frontend::columns::Align;
    let mut out = String::new();
    for (i, (col, w, style)) in cols.iter().enumerate() {
        // #117: label shared TUI/GUI (spec's custom header → Fluent →
        // masked catalog → id). It is NOT re-masked here: `header_label`
        // already returns safe text.
        let label = norte_frontend::columns::header_label(col, style, catalog);
        let active = norte_frontend::columns::sort_column_id(col).as_ref() == Some(&sort.column);
        let w = usize::from(*w);
        let arrow = if sort.dir == SortDir::Asc {
            '▲'
        } else {
            '▼'
        };
        if i == 0 {
            // Name: left-aligned (leaves room for the gutter). The arrow is
            // added AFTER clipping (review MN2): the direction indicator
            // survives any locale; clip by WIDTH (take_width), never by
            // chars. No `align` touches the name's layout (#108 7b): its
            // block rules.
            // The gutter never overflows the column: on one of one or two
            // cells — what `full` leaves on a 40-column terminal — what
            // fits is painted and the rest of the headers stay in place.
            let icon_gutter = icon_gutter.min(w);
            let w = w - icon_gutter;
            let budget = if active { w.saturating_sub(1) } else { w };
            let mut cab = take_width(&label, budget);
            if active {
                cab.push(arrow);
            }
            let pad = w.saturating_sub(cab.width());
            out.push_str(&" ".repeat(icon_gutter));
            out.push_str(&cab);
            out.push_str(&" ".repeat(pad));
        } else {
            // Non-name: the width includes the separator — content within
            // w-1, same count as the cell. Right: padding up front.
            // Left (#108 7b): the separator keeps OPENING the width, the
            // content follows it and the padding falls on the right.
            let content = w.saturating_sub(1);
            let budget = if active {
                content.saturating_sub(1)
            } else {
                content
            };
            let mut cab = take_width(&label, budget);
            if active {
                cab.push(arrow);
            }
            match style.align {
                Align::Right => {
                    let pad = w.saturating_sub(cab.width());
                    out.push_str(&" ".repeat(pad));
                    out.push_str(&cab);
                }
                Align::Left => {
                    // m1 review 7b: emission clamped to EXACTLY `w` cells —
                    // with `w == 1` and an active arrow, "space + arrow"
                    // used to emit 2 and shift the whole header to the
                    // right (the separator wins: it opens the width, as in
                    // the cells).
                    let clamped = take_width(&format!(" {cab}"), w);
                    let pad = w.saturating_sub(clamped.width());
                    out.push_str(&clamped);
                    out.push_str(&" ".repeat(pad));
                }
            }
        }
    }
    out
}

/// A pane's LIVE columns with their style resolved (#108 7b, #117 over
/// `ColumnId`): the shared layout's widths plus `style_for_id`, ONCE per
/// column and per frame (`style_for_id` folds maps and clones the header —
/// per row × column would be O(rows × columns) of identical lookups). The
/// catalog comes from `App`'s per-scheme cache (#117 task 2): it refines
/// the attr columns' defaults (hint); `None` = it has not arrived yet or
/// failed — Opaque defaults, never blocks the render.
pub(crate) fn styled_columns(
    settings: &norte_frontend::columns::ColumnsSettings,
    pane: &Pane,
    inner_w: u16,
    catalog: Option<&norte_proto::AttrCatalog>,
) -> Vec<(
    norte_frontend::columns::ColumnId,
    u16,
    norte_frontend::columns::ColumnStyle,
)> {
    let scheme = pane.dir().scheme();
    pane_columns(settings, pane, inner_w, catalog)
        .into_iter()
        .map(|f| {
            let s = settings
                .style_for_id(scheme, &f.id, catalog)
                .compacted(f.compact);
            (f.id, f.width, s)
        })
        .collect()
}

/// A pane's columns, adjusted so their NAMES can be read
/// ([`norte_frontend::columns::fitted_columns`]). The single source of
/// widths: the painting and the border the mouse drags come from here, and
/// two different calculations would make the drag grab the column next
/// door.
///
/// What the name wants is what its names measure plus what goes ahead of
/// them on the row: gutter, badge, class and — if there are any — icons.
pub(crate) fn pane_columns(
    settings: &norte_frontend::columns::ColumnsSettings,
    pane: &Pane,
    inner_w: u16,
    // This pane's scheme's catalog, if it already arrived: decides whether
    // the permissions column the listing sets is painted (spec 2026-09-20).
    catalog: Option<&norte_proto::AttrCatalog>,
) -> Vec<norte_frontend::columns::Fitted> {
    let ahead = 3 + if pane.any_icon() { ICON_GUTTER } else { 0 };
    let wants = pane
        .name_width_p80()
        .saturating_add(u16::try_from(ahead).unwrap_or(u16::MAX));
    norte_frontend::columns::fitted_columns(settings, pane.dir().scheme(), inner_w, wants, catalog)
}

/// The pane's border title: where it is, what is happening to it and where
/// it is going.
///
/// Split out of [`draw_pane`] for size — there were six marks chained onto
/// the same `String` — and stays next to it because ORDER is the rule: the
/// status markers (pagination, unlisted) hang off the name, the destination
/// badge goes in front of it, and waiting replaces the name with the
/// DESTINATION. Each mark goes OUTSIDE the directory's name on purpose: a
/// directory named "→" cannot pretend to be the destination.
///
/// `width` is the border's: the waiting title IS clipped, because it is the
/// first one to carry, on purpose, a long text (a remote destination with a
/// scheme and a host) and ratatui clips on the right WITHOUT marking the
/// cut. Losing a path's tail is losing which folder it is; losing the head,
/// where you are — so it is clipped in the middle, like the status bar
/// does.
fn pane_title(
    pane: &Pane,
    is_dest: bool,
    busy: Option<&norte_frontend::busy::Busy>,
    width: u16,
) -> String {
    let (title, title_hostile) =
        norte_frontend::path_display_with(pane.dir(), pane.name_encoding());
    let mut title = if title_hostile {
        format!("{HOSTILE_BADGE} {title}")
    } else {
        title
    };
    // A listing FILLING UP (pagination, ADR 0017) is ALWAYS marked: an
    // incomplete listing is never silent.
    if pane.loading() {
        use std::fmt::Write as _;
        // The PHRASE is written by the shared crate, which is also where
        // the window's header takes it from; the brackets belong to this
        // header and stay here.
        let _ = write!(
            title,
            " [{}]",
            norte_frontend::notes::filling(true, pane.entries().len(), norte_i18n::active())
        );
    }
    // A pane that could NOT be listed when restoring the session says so
    // for as long as it lasts (#235): without this the screen claims the
    // directory is empty, which is precisely what is not known. It goes
    // where pagination does and for the same reason — a listing that is
    // not the listing is never silent.
    if pane.unlisted {
        use std::fmt::Write as _;
        let _ = write!(
            title,
            " [{}]",
            norte_frontend::notes::unlisted(true, norte_i18n::active())
        );
    }
    // The DESTINATION is marked in the chrome, and only when needed: with
    // two panels the destination is the other one and nobody needs to be
    // told, but from three on a copy toward a panel the reader did not have
    // in mind is silent data loss (ADR 0058 D7).
    if is_dest {
        title = format!("{TARGET_BADGE} {title}");
    }
    // Waiting (#323): the spinner goes UP FRONT and the title becomes the
    // DESTINATION, not the current directory. The body keeps showing the
    // previous listing — on purpose: if the connection fails, the reader
    // stays where they were — and without the destination in the header
    // that mix could not be read ("what is this doing?"). With the spinner
    // next to it, it reads "going here".
    if let Some(b) = busy {
        let Some(dest) = b.target.as_ref() else {
            return format!("{} {title}", b.frame());
        };
        // Through `path_display_with` with the PANE's encoding, same as
        // above: it is the same pane that is going to land, its
        // reinterpretation (#98/F2) is the one that rules, and the
        // altered-name badge is kept. Rendering it with no badge was
        // painting a masked path with no mark saying it was masked, right
        // where cancel is also offered.
        let (dest, altered) = norte_frontend::path_display_with(dest, pane.name_encoding());
        // 2 cells for the spinner + the space, plus the badge if it carries
        // one.
        let spent = 2 + usize::from(altered) * (norte_frontend::display::cells(HOSTILE_BADGE) + 1);
        let slot = usize::from(width).saturating_sub(spent);
        let dest = norte_frontend::middle_ellipsis(&dest, slot);
        title = if altered {
            format!("{} {HOSTILE_BADGE} {dest}", b.frame())
        } else {
            format!("{} {dest}", b.frame())
        };
    }
    title
}

// Eleven arguments: this is a pane's render wiring, not an API. Grouping
// them into a struct would only move the list somewhere else and add a
// type nobody uses twice.
//
// And one line over `too_many_lines`'s cap, AFTER pulling out everything
// here that had a name of its own: the window's layout (`pane_window`) and
// the list's state (`list_state`), 34 lines between the two. What is left
// is a painting sequence with no natural joints — block, header, listing —
// and chopping it up further to gain one line would be splitting it where
// it does not split.
/// A cell's text within `content` cells, with the cut MARKED.
///
/// A cut is visible (encoding audit, 2026-09-20). It used to be clipped
/// plainly and the result still looked like a whole value: narrowing the
/// permissions column with the mouse, `-rw-r--r--` and `-rw-r-----` both
/// ended up as `-rw-r--`, and "everyone can read it" and "only the group
/// can" became the same cell with nothing saying text was missing. It is
/// the rule the corpus calls `truncation_twins` and that `sanitize_cell`
/// already satisfied for a plugin's text.
///
/// The mark spends one of the cells there ARE, not an extra one: going over
/// budget would shift the next column.
fn clipped_cell(cell: String, content: usize) -> String {
    if cell.width() <= content {
        return cell;
    }
    let mut s = take_width(&cell, content.saturating_sub(1));
    if content > 0 {
        s.push('…');
    }
    s
}

/// Puts the pyjama stripe under an ODD row, and leaves the even one as it
/// was.
///
/// `band` arrives already resolved against the theme: `None` is pyjama
/// turned off or a theme that does not define it, and both things mean the
/// same to whoever paints — no stripe.
///
/// The parity is the PAINTED row's. As the `ListItem`'s BASE style, so that
/// the row's spans are painted on top and `highlight_style` — the cursor —
/// wins by coming after.
fn stripe(band: Option<ratatui::style::Style>, row: usize, item: ListItem<'_>) -> ListItem<'_> {
    match band {
        Some(style) if row % 2 == 1 => item.style(style),
        _ => item,
    }
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "a pane's render wiring; what could be extracted already went to pane_window, list_state, clipped_cell and stripe"
)]
pub(crate) fn draw_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    pane: &Pane,
    focused: bool,
    theme: &TuiTheme,
    now_ms: i64,
    settings: &norte_frontend::columns::ColumnsSettings,
    catalog: Option<&norte_proto::AttrCatalog>,
    tabs: Option<&TabStrip>,
    is_dest: bool,
    // The wait affecting THIS panel, if any and if it is already past the
    // threshold. The filtering is done by the caller, which is the one who
    // knows what index this is.
    busy: Option<&norte_frontend::busy::Busy>,
    // The panel's footer (spec 2026-09-10), already composed by the caller,
    // which is the one that has the volumes and the setting. `None` = off.
    footer: Option<&str>,
    // `[ui] dir_indicator` (spec 2026-09-15): what to do with folders' `/`.
    // The KEY arrives, not the boolean, because `auto` depends on something
    // only known here: whether this listing opened the icon column.
    dir_indicator: norte_config::load::DirIndicator,
    // `[ui] row_stripes` (spec 2026-09-20): the "pyjama". The parity is the
    // PAINTED row's, not the entry's index — a listing filtered by quick
    // search still alternates, which is what the stripe is about.
    stripes: bool,
) {
    let border_style = if focused {
        theme.role(Role::BorderFocus)
    } else {
        theme.role(Role::BorderUnfocused)
    };
    // The BORDER's width minus its two corners: it is what ratatui leaves
    // for the title before clipping without warning.
    let title = pane_title(pane, is_dest, busy, area.width.saturating_sub(2));
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title_style(theme.role(Role::Title))
        .title(title);
    // Active quick search (spec 2026-07-18): input line at the pane's
    // footer `/{query} n/m` (+ "partial" if the fill is still going: it
    // filters over what has ALREADY drained, never silently). The query
    // goes through the SAME mask as the names (review MINOR-1 T4): "the
    // user typed it" breaks with a PASTE — with no bracketed paste it
    // arrives as a stream of Chars, and a pasted hostile name would paint
    // raw bidi/invisibles on the border.
    if let Some(q) = pane.quick() {
        let (query, _) = display_name(q.query_display().as_bytes());
        let mut input = format!(" /{} {}/{}", query, q.visible().len(), pane.entries().len());
        if pane.loading() {
            input.push(' ');
            input.push_str(&t("quicksearch-partial"));
        }
        input.push(' ');
        block = block.title_bottom(Line::styled(input, theme.role(Role::Title)));
    } else if let Some(footer) = footer {
        // The footer (spec 2026-09-10) goes in the same slot as the search
        // box and the more specific one rules: while typing, what is typed.
        // It is clipped in cells to what the border leaves, without eating
        // into the corners.
        let room = usize::from(area.width.saturating_sub(4));
        let text = format!(" {} ", middle_ellipsis(footer, room));
        // With the border role of ITS OWN panel, not always the unfocused
        // panel's (spec 2026-09-15): the footer is written OVER the border,
        // and always painting it dimmed left it illegible right on the
        // panel the reader is looking at.
        block = block.title_bottom(Line::styled(text, border_style));
    }
    // Active filter: ONLY the visible indices, with the visual cursor at
    // the position WITHIN the filtering. In Jump (quick_visible = None) the
    // whole listing is used and the real cursor rules.
    let reinterpret = pane.name_encoding();
    // #108 L5: column widths from the pane's INTERIOR width, once per
    // frame — the rows and the header share the same layout (with the 7b
    // style resolved per column, see `styled_columns`).
    let inner_w = block.inner(area).width;
    let cols = &styled_columns(settings, pane, inner_w, catalog);
    // The PAINTED selection comes from the same function the mouse's hit
    // test uses ([`painted_len_and_selection`]): the scroll below is
    // derived from it, and two different calculations would make a click
    // land on the row next door.
    let (painted_len, selected) = painted_len_and_selection(pane);
    // The icon column is decided by the LISTING, once: if any row has an
    // icon, all of them carry the slot.
    let icons = pane.any_icon();
    let dir_slash = paint_slash(dir_indicator, icons);
    let window = pane_window(pane, area, tabs.is_some(), painted_len);
    // The odd rows' stripe. Resolved ONCE per paint: the role with no
    // color (the theme does not define it) leaves the listing exactly as
    // it was, so the setting being on over a mute theme is not a failure,
    // it is a normal listing.
    let band = stripes.then(|| theme.role(Role::Stripe));
    let items: Vec<ListItem<'_>> = match pane.quick_visible() {
        Some(vis) => vis
            .iter()
            .enumerate()
            .skip(window.start)
            .take(window.len())
            .filter_map(|(row, &i)| pane.entries().get(i).map(|e| (row, i, e)))
            .map(|(row, i, e)| {
                stripe(
                    band,
                    row,
                    entry_item(
                        e,
                        theme,
                        reinterpret,
                        pane.decoration_for(&e.path),
                        pane.is_marked(e),
                        cols,
                        Some(pane),
                        now_ms,
                        pane.is_parent_row(i),
                        icons,
                        dir_slash,
                    ),
                )
            })
            .collect(),
        None => pane
            .entries()
            .iter()
            .enumerate()
            .skip(window.start)
            .take(window.len())
            .map(|(i, e)| {
                stripe(
                    band,
                    i,
                    entry_item(
                        e,
                        theme,
                        reinterpret,
                        pane.decoration_for(&e.path),
                        pane.is_marked(e),
                        cols,
                        Some(pane),
                        now_ms,
                        pane.is_parent_row(i),
                        icons,
                        dir_slash,
                    ),
                )
            })
            .collect(),
    };
    // #108 L5: hand-built block — inside, ONE column-header line (dim,
    // with the active sort's ▲/▼ indicator) and the listing below.
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let (header_area, list_area) = draw_tab_strip(frame, inner, tabs, theme);
    frame.render_widget(
        Paragraph::new(column_header_line(
            cols,
            &pane.sort(),
            catalog,
            if icons { ICON_GUTTER } else { 0 },
        ))
        .style(ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM)),
        header_area,
    );
    // Two cursors equally alive do not say which receives the keys: the
    // unfocused panel's carries its own role (spec 2026-09-10).
    let cursor_role = if focused {
        Role::Selection
    } else {
        Role::SelectionUnfocused
    };
    let list = List::new(items).highlight_style(theme.role(cursor_role));
    let mut state = list_state(selected, &window);
    frame.render_stateful_widget(list, list_area, &mut state);
}

/// Is the `/` painted in front of a directory? (`[ui] dir_indicator`, spec
/// 2026-09-15.)
///
/// `auto` removes it when the icon column is open: the icon already says
/// what the row is, and the slash only spends a cell of the name.
fn paint_slash(indicator: norte_config::load::DirIndicator, icons: bool) -> bool {
    match indicator {
        norte_config::load::DirIndicator::Slash => true,
        norte_config::load::DirIndicator::None => false,
        norte_config::load::DirIndicator::Auto => !icons,
    }
}

// Four booleans: they are four FACTS about the listing the row cannot
// deduce (marked, parent row, icon column open, directory slash), and
// grouping them into a struct would move the list elsewhere without
// removing any of them. Same criterion as `draw_pane`'s
// `too_many_arguments`.
#[expect(
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    reason = "render row: each arg is a source of painting, not an API"
)]
pub(crate) fn entry_item<'a>(
    entry: &'a norte_proto::Entry,
    theme: &TuiTheme,
    reinterpret: Option<norte_encoding::NameEncoding>,
    decoration: Option<&norte_frontend::Decoration>,
    marked: bool,
    cols: &[(
        norte_frontend::columns::ColumnId,
        u16,
        norte_frontend::columns::ColumnStyle,
    )],
    // #117-follow-up: source of the `plugin:` cells (the pane's side-map —
    // their values do not live in the `Entry`). `None` only in format tests
    // with no plugin columns.
    plugin_cells: Option<&Pane>,
    now_ms: i64,
    // The PARENT row (`[ui] parent_entry`): `..` is painted, not the parent
    // directory's name, which is what its path says. The parent's name on
    // the first row reads as "there is a directory here called that",
    // which is exactly what there is not.
    parent_row: bool,
    // The icon column (ADR 0105) is open in this listing: SOME row has an
    // icon, so all of them carry the slot, with or without one, so the
    // names stay aligned. The pane decides it, not the row.
    icons: bool,
    // Paints the `/` in front of a directory (`[ui] dir_indicator`, spec
    // 2026-09-15). It arrives ALREADY DECIDED: whoever paints the listing
    // knows whether the icon column is open, and `auto` means "only if it
    // is not".
    dir_slash: bool,
) -> ListItem<'a> {
    let name = entry.path.file_name().map_or(&[][..], |n| n.as_bytes());
    // #57: with reinterpretation active, non-UTF8 names are decoded with
    // the chosen encoding (display-only; the hostile badge is kept — the
    // painted text differs from the real bytes).
    let (text, hostile) = if parent_row {
        // Two dots and nothing else: no hostile badge — `..` is two ASCII
        // chars — nor reinterpretation, because it is nobody's name.
        ("..".to_owned(), false)
    } else {
        norte_frontend::display_name_with(name, reinterpret)
    };
    // `[ui] dir_indicator` (spec 2026-09-15): a folder's `/` is from when
    // there were no icons. With the icon column open, the icon already
    // says what the row is and the slash only spends a cell of the name;
    // `auto` removes it there and keeps it where it is still needed. The
    // symlink's `@` is not touched: there is no icon that says it.
    let kind_glyph = match entry.kind {
        EntryKind::Dir if dir_slash => "/",
        EntryKind::Symlink => "@",
        // A directory with NO slash is painted like a file: the slot
        // stays, so the names remain aligned.
        EntryKind::Dir | EntryKind::File | EntryKind::Other => " ",
    };
    let badge = Span::styled(
        if hostile { HOSTILE_BADGE } else { " " },
        theme.role(Role::HostileBadge),
    );
    // Color by the entry's type/extension (ADR 0020 D2).
    let body = Span::styled(format!("{kind_glyph}{text}"), theme.entry(name, entry.kind));
    // Mark gutter (#103): a TEXTUAL signal, never color alone — the
    // monochrome fallback of `Role::Mark` is `dim`, which on its own reads
    // as "inactive", not "selected". It goes BEFORE the hostile badge so
    // that neither the badge nor the decoration change column compared to
    // how they used to be painted.
    //
    // The STYLE also has to be conditional, not just the glyph (BLOCKER
    // review): every bundled preset defines `mark` as ONLY a `bg` (see
    // `crates/norte-theme/presets/*.toml`), so an unconditional
    // `Span::styled` painted that color stripe in column 1 of EVERY
    // unmarked row — a permanent stripe, not a mark signal.
    let gutter = if marked {
        Span::styled("*", theme.role(Role::Mark))
    } else {
        Span::raw(" ")
    };
    let mut spans = vec![gutter, badge];
    spans.extend(icons.then(|| icon_span(decoration, theme, name, entry.kind)));
    spans.push(body);
    // What goes ahead of the name and is not clipped: gutter, badge, icons.
    let fijos = spans.len() - 1;
    // G3b (ADR 0037): decorator badge, AFTER the hostile-badge slot —
    // already SANITIZED and bounded (`norte_frontend::sanitize_decoration`,
    // applied before it gets here). With no decoration for this entry, no
    // extra span (not even a slot): the row looks EXACTLY like it did
    // before G3b for anyone not using decorators.
    if let Some(badge_text) = decoration.and_then(|d| d.badge.as_deref()) {
        let style = match decoration.and_then(|d| d.role) {
            Some(role) => theme.role(role),
            // No recognized role: dim by default — visible but discreet,
            // never the entry's "normal" color (it would be confused with
            // the name) nor a color invented by this frontend (ADR 0037:
            // the user's theme rules, never a raw color it did not ask
            // for).
            None => ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM),
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(badge_text.to_string(), style));
    }
    // #108 L5: column cells after the name. The name block
    // (gutter+badge+glyph+text+decoration) is TRUNCATED to its layout width
    // (middle ellipsis, cell-aware — CJK/emoji do not overflow) and padded;
    // each non-name cell is aligned according to its style (#108 7b, right
    // by default) within its width, dim, with a separating space. Absence =
    // a blank cell, never a fabricated 0.
    if let Some((_, name_w, _)) = cols.first() {
        let name_w = usize::from(*name_w);
        // review #108-5 M2: the DECORATION also counts against the name's
        // budget — a CJK badge (8 chars = 16 cells) used to shift every
        // cell in the row. If it does not fit while leaving ≥3 cells for
        // the name, out goes the whole decoration (separator included): the
        // name rules.
        if spans.len() > fijos + 1 {
            let deco: usize = spans[fijos + 1..].iter().map(|sp| sp.content.width()).sum();
            let fixed: usize = spans[..fijos].iter().map(|sp| sp.content.width()).sum();
            if fixed + deco + 3 > name_w {
                spans.truncate(fijos + 1);
            }
        }
        let used: usize = spans.iter().map(|sp| sp.content.width()).sum();
        if used > name_w {
            // Clips the name's TEXT (the body span, index `fijos`: after
            // the gutter, the badge and — if there is one — the icon
            // column) with middle ellipsis to what is left after the other
            // spans — the fixed ones and the decoration stay. With a
            // literal index here, the icon column used to shift the name
            // over by one slot and the clip ate the ICON of every long row
            // instead of the name.
            let others: usize = spans
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != fijos)
                .map(|(_, sp)| sp.content.width())
                .sum();
            let body_w = name_w.saturating_sub(others);
            let truncated = middle_ellipsis(&spans[fijos].content, body_w);
            spans[fijos] = Span::styled(truncated, spans[fijos].style);
        }
        let used: usize = spans.iter().map(|sp| sp.content.width()).sum();
        // Not even with the name clipped to zero does it always fit: in a
        // column of one or two cells — what `full` leaves on a 40-column
        // terminal — the gutter and the badge already fill it on their
        // own. The WHOLE block is clipped on the right. This used to be a
        // `debug_assert`, which in tests is a panic and in release a row
        // painting outside its column.
        if used > name_w {
            spans = clamp_spans(std::mem::take(&mut spans), name_w);
        }
        let used: usize = spans.iter().map(|sp| sp.content.width()).sum();
        debug_assert!(
            used <= name_w,
            "the name block overflows its column: {used} > {name_w}"
        );
        if used < name_w {
            spans.push(Span::raw(" ".repeat(name_w - used)));
        }
        for (col, w, style) in cols.iter().skip(1) {
            // #117-follow-up: the `plugin:` cells come from the pane's
            // side-map (re-masked there); the rest, from the Entry as
            // always. Absence = blank on both paths.
            let cell = match col {
                norte_frontend::columns::ColumnId::Plugin { .. } => plugin_cells
                    .and_then(|p| p.plugin_cell(&col.to_string(), &entry.path))
                    .unwrap_or_default(),
                _ => norte_frontend::columns::styled_cell(entry, col, now_ms, style)
                    .unwrap_or_default(),
            };
            // The width INCLUDES the separator (default_layout_items): the
            // content lives within w-1 and there is always ≥1 separator
            // space left. Right (default): padding up front. Left (#108
            // 7b): the separator keeps OPENING the budget, the content
            // follows it and the padding falls on the right — the same
            // count, reversed.
            let w = usize::from(*w);
            let content = w.saturating_sub(1);
            let truncated = clipped_cell(cell, content);
            let text = match style.align {
                norte_frontend::columns::Align::Right => {
                    let pad = w.saturating_sub(truncated.width());
                    format!("{}{truncated}", " ".repeat(pad))
                }
                norte_frontend::columns::Align::Left => {
                    let pad = w.saturating_sub(truncated.width().saturating_add(1));
                    format!(" {truncated}{}", " ".repeat(pad))
                }
            };
            spans.push(Span::styled(
                text,
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ));
        }
    }
    ListItem::new(Line::from(spans))
}

#[cfg(test)]
mod entry_item_columns_tests {
    use super::*;
    use norte_proto::{EntryKind, VPath};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{List, Widget as _};

    /// review #108-5 M2: a CJK decoration (16 cells) with the name column
    /// at its minimum does NOT shift the cells — the decoration falls
    /// before it breaks alignment, and the row's total width is EXACT.
    #[test]
    fn a_wide_decoration_never_shifts_the_columns() {
        use norte_frontend::columns::{Builtin, ColumnId, ColumnStyle, LayoutItem, WidthPolicy};
        let entry = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse("mem:///f.txt").unwrap(),
            kind: EntryKind::File,
            size: Some(7),
            mtime_ms: None,
        };
        let deco = norte_frontend::Decoration {
            badge: Some("全全全全全全全全".to_owned()),
            ..Default::default()
        };
        let theme = TuiTheme::default();
        let widths = [
            (
                ColumnId::Builtin(Builtin::Name),
                10u16,
                ColumnStyle::default_for(Builtin::Name),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                11u16,
                ColumnStyle::default_for(Builtin::Size),
            ),
        ];
        let _ = LayoutItem {
            policy: WidthPolicy::Auto,
            measured: 0,
            is_name: false,
        };
        let item = entry_item(
            &entry,
            &theme,
            None,
            Some(&deco),
            false,
            &widths,
            None,
            0,
            false,
            // (columna de iconos, barra de directorio)
            false,
            false,
        );
        // Renders to a buffer of the EXACT width of the budget: if the row
        // overflowed, the size cell would lose its tail.
        let area = Rect::new(0, 0, 21, 1);
        let mut buf = Buffer::empty(area);
        List::new(vec![item]).render(area, &mut buf);
        let row: String = (0..21).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(row, "   f.txt          7 B", "{row:?}");
    }

    /// ADR 0105: with the icon column open, a name that does not fit is
    /// clipped ITSELF — with its middle ellipsis, which is what saves the
    /// extension — and the icon stays. With a literal index, the clip used
    /// to eat the icon of every long row.
    #[test]
    fn with_icons_the_clip_hits_the_name_and_the_icon_stays() {
        use norte_frontend::columns::{Builtin, ColumnId, ColumnStyle};
        let entry = norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse("mem:///un_nombre_bastante_largo.png").unwrap(),
            kind: EntryKind::File,
            size: Some(7),
            mtime_ms: None,
        };
        let deco = norte_frontend::Decoration {
            icon: Some("🦀".to_owned()),
            ..Default::default()
        };
        let theme = TuiTheme::default();
        let widths = [
            (
                ColumnId::Builtin(Builtin::Name),
                18u16,
                ColumnStyle::default_for(Builtin::Name),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                6u16,
                ColumnStyle::default_for(Builtin::Size),
            ),
        ];
        let item = entry_item(
            &entry,
            &theme,
            None,
            Some(&deco),
            false,
            &widths,
            None,
            0,
            false,
            // (icon column, directory slash: with an icon, `auto` removes
            // it — which is exactly what this test paints)
            true,
            false,
        );
        let area = Rect::new(0, 0, 24, 1);
        let mut buf = Buffer::empty(area);
        List::new(vec![item]).render(area, &mut buf);
        let row: String = (0..24).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row.starts_with("  🦀 "), "the icon stays: {row:?}");
        assert!(
            row.contains('…'),
            "and the name carries the ellipsis: {row:?}"
        );
        assert!(
            row.ends_with("7 B"),
            "and the size cell is in its place: {row:?}"
        );
    }
}

#[cfg(test)]
mod draw_pane_attr_tests {
    use super::*;
    use norte_proto::{Segment, VPath};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn entry(dir: &VPath, name: &str) -> norte_proto::Entry {
        norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    /// #117 task 2: attr cells with HOSTILE values from a provider, painted
    /// end-to-end by `draw_pane` (resolved config → layout → cell): never a
    /// raw dangerous char, FLAGGED lossy (U+FFFD) for non-UTF8 Bytes,
    /// absence = blank, and a header with the id as fallback (no catalog).
    #[test]
    fn hostile_attr_cells_are_masked_and_absence_is_blank() {
        use norte_proto::attrs::AttrValue;
        // Config: name + attr:mem.owner (non-UTF8 Bytes) + attr:mem.note
        // (bidi RTL + ZWJ).
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec![
                "name".into(),
                "attr:mem.owner".into(),
                "attr:mem.note".into(),
            ]),
            ..Default::default()
        };
        let settings = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
        let dir = VPath::parse("mem:///d").unwrap();
        let mut e1 = entry(&dir, "aaa");
        e1.attrs.insert(
            "mem.owner".into(),
            AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()),
        );
        e1.attrs.insert(
            "mem.note".into(),
            AttrValue::Text("\u{202e}at\u{f3}n\u{202c} a\u{200d}b".into()),
        );
        let e2 = entry(&dir, "bbb"); // NO attrs: blank cells
        let pane = Pane::new(dir, vec![e1, e2]);
        let theme = TuiTheme::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("test terminal");
        terminal
            .draw(|f| {
                draw_pane(
                    f,
                    f.area(),
                    &pane,
                    true,
                    &theme,
                    0,
                    &settings,
                    None,
                    None,
                    false,
                    None,
                    None,
                    norte_config::load::DirIndicator::default(),
                    false,
                );
            })
            .expect("draw");
        let text = terminal.backend().to_string();
        // 1. No cell in the buffer carries a raw dangerous char (controls,
        //    bidi overrides, invisibles — spec §6). Per line: the `\n`s
        //    `to_string` joins with are the harness's, not the buffer's.
        assert!(
            text.lines()
                .all(|l| l.chars().all(|c| !norte_encoding::is_terminal_hazard(c))),
            "raw hazard in the render: {text:?}"
        );
        // 2. e1's row paints the owner LOSSY and FLAGGED (visible U+FFFD).
        let row_e1 = text.lines().find(|l| l.contains("aaa")).expect("aaa's row");
        assert!(
            row_e1.contains('\u{FFFD}'),
            "lossy owner not flagged: {row_e1:?}"
        );
        // 3. e2's row (no attrs) paints the attr columns BLANK: removing
        //    the name, the borders and the spaces, nothing is left (blank
        //    = ABSENT, never a fabricated value).
        let row_e2 = text.lines().find(|l| l.contains("bbb")).expect("bbb's row");
        // (The quotes per line are put there by `TestBackend`'s Display.)
        let rest: String = row_e2
            .replace("bbb", "")
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '│' && *c != '"')
            .collect();
        assert_eq!(rest, "", "absence must be blank: {row_e2:?}");
        // 4. The header carries the id as fallback (no catalog here).
        assert!(text.contains("mem.owner"), "header with no id: {text}");
    }

    /// #117 encoding-audit L2: a WIDE attr cell (CJK double-width + the
    /// corpus's ZWJ emoji family — `MemProvider`'s `mem.wide` value) NEVER
    /// shifts the neighboring column: the size cell's x is identical
    /// between the wide row and a blank row (mirror of
    /// `a_wide_decoration_never_shifts_the_columns`). What is pinned is the
    /// ratatui buffer's cell alignment; ZWJ collapsing on a real terminal
    /// is the preexisting limitation the name column already shares.
    #[test]
    fn a_wide_attr_cell_never_shifts_the_neighboring_column() {
        use norte_proto::attrs::AttrValue;
        let cfg = norte_config::ColumnsConfig {
            default_columns: Some(vec!["name".into(), "attr:mem.wide".into(), "size".into()]),
            ..Default::default()
        };
        let settings = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
        let dir = VPath::parse("mem:///d").unwrap();
        let mut e1 = entry(&dir, "aaa");
        e1.attrs.insert(
            "mem.wide".into(),
            AttrValue::Text("日本語👨\u{200d}👩\u{200d}👧\u{200d}👦".into()),
        );
        let e2 = entry(&dir, "bbb"); // NO attrs: the wide cell blank
        let pane = Pane::new(dir, vec![e1, e2]);
        let theme = TuiTheme::default();
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).expect("test terminal");
        terminal
            .draw(|f| {
                draw_pane(
                    f,
                    f.area(),
                    &pane,
                    true,
                    &theme,
                    0,
                    &settings,
                    None,
                    None,
                    false,
                    None,
                    None,
                    norte_config::load::DirIndicator::default(),
                    false,
                );
            })
            .expect("draw");
        let buf = terminal.backend().buffer();
        // The x (in buffer CELLS, not chars) of the size's "1" in the row
        // that contains `name`.
        let size_x = |name: &str| -> u16 {
            for y in 0..buf.area.height {
                let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                if row.contains(name) {
                    for x in 0..buf.area.width {
                        if buf[(x, y)].symbol() == "1" {
                            return x;
                        }
                    }
                }
            }
            panic!("row {name} has no size cell");
        };
        assert_eq!(
            size_x("aaa"),
            size_x("bbb"),
            "the wide cell shifted the size column"
        );
    }

    /// #323: while waiting, the header says WHERE it is going and spins;
    /// the body keeps showing the previous listing.
    ///
    /// Both halves matter. If the header kept the current directory, a
    /// reader with two panels would not know which of the two is waiting
    /// nor for what; and if the body emptied out, a failed connection would
    /// have cost them the place they were at.
    #[test]
    fn while_waiting_the_header_spins_and_says_the_destination() {
        use norte_frontend::busy::{Busy, BusyKind};
        let settings = norte_frontend::columns::ColumnsSettings::resolve(
            &norte_config::ColumnsConfig::default(),
        );
        let dir = VPath::parse("mem:///aqui").unwrap();
        let pane = Pane::new(dir.clone(), vec![entry(&dir, "fichero-de-antes")]);
        let theme = TuiTheme::default();

        let paint = |busy: Option<&Busy>| {
            let mut terminal = Terminal::new(TestBackend::new(60, 6)).expect("test terminal");
            terminal
                .draw(|f| {
                    draw_pane(
                        f,
                        f.area(),
                        &pane,
                        true,
                        &theme,
                        0,
                        &settings,
                        None,
                        None,
                        false,
                        busy,
                        None,
                        norte_config::load::DirIndicator::default(),
                        false,
                    );
                })
                .expect("draw");
            terminal.backend().to_string()
        };

        let dest = VPath::parse("mem:///alli").unwrap();
        let mut busy = Busy::new(BusyKind::Connecting, Some(dest), Some(0));
        busy.elapsed = norte_frontend::busy::THRESHOLD;
        let waiting = paint(Some(&busy));
        assert!(
            waiting.contains("alli"),
            "the header does not say the destination: {waiting}"
        );
        // The role marker ALWAYS goes up front, and it is what keeps a
        // directory named "⠋ connecting…" from passing itself off as a
        // waiting header. For a scheme that is not the usual one, that
        // marker is `⟨scheme⟩`; for `file` it is the leading `/`, which a
        // name cannot carry. BOTH are checked: the claim "⟨ always appears"
        // stopped being true the day local paths stopped announcing
        // themselves, and this test stayed green because it only tested
        // `mem`.
        assert!(
            waiting.contains('⟨'),
            "the header lost the scheme marker: {waiting}"
        );
        let local = VPath::parse("file:///alli").unwrap();
        let mut busy_local = Busy::new(BusyKind::Connecting, Some(local), Some(0));
        busy_local.elapsed = norte_frontend::busy::THRESHOLD;
        let waiting_local = paint(Some(&busy_local));
        assert!(
            waiting_local.contains("/alli"),
            "a local path carries its slash up front: {waiting_local}"
        );
        assert!(
            waiting.contains(busy.frame()),
            "the header does not carry the spinner: {waiting}"
        );
        assert!(
            waiting.contains("fichero-de-antes"),
            "the body emptied out while waiting: {waiting}"
        );

        // With no wait, the header is the usual one and there is no trace
        // of anything.
        let idle = paint(None);
        assert!(!idle.contains("alli"), "{idle}");
        assert!(!idle.contains(busy.frame()), "{idle}");
    }

    /// #323, encoding-audit BLOCKER finding: a wait's destination carries the
    /// SAME altered-name badge it will carry once the panel lands.
    ///
    /// Without this, the reader saw the masked path with no mark saying it
    /// was masked — and precisely while cancel is being offered — and if
    /// the connection failed the panel never landed, so the badge never got
    /// to be painted at all. The repository's rule allows no nuance: the
    /// text that is painted is always lossy and FLAGGED.
    #[test]
    fn the_altered_destination_carries_its_badge_while_waiting() {
        use norte_frontend::busy::{Busy, BusyKind};
        let settings = norte_frontend::columns::ColumnsSettings::resolve(
            &norte_config::ColumnsConfig::default(),
        );
        let here = VPath::parse("mem:///aqui").unwrap();
        let pane = Pane::new(here.clone(), vec![entry(&here, "x")]);
        let theme = TuiTheme::default();
        // The hostile corpus's `rtl_override`: `abc<U+202E>gpj.exe`.
        let hostile = VPath::parse("mem:///abc%E2%80%AEgpj.exe").unwrap();

        let mut busy = Busy::new(BusyKind::Connecting, Some(hostile), Some(0));
        busy.elapsed = norte_frontend::busy::THRESHOLD;
        let mut terminal = Terminal::new(TestBackend::new(60, 6)).expect("test terminal");
        terminal
            .draw(|f| {
                draw_pane(
                    f,
                    f.area(),
                    &pane,
                    true,
                    &theme,
                    0,
                    &settings,
                    None,
                    None,
                    false,
                    Some(&busy),
                    None,
                    norte_config::load::DirIndicator::default(),
                    false,
                );
            })
            .expect("draw");
        let painted = terminal.backend().to_string();
        assert!(
            painted.contains(HOSTILE_BADGE),
            "the altered destination was painted with NO badge: {painted}"
        );
        assert!(
            !painted.contains('\u{202e}'),
            "the bidi override reached the terminal raw: {painted}"
        );
    }

    /// The destination is bounded to the border's width: ratatui clips on
    /// the right and does NOT mark the cut, so with no budget a long path
    /// silently loses its end — which folder it is. With two hosts that
    /// share the first 40 characters, that is two different destinations
    /// painted the same.
    #[test]
    fn a_long_destination_is_clipped_in_the_middle_and_says_so() {
        use norte_frontend::busy::{Busy, BusyKind};
        let settings = norte_frontend::columns::ColumnsSettings::resolve(
            &norte_config::ColumnsConfig::default(),
        );
        let here = VPath::parse("mem:///aqui").unwrap();
        let pane = Pane::new(here.clone(), vec![entry(&here, "x")]);
        let theme = TuiTheme::default();
        let long =
            VPath::parse("mem:///produccion/equipo/almacen/interno/example/org/carpeta").unwrap();

        let mut busy = Busy::new(BusyKind::Connecting, Some(long), Some(0));
        busy.elapsed = norte_frontend::busy::THRESHOLD;
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).expect("test terminal");
        terminal
            .draw(|f| {
                draw_pane(
                    f,
                    f.area(),
                    &pane,
                    true,
                    &theme,
                    0,
                    &settings,
                    None,
                    None,
                    false,
                    Some(&busy),
                    None,
                    norte_config::load::DirIndicator::default(),
                    false,
                );
            })
            .expect("draw");
        let painted = terminal.backend().to_string();
        assert!(
            painted.contains('…'),
            "it was clipped with no mark for the cut: {painted}"
        );
        assert!(
            painted.contains("carpeta"),
            "the TAIL was lost, which is what folder it is: {painted}"
        );
    }

    /// Paints a listing with pyjama on and returns each row's background,
    /// top to bottom. `filter` types a quick search.
    fn pyjama_backgrounds(names: &[&str], filter: Option<&str>) -> Vec<ratatui::style::Color> {
        use norte_theme::{Role, Style, Theme};
        let dir = VPath::parse("mem:///d").expect("vpath");
        let entries: Vec<_> = names.iter().map(|n| entry(&dir, n)).collect();
        let mut pane = Pane::new(dir, entries);
        if let Some(f) = filter {
            pane.quick_start(crate::nav::Mode::Filter);
            for c in f.chars() {
                pane.quick_char(c);
            }
        }
        // A theme that DOES define the stripe: with no color, the role
        // falls back to monochrome and the test could not tell "off" apart
        // from "no theme".
        let mut theme = Theme::preset_default();
        theme.roles.insert(
            Role::Stripe,
            Style::new().bg(norte_theme::Color::rgb(0x33, 0x33, 0x33)),
        );
        let theme = TuiTheme::new(theme, norte_theme::ColorDepth::Truecolor);
        let settings = norte_frontend::columns::ColumnsSettings::default();
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).expect("test terminal");
        terminal
            .draw(|f| {
                draw_pane(
                    f,
                    f.area(),
                    &pane,
                    true,
                    &theme,
                    0,
                    &settings,
                    None,
                    None,
                    false,
                    None,
                    None,
                    norte_config::load::DirIndicator::default(),
                    true,
                );
            })
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        // The listing's first row comes after the border and the header.
        (0..4).map(|i| buf[(2, 2 + i)].bg).collect()
    }

    /// The pyjama paints the ODD rows and leaves the even ones as they were
    /// (spec 2026-09-20).
    ///
    /// Row 0 is the CURSOR's, and that is why the test starts at row 1:
    /// that it carries neither the stripe nor the plain background is the
    /// other half of what needs checking, and it is below.
    #[test]
    fn the_pyjama_alternates_the_rows() {
        let fondos = pyjama_backgrounds(&["a0", "a1", "a2", "a3"], None);
        let band = ratatui::style::Color::Rgb(0x33, 0x33, 0x33);
        assert_eq!(fondos[1], band, "row 1 is odd: stripe");
        assert_eq!(fondos[3], band, "so is row 3");
        assert_ne!(fondos[2], fondos[1], "row 2 is even: no stripe");
    }

    /// The cursor is painted OVER the stripe: that is what the ADR, the
    /// role and the two help themes promise, and what turns the pyjama
    /// into a reading aid instead of a lie about where the keys go.
    #[test]
    fn the_cursor_beats_the_stripe() {
        // With the cursor on an ODD row, which is where the stripe would be.
        let fondos = pyjama_backgrounds(&["a0", "a1", "a2", "a3"], None);
        let band = ratatui::style::Color::Rgb(0x33, 0x33, 0x33);
        assert_ne!(fondos[0], band, "the cursor's row carries no stripe");
        assert_ne!(fondos[0], fondos[2], "nor the even rows' plain background");
    }

    /// The parity is the PAINTED row's, not the entry's index.
    ///
    /// This is the subtle part: with quick search filtering, a listing that
    /// alternated by entry index would show two stripes in a row as soon as
    /// the filter skipped a row — which is exactly when the pyjama is good
    /// for something.
    #[test]
    fn the_stripe_alternates_what_is_seen_not_what_there_is() {
        // `b` falls on indices 1 and 3 of the whole listing; filtered, they
        // are painted rows 0 and 1, so they have to come out DIFFERENT.
        let fondos = pyjama_backgrounds(&["a0", "b0", "a1", "b1"], Some("b"));
        assert_ne!(
            fondos[0], fondos[1],
            "two consecutive rows of the filtered listing with the same stripe"
        );
    }
}

#[cfg(test)]
mod entry_item_tests {
    use super::{HOSTILE_BADGE, entry_item};
    use crate::theme::TuiTheme;
    use norte_proto::{Entry, EntryKind, VPath};
    use ratatui::widgets::ListItem;

    fn e(wire: &str, k: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).unwrap(),
            kind: k,
            size: None,
            mtime_ms: None,
        }
    }

    /// Non-UTF8 name (raw bytes via `Segment`): triggers the hostile badge
    /// with no reinterpretation involved.
    fn e_hostile() -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse("mem:///")
                .unwrap()
                .join(norte_proto::Segment::new(b"\xFF\xFE".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        }
    }

    /// `ListItem`'s span content is private to ratatui, so — like the
    /// crate's other render tests (`tests/theme_render.rs`,
    /// `tests/render.rs`) — this renders the row into a real `Buffer` and
    /// reads it back cell by cell. The gutter and the hostile badge are each
    /// exactly one cell wide by construction, so `span_texts()[0]` and `[1]`
    /// are the true first two spans' text; later cells belong to the
    /// (possibly multi-char) name span and are not meant to be compared
    /// one-for-one with spans.
    fn span_texts(item: &ListItem<'_>) -> Vec<String> {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::{List, Widget as _};
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        List::new(vec![item.clone()]).render(area, &mut buf);
        (0..area.width)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect()
    }

    fn first_span_text(item: &ListItem<'_>) -> String {
        span_texts(item).into_iter().next().unwrap_or_default()
    }

    /// A marked row carries a TEXTUAL cue, never colour alone: `Role::Mark`'s
    /// monochrome fallback is `dim`, which on its own reads as "inactive"
    /// rather than "selected" (#103).
    #[test]
    fn a_marked_row_starts_with_the_mark_gutter() {
        let entry = e("mem:///a", EntryKind::File);
        let theme = TuiTheme::default();
        let marked = entry_item(
            &entry,
            &theme,
            None,
            None,
            true,
            &[],
            None,
            0,
            false,
            false,
            true,
        );
        let plain = entry_item(
            &entry,
            &theme,
            None,
            None,
            false,
            &[],
            None,
            0,
            false,
            false,
            true,
        );
        assert_eq!(first_span_text(&marked), "*");
        assert_eq!(first_span_text(&plain), " ");
    }

    /// The gutter goes BEFORE the hostile badge, so the badge column and the
    /// decorator badge keep the positions they have today.
    #[test]
    fn the_gutter_precedes_the_hostile_badge() {
        let entry = e_hostile();
        let theme = TuiTheme::default();
        let item = entry_item(
            &entry,
            &theme,
            None,
            None,
            true,
            &[],
            None,
            0,
            false,
            false,
            true,
        );
        let texts = span_texts(&item);
        assert_eq!(texts[0], "*");
        assert_eq!(texts[1], HOSTILE_BADGE);
    }
}

#[cfg(test)]
mod column_header_line_tests {
    use super::column_header_line;
    use norte_frontend::columns::{Align, Builtin, ColumnId, ColumnStyle};
    use norte_frontend::{SortColumn, SortDir, SortSpec};
    use unicode_width::UnicodeWidthStr;

    fn style(b: Builtin, align: Align, header: &str) -> ColumnStyle {
        ColumnStyle {
            align,
            header: Some(header.to_owned()),
            ..ColumnStyle::default_for(b)
        }
    }

    /// m1 review 7b: LEFT-aligned column of one cell with the sort's arrow
    /// active — the emission stays clamped to exactly `w` (before,
    /// "space + arrow" were 2 cells and shifted the whole header to the
    /// right; with 2 cells the arrow does fit after the separator).
    #[test]
    fn a_left_header_of_one_cell_with_an_arrow_does_not_overflow() {
        let sort = SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Asc,
            dirs_first: true,
        };
        let cols = [
            (
                ColumnId::Builtin(Builtin::Name),
                6,
                style(Builtin::Name, Align::Left, "N"),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                1,
                style(Builtin::Size, Align::Left, "S"),
            ),
        ];
        let line = column_header_line(&cols, &sort, None, 0);
        assert_eq!(line.width(), 7, "exactly the sum of widths: {line:?}");
        assert_eq!(line, "N      ");
        let cols = [
            (
                ColumnId::Builtin(Builtin::Name),
                6,
                style(Builtin::Name, Align::Left, "N"),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                2,
                style(Builtin::Size, Align::Left, "S"),
            ),
        ];
        let line = column_header_line(&cols, &sort, None, 0);
        assert_eq!(line.width(), 8, "{line:?}");
        assert_eq!(line, "N      ▲");
    }

    /// ADR 0105: the header shifts by the same amount as the names when the
    /// icon column is open, and NEVER overflows its column — not even when
    /// the name has fewer cells than the gutter.
    #[test]
    fn the_header_shifts_with_the_icons_and_does_not_overflow() {
        let sort = SortSpec {
            column: SortColumn::Size,
            dir: SortDir::Asc,
            dirs_first: true,
        };
        let cols = [
            (
                ColumnId::Builtin(Builtin::Name),
                8,
                style(Builtin::Name, Align::Left, "Nombre"),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                4,
                style(Builtin::Size, Align::Left, "S"),
            ),
        ];
        let line = column_header_line(&cols, &sort, None, 3);
        assert_eq!(line.width(), 12, "{line:?}");
        assert!(line.starts_with("   Nombr"), "{line:?}");
        let cols = [
            (
                ColumnId::Builtin(Builtin::Name),
                2,
                style(Builtin::Name, Align::Left, "N"),
            ),
            (
                ColumnId::Builtin(Builtin::Size),
                4,
                style(Builtin::Size, Align::Left, "S"),
            ),
        ];
        let line = column_header_line(&cols, &sort, None, 3);
        assert_eq!(line.width(), 6, "a name narrower than the gutter: {line:?}");
    }
}
