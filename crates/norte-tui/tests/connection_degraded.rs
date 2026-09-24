//! #44: a remote session's degradation to plain text is painted as a
//! PERSISTENT indicator in the status bar. Unlike `app.message`
//! (transient), the degradation notice survives keystrokes and keeps
//! warning on every frame while there is no message, no live search and no
//! Lua hook.
//!
//! H3d: what `App` retains is the value STRUCTURED by scheme
//! (`note_degraded`), and the bar (`connection_banner`) composes the
//! sentence — #44's already-formatted `Option<String>` threw away scheme
//! and host and could not answer "which connection degraded".

use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid wire")
}

fn render(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().to_string()
}

#[test]
fn connection_warning_is_painted_in_the_status_bar() {
    let dir = vp("file:///home");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    // No transient message, no live search, no Lua hook: the persistent
    // notice must fall through to the status line.
    // H3d: the STRUCTURED value from the wire goes in; the bar composes the
    // sentence.
    app.note_degraded(norte_proto::methods::ConnectionDegraded {
        scheme: "sftp".to_owned(),
        host: "remote.example".to_owned(),
        reason: "tls-auth-rejected".to_owned(),
        detail: None,
    });

    let out = render(&app);
    assert!(
        out.contains("remote.example"),
        "the degradation notice did not show up in the status bar:\n{out}"
    );
}
