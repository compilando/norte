//! The four modal pickers — theme, columns, connections and layout — and
//! the layout preview.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use super::{centered, clear_themed};
use crate::theme::TuiTheme;
use norte_i18n::{t, ta};

/// The theme picker: the list of available themes over a centered modal,
/// with the current one highlighted.
pub fn draw_theme_picker(
    frame: &mut Frame<'_>,
    picker: &crate::app::ThemePicker,
    theme: &TuiTheme,
    hint: &str,
) {
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let width = u16::try_from(footer_w.saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(34)
        .min(frame.area().width);
    let rows = u16::try_from(picker.names.len()).unwrap_or(8) + 2;
    let area = centered(frame.area(), width, rows.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
    let items: Vec<ListItem<'_>> = picker
        .names
        .iter()
        .map(|n| ListItem::new(Line::raw(format!(" {n}"))))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("theme-picker-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {hint} ")))
        .border_style(theme.role(Role::ModalBorder));
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(Some(picker.cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

/// The columns picker overlay (#108 7a): a cursor list — checkbox, label
/// (Fluent for builtins; the model's `label` for attr/plugin, #117; the RAW
/// id masked for the ones that do not parse — user config text, #73: it is
/// painted with `mask_terminal_hazards`) and the sort arrow on its column's
/// row. Same skeleton as [`draw_theme_picker`] (Clear + centered, `List` +
/// `ListState` with `Role::Selection` highlight, generated hint in
/// `title_bottom`, content width in CELLS with the footer as floor).
pub(crate) fn draw_columns_picker(
    frame: &mut Frame<'_>,
    p: &norte_frontend::columns_picker::ColumnsPicker,
    theme: &TuiTheme,
    hint: &str,
) {
    use norte_frontend::columns::{Builtin, sort_column};
    let target = if p.scheme_override() {
        p.scheme().to_owned()
    } else {
        t("columns-picker-target-default")
    };
    let title = ta("columns-picker-title", &[("target", &target)]);
    let rows_text: Vec<String> = p
        .rows()
        .iter()
        .map(|r| {
            let mark = if r.enabled { "[x]" } else { "[ ]" };
            let label = match r.builtin {
                Some(Builtin::Name) => t("col-header-name"),
                Some(Builtin::Size) => t("col-header-size"),
                Some(Builtin::Mtime) => t("col-header-mtime"),
                Some(Builtin::Kind) => t("col-header-kind"),
                // #117: attr/plugin bring `label` (header_label, ALREADY
                // masked on open); the ones that do not parse fall back to
                // the id. The re-masking is a belt, not the choke point;
                // the cap (encoding-audit L1, GUI parity) keeps a
                // mile-long config id from widening the whole overlay.
                None => norte_encoding::mask_terminal_hazards(r.label.as_deref().unwrap_or(&r.id))
                    .chars()
                    .take(norte_frontend::columns::HEADER_MAX_CHARS)
                    .collect(),
            };
            let arrow = match r.builtin.and_then(sort_column) {
                Some(sc) if sc == p.sort().column => {
                    if p.sort().dir == norte_frontend::SortDir::Asc {
                        " ▲"
                    } else {
                        " ▼"
                    }
                }
                _ => "",
            };
            // #108 7b: the row's current format (closed ASCII vocabulary —
            // not masked), cyclable with `f`.
            let format = r
                .format
                .as_deref()
                .map(|f| format!(" · {f}"))
                .unwrap_or_default();
            format!(" {mark} {label}{arrow}{format}")
        })
        .collect();
    let footer_w = Line::raw(format!(" {hint} ")).width();
    let content_w = rows_text
        .iter()
        .map(|f| Line::raw(f.as_str()).width())
        .max()
        .unwrap_or(0);
    let width = u16::try_from(footer_w.max(content_w).saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(34)
        .min(frame.area().width);
    // M2 review 7a: saturating — a hostile config with 65k ids would
    // overflow the `+ 2` in debug; the `.min(frame height)` below still
    // clamps it.
    let rows = u16::try_from(p.rows().len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), width, rows.min(frame.area().height.max(3)));
    clear_themed(frame, area, theme);
    let items: Vec<ListItem<'_>> = rows_text.into_iter().map(ListItem::new).collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(format!(" {hint} ")))
        .border_style(theme.role(Role::ModalBorder));
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(Some(p.cursor()));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Width, in cells, of the layout picker's preview.
///
/// Fixed, not proportional to the frame: the preview is a scale DRAWING of
/// the screen, and its resemblance to what will come out does not improve by
/// being bigger.
pub(crate) const LAYOUT_PREVIEW_W: u16 = 30;

/// Height of that same preview. Proportion matters more than size — a
/// square preview would make a `simple` pass for an `orthodox`.
pub(crate) const LAYOUT_PREVIEW_H: u16 = 10;

/// Phase A: the layout picker. Rows on the left and, on the right, the
/// screen the one under the cursor would produce.
///
/// **The preview comes from the tree's LAYOUT**, not a drawing saved next to
/// the file: a saved drawing starts lying as soon as anyone touches a size,
/// and the reader has no way to know which of the two is the real screen.
///
/// Only a FACTORY preset's is drawn, whose TOML is embedded. A user layout
/// lives on disk, and reading a file in the paint path — once per frame — is
/// the kind of cost that does not show up until the config is on a network
/// directory.
/// The connections picker (#140).
///
/// Name and address, which is what is in `connections.toml`: never a
/// secret — credentials are referenced (ADR 0015) and never arrive here.
/// Both are masked the same way: they are text from a file the user wrote,
/// and a bidi name does not reorder this box.
pub(crate) fn draw_connections_picker(
    frame: &mut Frame<'_>,
    p: &norte_frontend::connections_picker::ConnectionsPicker,
    theme: &TuiTheme,
    hint: &str,
) {
    let rows: Vec<String> = p
        .rows()
        .iter()
        .map(|r| {
            format!(
                " {} · {}",
                norte_encoding::mask_terminal_hazards(&r.name),
                norte_encoding::mask_terminal_hazards(&r.url)
            )
        })
        .collect();
    // With no connections, WHY it is empty and where to add them is shown:
    // an empty box leaves the reader thinking the key broke.
    let body: Vec<String> = if rows.is_empty() {
        vec![format!(" {}", t("connections-picker-empty"))]
    } else {
        rows
    };
    let width = body
        .iter()
        .map(|f| Line::raw(f.as_str()).width())
        .max()
        .unwrap_or(0);
    let width = u16::try_from(width).unwrap_or(u16::MAX).max(24);
    let footer = format!(" {hint} ");
    let width = width.max(u16::try_from(footer.chars().count()).unwrap_or(u16::MAX));
    let height = u16::try_from(body.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), width.saturating_add(2), height);
    clear_themed(frame, area, theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("connections-picker-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(footer))
        .border_style(theme.role(Role::ModalBorder));
    let inside = block.inner(area);
    frame.render_widget(block, area);
    let lines: Vec<Line<'_>> = body
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let l = Line::raw(f.as_str());
            if i == p.cursor() && !p.rows().is_empty() {
                l.style(theme.role(Role::Selection))
            } else {
                l
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inside);
}

/// The PROFILE picker (ADR 0079).
///
/// No preview, unlike the layout one: a profile's screen depends on what
/// the reader left saved, not just a file, so a thumbnail drawn from its
/// `[ui] layout` would lie exactly in the normal case — that of a profile
/// that already has state.
///
/// What every row does carry are the warnings the spec asks for by name:
/// which one is active, which one cannot save state (D4), and which one
/// shares a name with something else. The note below talks about the row
/// UNDER THE CURSOR, which is what the reader is about to choose.
pub(crate) fn draw_profile_picker(
    frame: &mut Frame<'_>,
    p: &norte_frontend::profile_picker::ProfilePicker,
    theme: &TuiTheme,
    hint: &str,
) {
    use norte_frontend::profile_picker::NameClash;

    let rows: Vec<String> = p
        .rows()
        .iter()
        .map(|r| {
            // The name is a DIRECTORY and may not be text: lossy MARKED
            // with its badge and hazards masked, like any other name on
            // screen (#246 m2/m3).
            let (name, hostile) = norte_frontend::display_os_name(&r.name);
            let name = norte_encoding::mask_terminal_hazards(&name);
            let badge = if hostile { " ⚠" } else { "" };
            let mark = if r.active { "▸" } else { " " };
            // The title accompanies the name, it NEVER replaces it: a
            // profile's identity is its directory, and two profiles can
            // share a title and still be two (D3).
            let title = r
                .title
                .as_deref()
                .map(|t| format!(" · {}", norte_encoding::mask_terminal_hazards(t)))
                .unwrap_or_default();
            let broken = if r.problem.is_some() {
                format!(" · {}", t("profile-picker-broken"))
            } else {
                String::new()
            };
            format!(" {mark} {name}{badge}{title}{broken}")
        })
        .collect();

    // The note: first why this row cannot be used, then why it does not
    // save state, and last the name clash. In that order because that is
    // what matters most to whoever is about to choose it.
    let note = p.current().and_then(|r| {
        r.problem
            .as_ref()
            .map(|e| format!(" {}", norte_encoding::mask_terminal_hazards(e)))
            .or_else(|| (!r.carries_state).then(|| format!(" {}", t("profile-picker-no-state"))))
            .or_else(|| match r.clash {
                NameClash::None => None,
                NameClash::Layout => Some(format!(" {}", t("profile-picker-clash-layout"))),
                NameClash::Keymap => Some(format!(" {}", t("profile-picker-clash-keymap"))),
                NameClash::Both => Some(format!(" {}", t("profile-picker-clash-both"))),
            })
    });
    let footer = format!(" {hint} ");

    // The box's height and width are reserved WHENEVER some row could ask
    // for the note, not only when the current one does: otherwise the box
    // shrinks and grows as the cursor moves across rows. Same rule as the
    // layout picker, and for the same reason.
    // TWO lines, and it wraps: a broken row's reason is written by the TOML
    // parser and can be as long as it likes ("unknown field `x`, expected
    // one of …"). Reserving its width would widen the box absurdly, and
    // keeping it to one line cuts it mid-word — which the layout picker
    // already argued is worse than not showing it.
    let any_note = p
        .rows()
        .iter()
        .any(|r| r.problem.is_some() || !r.carries_state || r.clash != NameClash::None);
    let note_height = if any_note { 2 } else { 0 };

    let list_w = rows
        .iter()
        .map(|f| Line::raw(f.as_str()).width())
        .max()
        .unwrap_or(0);
    let width = u16::try_from(list_w)
        .unwrap_or(u16::MAX)
        .max(u16::try_from(Line::raw(footer.as_str()).width()).unwrap_or(u16::MAX))
        .max(24)
        .saturating_add(2)
        .min(frame.area().width);
    let height = u16::try_from(rows.len())
        .unwrap_or(u16::MAX)
        .max(1)
        .saturating_add(2)
        .saturating_add(note_height)
        .min(frame.area().height.max(3));
    let area = centered(frame.area(), width, height);
    clear_themed(frame, area, theme);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("profile-picker-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(footer))
        .border_style(theme.role(Role::ModalBorder));
    let inside_area = block.inner(area);
    frame.render_widget(block, area);

    let bands = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(note_height)])
        .split(inside_area);
    if let Some(note) = &note
        && bands[1].height > 0
    {
        frame.render_widget(
            Paragraph::new(Line::raw(note.as_str()))
                .wrap(ratatui::widgets::Wrap { trim: true })
                .style(theme.role(Role::Info)),
            bands[1],
        );
    }

    // An EMPTY list is not an error: it means you have not created one yet,
    // and it says so instead of leaving a blank box.
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::raw(format!(" {}", t("profile-picker-empty"))))
                .style(theme.role(Role::Info)),
            bands[0],
        );
        return;
    }
    let items: Vec<ListItem<'_>> = rows.into_iter().map(ListItem::new).collect();
    let list = List::new(items).highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(Some(p.cursor()));
    frame.render_stateful_widget(list, bands[0], &mut state);
}

pub(crate) fn draw_layout_picker(
    frame: &mut Frame<'_>,
    p: &norte_frontend::layout_picker::LayoutPicker,
    theme: &TuiTheme,
    hint: &str,
    // The live registry, for the thumbnail on the right: see
    // [`draw_layout_preview`].
    kinds: &norte_frontend::layout::KindRegistry,
) {
    let rows: Vec<String> = p
        .rows()
        .iter()
        .map(|r| {
            let origin = if r.factory {
                t("layout-picker-factory")
            } else {
                t("layout-picker-mine")
            };
            // The name is a file STEM and may not be text: lossy MARKED
            // with its badge and hazards masked, like any other name on
            // screen (#246 m2/m3).
            let (name, hostile) = norte_frontend::display_os_name(&r.name);
            let name = norte_encoding::mask_terminal_hazards(&name);
            let badge = if hostile { " ⚠" } else { "" };
            format!(" {name}{badge} · {origin}")
        })
        .collect();
    // The keymap note talks about the row UNDER THE CURSOR, not the list:
    // it is a warning about what the reader is about to choose.
    // The note goes INSIDE the box, on its own line, and not in the footer:
    // a warning cut mid-sentence for not fitting on the border is worse
    // than not giving it, and at 80 columns the footer has no room for
    // both.
    let note_text = format!(" {}", t("layout-picker-keymap-note"));
    let has_note = p
        .rows()
        .get(p.cursor())
        .is_some_and(|r| r.shares_keymap_name);
    let footer = format!(" {hint} ");

    let list_w = rows
        .iter()
        .map(|f| Line::raw(f.as_str()).width())
        .max()
        .unwrap_or(0);
    let list_w = u16::try_from(list_w).unwrap_or(u16::MAX).max(18);
    let inside = list_w.saturating_add(LAYOUT_PREVIEW_W).saturating_add(1);
    // `+ 2` for the borders, and the footer is measured INSIDE them: without
    // adding them here the keymap note cuts mid-word, which is worse than
    // not giving it.
    // The width is reserved for the note WHENEVER some row could ask for
    // it, not only when the current one does: otherwise the box shrinks
    // and grows as the cursor moves across rows, and what is being
    // compared is exactly the drawing inside.
    let note_w = if p.rows().iter().any(|r| r.shares_keymap_name) {
        u16::try_from(Line::raw(note_text.as_str()).width()).unwrap_or(u16::MAX)
    } else {
        0
    };
    let width = inside
        .max(u16::try_from(Line::raw(footer.as_str()).width()).unwrap_or(u16::MAX))
        .max(note_w)
        .saturating_add(2)
        .min(frame.area().width);
    let rows_height = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    // The note's line is reserved WHENEVER the list could ask for it, for
    // the same reason as the width: the box must not change height while
    // moving.
    let note_height = u16::from(note_w > 0);
    let height = rows_height
        .max(LAYOUT_PREVIEW_H)
        .saturating_add(2)
        .saturating_add(note_height)
        .min(frame.area().height.max(3));
    let area = centered(frame.area(), width, height);
    clear_themed(frame, area, theme);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("layout-picker-title")))
        .title_style(theme.role(Role::Title))
        .title_bottom(Line::raw(footer))
        .border_style(theme.role(Role::ModalBorder));
    let inside_area = block.inner(area);
    frame.render_widget(block, area);

    let bands = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(note_height)])
        .split(inside_area);
    if has_note && bands[1].height > 0 {
        frame.render_widget(
            Paragraph::new(Line::raw(note_text.as_str())).style(theme.role(Role::Info)),
            bands[1],
        );
    }
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(list_w.min(bands[0].width)),
            Constraint::Min(0),
        ])
        .split(bands[0]);

    let items: Vec<ListItem<'_>> = rows.into_iter().map(ListItem::new).collect();
    let list = List::new(items).highlight_style(theme.role(Role::Selection));
    let mut state = ListState::default();
    state.select(Some(p.cursor()));
    frame.render_stateful_widget(list, halves[0], &mut state);

    if halves[1].width == 0 || halves[1].height == 0 {
        return; // a narrow frame keeps the list, which is what gets chosen
    }
    if let Some(row) = p.current() {
        draw_layout_preview(frame, halves[1], row, theme, kinds);
    }
}

