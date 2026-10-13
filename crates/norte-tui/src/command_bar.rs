//! The command bar in the terminal (ADR 0181): the shared model
//! (`norte_frontend::command_bar`) under the panels, with fixed keys while
//! it has the keyboard, and each order queued as a new terminal-panel
//! entry.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;

/// An order waiting for the run loop to start it in the terminal panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// `$SHELL -c <order>`.
    pub argv: Vec<std::ffi::OsString>,
    /// The pane's directory.
    pub cwd: std::path::PathBuf,
    /// The entry's title: the order. Never logged.
    pub title: String,
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
            let shell = norte_frontend::shell::login_shell();
            app.command_bar.complete_for(&refs, &shell);
        }
        _ => {}
    }
    true
}

/// `Alt+Enter`: the cursor entry's name, quoted for the reader's shell; a
/// shell norte cannot quote for gets only names that mean nothing to it.
fn insert_cursor_name(app: &mut App) {
    let Some(name) = app
        .focused()
        .cursor_entry()
        .and_then(|e| e.path.file_name().map(|n| n.as_bytes().to_vec()))
    else {
        return;
    };
    let shell = norte_frontend::shell::login_shell();
    if !app.command_bar.insert_name(&shell, &name) {
        let program = shell
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        app.message = Some(norte_i18n::ta(
            "msg-bar-cannot-quote",
            &[("shell", &norte_encoding::mask_terminal_hazards(&program))],
        ));
    }
}

/// `Enter`: refuses off disk with the shared table's reason; otherwise the
/// order is queued, and remembered (off the loop) unless it was typed with
/// a leading space.
fn submit(app: &mut App) {
    if let Some(r) =
        norte_frontend::availability::verdict("app.terminal", &app.help_facts()).reason()
    {
        app.message = Some(norte_i18n::t(norte_frontend::availability::reason_key(r)));
        return;
    }
    let Some(cwd) = norte_frontend::shell::shell_dir(app.focused().dir()) else {
        app.message = Some(norte_i18n::t("reason-not-on-this-disk"));
        return;
    };
    let Some(order) = app.command_bar.take() else {
        return;
    };
    let remembered = app.command_bar.history().last() == Some(&order);
    if remembered && let Some(path) = app.command_history_path.clone() {
        let o = order.clone();
        // Off the loop: a slow disk must not freeze the keyboard.
        let _ = std::thread::Builder::new()
            .name("command bar history".into())
            .spawn(move || {
                let max = norte_frontend::command_bar::HISTORY_MAX;
                if let Err(e) = norte_config::command_history::append(&path, &o, max) {
                    tracing::warn!(error = %e, "command bar: history not saved");
                }
            });
    }
    let shell = norte_frontend::shell::login_shell();
    app.pending_order = Some(Order {
        argv: norte_frontend::command_bar::argv(&shell, &order),
        cwd,
        title: order,
    });
}
