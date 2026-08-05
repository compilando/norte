//! #44: la degradación de una sesión remota a texto plano se pinta como
//! indicador PERSISTENTE en la status bar. A diferencia de `app.message`
//! (transitorio), el aviso de degradación sobrevive a las teclas y sigue
//! avisando en cada frame mientras no haya mensaje, búsqueda viva ni hook Lua.
//!
//! H3d: lo que `App` retiene es el valor ESTRUCTURADO por scheme
//! (`note_degraded`) y la frase la compone la barra (`connection_banner`) — el
//! `Option<String>` ya formateado de #44 tiraba scheme y host y no sabía
//! responder «qué conexión se degradó».

use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

fn render(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().to_string()
}

#[test]
fn connection_warning_se_pinta_en_la_status_bar() {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    // Sin mensaje transitorio, sin búsqueda viva, sin hook Lua: el aviso
    // persistente debe caer en la línea de estado.
    // H3d: entra el valor ESTRUCTURADO del wire; la frase la compone la barra.
    app.note_degraded(norte_proto::methods::ConnectionDegraded {
        scheme: "sftp".to_owned(),
        host: "remoto.example".to_owned(),
        reason: "tls-auth-rejected".to_owned(),
        detail: None,
    });

    let out = render(&app);
    assert!(
        out.contains("remoto.example"),
        "el aviso de degradación no salió en la status bar:\n{out}"
    );
}