/// The picker's right half: the screen of the row under the cursor.
///
/// It comes from the row, whether factory or the user's. Filtering by
/// `factory` left the right half blank for the user's own files — and for a
/// factory one COVERED by a file — while help promised that every row draws
/// its screen (#244 M3).
pub(crate) fn draw_layout_preview(
    frame: &mut Frame<'_>,
    area: Rect,
    row: &norte_frontend::layout_picker::Row,
    theme: &TuiTheme,
    // The LIVE registry, not a freshly made stock one: since phase 3 it
    // carries the panels plugins contribute, and without it a saved layout
    // that includes one was drawn here with a blank slot — the thumbnail
    // said one thing and the real screen another.
    kinds: &norte_frontend::layout::KindRegistry,
) {
    use norte_frontend::layout_picker::preview;

    if let Some(tree) = row.tree.as_ref() {
        let lines = preview(tree, area.width, area.height, kinds);
        let text: Vec<Line<'_>> = lines.into_iter().map(Line::raw).collect();
        frame.render_widget(Paragraph::new(text), area);
    } else if let Some(problem) = row.problem.as_deref() {
        // A file that does not parse SAYS why, in the spot where its screen
        // would go: a blank slot cannot be told apart from an empty layout.
        // The diagnosis comes from a file, so it is masked.
        let text = norte_encoding::mask_terminal_hazards(problem);
        frame.render_widget(
            Paragraph::new(text)
                .style(theme.role(Role::Warning))
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}
