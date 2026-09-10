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

/// La barra de paneles con NOMBRES (spec 2026-09-10): cada botón pinta su
/// nombre con la letra de acceso subrayada; con `letters` o sin sitio para
/// todos, vuelve a las letras; y las zonas del ratón miden lo mismo que lo
/// pintado en los dos casos.
#[test]
fn la_barra_de_paneles_pinta_nombres_con_la_letra_subrayada_y_cae_a_letras() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.panel_bar = true;
    app.chrome.panel_bar_style = Some(norte_config::PanelBarStyle::Names);

    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let fila: String = (0..80)
        .map(|x| terminal.backend().buffer()[(x, 1)].symbol().to_string())
        .collect();
    assert!(
        fila.contains("Sitios"),
        "con nombres se lee el nombre: {fila:?}"
    );
    let s = fila.find('S').expect("la S de Sitios");
    let s = u16::try_from(fila[..s].chars().count()).expect("cabe");
    assert!(
        terminal.backend().buffer()[(s, 1)]
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED),
        "la letra de acceso va subrayada"
    );
    let zonas = ui::panel_zones(&app, ratatui::layout::Rect::new(0, 0, 80, 16));
    let ancho_zona = zonas[0].x1 - zonas[0].x0 + 1;
    assert_eq!(
        usize::from(ancho_zona),
        "Sitios".len() + 2,
        "la zona mide lo pintado"
    );

    // Sin sitio para todos los nombres: letras, y zonas de tres celdas.
    let mut terminal = Terminal::new(TestBackend::new(30, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let fila: String = (0..30)
        .map(|x| terminal.backend().buffer()[(x, 1)].symbol().to_string())
        .collect();
    assert!(!fila.contains("Sitios"), "sin sitio, letras: {fila:?}");
    let zonas = ui::panel_zones(&app, ratatui::layout::Rect::new(0, 0, 30, 16));
    assert_eq!(zonas[0].x1 - zonas[0].x0 + 1, 3);

    // `letters` pedido a mano, con sitio de sobra: letras igual.
    app.chrome.panel_bar_style = Some(norte_config::PanelBarStyle::Letters);
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let fila: String = (0..80)
        .map(|x| terminal.backend().buffer()[(x, 1)].symbol().to_string())
        .collect();
    assert!(!fila.contains("Sitios"), "{fila:?}");
}

/// El pie del panel (spec 2026-09-10): con `[ui] pane_footer` encendido, el
/// borde inferior dice cuántos directorios y ficheros hay y el espacio libre
/// del volumen cacheado en `App`; apagado, el borde queda limpio; y con el
/// buscador incremental abierto manda el buscador.
#[test]
fn el_pie_del_panel_cuenta_y_dice_el_espacio_libre() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.chrome.pane_footer = Some(true);
    app.volumes = vec![norte_proto::methods::Volume {
        mount: vp("file:///"),
        label: None,
        fs_type: "ext4".to_owned(),
        kind: norte_proto::methods::VolumeKind::Fixed,
        total_bytes: Some(200 << 30),
        free_bytes: Some(120 << 30),
        read_only: false,
    }];
    let fila_baja = |app: &App| -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).expect("terminal");
        terminal.draw(|f| ui::draw(f, app)).expect("draw");
        // La fila del borde inferior de los paneles: la última menos la barra
        // de estado.
        (0..100)
            .map(|x| terminal.backend().buffer()[(x, 14)].symbol().to_string())
            .collect()
    };
    let con = fila_baja(&app);
    assert!(
        con.contains("1 dirs") && con.contains("0 ficheros"),
        "{con:?}"
    );
    assert!(con.contains("120") && con.contains("libres"), "{con:?}");

    app.chrome.pane_footer = Some(false);
    let sin = fila_baja(&app);
    assert!(
        !sin.contains("dirs"),
        "apagado, el borde queda limpio: {sin:?}"
    );
}

