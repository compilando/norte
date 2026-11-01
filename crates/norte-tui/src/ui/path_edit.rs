//! Paints the path line (ADR 0190) over the focused pane's top border,
//! where its title — the path — was.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::App;

/// Nothing unless the line is open; `pane` is the focused pane's rect.
pub(crate) fn draw(frame: &mut Frame<'_>, app: &App, pane: Rect) {
    let Some(edit) = &app.path_edit else {
        return;
    };
    // Inside the corners, one cell of border kept on each side.
    let area = Rect {
        x: pane.x.saturating_add(1),
        y: pane.y,
        width: pane.width.saturating_sub(2),
        height: u16::from(pane.height > 0),
    };
    let rest = usize::from(area.width).saturating_sub(2);
    let chars: Vec<char> = edit.text().chars().collect();
    let cursor = edit.cursor().min(chars.len());
    let cells = |s: &[char]| s.iter().collect::<String>().width();
    // Scrolls so the CURSOR stays in view, the cut marked with `…`, as the
    // command bar does: a path is long exactly where it matters, its end.
    let mut skip = 0;
    while skip < cursor && cells(&chars[skip..cursor]) + 2 > rest {
        skip += 1;
    }
    let lead = if skip > 0 { "…" } else { "" };
    let shown = format!("{lead}{}", chars[skip..].iter().collect::<String>());
    let shown = super::text::take_width(&shown, rest);
    let style = if edit.all_selected() {
        app.theme.role(Role::Selection)
    } else {
        app.theme.role(Role::Regular)
    };
    let pad = " ".repeat(rest.saturating_sub(shown.width()));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ", app.theme.role(Role::BorderFocus)),
            Span::styled(shown, style),
            Span::styled(pad, app.theme.role(Role::Regular)),
            Span::styled(" ", app.theme.role(Role::BorderFocus)),
        ])),
        area,
    );
    if crate::path_edit::has_keys(app) {
        let x = 1 + lead.width() + cells(&chars[skip..cursor]);
        if x < usize::from(area.width) {
            frame.set_cursor_position((
                area.x.saturating_add(u16::try_from(x).unwrap_or(u16::MAX)),
                area.y,
            ));
        }
    }
}
