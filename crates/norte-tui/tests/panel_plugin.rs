//! El panel que pinta un PLUGIN (fase 3), por donde pasa de verdad: que su
//! marco se PINTE, que una zona pulsada despache un comando del catálogo, que
//! una respuesta vieja no pise la de ahora, y que el texto de un tercero se
//! enmascare.
//!
//! El último no es teórico: la primera versión de `marco_de_wire` copiaba los
//! campos del wire a mano y se saltaba el enmascarado que sí hacía la preview
//! estilada. Un panel podía colar escapes de terminal por el único camino que
//! no pasaba por `norte_frontend::ansi::span_de_wire`.

use norte_frontend::ansi::StyledSpan;
use norte_frontend::frame::{Hit, StyledFrame};
use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};
use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::{mouse, panelplugin, ui};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// El hueco donde vive el panel del plugin en todos estos tests.
const PANEL: SlotId = SlotId(71);

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

/// Un plugin aprobado y activo que aporta el panel `status`.
fn plugin_con_panel() -> norte_proto::methods::PluginInfo {
    norte_proto::methods::PluginInfo {
        id: "git".to_owned(),
        name: "Git".to_owned(),
        publisher: String::new(),
        version: "1.0.0".to_owned(),
        category: "panel".to_owned(),
        capabilities: Vec::new(),
        approved: true,
        enabled: true,
        description: None,
        commands: Vec::new(),
        columns: Vec::new(),
        panels: vec![norte_proto::methods::PluginPanelInfo {
            kind: "status".to_owned(),
            title: "Git".to_owned(),
            min_cols: None,
            min_rows: None,
        }],
        has_help: false,
        manifest_digest: None,
    }
}

/// Una pantalla con un listado y el panel del plugin al lado.
fn app_con_panel() -> App {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.kinds.insert_panels(&[plugin_con_panel()]);
    app.set_layout(Node::Split {
        dir: Dir::Horizontal,
        sizes: vec![Size::Weight(1), Size::Fixed(24)],
        children: vec![
            Node::slot(SlotId(70), KindId::browser()),
            Node::slot(PANEL, KindId::new("plugin:git:status")),
        ],
    });
    app
}

fn marco(texto: &str, hits: Vec<Hit>) -> StyledFrame {
    StyledFrame::clamped(
        vec![vec![StyledSpan {
            text: texto.to_owned(),
            role: None,
            fg: None,
            bg: None,
        }]],
        hits,
    )
}

fn texto_de(f: Option<&StyledFrame>) -> String {
    f.map(|f| {
        f.lines
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.clone()))
            .collect::<String>()
    })
    .unwrap_or_default()
}

fn pantalla(app: &App, ancho: u16, alto: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(ancho, alto)).expect("backend");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    (0..alto)
        .flat_map(|y| (0..ancho).map(move |x| (x, y)))
        .map(|(x, y)| terminal.backend().buffer()[(x, y)].symbol().to_owned())
        .collect()
}

/// Lo que el guest describe se PINTA, dentro de un marco que dice qué panel es.
///
/// El kind de un panel aportado no se conoce al compilar, así que no pasa por
/// la cadena de `placed_of_kind` que tienen sus vecinos: se resuelve por
/// prefijo. Sin ese camino, el hueco se colocaba y se quedaba en blanco.
#[test]
fn el_marco_del_plugin_se_pinta_con_su_titulo() {
    let mut app = app_con_panel();
    app.paneles.entry(PANEL).frame = Some(marco("rama: main", Vec::new()));

    let visto = pantalla(&app, 80, 16);
    assert!(visto.contains("rama: main"), "el marco se pinta: {visto:?}");
    assert!(visto.contains("status"), "y el título dice de qué es");
}

