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
        attrs: std::collections::BTreeMap::new(),
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

/// #103 review BLOCKER: el gutter de marca (`entry_item`, `ui.rs`) pintaba
/// `theme.role(Role::Mark)` INCONDICIONALMENTE — solo el GLYPH (`*`/` `) era
/// condicional, no el estilo. Todo preset embarcado define `mark` como SOLO
/// un `bg` (`crates/norte-theme/presets/*.toml`), así que esa franja de
/// color aparecía en la columna 1 de CADA fila, marcada o no. Este test pasa
/// por `ui::draw` de verdad (una snapshot de texto NO puede ver un color) y
/// pinea las dos mitades: sin marcas, el `bg` de marca no aparece en NINGUNA
/// celda; con una fila marcada, aparece en EXACTAMENTE una celda, y esa
/// celda es el glyph `*` del gutter.
#[test]
fn el_gutter_de_marca_solo_pinta_la_fila_marcada() {
    let dir = vp("file:///casa");
    let entries = vec![
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"a".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        },
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"b".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        },
    ];
    let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));
    let theme = Theme::preset("default").unwrap().unwrap();
    app.theme = TuiTheme::new(theme, ColorDepth::Truecolor);
    // `default.toml`: `mark = { bg = "#3d3315" }`.
    let mark_bg = Color::Rgb(0x3d, 0x33, 0x15);

    let mark_bg_cells = |app: &App| -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        terminal.draw(|f| ui::draw(f, app)).expect("draw");
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.bg == mark_bg)
            .map(|cell| cell.symbol().to_string())
            .collect()
    };

    assert!(
        mark_bg_cells(&app).is_empty(),
        "sin marcas, el bg de marca no debe aparecer en ninguna celda: {:?}",
        mark_bg_cells(&app)
    );

    // Marca la fila 0 ("a") y mueve el cursor a la fila 1 ("b") para que el
    // resaltado del cursor (`Role::Selection`, otro color) no se solape con
    // la fila marcada al pintar.
    app.focused_mut().toggle_mark();
    app.focused_mut().move_down(1);

    let cells = mark_bg_cells(&app);
    assert_eq!(
        cells.len(),
        1,
        "el bg de marca debe aparecer en EXACTAMENTE la celda del gutter de la fila marcada: {cells:?}"
    );
    assert_eq!(
        cells[0], "*",
        "la celda con bg de marca debe ser el glyph del gutter"
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
