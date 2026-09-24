//! The two-pane comparison screen: its layout, the title with both halves,
//! the faces header, and the style by verdict.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::HOSTILE_BADGE;
use crate::theme::TuiTheme;
use norte_i18n::t;

/// Paints the differences panel (`Shift+F2`): a header with both roots, one
/// row per pair with both faces and both marks between them, and a footer
/// with the active side, the filters and the run's status.
///
/// Nothing that decides WHAT is shown lives here (hard rule 7): the visible
/// rows, the selection and the marks come from [`norte_frontend::compare`],
/// which is tested without a terminal. This side allocates widths and picks
/// colors.
///
/// `size_hints` is the presentation cache from probe #157
/// (`App::compare_size_hints`): an overlay on top of `RowFace::size`, NOT a
/// mutation of the model's rows (`ComparePane` exposes no path for that, on
/// purpose — its rows do not change after `extend`). It is only consulted
/// when the `Entry` itself carried no size; a real size the listing brought
/// is never overridden.
pub fn draw_compare<S: std::hash::BuildHasher>(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &crate::app::CompareView,
    theme: &TuiTheme,
    size_hints: &std::collections::HashMap<norte_proto::VPath, u64, S>,
) {
    use norte_frontend::compare::cells_for;

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.role(Role::BorderFocus))
        // #185: each root arrives in its own span, with the separator in
        // its own — see `compare_title` for why.
        .title(compare_title(view, area.width, theme))
        .title_bottom(Span::styled(
            compare_status_line(view),
            theme.role(Role::Info),
        ));
    let outer = block.inner(area);
    frame.render_widget(block, area);
    if outer.width == 0 || outer.height == 0 {
        return;
    }
    // Two footer rows INSIDE the frame: the filters with their counts, and
    // the keys. All three used to go in the bottom title and at 80 columns
    // it cut mid-word — the snapshot caught it, which is exactly what it is
    // for. With a frame short enough that they do not fit, the list keeps
    // everything: a panel with no rows explains nothing.
    let (header, inner, filters_area, keys_area) = compare_layout(outer);

    // Widths: the two marks and their separation in the center, the rest
    // split evenly between the two faces. `saturating_sub` because a narrow
    // terminal is a terminal, not a panic.
    let sides = inner.width.saturating_sub(COMPARE_MARKS_W + 1);
    let face_w = usize::from(sides / 2).max(1);

    if let Some(a) = header {
        frame.render_widget(compare_header(face_w, theme), a);
    }

    // Only what fits on screen is BUILT (review BLOCKER-2). Before, a
    // `ListItem` — three spans and two `format!`s — was built for every
    // VISIBLE row, not every painted one: at a hundred thousand rows that
    // is half a million allocations per frame, ten times a second while the
    // walk keeps feeding. Remotely, the painter itself then became what
    // filled the row channel, whose `route_batch` batches it DISCARDS —
    // that is, the client destroyed the response's completeness and then
    // blamed the transport with "some were lost along the way."
    let visible = view.pane.visible_len();
    let height = usize::from(inner.height);
    let selected = view.pane.visible_index();
    // The window is decided by the MODEL (#210, sticky like the listing's).
    let offset = view.pane.viewport_offset().min(visible.saturating_sub(1));
    let rows: Vec<ListItem<'_>> = view
        .pane
        .visible()
        .skip(offset)
        .take(height)
        .map(|row| {
            let mut cells = cells_for(row, view.left_encoding, view.right_encoding);
            // #157: the `Entry` carried no size (orphan, directory or
            // symlink — no comparison rung looks at it), but the selected
            // row's probe may have hydrated it since. Only the GAP gets
            // filled: a size the listing did bring is left untouched.
            for (face, entry) in [
                (cells.left.as_mut(), row.left.as_ref()),
                (cells.right.as_mut(), row.right.as_ref()),
            ] {
                if let (Some(face), Some(entry)) = (face, entry)
                    && face.size.is_none()
                    && let Some(&hinted) = size_hints.get(&entry.path)
                {
                    face.size = Some(hinted);
                }
            }
            // The selection mark goes all the way to the LEFT, outside both
            // faces: it is a decision by the reader about the whole row,
            // not about one of the two sides.
            let mark = if view.pane.is_marked(row.id) {
                '*'
            } else {
                ' '
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark.to_string(), theme.role(Role::Selection)),
                compare_face_span(cells.left.as_ref(), face_w, theme),
                Span::styled(
                    format!(" {}{} ", cells.glyphs.verdict, cells.glyphs.confidence),
                    compare_mark_style(theme, row.verdict),
                ),
                compare_face_span(cells.right.as_ref(), face_w, theme),
            ]))
        })
        .collect();
    if visible == 0 {
        // "There are no rows yet" and "they are all hidden" are not the same
        // thing: the latter is contradicted by the filter line's own
        // counts, and what to do next is different (rust review MINOR-2 of
        // the GUI; the TUI had the same gap).
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                t(if view.pane.is_empty() {
                    "compare-empty"
                } else {
                    "compare-all-filtered"
                }),
                theme.role(Role::Info),
            ))),
            inner,
        );
        return;
    }
    // The window is already trimmed, so the widget's index is relative to
    // it. A selection a filter hides highlights nothing, which is the
    // honest answer.
    let mut state = ListState::default();
    state.select(
        selected
            .and_then(|i| i.checked_sub(offset))
            .filter(|i| *i < height),
    );
    frame.render_stateful_widget(
        List::new(rows).highlight_style(theme.role(Role::Selection)),
        inner,
        &mut state,
    );
    if let Some(a) = filters_area {
        frame.render_widget(
            Paragraph::new(Line::from(compare_filter_spans(view, theme))),
            a,
        );
    }
    if let Some(a) = keys_area {
        // The sync keys fit on the SAME line, and that is why the whole
        // line lost its brackets: at 80 columns the frame has 78 and the
        // version with brackets cut mid-word. The mark count is not here
        // but in the frame's footer, which does have room — the snapshot is
        // what uncovered it, which is exactly what it is for.
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                t("compare-hint"),
                theme.role(Role::Info),
            ))),
            a,
        );
    }
}