/// Pulsar una zona del marco despacha SU comando, por el camino de siempre.
///
/// Un `Hit` no ejecuta nada por su cuenta: nombra un comando del catálogo y lo
/// despacha norte, así que un clic no puede hacer nada que una tecla no
/// pudiera (regla dura 9). Lo que este test fija es que la cuenta de
/// coordenadas es la correcta —el guest habla de celdas DENTRO del marco— y
/// que el comando acaba en la misma cola que un botón de la barra.
#[test]
fn pulsar_una_zona_del_panel_deja_su_comando_para_el_despacho() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 16);
    let mut app = app_con_panel();
    app.paneles.entry(PANEL).frame = Some(marco(
        "recargar",
        vec![Hit {
            row: 0,
            col: 0,
            width: 8,
            command: "layout.focus-next".to_owned(),
            arg: None,
        }],
    ));

    let slots = ui::panel_slots(&app, area);
    let hueco = slots
        .iter()
        .find(|s| s.slot == PANEL)
        .expect("el panel se colocó");
    // La primera celda de DENTRO: una más allá del borde, en las dos
    // direcciones.
    let (col, row) = (hueco.x + 1, hueco.y + 1);
    mouse::after_frame(
        &mut app,
        None,
        mouse::FrameZones {
            slots,
            ..Default::default()
        },
    );

    let ev = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let after = mouse::handle_at(&mut app, ev, std::time::Instant::now());
    assert!(
        matches!(after, mouse::After::PanelBar),
        "va por el despacho de la barra, que es el de su atajo"
    );
    assert_eq!(
        app.pending_panel_command.as_deref(),
        Some("layout.focus-next")
    );
}

/// Una zona que nombra un comando FUERA de su alcance no ejecuta nada.
///
/// El plugin elige la etiqueta y el comando, y nada los ata: una zona que pone
/// «Actualizar» puede nombrar `pane.unpack`, que copia ficheros. El clic tiene
/// el mismo alcance que la tecla de un panel enfocado, ni una más.
#[test]
fn una_zona_no_puede_nombrar_un_comando_fuera_de_su_alcance() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 16);
    let mut app = app_con_panel();
    app.paneles.entry(PANEL).frame = Some(marco(
        "Actualizar",
        vec![Hit {
            row: 0,
            col: 0,
            width: 10,
            command: "pane.unpack".to_owned(),
            arg: None,
        }],
    ));
    let slots = ui::panel_slots(&app, area);
    let hueco = slots
        .iter()
        .find(|s| s.slot == PANEL)
        .expect("el panel se colocó");
    let (col, row) = (hueco.x + 1, hueco.y + 1);
    mouse::after_frame(
        &mut app,
        None,
        mouse::FrameZones {
            slots,
            ..Default::default()
        },
    );

    let ev = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let _ = mouse::handle_at(&mut app, ev, std::time::Instant::now());
    assert_eq!(
        app.pending_panel_command, None,
        "un plugin no conduce el gestor desde una zona"
    );
}

/// Pulsar el BORDE del panel no dispara la zona de debajo.
///
/// La cuenta de celdas protegía el arriba-izquierda y no el otro lado: en el
/// borde derecho daba la columna siguiente a la última de dentro, así que una
/// zona que ocupa el ancho entero se disparaba al pulsar el propio marco —por
/// ejemplo, yendo a arrastrarlo.
#[test]
fn pulsar_el_borde_del_panel_no_dispara_su_zona() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 16);
    let mut app = app_con_panel();
    app.paneles.entry(PANEL).frame = Some(marco(
        "ancho entero",
        vec![Hit {
            row: 0,
            col: 0,
            width: u16::MAX,
            command: "layout.focus-next".to_owned(),
            arg: None,
        }],
    ));
    let slots = ui::panel_slots(&app, area);
    let hueco = slots
        .iter()
        .find(|s| s.slot == PANEL)
        .expect("el panel se colocó");
    // El borde DERECHO, a la altura de la primera fila de dentro.
    let (col, row) = (hueco.x + hueco.width - 1, hueco.y + 1);
    mouse::after_frame(
        &mut app,
        None,
        mouse::FrameZones {
            slots,
            ..Default::default()
        },
    );

    let ev = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let _ = mouse::handle_at(&mut app, ev, std::time::Instant::now());
    assert_eq!(app.pending_panel_command, None, "el marco no es la zona");
}