/// La barra de teclas (spec 2026-09-10): con `[ui] key_bar` encendido ocupa
/// la ÚLTIMA fila con el número y la etiqueta de cada tecla atada, la barra
/// de estado sube una fila, una celda vacía no es zona, y un clic en una
/// celda deja la tecla sintetizada para que el bucle la despache por
/// `on_key`. Apagada, la última fila vuelve a ser la de estado.
#[test]
fn la_barra_de_teclas_pinta_lo_atado_y_un_clic_es_la_tecla() {
    use norte_frontend::keybar::KeyCell;
    let mut app = app_con_dir(ColorDepth::Truecolor);
    app.chrome.key_bar = Some(true);
    app.key_bars.browse = vec![
        KeyCell {
            key: 1,
            label: String::new(),
            command: None,
        },
        KeyCell {
            key: 2,
            label: "Copiar".to_owned(),
            command: Some("pane.copy".to_owned()),
        },
    ];
    let area = ratatui::layout::Rect::new(0, 0, 80, 16);
    let fila = |app: &App, y: u16| -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        terminal.draw(|f| ui::draw(f, app)).expect("draw");
        (0..80)
            .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_string())
            .collect()
    };
    let ultima = fila(&app, 15);
    assert!(ultima.contains("2Copiar"), "la celda atada: {ultima:?}");
    assert!(
        ultima.starts_with('1'),
        "la vacía solo lleva el número: {ultima:?}"
    );
    assert!(
        fila(&app, 14).contains("/casa"),
        "la barra de estado sube una fila"
    );

    let zonas = ui::key_zones(&app, area);
    assert_eq!(zonas.len(), 1, "una celda vacía no es zona: {zonas:?}");
    assert_eq!((zonas[0].key, zonas[0].row), (2, 15));
    norte_tui::mouse::after_frame(
        &mut app,
        None,
        norte_tui::mouse::FrameZones {
            keys: zonas.clone(),
            ..Default::default()
        },
    );
    let clic = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: zonas[0].x0,
        row: 15,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    assert_eq!(
        norte_tui::mouse::handle(&mut app, clic),
        norte_tui::mouse::After::SynthKey
    );
    assert_eq!(
        app.pending_key.map(|k| k.code),
        Some(crossterm::event::KeyCode::F(2)),
        "el clic deja la tecla para `on_key`"
    );

    app.chrome.key_bar = Some(false);
    assert!(
        fila(&app, 15).contains("/casa"),
        "apagada, la última fila es la de estado"
    );
}

/// Un panel CERRADO no se pinta como un bloque encendido.
///
/// La barra estilaba `Closed` con `Role::StatusBar`, que es el estilo de la
/// BARRA DE ESTADO — en la mitad de los temas, fondo vivo y texto oscuro. La
/// barra de paneles se limpia con el fondo base, así que los botones cerrados
/// salían como bloques de color sobre ella y los abiertos como texto normal:
/// el peso visual, al revés. Mirar la barra contestaba lo contrario de lo que
/// preguntas, y por eso parecía que el estado iba desincronizado.
///
/// Lo que se fija es la PROPIEDAD, no un color: un botón cerrado no puede
/// llevar un fondo distinto del de la barra. Con eso, cualquier tema que
/// invierta ese rol vuelve a romperlo y se ve aquí.
#[test]
fn un_panel_cerrado_no_se_pinta_como_un_bloque_encendido() {
    for preset in norte_theme::preset_names() {
        let mut app = app_con_dir(ColorDepth::Truecolor);
        let theme = norte_theme::Theme::preset(preset)
            .expect("preset válido")
            .expect("preset existe");
        app.theme = TuiTheme::new(theme, ColorDepth::Truecolor);
        app.panel_bar = true;

        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        let buf = terminal.backend().buffer().clone();
        // La fila 1 es la barra de paneles: la 0 es el menú. Con la `App` de
        // este test no hay ningún panel lateral abierto, así que TODOS los
        // botones están cerrados y la fila entera tiene que leerse como
        // fondo de barra con texto encima.
        let fondo = buf[(0, 1)].bg;
        let distintos: Vec<String> = (0..80)
            .filter(|x| buf[(*x, 1)].bg != fondo)
            .map(|x| buf[(x, 1)].symbol().to_string())
            .collect();
        assert!(
            distintos.is_empty(),
            "[{preset}] con todos los paneles CERRADOS, la barra pinta bloques de \
             color en {distintos:?} — el peso visual al revés: lo apagado \
             destacando y lo abierto como texto normal"
        );
    }
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

/// Ningún glifo VISIBLE puede quedarse con el frente del TERMINAL cuando el
/// tema fija un fondo propio: sobre un tema claro con un terminal oscuro (o
/// al revés) eso pinta texto del color del fondo — invisible. Así
/// desaparecían las celdas de Tamaño/Fecha/Tipo y la cabecera de columnas,
/// que se pintaban con un `Modifier::DIM` a secas: `dim` + `fg` del
/// terminal.
///
/// Se comprueba en TODOS los presets embarcados y con los overlays de texto
/// abiertos (ayuda, paleta, ajustes, modal), que también pintaban con
/// `Line::raw`.
#[test]
fn ningun_texto_hereda_el_frente_del_terminal_con_tema_de_fondo() {
    for nombre in norte_theme::preset_names() {
        let theme = Theme::preset(nombre)
            .expect("preset parsea")
            .expect("preset existe");
        let dir = vp("file:///casa");
        let entries = vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(Segment::new(b"docs".to_vec()).unwrap()),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(Segment::new(b"notas.txt".to_vec()).unwrap()),
                kind: EntryKind::File,
                size: Some(4096),
                mtime_ms: Some(1),
            },
        ];
        let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));
        app.theme = TuiTheme::new(theme, ColorDepth::Truecolor);
        app.render_now_ms = Some(2);
        app.help = Some(norte_tui::app::HelpView::new(
            norte_i18n::Lang::En,
            vec![ratatui::text::Line::raw("  f1             ayuda")],
        ));
        // H3b: el cuerpo se maqueta antes de pintar (lo hace el run loop);
        // sin esto el overlay pintaría solo su lateral y el barrido de
        // glifos huérfanos no vería el cuerpo del tema abierto.
        app.refresh_help(50, 16);

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let huerfanas: Vec<String> = buffer
            .content
            .iter()
            .filter(|c| c.symbol() != " " && c.fg == Color::Reset)
            .map(|c| c.symbol().to_owned())
            .collect();
        assert!(
            huerfanas.is_empty(),
            "{nombre}: {} glifos con el frente del terminal sobre el fondo del tema: {:?}",
            huerfanas.len(),
            &huerfanas[..huerfanas.len().min(20)]
        );
    }
}

