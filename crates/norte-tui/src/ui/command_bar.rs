//! Paints the command bar (ADR 0181): the focused pane's folder, cut from
//! the start, `>` and the line; dimmed with its reason where it cannot run.

use norte_theme::Role;
use ratatui::Frame;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::App;

/// Nothing when the bar is hidden.
pub(crate) fn draw(frame: &mut Frame<'_>, app: &App) {
    let Some(area) = super::geometry::command_bar_area(app, frame.area()) else {
        return;
    };
    super::clear_themed(frame, area, &app.theme);
    let width = usize::from(area.width);
    let blocked = norte_frontend::availability::verdict("app.terminal", &app.help_facts())
        .reason()
        .map(|r| norte_i18n::t(norte_frontend::availability::reason_key(r)));
    // The folder keeps its END, which says where you are; at most a third of
    // the row, so the line itself always has room.
    let (dir, _) = norte_frontend::path_display(app.focused().dir());
    let dir = super::text::take_width_end(&dir, width / 3);
    let prompt = format!("{dir}> ");
    let used = UnicodeWidthStr::width(prompt.as_str());
    let rest = width.saturating_sub(used);
    let (text, style) = match &blocked {
        Some(reason) if app.command_bar.text().is_empty() => {
            (reason.clone(), app.theme.role(Role::Muted))
        }
        _ => (
            app.command_bar.text().to_owned(),
            app.theme.role(Role::Regular),
        ),
    };
    let text = super::text::take_width(&text, rest);
    let prompt_style = if app.command_bar_focus {
        app.theme.role(Role::BorderFocus)
    } else {
        app.theme.role(Role::Muted)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prompt, prompt_style),
            Span::styled(text, style),
        ])),
        area,
    );
    if app.command_bar_focus {
        // The terminal's own cursor, where the next char goes.
        let before: String = app
            .command_bar
            .text()
            .chars()
            .take(app.command_bar.cursor())
            .collect();
        let x = used + UnicodeWidthStr::width(before.as_str());
        if x < width {
            frame.set_cursor_position((
                area.x.saturating_add(u16::try_from(x).unwrap_or(u16::MAX)),
                area.y,
            ));
        }
    }
}
