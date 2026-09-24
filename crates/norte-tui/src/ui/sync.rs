//! The sync screen: the layout, the summary, and the per-step row with its
//! undo style.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::HOSTILE_BADGE;
use super::text::wrapped_rows;
use crate::theme::TuiTheme;
use norte_i18n::t;

/// Paints the sync panel: the plan's summary, its steps and whatever
/// question is pending.
///
/// Everything it says comes from [`norte_frontend::sync`] (hard rule 7): the
/// summary, each step's three marks, what undo returns and the second
/// question. Here only the room is allocated and the color chosen, and color
/// is never the only thing that tells anything apart (§17) — the marks are
/// ASCII glyphs.
pub(crate) fn draw_sync(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &crate::app::SyncView,
    theme: &TuiTheme,
) {
    let (source_txt, source_hostile) =
        norte_frontend::path_display_with(&view.source_root, view.source_encoding);
    // With the DESTINATION's reinterpretation, not the source's: a CP1251
    // share on the other pane was painted `????` in the title even if the
    // reader had pressed `Alt+E` on it.
    let (dest_txt, dest_hostile) =
        norte_frontend::path_display_with(&view.dest_root, view.dest_encoding);
    let badge = |h: bool| if h { HOSTILE_BADGE } else { "" };
    // The `_` arm does NOT fall into "update": `SyncMode` is
    // `#[non_exhaustive]`, and saying "this does not delete" of a mode this
    // build cannot name is asserting the SAFE half of what has to be
    // approved. Same rule as `RelAnchor::Either` and `StepUndo::Unclear` in
    // the same model.
    // Through the shared function: this decision was also written into the
    // GUI, with the same rule that the `_` does NOT fall to "update" (C2
    // branch review, rust MAJOR-3).
    let mode = norte_frontend::sync::mode_label(view.mode, norte_i18n::active());
    // The ARROW is the direction, and it is half of what gets approved:
    // source to the left of the `→`, destination to the right, always,
    // regardless of which pane is which.
    //
    // #185 covers this title TOO, and here it is the line that says which
    // tree gets overwritten: the two roots go joined in a single string, so
    // a `→` inside a name fakes the pair, and a long source root pushes out
    // the whole destination one — with no `…` — because the block title's
    // truncation is ratatui's. The differences panel has the same note
    // about its `↔`; the GUI closes both with structural separators.
    let title = format!(
        " {} ({mode}) — {}{} → {}{} ",
        t("sync-title"),
        badge(source_hostile),
        source_txt,
        badge(dest_hostile),
        dest_txt
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.role(if view.confirming.is_some() {
            Role::Warning
        } else {
            Role::BorderFocus
        }))
        .title(Span::styled(title, theme.role(Role::Title)))
        .title_bottom(Span::styled(sync_status_line(view), theme.role(Role::Info)));
    let outer = block.inner(area);
    frame.render_widget(block, area);
    if outer.width == 0 || outer.height == 0 {
        return;
    }
    let (summary_area, inner, keys_area) = sync_layout(outer, view);
    if let Some(a) = summary_area {
        frame.render_widget(sync_summary(view, theme), a);
    }
    // Steps are painted AS THEY ARRIVE, not only once closed: while the plan
    // is in transit `SyncState::plan()` answers `None` and the footer is
    // already counting "6 steps" — an empty gap below was the screen
    // contradicting itself.
    let steps = view.steps();
    if steps.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                t("sync-empty"),
                theme.role(Role::Info),
            ))),
            inner,
        );
    } else {
        let height = usize::from(inner.height);
        let selected = view
            .state
            .plan()
            .and_then(norte_frontend::sync::SyncPlan::selected_id)
            .and_then(|id| steps.iter().position(|s| s.id == id));
        let offset = view
            .state
            .plan()
            .map_or(0, norte_frontend::sync::SyncPlan::viewport_offset)
            .min(steps.len().saturating_sub(1));
        // Only what fits is built, for the same reason as the differences
        // panel: a plan can have hundreds of thousands of steps and this
        // repaints ten times a second while more keep arriving.
        let rows: Vec<ListItem<'_>> = steps
            .iter()
            .skip(offset)
            .take(height)
            .map(|step| sync_step_item(step, view, usize::from(inner.width), theme))
            .collect();
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
    }
    if let Some(a) = keys_area {
        // The question and HOW it is answered go in two lines, not one: at
        // 80 columns the question alone already fills the row, and the
        // joined version was cut off right where it said which key answers
        // it — which is the half that is needed. The snapshot caught it.
        //
        // Which line applies is decided by `norte_frontend::sync::hint_id`,
        // the SHARED one (#161): this `match` had the `sync-hint` arm
        // conditioned only on `awaiting_approval()`, so a plan that was
        // closed but NOT approvable — blocked by the daemon, or with its
        // Task cancelled — kept offering "to approve" over a footer that
        // already said "this plan cannot be approved." It is the same
        // disagreement review MAJOR-1 fixed between the footer and the key;
        // now there is ONE answer and both frontends share it.
        let id = norte_frontend::sync::hint_id(view);
        let lines = match &view.confirming {
            Some(c) => vec![
                Line::from(Span::styled(c.text.clone(), theme.role(Role::Warning))),
                Line::from(Span::styled(t(id), theme.role(Role::Warning))),
            ],
            None => vec![Line::from(Span::styled(t(id), theme.role(Role::Info)))],
        };
        // WRAPPED, and `sync_layout` reserves the gap with the same count:
        // the sentence grew when it started saying that a tree is
        // re-checked at the directory and not inside it, and without
        // wrapping it was cut right before "Continue?" — the question
        // disappeared from the very screen that asks it. Caught by the
        // snapshot, again.
        frame.render_widget(
            Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
            a,
        );
    }
}

