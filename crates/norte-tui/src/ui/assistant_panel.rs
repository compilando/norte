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

/// `text` masked and cut into lines of at most `width` CELLS, at a space
/// when the line has one: a wide character takes two cells, and a word cut
/// in the middle reads as two.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let width = width.max(2);
    let masked = norte_encoding::mask_terminal_hazards(text);
    let mut out = Vec::new();
    for raw in masked.lines() {
        let mut line = String::new();
        let mut cells = 0;
        // Where the last space in `line` is, as (byte index, cells before).
        let mut space: Option<(usize, usize)> = None;
        for c in raw.chars() {
            let w = c.width().unwrap_or(0);
            if cells + w > width {
                match space {
                    Some((at, before)) if at > 0 => {
                        let rest = line.split_off(at + 1);
                        line.truncate(at);
                        out.push(std::mem::replace(&mut line, rest));
                        cells -= before + 1;
                    }
                    _ => {
                        out.push(std::mem::take(&mut line));
                        cells = 0;
                    }
                }
                space = None;
            }
            if c == ' ' {
                space = Some((line.len(), cells));
            }
            line.push(c);
            cells += w;
        }
        out.push(line);
    }
    out
}

/// The end of `s` that fits `width` cells: the input shows what was typed
/// last.
fn tail_cells(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut cells = 0;
    let mut out: Vec<char> = Vec::new();
    for c in s.chars().rev() {
        cells += c.width().unwrap_or(0);
        if cells > width {
            break;
        }
        out.push(c);
    }
    out.into_iter().rev().collect()
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
    // Where the selected turn starts: the view must show it, hint included,
    // or Enter would apply an order the reader cannot see.
    let mut selected_from = None;
    for (i, turn) in a.turns().iter().enumerate() {
        let selected = with_keyboard && i == a.cursor();
        if selected {
            selected_from = Some(lines.len());
        }
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
    // Anchored at the bottom, unless that hides the selected turn's start.
    let skip = lines
        .len()
        .saturating_sub(room)
        .min(selected_from.unwrap_or(usize::MAX));
    let body = Rect {
        height: inner.height.saturating_sub(1),
        ..inner
    };
    frame.render_widget(Paragraph::new(lines.split_off(skip)), body);
    let input = norte_encoding::mask_terminal_hazards(a.input());
    let tail = tail_cells(&input, width);
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

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn wraps_at_a_space_and_by_cells() {
        assert_eq!(wrapped("copy the pdfs", 9), ["copy the", "pdfs"]);
        // Wide characters: four ideographs are eight cells.
        let w = wrapped("日本語日本語", 8);
        assert_eq!(w, ["日本語日", "本語"]);
        assert!(w.iter().all(|l| l.width() <= 8));
        // A word longer than the line is cut, not lost.
        assert_eq!(wrapped("abcdefghij", 4).concat(), "abcdefghij");
        assert!(wrapped("", 4).is_empty(), "an empty note takes no line");
    }

    #[test]
    fn the_input_shows_its_end_by_cells() {
        assert_eq!(tail_cells("hola mundo", 5), "mundo");
        assert_eq!(tail_cells("ab日本", 4), "日本");
        assert_eq!(tail_cells("ab日本", 3), "本");
    }
}