/// Una celda del marco donde NO hay zona no ejecuta nada.
#[test]
fn fuera_de_una_zona_no_se_despacha_nada() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 16);
    let mut app = app_con_panel();
    app.paneles.entry(PANEL).frame = Some(marco(
        "recargar",
        vec![Hit {
            row: 0,
            col: 0,
            width: 8,
            command: "pane.reload".to_owned(),
            arg: None,
        }],
    ));
    let slots = ui::panel_slots(&app, area);
    let hueco = slots
        .iter()
        .find(|s| s.slot == PANEL)
        .expect("el panel se colocó");
    // Dos filas más abajo: dentro del panel, fuera de la única zona.
    let (col, row) = (hueco.x + 1, hueco.y + 3);
    mouse::after_frame(
        &mut app,
        None,
        mouse::FrameZones {
            slots,
            ..Default::default()
        },
    );

    let ev = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let _ = mouse::handle_at(&mut app, ev, std::time::Instant::now());
    assert_eq!(app.pending_panel_command, None);
}

/// Una respuesta de una petición VIEJA no pisa el marco de ahora.
///
/// Mientras volaba, el cursor pudo moverse: ese marco describe una pantalla
/// que ya no es. Y no limpia la petición viva, que es de otra firma y sigue
/// siendo la que manda.
#[test]
fn una_respuesta_vieja_no_pisa_el_marco_de_ahora() {
    let mut app = app_con_panel();
    app.paneles.entry(PANEL).frame = Some(marco("lo de ahora", Vec::new()));
    let vieja = panelplugin::Firma {
        kind: "plugin:git:status".to_owned(),
        dir: vp("file:///otro"),
        cols: 22,
        rows: 4,
        cursor: None,
    };
    let viva = panelplugin::Firma {
        kind: "plugin:git:status".to_owned(),
        dir: vp("file:///casa"),
        cols: 22,
        rows: 4,
        cursor: None,
    };
    app.paneles.entry(PANEL).en_vuelo = Some(viva);

    panelplugin::aterrizar(
        &mut app,
        PANEL,
        &vieja,
        Some(Ok(Some(norte_proto::methods::PanelFrame {
            plugin_id: "git".to_owned(),
            lines: vec![vec![norte_proto::methods::SpanWire {
                text: "lo viejo".to_owned(),
                role: None,
                fg: None,
                bg: None,
            }]],
            hits: Vec::new(),
            state: None,
        }))),
    );

    let panel = app.paneles.entry(PANEL);
    assert!(panel.en_vuelo.is_some(), "la petición viva sigue viva");
    assert_eq!(texto_de(panel.frame.as_ref()), "lo de ahora");
}

/// El texto del guest se ENMASCARA, y un rol del cromo no se le concede.
///
/// Las dos puertas viven en `norte_frontend::ansi::span_de_wire`, y este test
/// existe porque el panel llegó a saltárselas: copiar los campos a mano
/// compilaba igual.
#[test]
fn el_texto_del_guest_se_enmascara_y_el_cromo_no_se_concede() {
    let mut app = app_con_panel();
    let firma = panelplugin::Firma {
        kind: "plugin:git:status".to_owned(),
        dir: vp("file:///casa"),
        cols: 22,
        rows: 4,
        cursor: None,
    };
    app.paneles.entry(PANEL).en_vuelo = Some(firma.clone());

    panelplugin::aterrizar(
        &mut app,
        PANEL,
        &firma,
        Some(Ok(Some(norte_proto::methods::PanelFrame {
            plugin_id: "git".to_owned(),
            lines: vec![vec![norte_proto::methods::SpanWire {
                text: "rama\u{1b}[31m roja".to_owned(),
                role: Some("scrollbar-slider".to_owned()),
                fg: None,
                bg: None,
            }]],
            hits: Vec::new(),
            state: Some(b"opaco".to_vec()),
        }))),
    );

    let panel = app.paneles.entry(PANEL);
    let pintado = texto_de(panel.frame.as_ref());
    assert!(
        !pintado.contains('\u{1b}'),
        "ningún escape llega a la pantalla: {pintado:?}"
    );
    let rol = panel
        .frame
        .as_ref()
        .and_then(|f| f.lines.first())
        .and_then(|l| l.first())
        .and_then(|s| s.role);
    assert!(
        rol.is_none(),
        "un rol del cromo no lo puede pedir un plugin"
    );
    assert_eq!(
        panel.state.as_deref(),
        Some(&b"opaco"[..]),
        "el estado opaco se guarda para la siguiente"
    );
}