/// Allocates the sync panel frame's interior: summary, list and keys.
///
/// The summary takes what its lines ask for, up to a third of the height:
/// these are the sentences that decide the approval, and truncating them to
/// a single line hides exactly the "this cannot be undone." With a frame so
/// short nothing fits, the LIST keeps it all.
pub(crate) fn sync_layout(
    outer: Rect,
    view: &crate::app::SyncView,
) -> (Option<Rect>, Rect, Option<Rect>) {
    if outer.height < 5 {
        return (None, outer, None);
    }
    // Lines WRAP, so the height is not their count: at 80 columns "the
    // destination has no trash: …" is two rows, and reserving one cut it in
    // half. The snapshot is what uncovered it.
    let summary_height: u16 = view
        .state
        .plan()
        .map(|p| p.summary_lines(norte_i18n::active()))
        .unwrap_or_default()
        .iter()
        .map(|l| wrapped_rows(l, outer.width))
        .sum();
    // Up to HALF the frame: these are the sentences that decide approval,
    // and truncating them to fit more steps hides exactly the "this cannot
    // be undone." Steps have a scrollbar; the summary does not.
    let summary = summary_height.min((outer.height / 2).max(1));
    // The second question takes the WRAPPED question plus the row for the
    // key that answers it. Two fixed rows are not enough: at 80 columns an
    // irreversible deletion's sentence is two rows by itself, and the one
    // below is what says "Continue?" Bounded like the summary — half the
    // frame — with a floor of 2 so the key never ends up with no room.
    let keys = view.confirming.as_ref().map_or(1, |c| {
        wrapped_rows(&c.text, outer.width)
            .saturating_add(1)
            .min((outer.height / 2).max(2))
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(summary),
            Constraint::Min(1),
            Constraint::Length(keys),
        ])
        .split(outer);
    ((summary > 0).then(|| rows[0]), rows[1], Some(rows[2]))
}

/// The plan's summary: what
/// [`norte_frontend::sync::SyncPlan::summary_lines`] said, wrapped.
pub(crate) fn sync_summary(view: &crate::app::SyncView, theme: &TuiTheme) -> Paragraph<'static> {
    let lines: Vec<Line<'static>> = view
        .state
        .plan()
        .map(|p| p.summary_lines(norte_i18n::active()))
        .unwrap_or_default()
        .into_iter()
        .map(|l| Line::from(Span::styled(l, theme.role(Role::Regular))))
        .collect();
    // Wrapped and NOT truncated to width: the first line is what undo
    // returns and the second which trash is being talked about. Cutting
    // them leaves the reader approving with half a sentence.
    Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false })
}

