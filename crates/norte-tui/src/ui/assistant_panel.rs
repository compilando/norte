//! Paints the assistant panel (ADR 0186): the turns, newest at the bottom,
//! and the input line. Model text is masked before it reaches the terminal —
//! a name the model echoes can carry an escape sequence.

use norte_frontend::assistant::Turn;
use norte_i18n::t;
use norte_theme::Role;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;

/// `text` masked and cut into lines of at most `width` chars.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    let masked = norte_encoding::mask_terminal_hazards(text);
    let mut out = Vec::new();
    for raw in masked.lines() {
        let chars: Vec<char> = raw.chars().collect();
        if chars.is_empty() {
            out.push(String::new());
        }
        for chunk in chars.chunks(width.max(1)) {
            out.push(chunk.iter().collect());
        }
    }
    out
}

pub(crate) fn draw_assistant(frame: &mut Frame<'_>, area: Rect, app: &App, with_keyboard: bool) {
    let theme = &app.theme;
    let border = if with_keyboard {
        Role::BorderFocus
    } else {
        Role::BorderUnfocused
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", t("assistant-title")))
        .title_style(theme.role(Role::Title))
        .border_style(theme.role(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }
    let width = usize::from(inner.width).saturating_sub(4);
    let here = app.focused().dir();
    let a = &app.assistant;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, turn) in a.turns().iter().enumerate() {
        let selected = with_keyboard && i == a.cursor();
        let mut push = |prefix: &str, text: &str, role: Role| {
            for (k, l) in wrapped(text, width).into_iter().enumerate() {
                let p = if k == 0 { prefix } else { "  " };
                let mut style = theme.role(role);
                if selected {
                    style = theme.role(Role::Selection);
                }
                lines.push(Line::from(vec![Span::styled(format!("{p}{l}"), style)]));
            }
        };
        match turn {
            Turn::Reader(s) => push("› ", s, Role::Title),
            Turn::Order {
                text,
                note,
                written_for,
            } => {
                push("✦ ", text, Role::Match);
                push("  ", note, Role::Info);
                if written_for != here {
                    let (w, _) = norte_frontend::path_display(written_for);
                    push(
                        "  ",
                        &norte_i18n::ta("assistant-written-for", &[("dir", &w)]),
                        Role::Warning,
                    );
                }
                if selected {
                    push("  ", &t("assistant-apply-hint"), Role::Info);
                }
            }
            Turn::Answer(s) => push("  ", s, Role::Regular),
            Turn::Failed(s) => push("! ", s, Role::Warning),
        }
    }
    if a.pending() {
        lines.push(Line::styled(
            format!("  {}", t("assistant-thinking")),
            theme.role(Role::Info),
        ));
    }
    if a.turns().is_empty() && !a.pending() {
        for l in wrapped(&t("assistant-empty"), width) {
            lines.push(Line::styled(format!("  {l}"), theme.role(Role::Info)));
        }
    }
    let room = usize::from(inner.height).saturating_sub(1);
    let skip = lines.len().saturating_sub(room);
    let body = Rect {
        height: inner.height.saturating_sub(1),
        ..inner
    };
    frame.render_widget(Paragraph::new(lines.split_off(skip)), body);
    let input = norte_encoding::mask_terminal_hazards(a.input());
    let tail: String = {
        let chars: Vec<char> = input.chars().collect();
        chars[chars.len().saturating_sub(width)..].iter().collect()
    };
    let caret = if with_keyboard { "_" } else { "" };
    let line = Line::from(vec![
        Span::styled("❯ ", theme.role(Role::Title)),
        Span::styled(format!("{tail}{caret}"), theme.role(Role::Regular)),
    ]);
    let at = Rect {
        y: inner.y + inner.height - 1,
        height: 1,
        ..inner
    };
    frame.render_widget(Paragraph::new(line), at);
}
