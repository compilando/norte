//! Paints the context menu (`crate::context_menu`): the dropdown's look,
//! at the click.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::context_menu::{Line as MenuLine, rect};

/// Over everything but modals; nothing when no menu is open.
pub(crate) fn draw(frame: &mut Frame<'_>, app: &App) {
    let Some(menu) = app.context_menu.as_ref() else {
        return;
    };
    let area = rect(menu, frame.area());
    super::clear_themed(frame, area, &app.theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(app.theme.role(Role::BorderFocus))
        .title(Span::styled(
            format!(
                " {} ",
                super::text::take_width(&menu.header, usize::from(area.width.saturating_sub(4)))
            ),
            app.theme.role(Role::Title),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = usize::from(inner.width);
    let lines: Vec<Line<'static>> = menu
        .lines()
        .into_iter()
        .map(|l| match l {
            MenuLine::Section(i) => section(app, menu.rows[i].section.as_deref(), width),
            MenuLine::Item(i) => item(app, &menu.rows[i], i == menu.cursor, width),
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A rule, with the section's name in it when it has one.
fn section(app: &App, name: Option<&str>, width: usize) -> Line<'static> {
    match name.filter(|n| !n.is_empty()) {
        None => Line::styled("─".repeat(width), app.theme.role(Role::Separator)),
        Some(n) => {
            let n = super::text::take_width(n, width.saturating_sub(4));
            let rest = width.saturating_sub(UnicodeWidthStr::width(n.as_str()) + 3);
            Line::from(vec![
                Span::styled("─ ", app.theme.role(Role::Separator)),
                Span::styled(n, app.theme.role(Role::Muted)),
                Span::styled(
                    format!(" {}", "─".repeat(rest)),
                    app.theme.role(Role::Separator),
                ),
            ])
        }
    }
}

/// An entry: label on the left, its chord (or, dimmed, its reason) on the
/// right.
fn item(
    app: &App,
    row: &norte_frontend::context_menu::Row,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let tail = if row.enabled { &row.chord } else { &row.reason };
    let tail = super::text::take_width(tail, width / 2);
    let tail_w = UnicodeWidthStr::width(tail.as_str());
    let label = super::text::take_width(&row.label, width.saturating_sub(tail_w + 3));
    let gap = width.saturating_sub(UnicodeWidthStr::width(label.as_str()) + tail_w + 2);
    let style = match (selected, row.enabled) {
        (true, _) => app.theme.role(Role::Selection),
        (false, true) => app.theme.role(Role::Regular),
        (false, false) => app.theme.role(Role::Muted),
    };
    Line::styled(format!(" {label}{}{tail} ", " ".repeat(gap)), style)
}
