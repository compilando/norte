//! The context menu (right click, the Menu key) over the shared model.
//!
//! WHAT is offered, dimmed and why is `norte_frontend::context_menu`, the
//! same function the window projects through: this module only opens it on
//! the right surface, measures where it goes, and hands the chosen command
//! to the same path a key takes.

use crossterm::event::KeyCode;
use norte_frontend::context_menu::{Action, Fingerprint, Row, Surface, entries, project};
use ratatui::layout::Rect;

use crate::app::App;

/// An open menu. The rows are projected ONCE, at opening, against the facts
/// of that moment, like the window's.
#[derive(Debug, Clone)]
pub struct ContextMenu {
    /// "acts on …", or the folder for the empty area.
    pub header: String,
    /// What is painted, in order.
    pub rows: Vec<Row>,
    /// The highlighted row.
    pub cursor: usize,
    /// Where it was asked for (the click, or the cursor's row).
    pub anchor: (u16, u16),
    /// The pane it acts on.
    pub pane: usize,
    /// What that pane looked like, to refuse a stale target.
    pub fingerprint: Fingerprint,
}

/// One painted line: a section rule, or row `n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// A rule, with the section's index into the rows.
    Section(usize),
    /// Row `n` of [`ContextMenu::rows`].
    Item(usize),
}

impl ContextMenu {
    /// The painted lines: a rule before every row that opens a section.
    #[must_use]
    pub fn lines(&self) -> Vec<Line> {
        let mut out = Vec::new();
        for (i, r) in self.rows.iter().enumerate() {
            if r.section.is_some() && i > 0 {
                out.push(Line::Section(i));
            }
            out.push(Line::Item(i));
        }
        out
    }
}

fn fingerprint(app: &App, pane: usize) -> Fingerprint {
    let p = &app.panes[pane];
    Fingerprint {
        dir: p.dir().clone(),
        entry: p.cursor_entry().map(|e| e.path.clone()),
        marks: p.marks_len(),
    }
}

/// Projects `surface` for the focused pane and installs the menu.
fn install(app: &mut App, surface: Surface, header: String, anchor: (u16, u16)) {
    let pane = app.focus();
    let list = entries(&surface);
    let implemented: Vec<&str> = list
        .iter()
        .filter_map(|e| match e.action {
            Action::Command(c) if crate::keymap::Command::parse(c).is_some() => Some(c),
            _ => None,
        })
        .collect();
    let rows = project(
        &list,
        &app.help_facts(),
        &implemented,
        |c| {
            app.palette_rows
                .iter()
                .find(|r| r.key == c)
                .map(|r| r.chord.clone())
                .filter(|s| !s.is_empty())
        },
        norte_i18n::active(),
        |_| None,
    );
    app.context_menu = Some(ContextMenu {
        header,
        rows,
        cursor: 0,
        anchor,
        pane,
        fingerprint: fingerprint(app, pane),
    });
}

/// A right click on row `i` of pane `pane`. A marked row acts on the marks;
/// an unmarked one drops them and takes the cursor (`row_target`).
pub fn open_row(app: &mut App, pane: usize, i: usize, anchor: (u16, u16)) {
    app.set_focus(pane);
    if app.panes[pane].is_parent_row(i) {
        open_empty(app, pane, anchor);
        return;
    }
    let p = &mut app.panes[pane];
    let marked = p.entries().get(i).is_some_and(|e| p.is_marked(e));
    if !marked {
        p.clear_marks();
    }
    p.set_cursor(i);
    let p = &app.panes[pane];
    let Some(entry) = p.entries().get(i) else {
        return;
    };
    let marks = marked.then(|| p.marked_entries());
    let target = norte_frontend::context_menu::row_target(entry, marks.as_deref());
    let header = norte_frontend::context_menu::acts_on(
        norte_i18n::active(),
        &target,
        entry,
        p.name_encoding(),
    );
    install(app, Surface::Row(target), header, anchor);
}

/// A right click on the empty area of pane `pane` (or on `..`): the folder's
/// menu. Touches no marks — nothing was pointed at.
pub fn open_empty(app: &mut App, pane: usize, anchor: (u16, u16)) {
    app.set_focus(pane);
    let p = &app.panes[pane];
    let (surface, header) =
        norte_frontend::context_menu::empty_menu(norte_i18n::active(), p.dir(), p.name_encoding());
    install(app, surface, header, anchor);
}