/// A panel row: the three marks, the path and the size.
///
/// The three marks belong to DIFFERENT columns and are kept apart, because
/// the alphabet is deliberately not shared between them (`!` is `Certain` in
/// one and `Irreversible` in another): together they would read as one
/// word.
pub(crate) fn sync_step_item(
    step: &norte_proto::methods::SyncStep,
    view: &crate::app::SyncView,
    width: usize,
    theme: &TuiTheme,
) -> ListItem<'static> {
    // Both reinterpretations, as one piece: `render_step` reads each path
    // with the one from the side it hangs off (#152). Here only the
    // SOURCE's was passed and `dest_rel` was rebuilt by hand — which left a
    // `DeleteTree`'s `rel`, a DESTINATION path, read with the untouched
    // tree's codepage.
    let cells = norte_frontend::sync::render_step(step, view.dest_trash(), view.encodings());
    let dest_rel = cells.dest_rel.clone();
    // The marks, the anchor and the size; whatever is left goes to the
    // path. Same story: three copies of this match in this branch, and the
    // CLI has none.
    let anchor =
        norte_frontend::sync::anchor_label(cells.anchor, norte_i18n::active()).unwrap_or_default();
    let size_txt = cells
        .size
        .map(norte_frontend::human_bytes)
        .unwrap_or_default();
    let marks = format!(
        "{} {} {} ",
        cells.glyphs.kind, cells.glyphs.confidence, cells.glyphs.undo
    );
    // By CELLS and not by `char`s: an anchor or a size with wide characters
    // would under-budget and the row would overflow the frame (#79).
    let path_w = width
        .saturating_sub(marks.width() + anchor.width() + size_txt.width() + 2)
        .max(1);
    // The DESTINATION's spelling when there is one (#152): the write lands
    // on IT, so showing only the source's would name a file that is not the
    // one about to be touched. Who decides "there is one" is `render_step`,
    // by BYTES and once for all three frontends: here the PAINTED text was
    // being compared, which is lossy, so two different files each with one
    // invalid byte folded into the same one and the field vanished from the
    // screen (encoding audit MAJOR-1).
    //
    // The badge goes PER HALF and not only on the source's: a clean source
    // path with a hostile destination spelling — the normal case when only
    // the destination pane carries an override, because reinterpreting
    // always marks — was painted with no mark at all (encoding audit
    // MAJOR-3). The CLI already did it per half and so did the GUI; this
    // was the only one of the three that did not.
    // #185, and here it weighs more than in the differences panel: the two
    // spellings go JOINED by a `→` in the same string, and `→` is an
    // ordinary printable that `display_name_with` does not mask — meaning a
    // file called `a → b.txt` (corpus `arrow_join_spoof`) arrives with NO
    // badge and fakes the pair. The GUI closes it with a structural
    // separator (each spelling in its own element); a ratatui `Line` has no
    // such option, so the design decision is the same one #185 lists for
    // the differences panel's title.
    //
    // And the badges and the `→` go in their OWN SPANS, outside what gets
    // truncated (branch review encoding audit, MAJOR-4). Building them
    // inside a single string and running it through `middle_ellipsis` put
    // them in the MIDDLE, which is exactly what that function drops: on a
    // narrow pane, `⚠ caf<FFFD>.txt → ⚠ caf<FFFD>2.txt` came out as
    // `⚠ caf…2.txt` and read as ONE truncated name. The `…` says "something
    // was cut," not "the pair collapsed," and the field that vanished is
    // exactly the one naming the file the write lands on. The TEXT of each
    // half is truncated, never its mark nor the separator.
    let path_style = theme.entry(&cells.rel.raw, norte_proto::EntryKind::File);
    let badge_of = |d: &norte_frontend::sync::RelDisplay| {
        if d.hostile { HOSTILE_BADGE } else { "" }
    };
    let mut spans = vec![Span::styled(marks, sync_undo_style(theme, cells.undo))];
    if let Some(d) = &dest_rel {
        const SEP: &str = " → ";
        let fixed = badge_of(&cells.rel).width() + SEP.width() + badge_of(d).width();
        let text_w = path_w.saturating_sub(fixed).max(2);
        // Split in half: both spellings are worth the same, and the
        // destination's is the one that says where the write lands.
        let half = (text_w / 2).max(1);
        spans.push(Span::styled(
            badge_of(&cells.rel),
            theme.role(Role::Warning),
        ));
        spans.push(Span::styled(
            norte_frontend::middle_ellipsis(&cells.rel.text, half),
            path_style,
        ));
        // The separator with its own role: a `→` INSIDE a name (corpus
        // `arrow_join_spoof`) is file text and is painted as such, so the
        // pair's real one is told apart by style even though the two
        // glyphs are the same. It is the most a ratatui `Line` allows; the
        // real structural separator is what #185 lists for this same class
        // of row.
        spans.push(Span::styled(SEP, theme.role(Role::Info)));
        spans.push(Span::styled(badge_of(d), theme.role(Role::Warning)));
        spans.push(Span::styled(
            norte_frontend::middle_ellipsis(&d.text, text_w - half),
            path_style,
        ));
    } else {
        let fixed = badge_of(&cells.rel).width();
        spans.push(Span::styled(
            badge_of(&cells.rel),
            theme.role(Role::Warning),
        ));
        spans.push(Span::styled(
            norte_frontend::middle_ellipsis(&cells.rel.text, path_w.saturating_sub(fixed).max(1)),
            path_style,
        ));
    }
    if !anchor.is_empty() {
        spans.push(Span::styled(format!(" {anchor}"), theme.role(Role::Info)));
    }
    if !size_txt.is_empty() {
        spans.push(Span::styled(format!(" {size_txt}"), theme.role(Role::Info)));
    }
    ListItem::new(Line::from(spans))
}