/// Width taken by the two center marks, with their separation.
pub(crate) const COMPARE_MARKS_W: u16 = 5;

/// The STRUCTURAL separator of the comparison panel's title (#185): it goes
/// in its own `Span`, with its own role, so a `↔` embedded in a root name
/// (fixture `arrow_join_spoof`) cannot be confused with it.
pub(crate) const COMPARE_TITLE_SEP: &str = " ↔ ";

/// The comparison panel frame's title: the two roots, each in its own
/// `Span`.
///
/// #185: before, the two roots went JOINED in a single string, and that
/// could be spoofed. `↔` is an ordinary printable — `display_name_with`
/// does not mask it and no badge appears — so a directory named
/// `docs ↔ ⟨file⟩/home/victim/backup` read as ANOTHER pair of roots; and a
/// long left root pushed out the entire right one through the block's
/// truncation, with no `…`. A ratatui `Block` title cannot be split into
/// elements the way the GUI does (`compare_view::title_text`): it is laid
/// out as a single line that the frame truncates ENTIRELY on the right if
/// it does not fit, even if that line carries several `Span`s. That is why
/// `compare_title_halves` allocates the width BEFORE building any span —
/// same as `sync_step_item` allocates `path_w` before separating source and
/// destination (commit d984f83) — and the separator goes in its OWN span
/// with a different role: a `↔` embedded in a name is root text and is
/// painted as such, so the real one is told apart by style even though the
/// glyph is the same.
pub(crate) fn compare_title(
    view: &crate::app::CompareView,
    frame_width: u16,
    theme: &TuiTheme,
) -> Line<'static> {
    let (left, right) = compare_title_halves(view, usize::from(frame_width));
    let badge_span = |h: bool| {
        Span::styled(
            if h { HOSTILE_BADGE } else { "" },
            theme.role(Role::Warning),
        )
    };
    Line::from(vec![
        Span::styled(
            format!(" {} — ", t("compare-title")),
            theme.role(Role::Title),
        ),
        badge_span(left.hostile),
        Span::styled(left.text, theme.role(Role::Title)),
        Span::styled(COMPARE_TITLE_SEP, theme.role(Role::Info)),
        badge_span(right.hostile),
        Span::styled(right.text, theme.role(Role::Title)),
        Span::raw(" "),
    ])
}

/// One of the two roots of the comparison panel's title, already truncated
/// to fit the budget it got.
pub(crate) struct CompareTitleHalf {
    /// The text ALREADY bounded by cells (`middle_ellipsis`).
    text: String,
    /// Whether sanitizing altered the name — the badge goes in its own
    /// span.
    hostile: bool,
}

