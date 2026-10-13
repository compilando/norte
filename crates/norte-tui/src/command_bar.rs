//! The command bar in the terminal (ADR 0181): the shared model
//! (`norte_frontend::command_bar`) under the panels, with fixed keys while
//! it has the keyboard, and each order queued as a new terminal-panel
//! entry.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use norte_frontend::shell::Shell;

use crate::app::App;

/// An order waiting for the run loop to start it in the terminal panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// `$SHELL -c <order>`.
    pub argv: Vec<std::ffi::OsString>,
    /// The pane's directory.
    pub cwd: std::path::PathBuf,
    /// The entry's title: the order.
    pub title: String,
}

/// The reader's login shell, as the quoting knows it (`None` = POSIX sh).
fn shell_kind() -> Option<Shell> {
    norte_frontend::shell::login_shell()
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(Shell::parse)
}

/// Gives the bar the keyboard (`pane.command-line`), showing it if hidden.
pub fn focus(app: &mut App) {
    app.command_bar_focus = true;
}

/// A key while the bar has the keyboard. `false` = not the bar's (it does
/// not have the keyboard).
pub fn on_key(app: &mut App, key: KeyEvent) -> bool {
    if !app.command_bar_focus {
        return false;
    }
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.command_bar_focus = false,
        KeyCode::Enter if alt => insert_cursor_name(app),
        KeyCode::Enter => submit(app),
        KeyCode::Char(c) if !ctrl && !alt => app.command_bar.insert(&c.to_string()),
        KeyCode::Backspace => app.command_bar.backspace(),
        KeyCode::Delete => app.command_bar.delete(),
        KeyCode::Left => app.command_bar.left(),
        KeyCode::Right => app.command_bar.right(),
        KeyCode::Home => app.command_bar.home(),
        KeyCode::End => app.command_bar.end(),
        KeyCode::Up => app.command_bar.history_prev(),
        KeyCode::Down => app.command_bar.history_next(),
        KeyCode::Tab => {
            let names: Vec<Vec<u8>> = app
                .focused()
                .entries()
                .iter()
                .filter_map(|e| e.path.file_name().map(|n| n.as_bytes().to_vec()))
                .collect();
            let refs: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
            app.command_bar.complete(&refs, shell_kind());
        }
        _ => {}
    }
    true
}

/// `Alt+Enter`: the cursor entry's name, quoted for the reader's shell.
fn insert_cursor_name(app: &mut App) {
    let Some(name) = app
        .focused()
        .cursor_entry()
        .and_then(|e| e.path.file_name().map(|n| n.as_bytes().to_vec()))
    else {
        return;
    };
    let word = norte_frontend::command_bar::quote_for(shell_kind(), &name);
    app.command_bar.insert(&word);
}

/// `Enter`: refuses off disk with the shared table's reason; otherwise the
/// order goes to the history (saved off the loop) and is queued.
fn submit(app: &mut App) {
    if let Some(r) =
        norte_frontend::availability::verdict("app.terminal", &app.help_facts()).reason()
    {
        app.message = Some(norte_i18n::t(norte_frontend::availability::reason_key(r)));
        return;
    }
    let Some(cwd) = norte_frontend::shell::shell_dir(app.focused().dir()) else {
        return;
    };
    let Some(order) = app.command_bar.take() else {
        return;
    };
    let history = app.command_bar.history().to_vec();
    if let Some(path) = app.command_history_path.clone() {
        // Off the loop: a slow disk must not freeze the keyboard.
        let _ = std::thread::Builder::new()
            .name("norte-history".into())
            .spawn(move || norte_config::command_history::save(&path, &history));
    }
    let shell = norte_frontend::shell::login_shell();
    app.pending_order = Some(Order {
        argv: norte_frontend::command_bar::argv(&shell, &order),
        cwd,
        title: order,
    });
}