/// A step's marks color. The GLYPH already tells it apart with no color at
/// all (§17); this only reinforces it for whoever does see color.
pub(crate) fn sync_undo_style(theme: &TuiTheme, undo: norte_frontend::sync::StepUndo) -> Style {
    use norte_frontend::sync::StepUndo as U;
    match undo {
        U::Reverts | U::Nothing => theme.role(Role::Regular),
        U::LeftBehind => theme.role(Role::Warning),
        U::Irreversible | U::Unclear => theme.role(Role::Error),
    }
}

/// The sync panel's footer: what point the dialog is at.
///
/// The whole sentence is composed by
/// [`norte_frontend::sync::status_line`], SHARED with the GUI since #161 —
/// its eight arms used to be here, and one of them is where "this plan can
/// be approved" reaches a human as words. Only the spacing is left here:
/// glued to the `└` it reads as part of the frame, same as the differences
/// panel's footer.
pub(crate) fn sync_status_line(view: &crate::app::SyncView) -> String {
    format!(
        " {} ",
        norte_frontend::sync::status_line(view, norte_i18n::active())
    )
}

#[cfg(test)]
mod sync_step_item_tests {
    use super::{HOSTILE_BADGE, TuiTheme, sync_step_item};
    use ratatui::widgets::ListItem;

    /// EACH span's text, without rendering to a buffer: what matters here
    /// is the span structure (what is a mark, what is a separator and what
    /// is a name), which is exactly what a flat buffer erases.
    fn spans(item: &ListItem<'_>) -> Vec<String> {
        // `ListItem` does not expose its lines; the same item is rebuilt.
        // Compared over the render, which is what the reader sees.
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::{List, Widget as _};
        let area = Rect::new(0, 0, 28, 1);
        let mut buf = Buffer::empty(area);
        List::new(vec![item.clone()]).render(area, &mut buf);
        vec![
            (0..area.width)
                .map(|x| buf[(x, 0)].symbol().to_string())
                .collect::<String>(),
        ]
    }

    fn view() -> crate::app::SyncView {
        crate::app::SyncView::new(
            norte_proto::TaskId::new(1),
            norte_proto::methods::SyncMode::Update,
            norte_proto::VPath::parse("file:///source").expect("vpath"),
            norte_proto::VPath::parse("file:///dest").expect("vpath"),
            None,
            None,
        )
    }

    /// A step whose TWO spellings are hostile and long.
    fn hostile_step() -> norte_proto::methods::SyncStep {
        // Invalid bytes: `render_step` decodes them lossily and marks both
        // halves as hostile, which is the normal case when the destination
        // pane carries a #57 override and the source does not.
        let seg = |b: &[u8]| {
            norte_proto::methods::RelPath::new(vec![
                norte_proto::Segment::new(b.to_vec()).expect("segment"),
            ])
        };
        let rel = seg(b"caf\xff_long_source.txt");
        let dest = seg(b"caf\xfe_long_dest.txt");
        norte_proto::methods::SyncStep {
            id: 1,
            kind: norte_proto::methods::SyncStepKind::Overwrite,
            rel,
            dest_rel: Some(dest),
            size: None,
            criterion: norte_proto::methods::CompareCriterion::Size,
            confidence: norte_proto::methods::CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::Delete),
            reason: None,
        }
    }

    /// Branch review encoding audit, MAJOR-4. The badge and the `→` were
    /// INSIDE the string that gets truncated, and `middle_ellipsis` drops
    /// the middle: on a narrow pane the row came out `⚠ caf…largo.txt`, that
    /// is, a single truncated name. Both the pair's separator and the
    /// DESTINATION spelling's mark — the one that says which file the write
    /// lands on — vanished with nothing saying the pair had collapsed.
    #[test]
    fn both_badges_and_the_arrow_survive_a_narrow_pane() {
        let theme = TuiTheme::default();
        let v = view();
        let item = sync_step_item(&hostile_step(), &v, 28, &theme);
        let painted = spans(&item).join("");
        assert!(
            painted.contains('\u{2192}'),
            "the pair's separator survives truncation: {painted:?}"
        );
        // The badge GLUED to each half, not a bare count: the marks
        // column's confidence glyph is the same character, so counting it
        // loose counts three and says nothing about where they are.
        assert_eq!(
            painted.matches(&format!("{HOSTILE_BADGE}caf")).count(),
            2,
            "BOTH halves stay marked, each in its own spot: {painted:?}"
        );
    }
}
