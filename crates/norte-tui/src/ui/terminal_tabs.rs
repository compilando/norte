//! The terminal panel's chrome in the TUI: the buttons on its top border
//! and, with two or more shells, VS Code's list on its right. Pure: where
//! things are, so the paint (`panels.rs`) and the mouse (`mouse.rs`) agree
//! by construction.

use ratatui::layout::Rect;

/// Columns the list takes on the right, when it shows (the window's too).
pub(crate) const LIST_COLS: u16 = 18;

/// Narrowest inside that still gets a list: below it the shell keeps all.
const LIST_MIN_INSIDE: u16 = LIST_COLS * 2 + 4;

/// The border buttons and the command each one runs, left to right.
pub(crate) const BUTTONS: [(&str, &str); 4] = [
    ("[+]", "terminal.new"),
    ("[▾]", "terminal.new-profile"),
    ("[✎]", "terminal.rename"),
    ("[✕]", "terminal.close"),
];

/// Display columns, as ratatui paints them.
pub(crate) fn width_of(s: &str) -> u16 {
    u16::try_from(unicode_width::UnicodeWidthStr::width(s)).unwrap_or(u16::MAX)
}

/// The buttons as the border paints them: ` [+] [▾] [✎] [✕] `, right-aligned
/// so it ends just before the top-right corner.
pub(crate) fn buttons_line() -> String {
    let mut s = String::from(" ");
    for (label, _) in BUTTONS {
        s.push_str(label);
        s.push(' ');
    }
    s
}

/// Is the panel wide enough for the buttons AND its name on the left?
pub(crate) fn buttons_fit(r: Rect) -> bool {
    r.width >= width_of(&buttons_line()) + 14
}

/// The command of the border button under `(col, row == r.y)`, if any.
pub(crate) fn button_at(r: Rect, col: u16) -> Option<&'static str> {
    if !buttons_fit(r) {
        return None;
    }
    let line = buttons_line();
    let start = (r.x + r.width).checked_sub(1 + width_of(&line))?;
    let mut x = start + 1;
    for (label, cmd) in BUTTONS {
        let w = width_of(label);
        if col >= x && col < x + w {
            return Some(cmd);
        }
        x += w + 1;
    }
    None
}

/// The grid's rectangle and, with two or more shells and room, the list's.
pub(crate) fn split(inside: Rect, shells: usize) -> (Rect, Option<Rect>) {
    if shells < 2 || inside.width < LIST_MIN_INSIDE {
        return (inside, None);
    }
    let grid = Rect {
        width: inside.width - LIST_COLS,
        ..inside
    };
    let list = Rect {
        x: inside.x + grid.width,
        width: LIST_COLS,
        ..inside
    };
    (grid, Some(list))
}

/// The first instance the list shows, so the one in front (`active`, its
/// position) is always among the `height` rows.
pub(crate) fn list_offset(active: usize, height: u16) -> usize {
    let h = usize::from(height);
    if h == 0 {
        0
    } else {
        active.saturating_sub(h - 1)
    }
}

/// The list row under `(col, row)`, if any: a painted row, from the top —
/// add [`list_offset`] for the instance's position.
pub(crate) fn list_row_at(list: Rect, col: u16, row: u16) -> Option<usize> {
    let inside = col >= list.x && col < list.x + list.width && row >= list.y;
    (inside && row < list.y + list.height).then(|| usize::from(row - list.y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    /// The buttons sit at the right end of the top border, before the
    /// corner, and each answers on its own three cells.
    #[test]
    fn each_button_answers_on_its_cells() {
        let r = Rect::new(10, 5, 60, 12);
        let line = buttons_line();
        let start = r.x + r.width - 1 - width_of(&line);
        assert_eq!(button_at(r, start + 1), Some("terminal.new"));
        assert_eq!(button_at(r, start + 3), Some("terminal.new"));
        assert_eq!(button_at(r, start + 4), None, "the gap is not a button");
        assert_eq!(button_at(r, start + 5), Some("terminal.new-profile"));
        assert_eq!(button_at(r, start + 9), Some("terminal.rename"));
        assert_eq!(button_at(r, start + 13), Some("terminal.close"));
        assert_eq!(button_at(r, r.x + r.width - 1), None, "the corner is not");
        assert_eq!(button_at(r, r.x + 2), None);
    }

    /// Too narrow for the buttons: none, rather than buttons over the title.
    #[test]
    fn a_narrow_panel_has_no_buttons() {
        let r = Rect::new(0, 0, 20, 5);
        assert_eq!(button_at(r, 10), None);
        assert!(!buttons_fit(r));
    }

    /// One shell: the grid takes the whole inside. Two: the list takes its
    /// columns on the right, and the grid the rest.
    #[test]
    fn the_list_takes_its_columns_only_with_two() {
        let inside = Rect::new(1, 1, 80, 10);
        assert_eq!(split(inside, 1), (inside, None));
        let (grid, list) = split(inside, 2);
        let list = list.expect("a list");
        assert_eq!(grid.width + list.width, 80);
        assert_eq!(list.width, LIST_COLS);
        assert_eq!(list.x, grid.x + grid.width);
    }

    /// A panel too narrow for a list keeps the whole width for the shell.
    #[test]
    fn a_narrow_panel_keeps_the_shell_whole() {
        let inside = Rect::new(1, 1, 30, 10);
        assert_eq!(split(inside, 3), (inside, None));
    }

    /// More shells than rows: the list scrolls so the one in front shows.
    #[test]
    fn the_list_scrolls_to_the_active_one() {
        assert_eq!(list_offset(0, 2), 0);
        assert_eq!(list_offset(1, 2), 0);
        assert_eq!(list_offset(2, 2), 1);
        assert_eq!(list_offset(9, 3), 7);
        assert_eq!(list_offset(5, 0), 0, "no rows, no scroll");
    }

    /// A row of the list names its instance by position.
    #[test]
    fn a_list_row_is_an_index() {
        let inside = Rect::new(1, 1, 80, 10);
        let (_, list) = split(inside, 3);
        let list = list.expect("a list");
        assert_eq!(list_row_at(list, list.x, list.y + 2), Some(2));
        assert_eq!(list_row_at(list, list.x - 1, list.y), None, "the grid");
    }
}