/// Allocates the frame title's available width between the two roots,
/// BEFORE building any span.
///
/// This is what avoids both #185 defects at once: the budget for the
/// prefix, the separator, the suffix and the two marks is subtracted
/// FIRST, and what is left over is split evenly between the two roots — so
/// a long left root never eats into the right one (it is truncated with
/// `…`, never silently), and the real `↔` always arrives in its own span
/// because it never competes for space with a root's text.
pub(crate) fn compare_title_halves(
    view: &crate::app::CompareView,
    frame_width: usize,
) -> (CompareTitleHalf, CompareTitleHalf) {
    let (left_txt, left_hostile) =
        norte_frontend::path_display_with(&view.left_root, view.left_encoding);
    let (right_txt, right_hostile) =
        norte_frontend::path_display_with(&view.right_root, view.right_encoding);
    let badge_w = |h: bool| if h { HOSTILE_BADGE.width() } else { 0 };
    let prefix_w = format!(" {} — ", t("compare-title")).width();
    // Frame borders (2) + prefix + separator + the trailing space + the two
    // marks — everything that is NOT root text, reserved before splitting
    // what is left.
    let fixed = 2
        + prefix_w
        + COMPARE_TITLE_SEP.width()
        + 1
        + badge_w(left_hostile)
        + badge_w(right_hostile);
    let roots_w = frame_width.saturating_sub(fixed).max(2);
    let left_w = (roots_w / 2).max(1);
    let right_w = roots_w.saturating_sub(left_w).max(1);
    (
        CompareTitleHalf {
            text: norte_frontend::middle_ellipsis(&left_txt, left_w),
            hostile: left_hostile,
        },
        CompareTitleHalf {
            text: norte_frontend::middle_ellipsis(&right_txt, right_w),
            hostile: right_hostile,
        },
    )
}

/// Allocates the frame's interior: column header, list, filters and keys.
///
/// With a frame short enough that the three chrome rows do not fit, the
/// LIST keeps them all: a panel with no rows explains nothing, and the keys
/// are already in help.
pub(crate) fn compare_layout(outer: Rect) -> (Option<Rect>, Rect, Option<Rect>, Option<Rect>) {
    if outer.height < 5 {
        return (None, outer, None, None);
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(outer);
    (Some(rows[0]), rows[1], Some(rows[2]), Some(rows[3]))
}

/// The differences panel's column header.
///
/// It is CHROME, outside the list: inside it, it used to be row 0 and
/// scrolled away as soon as the first screen was passed. The normal panes
/// paint theirs the same way, for the same reason.
pub(crate) fn compare_header(face_w: usize, theme: &TuiTheme) -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        format!(
            " {:<face_w$} {:^3} {:<face_w$}",
            norte_frontend::middle_ellipsis(&t("compare-header-left"), face_w),
            "",
            norte_frontend::middle_ellipsis(&t("compare-header-right"), face_w),
        ),
        theme
            .role(Role::Regular)
            .add_modifier(ratatui::style::Modifier::DIM),
    )))
}

/// One face of a differences panel row: the name ALREADY masked (badged if
/// sanitizing altered it) and its size flush to the right.
///
/// An orphan's empty side is painted BLANK and not with a dash or an "—":
/// the column next to it already says `<` or `>`, and an invented filler in
/// the empty face is what makes an orphan read as a pair.
pub(crate) fn compare_face_span(
    face: Option<&norte_frontend::compare::RowFace>,
    face_w: usize,
    theme: &TuiTheme,
) -> Span<'static> {
    let Some(f) = face else {
        return Span::raw(" ".repeat(face_w));
    };
    let name = if f.hostile {
        format!("{HOSTILE_BADGE} {}", f.name)
    } else {
        f.name.clone()
    };
    let size = f.size.map_or_else(String::new, norte_frontend::human_bytes);
    // The name is truncated through the MIDDLE (#79: by CELLS and not by
    // chars — a CJK name would overflow the budget and eat the tail on the
    // right).
    let room = face_w.saturating_sub(size.chars().count() + 1).max(1);
    let name = norte_frontend::middle_ellipsis(&name, room);
    let pad = face_w.saturating_sub(UnicodeWidthStr::width(name.as_str()) + size.chars().count());
    Span::styled(
        format!("{name}{}{size}", " ".repeat(pad.max(1))),
        // From the RAW name and not the masked one: the theme matches the
        // extension against the real bytes, and matching it against the
        // painted form would give a non-UTF8 name one color in the listing
        // and another in that same listing's comparison.
        theme.entry(&f.raw_name, f.kind),
    )
}

