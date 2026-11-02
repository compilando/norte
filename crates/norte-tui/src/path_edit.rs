//! Editing the focused pane's path in place (`pane.edit-path`, ADR 0190):
//! the shared line (`norte_frontend::path_edit`) painted over the pane's
//! top border, with fixed keys while it is open.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use norte_frontend::goto::Action;
use norte_frontend::path_edit::{self, PathEdit};
use norte_proto::VPath;

use crate::app::App;

/// What a key asks the loop to do once the line has handled it.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing beyond the line itself.
    Kept,
    /// Enter on a path: navigate, and reopen on `text` if it fails.
    Go {
        /// Where.
        dir: VPath,
        /// What was typed, to give back if the folder cannot be listed.
        text: String,
    },
    /// `Ctrl+Insert`: put the line on the clipboard.
    Copy(String),
}

/// Whether the line is open AND nothing is in front of it: then it takes
/// keys and pastes, and the terminal's cursor sits on it.
#[must_use]
pub fn has_keys(app: &App) -> bool {
    app.path_edit.is_some()
        && app.context_menu.is_none()
        && app.menu.is_none()
        && !crate::overlays::modal_wins(app)
        && !crate::mouse::overlay_open(app)
}

/// Opens the line on the focused pane's path, or says why it cannot.
pub fn open(app: &mut App) {
    match path_edit::initial_text(app.focused().dir()) {
        Some(text) => app.path_edit = Some(PathEdit::new(text)),
        None => app.message = Some(norte_i18n::t("msg-path-edit-refused")),
    }
}

/// Gives the line back after a navigation that failed: what was typed,
/// unselected, so the reader fixes it instead of retyping it.
pub fn reopen(app: &mut App, text: String) {
    let mut edit = PathEdit::new(text);
    edit.end();
    app.path_edit = Some(edit);
}

/// A key while the line is open. Its keys are fixed: the line is text, and
/// a keymap chord over it would type into the panel behind.
pub fn on_key(app: &mut App, key: KeyEvent) -> Outcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    // Read before borrowing the line: Tab completes against the listing.
    let dirs: Vec<Vec<u8>> = if key.code == KeyCode::Tab {
        let pane = app.focused();
        path_edit::folders_in(pane.dir(), pane.entries())
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect()
    } else {
        Vec::new()
    };
    let pane_dir = app.focused().dir().clone();
    let Some(edit) = app.path_edit.as_mut() else {
        return Outcome::Kept;
    };
    match key.code {
        KeyCode::Esc => app.path_edit = None,
        KeyCode::Enter => match path_edit::target(edit.text()) {
            Action::Ir(dir) => {
                let text = edit.text().to_owned();
                app.path_edit = None;
                return Outcome::Go { dir, text };
            }
            Action::Nothing(reason) => app.message = Some(norte_i18n::t(reason)),
            _ => app.message = Some(norte_i18n::t("msg-goto-bad-path")),
        },
        KeyCode::Insert if ctrl => return Outcome::Copy(edit.text().to_owned()),
        KeyCode::Char(c) if !ctrl && !alt => edit.insert(&c.to_string()),
        KeyCode::Backspace => edit.backspace(),
        KeyCode::Delete => edit.delete(),
        KeyCode::Left => edit.left(),
        KeyCode::Right => edit.right(),
        KeyCode::Home => edit.home(),
        KeyCode::End => edit.end(),
        KeyCode::Tab => {
            let refs: Vec<&[u8]> = dirs.iter().map(Vec::as_slice).collect();
            edit.complete(&pane_dir, &refs);
        }
        _ => {}
    }
    Outcome::Kept
}
