//! El tema PINTA de verdad (ADR 0020, fase T5): las snapshots son solo texto,
//! así que aquí se inspecciona el BUFFER de ratatui — que un directorio salga
//! con el color del tema y que la degradación de profundidad se aplique.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_theme::{ColorDepth, Theme};
use norte_tui::app::{App, Pane};
use norte_tui::theme::TuiTheme;
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

fn app_con_dir(depth: ColorDepth) -> App {
    let dir = vp("file:///casa");
    let entries = vec![Entry {
        path: dir.join(Segment::new(b"docs".to_vec()).unwrap()),
        kind: EntryKind::Dir,
        size: None,
        mtime_ms: None,
    }];
    let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));
    let theme = Theme::preset("catppuccin-mocha").unwrap().unwrap();
    app.theme = TuiTheme::new(theme, depth);
    app
}

/// `true` si ALGUNA celda del buffer tiene ese color de frente.
fn hay_fg(app: &App, want: Color) -> bool {
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .any(|cell| cell.fg == want)
}

#[test]
fn truecolor_pinta_el_dir_con_el_azul_del_tema() {
    // catppuccin-mocha: dir = #89b4fa. En truecolor sale tal cual.
    let app = app_con_dir(ColorDepth::Truecolor);
    assert!(
        hay_fg(&app, Color::Rgb(0x89, 0xb4, 0xfa)),
        "el directorio no salió con el azul del tema en truecolor"
    );
}

#[test]
fn degradacion_evita_rgb_en_terminal_pobre() {
    // En 16 colores NO puede haber ningún Rgb: todo va indexado.
    let app = app_con_dir(ColorDepth::Ansi16);
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let ningun_rgb = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .all(|cell| !matches!(cell.fg, Color::Rgb(..)) && !matches!(cell.bg, Color::Rgb(..)));
    assert!(ningun_rgb, "un terminal de 16 colores no debe recibir Rgb");
}