/// The bottom title: how the comparison is going (or how it ended), and
/// which side the usual commands act on.
///
/// The sentence is composed by [`norte_frontend::compare::status_line`],
/// SHARED with the GUI: it is the one that says whether the response is
/// complete, and in a comparison that IS the whole response — two surfaces
/// composing it on their own is exactly what made the CLI (phase A) and the
/// MCP tool (phase B) treat a response missing batches as complete. Only the
/// frame title's spacing is left here.
pub(crate) fn compare_status_line(view: &crate::app::CompareView) -> String {
    format!(
        " {} ",
        norte_frontend::compare::status_line(view, view.pane.marked_len(), norte_i18n::active())
    )
}

/// The filter row: the key, whether it is on or off, the name and the count
/// of rows in that category.
///
/// An OFF filter is marked with a glyph (`-` versus `+`) and not only a
/// color (spec §17), and the count is still shown: hiding categories is
/// exactly what would make the panel lie if it did not say so.
pub(crate) fn compare_filter_spans(
    view: &crate::app::CompareView,
    theme: &TuiTheme,
) -> Vec<Span<'static>> {
    use norte_frontend::compare::CATEGORIES;

    let mut spans = Vec::new();
    for (i, c) in CATEGORIES.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", theme.role(Role::Info)));
        }
        let off = view.pane.is_hidden(*c);
        let mark = if off { '-' } else { '+' };
        spans.push(Span::styled(
            format!(
                "{}{mark}{} {}",
                i + 1,
                c.label(norte_i18n::active()),
                view.pane.count_of(*c)
            ),
            if off {
                theme
                    .role(Role::Info)
                    .add_modifier(ratatui::style::Modifier::DIM)
            } else {
                theme.role(Role::Regular)
            },
        ));
    }
    spans
}

/// The color of a row's two marks. The GLYPH already tells the verdict apart
/// with no color at all (spec §17, `norte_frontend::compare::verdict_glyph`);
/// this only reinforces it for whoever does see color.
pub(crate) fn compare_mark_style(
    theme: &TuiTheme,
    verdict: norte_proto::methods::CompareVerdict,
) -> Style {
    use norte_proto::methods::CompareVerdict as V;
    match verdict {
        V::Same => theme.role(Role::Regular),
        V::Different | V::OnlyLeft | V::OnlyRight => theme.role(Role::Warning),
        V::TypeMismatch | V::Ambiguous | V::Error => theme.role(Role::Error),
        _ => theme.role(Role::Info),
    }
}

#[cfg(test)]
mod compare_title_tests {
    use super::compare_title_halves;
    use norte_proto::VPath;

    fn vp(s: &str) -> VPath {
        VPath::parse(s).expect("vpath")
    }

    fn view(left: VPath, right: VPath) -> crate::app::CompareView {
        crate::app::CompareView::new(left, right, 0, None, None)
    }

    /// #185: a name with an arrow INSIDE it (fixture `arrow_join_spoof`)
    /// stays in its own half — never confused with the real separator, and
    /// the other root arrives intact.
    #[test]
    fn arrow_join_spoof_does_not_fake_a_pair() {
        let spoof = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "arrow_join_spoof")
            .expect("corpus fixture");
        let seg = norte_proto::Segment::new(spoof.bytes.clone()).expect("segment");
        let left_path = vp("mem:///left").join(seg);
        let right_path = vp("mem:///right/for/real");
        let (left, right) = compare_title_halves(&view(left_path, right_path), 200);
        assert!(
            left.text.contains('→'),
            "the arrow stays INSIDE its half: {}",
            left.text
        );
        assert!(
            right.text.ends_with("for/real"),
            "and the right one arrives intact at its own: {}",
            right.text
        );
    }

    /// A mile-long left root is truncated WITH a mark (`…`), never silently,
    /// and does not eat into the right one: the width split is BY HALF,
    /// reserved before building any span.
    #[test]
    fn long_root_is_truncated_and_does_not_push_out_the_other() {
        let long =
            vp("mem:///").join(norte_proto::Segment::new(vec![b'x'; 4096]).expect("segment"));
        let right_path = vp("mem:///right/for/real");
        let (left, right) = compare_title_halves(&view(long, right_path), 60);
        assert!(left.text.contains('…'), "the cut is MARKED: {}", left.text);
        assert!(
            right.text.ends_with("for/real") || right.text.contains("for/real"),
            "the other root is still intact: {}",
            right.text
        );
    }

    /// With room to spare both roots arrive whole, with no badge (they are
    /// not hostile).
    #[test]
    fn no_sanitizing_both_roots_arrive_whole() {
        let (left, right) = compare_title_halves(&view(vp("mem:///left"), vp("mem:///right")), 200);
        assert!(!left.hostile);
        assert!(!right.hostile);
        assert!(left.text.contains("left"));
        assert!(right.text.contains("right"));
    }
}