/// Contraste de TEXTO por preset (segunda mitad de la revisión de temas):
/// ningún glifo de texto puede caer por debajo de 3:1 sobre su propio fondo
/// —el umbral WCAG AA de componentes de interfaz—, que es justo lo que
/// rompía el bug de las columnas (frente del terminal sobre fondo del tema:
/// contraste ~1:1, invisible).
///
/// El listón NO es el 4.5 de texto de cuerpo a propósito: las barras y
/// acentos de los presets (p. ej. la barra de estado de `gruvbox-light`,
/// crema sobre ámbar, 3.33:1) son decisiones de paleta del tema, no
/// accidentes, y subirlas es cambiar su aspecto. Los glifos de dibujo de
/// caja quedan fuera —un borde sin foco es decoración deliberadamente
/// apagada (`dim`)—; lo pintado con `Modifier::DIM` (celdas de columna,
/// cabecera) se mide por su color SIN atenuar, que es lo único que el buffer
/// conoce: la atenuación la aplica el terminal, y sobre un fondo claro la
/// oscurece (más contraste, no menos).
#[test]
fn el_texto_de_cada_preset_llega_al_suelo_de_contraste() {
    fn luminancia(c: (u8, u8, u8)) -> f64 {
        let canal = |v: u8| {
            let s = f64::from(v) / 255.0;
            if s <= 0.039_28 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * canal(c.0) + 0.7152 * canal(c.1) + 0.0722 * canal(c.2)
    }
    fn contraste(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (l1, l2) = (luminancia(a), luminancia(b));
        let (height, bajo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (height + 0.05) / (bajo + 0.05)
    }
    fn rgb(c: Color) -> Option<(u8, u8, u8)> {
        match c {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            _ => None,
        }
    }

    let dir = vp("file:///casa");
    for nombre in norte_theme::preset_names() {
        let entries = vec![
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(Segment::new(b"docs".to_vec()).unwrap()),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            },
            Entry {
                attrs: std::collections::BTreeMap::new(),
                path: dir.join(Segment::new(b"notas.txt".to_vec()).unwrap()),
                kind: EntryKind::File,
                size: Some(4096),
                mtime_ms: Some(1),
            },
        ];
        let mut app = App::new(
            Pane::new(dir.clone(), entries),
            Pane::new(dir.clone(), Vec::new()),
        );
        app.theme = TuiTheme::new(
            Theme::preset(nombre).expect("parsea").expect("existe"),
            ColorDepth::Truecolor,
        );
        app.render_now_ms = Some(2);
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        for celda in &terminal.backend().buffer().content {
            let glifo = celda.symbol();
            let decoracion = glifo == " "
                || glifo
                    .chars()
                    .all(|c| matches!(c, '\u{2500}'..='\u{257f}' | '\u{2580}'..='\u{259f}'));
            if decoracion {
                continue;
            }
            let (Some(fg), Some(bg)) = (rgb(celda.fg), rgb(celda.bg)) else {
                continue;
            };
            let r = contraste(fg, bg);
            assert!(
                r >= 3.0,
                "{nombre}: el glifo {glifo:?} se pinta a {r:.2}:1 sobre su fondo (el suelo es 3.0)"
            );
        }
    }
}
