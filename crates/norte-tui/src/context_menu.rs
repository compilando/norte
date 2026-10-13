//! The context menu (right click, the Menu key) over the shared model.
//!
//! WHAT is offered, dimmed and why is `norte_frontend::context_menu`, the
//! same function the window projects through: this module only opens it on
//! the right surface, measures where it goes, and hands the chosen command
//! to the same path a key takes.

use std::borrow::Cow;

use crossterm::event::KeyCode;
use norte_frontend::context_menu::{
    Action, Fingerprint, PlaceTarget, Row, Surface, Verb, entries, project,
};
use norte_proto::VPath;
use ratatui::layout::Rect;

use crate::app::App;

/// What a menu-local verb acts on, frozen at opening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// A listing (its rows and empty area): commands only.
    Listing,
    /// Column `id` of pane `pane`'s header.
    Column {
        /// The pane.
        pane: usize,
        /// The column's id as the header names it.
        id: String,
    },
    /// Places row `index`, with what it pointed at.
    Place {
        /// Its index in the bar's rows.
        index: usize,
        /// Where it leads; `None` for a section header.
        path: Option<VPath>,
        /// A favorite's raw name (the key `hotlist_remove` needs).
        favorite: Option<String>,
        /// A favorite whose target did not parse: its error key.
        broken: Option<String>,
    },
    /// Tree row `index`, with its path.
    Branch {
        /// Its index among the visible branches.
        index: usize,
        /// Its directory.
        path: VPath,
    },
}

/// What a pick runs: the entry's action and what it acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    /// The entry's action.
    pub action: Action,
    /// What the menu was opened on.
    pub subject: Subject,
}

/// An open menu. The rows are projected ONCE, at opening, against the facts
/// of that moment, like the window's.
#[derive(Debug, Clone)]
pub struct ContextMenu {
    /// What a verb acts on.
    pub subject: Subject,
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

/// The TUI's verb vetoes, the window's two (`verb_reason` in the host):
/// hiding `name`, and the path verbs on a favorite whose target is broken.
fn verb_reason(subject: &Subject, v: Verb) -> Option<Cow<'static, str>> {
    match (v, subject) {
        (Verb::HideColumn, Subject::Column { id, .. }) if id == "name" => {
            Some(Cow::Borrowed("reason-wrong-target"))
        }
        (
            Verb::OpenHere | Verb::OpenInOther | Verb::OpenInNewTab | Verb::CopyPath,
            Subject::Place {
                broken: Some(key), ..
            },
        ) => Some(Cow::Owned(key.clone())),
        _ => None,
    }
}

/// Projects `surface` for the focused pane and installs the menu.
fn install(app: &mut App, surface: Surface, header: String, anchor: (u16, u16)) {
    install_on(app, surface, Subject::Listing, header, anchor);
}

fn install_on(
    app: &mut App,
    surface: Surface,
    subject: Subject,
    header: String,
    anchor: (u16, u16),
) {
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
        |v| verb_reason(&subject, v),
    );
    app.context_menu = Some(ContextMenu {
        subject,
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

/// A right click on Places row `index`: a section header folds, a drive or a
/// favorite opens, is copied, and joins or leaves the favorites.
pub fn open_place(app: &mut App, index: usize, anchor: (u16, u16)) {
    use norte_frontend::places::PlaceRow;
    let Some(row) = app
        .places_slot()
        .and_then(|id| app.panes.places(id))
        .and_then(|s| s.rows().get(index).cloned())
    else {
        return;
    };
    let (target, path, favorite, broken) = match &row {
        PlaceRow::Header { .. } => (PlaceTarget::SectionHeader, None, None, None),
        PlaceRow::Drive { mount, .. } => (PlaceTarget::Drive, Some(mount.clone()), None, None),
        PlaceRow::Favorite { name, target } => (
            PlaceTarget::Favorite,
            target.as_ref().ok().cloned(),
            Some(name.clone()),
            target.as_ref().err().cloned(),
        ),
    };
    let header = path.as_ref().map_or_else(
        || norte_i18n::t("ctx-toggle-fold"),
        |p| norte_frontend::context_menu::elide(&norte_frontend::path_display(p).0),
    );
    install_on(
        app,
        Surface::Place(target),
        Subject::Place {
            index,
            path,
            favorite,
            broken,
        },
        header,
        anchor,
    );
}

/// A right click on tree row `index`.
pub fn open_branch(app: &mut App, index: usize, anchor: (u16, u16)) {
    let Some(path) = app
        .tree()
        .and_then(|t| t.rows().get(index).map(|r| r.path.clone()))
    else {
        return;
    };
    let header = norte_frontend::context_menu::elide(&norte_frontend::path_display(&path).0);
    install_on(
        app,
        Surface::Branch,
        Subject::Branch { index, path },
        header,
        anchor,
    );
}

/// A right click on column `id` of pane `pane`'s header.
pub fn open_header(app: &mut App, pane: usize, id: String, anchor: (u16, u16)) {
    app.set_focus(pane);
    let header = id.clone();
    let hideable = id != "name";
    install_on(
        app,
        Surface::Header { hideable },
        Subject::Column { pane, id },
        header,
        anchor,
    );
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
pub fn choose(app: &mut App, i: usize) -> Option<Picked> {
    let menu = app.context_menu.as_ref()?;
    let row = menu.rows.get(i)?;
    if !row.enabled {
        app.message = Some(row.reason.clone());
        return None;
    }
    let action = row.action;
    // A side-panel subject is re-checked when its verb runs (its row may
    // have moved); a listing's is its pane's fingerprint.
    let stale = match menu.subject {
        Subject::Listing | Subject::Column { .. } => {
            menu.pane >= app.panes.len() || fingerprint(app, menu.pane) != menu.fingerprint
        }
        Subject::Place { .. } | Subject::Branch { .. } => false,
    };
    let subject = menu.subject.clone();
    app.context_menu = None;
    (!stale).then_some(Picked { action, subject })
}

/// A key while the menu is open: arrows walk, `Enter` picks, `Esc` closes.
/// Fixed keys, like the menu bar's: no `dialog.*` verb names them.
pub fn on_key(app: &mut App, code: KeyCode) -> Option<Picked> {
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