/// The Menu key (`pane.context-menu`): on the cursor's row, or the folder's
/// menu when the cursor is on `..` or the listing is empty. Anchored just
/// under the cursor's painted row when it is visible.
pub fn open_on_cursor(app: &mut App) {
    let pane = app.focus();
    let anchor = app
        .mouse
        .geometry()
        .and_then(|g| {
            g.get(pane)
                .map(|r| (r.x.saturating_add(2), r.first_list_row.saturating_add(1)))
        })
        .unwrap_or((2, 2));
    let p = &app.panes[pane];
    let i = p.cursor();
    if p.entries().get(i).is_some() && !p.is_parent_row(i) {
        open_row(app, pane, i, anchor);
    } else {
        open_empty(app, pane, anchor);
    }
}

/// Where the menu is painted inside `frame`: below and right of the anchor,
/// pushed back in when it would leave the screen.
#[must_use]
pub fn rect(menu: &ContextMenu, frame: Rect) -> Rect {
    use unicode_width::UnicodeWidthStr;
    let content = menu
        .rows
        .iter()
        .map(|r| {
            let tail = if r.enabled { &r.chord } else { &r.reason };
            UnicodeWidthStr::width(r.label.as_str()) + 3 + UnicodeWidthStr::width(tail.as_str())
        })
        .chain(std::iter::once(
            UnicodeWidthStr::width(menu.header.as_str()) + 2,
        ))
        .max()
        .unwrap_or(10);
    let w = u16::try_from(content + 4)
        .unwrap_or(u16::MAX)
        .min(frame.width)
        .min(70);
    let h = u16::try_from(menu.lines().len() + 2)
        .unwrap_or(u16::MAX)
        .min(frame.height);
    let right = frame.x.saturating_add(frame.width);
    let bottom = frame.y.saturating_add(frame.height);
    let x = menu.anchor.0.min(right.saturating_sub(w)).max(frame.x);
    let y = menu.anchor.1.min(bottom.saturating_sub(h)).max(frame.y);
    Rect::new(x, y, w, h)
}

/// The row painted at `(col, row)`, if any.
#[must_use]
pub fn row_at(app: &App, col: u16, row: u16) -> Option<usize> {
    let menu = app.context_menu.as_ref()?;
    let r = rect(menu, app.last_frame?);
    if col <= r.x || col >= r.x + r.width - 1 || row <= r.y || row >= r.y + r.height - 1 {
        return None;
    }
    match menu.lines().get(usize::from(row - r.y - 1))? {
        Line::Item(i) => Some(*i),
        Line::Section(_) => None,
    }
}

/// Whether `(col, row)` falls inside the painted menu (border included).
#[must_use]
pub fn contains(app: &App, col: u16, row: u16) -> bool {
    let (Some(menu), Some(frame)) = (app.context_menu.as_ref(), app.last_frame) else {
        return false;
    };
    let r = rect(menu, frame);
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

/// A cell of `command`'s row, for tests and for the mouse: `(col, row)`.
#[must_use]
pub fn cell_of(app: &App, command: &str) -> Option<(u16, u16)> {
    let menu = app.context_menu.as_ref()?;
    let r = rect(menu, app.last_frame?);
    let line = menu.lines().iter().position(
        |l| matches!(l, Line::Item(i) if matches!(menu.rows[*i].action, Action::Command(c) if c == command)),
    )?;
    Some((r.x + 2, r.y + 1 + u16::try_from(line).ok()?))
}

/// Activates row `i`: `None` and the menu stays when it is dimmed; `None`
/// and the menu closes when the pane changed under it (a stale target is a
/// wrong-file operation); the action otherwise, menu closed first.
pub fn choose(app: &mut App, i: usize) -> Option<Action> {
    let menu = app.context_menu.as_ref()?;
    let row = menu.rows.get(i)?;
    if !row.enabled {
        app.message = Some(row.reason.clone());
        return None;
    }
    let action = row.action;
    let stale = menu.pane >= app.panes.len() || fingerprint(app, menu.pane) != menu.fingerprint;
    app.context_menu = None;
    (!stale).then_some(action)
}

/// A key while the menu is open: arrows walk, `Enter` picks, `Esc` closes.
/// Fixed keys, like the menu bar's: no `dialog.*` verb names them.
pub fn on_key(app: &mut App, code: KeyCode) -> Option<Action> {
    let menu = app.context_menu.as_mut()?;
    let n = menu.rows.len();
    match code {
        KeyCode::Esc => app.context_menu = None,
        KeyCode::Up if n > 0 => menu.cursor = (menu.cursor + n - 1) % n,
        KeyCode::Down if n > 0 => menu.cursor = (menu.cursor + 1) % n,
        KeyCode::Home => menu.cursor = 0,
        KeyCode::End => menu.cursor = n.saturating_sub(1),
        KeyCode::Enter => {
            let i = menu.cursor;
            return choose(app, i);
        }
        _ => {}
    }
    None
}
