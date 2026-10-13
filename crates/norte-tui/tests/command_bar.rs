//! The command bar (ADR 0181): one line under the panels; each order runs
//! as a new terminal-panel entry, never typed into an existing shell.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::command_bar;

fn entry(dir: &VPath, name: &str) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(name.as_bytes().to_vec()).unwrap()),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: None,
    }
}

fn app_in(dir: &std::path::Path, names: &[&str]) -> App {
    let vp = norte_vfs::native::vpath_from_native(dir).expect("vpath");
    let es: Vec<Entry> = names.iter().map(|n| entry(&vp, n)).collect();
    let mut app = App::new(Pane::new(vp.clone(), es), Pane::new(vp, Vec::new()));
    app.set_focus(0);
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn typed(app: &mut App, s: &str) {
    for c in s.chars() {
        assert!(command_bar::on_key(app, key(KeyCode::Char(c))));
    }
}

/// `pane.command-line` with the bar hidden shows and focuses it; `Esc`
/// gives the keyboard back and hides it again.
#[test]
fn the_command_line_key_focuses_the_bar_and_esc_hides_it() {
    let d = tempfile::tempdir().unwrap();
    let mut app = app_in(d.path(), &[]);
    assert!(!app.command_bar_visible());
    command_bar::focus(&mut app);
    assert!(app.command_bar_focus && app.command_bar_visible());
    assert!(command_bar::on_key(&mut app, key(KeyCode::Esc)));
    assert!(!app.command_bar_focus && !app.command_bar_visible());
}

/// `Enter` queues ONE order: the login shell's `-c`, in the pane's
/// directory, titled with the order and kept after a clean exit. Nothing is
/// written to any shell.
#[test]
fn enter_queues_one_order_in_the_panes_folder() {
    let d = tempfile::tempdir().unwrap();
    let mut app = app_in(d.path(), &[]);
    command_bar::focus(&mut app);
    typed(&mut app, "echo hi");
    assert!(command_bar::on_key(&mut app, key(KeyCode::Enter)));
    let order = app.pending_order.take().expect("queued");
    let shell = norte_frontend::shell::login_shell();
    assert_eq!(
        order.argv,
        norte_frontend::command_bar::argv(&shell, "echo hi")
    );
    assert_eq!(
        std::fs::canonicalize(&order.cwd).unwrap(),
        std::fs::canonicalize(d.path()).unwrap()
    );
    assert_eq!(order.title, "echo hi");
    assert_eq!(app.command_bar.text(), "");
    assert_eq!(app.command_bar.history(), ["echo hi"]);
}

/// `Alt+Enter` puts the name under the cursor in the line, quoted for the
/// reader's shell.
#[test]
fn alt_enter_inserts_the_cursor_name_quoted() {
    let d = tempfile::tempdir().unwrap();
    let mut app = app_in(d.path(), &["a b"]);
    let i = app.panes[0]
        .entries()
        .iter()
        .position(|e| e.path.file_name().is_some_and(|n| n.as_bytes() == b"a b"))
        .unwrap();
    app.panes[0].set_cursor(i);
    command_bar::focus(&mut app);
    typed(&mut app, "cat ");
    assert!(command_bar::on_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
    ));
    let shell = norte_frontend::shell::login_shell();
    let kind = shell
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(norte_frontend::shell::Shell::parse);
    assert_eq!(
        app.command_bar.text(),
        format!(
            "cat {}",
            norte_frontend::command_bar::quote_for(kind, b"a b")
        )
    );
}

/// Off disk there is nowhere to run it: the bar says why and queues
/// nothing.
#[test]
fn off_disk_nothing_runs_and_the_bar_says_why() {
    let vp = VPath::parse("mem:///x").unwrap();
    let mut app = App::new(Pane::new(vp.clone(), Vec::new()), Pane::new(vp, Vec::new()));
    app.set_focus(0);
    command_bar::focus(&mut app);
    typed(&mut app, "ls");
    assert!(command_bar::on_key(&mut app, key(KeyCode::Enter)));
    assert!(app.pending_order.is_none());
    assert_eq!(
        app.message.as_deref(),
        Some(&*norte_i18n::t("reason-not-on-this-disk"))
    );
}
