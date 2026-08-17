//! El sidebar de sitios dentro de la `App` (L3): abrirlo, enfocarlo, cerrarlo
//! y, sobre todo, NO tocar los listados al hacerlo.
//!
//! La regla que estos tests protegen es la 7 del spec: `app.panes[i]` sigue
//! queriendo decir «el i-ésimo LISTADO». Un sidebar no es un lado, y el día
//! que lo fuera, una copia podría tener por destino una lista de discos.

use norte_proto::methods::{Volume, VolumeKind};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Pane};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    // Idioma fijo: el snapshot congela texto localizado.
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn entradas(dir: &VPath) -> Vec<Entry> {
    (0..3)
        .map(|i| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir
                .join(Segment::new(format!("f{i:02}").into_bytes()).expect("segmento"))
                .clone(),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        })
        .collect()
}

fn app_de_prueba() -> App {
    let dir = vp("file:///casa");
    App::new(
        Pane::new(dir.clone(), entradas(&dir)),
        Pane::new(dir.clone(), entradas(&dir)),
    )
}

/// Abrir el sidebar no cambia cuántos LISTADOS hay, ni cuál está enfocado, ni
/// dónde está su cursor. Es la regla 7 del spec: el sidebar no es un lado.
#[test]
fn abrir_el_sidebar_no_toca_los_lados() {
    let mut app = app_de_prueba();
    let antes = (app.panes.len(), app.focus(), app.focused().dir().clone());
    app.toggle_places();
    assert_eq!(app.panes.len(), antes.0, "siguen siendo dos listados");
    assert_eq!(app.focus(), antes.1);
    assert_eq!(*app.focused().dir(), antes.2);
    assert!(app.places_slot().is_some());
    assert_eq!(app.key_owner(), KeyOwner::Places);
}

/// Y cerrarlo deja el árbol EXACTAMENTE como estaba: sin un `Split` degenerado
/// acumulándose cada vez que alguien abre y cierra el sidebar.
#[test]
fn cerrar_el_sidebar_devuelve_el_arbol_de_antes() {
    let mut app = app_de_prueba();
    let antes = app.layout.clone();
    app.toggle_places();
    assert_ne!(app.layout, antes, "abrirlo sí cambia el árbol");
    app.toggle_places();
    assert_eq!(app.layout, antes);
    assert!(app.places_slot().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// Segunda pulsación con el teclado en los listados: ENFOCA, no cierra.
/// Cerrar algo que el lector acaba de mirar de reojo es la respuesta
/// equivocada.
#[test]
fn con_el_sidebar_abierto_y_el_teclado_fuera_la_tecla_lo_enfoca() {
    let mut app = app_de_prueba();
    app.toggle_places();
    app.return_keys_to_panes();
    app.toggle_places();
    assert!(app.places_slot().is_some(), "sigue abierto");
    assert_eq!(app.key_owner(), KeyOwner::Places);
}

/// El hueco del sidebar existe en el árbol pero NO es un `browser`: iterar los
/// panes sigue dando solo listados, que es de lo que vive medio run loop.
#[test]
fn el_hueco_del_sidebar_no_aparece_como_listado() {
    let mut app = app_de_prueba();
    app.toggle_places();
    let sidebar = app.places_slot().expect("abierto");
    assert!(app.layout.slot_ids().contains(&sidebar));
    assert_eq!(app.panes.iter().count(), 2);
    assert!(app.panes.browser(sidebar).is_none());
    assert!(app.panes.places(sidebar).is_some());
}

/// Un `Split` partido de más no aparece por abrir el sidebar dos veces: la
/// segunda pulsación no acuña otro hueco.
#[test]
fn abrirlo_dos_veces_no_acuna_dos_huecos() {
    let mut app = app_de_prueba();
    app.toggle_places();
    let primero = app.places_slot().expect("abierto");
    app.return_keys_to_panes();
    app.toggle_places();
    assert_eq!(app.places_slot(), Some(primero));
    assert_eq!(
        app.layout
            .slot_ids()
            .iter()
            .filter(|id| app
                .layout
                .kind_of(**id)
                .is_some_and(|k| k.as_str() == "places"))
            .count(),
        1
    );
}

fn volumen(mount: &str, free: u64, total: u64) -> Volume {
    Volume {
        mount: vp(mount),
        label: None,
        fs_type: "ext4".to_owned(),
        kind: VolumeKind::Fixed,
        total_bytes: Some(total),
        free_bytes: Some(free),
        read_only: false,
    }
}

/// Una `App` con el sidebar abierto y poblado, lista para pintar.
fn app_con_sidebar() -> App {
    let mut app = app_de_prueba();
    app.render_now_ms = Some(0);
    app.toggle_places();
    let id = app.places_slot().expect("abierto");
    let sidebar = app.panes.places_mut(id).expect("es un sidebar");
    sidebar.set_drives(&[
        volumen("file:///", 41_000_000_000, 120_000_000_000),
        volumen("file:///boot", 402_000_000, 1_000_000_000),
    ]);
    sidebar.set_favorites(&[
        ("trabajo".to_owned(), Ok(vp("file:///trabajo"))),
        ("roto".to_owned(), Err("hotlist-invalid".to_owned())),
    ]);
    app
}

fn buffer_de(app: &App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, app))
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// Las filas del buffer, celda a celda.
///
/// A mano y NO con `TestBackend::to_string()`: ese envuelve cada fila en
/// comillas, así que todo recorte por columna sale desplazado una celda —y un
/// `contains()` lo tapa. Un test de geometría con `contains` no comprueba
/// geometría.
fn filas(buf: &ratatui::buffer::Buffer) -> Vec<String> {
    (buf.area.top()..buf.area.bottom())
        .map(|y| {
            (buf.area.left()..buf.area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect()
        })
        .collect()
}

/// El sidebar mide 16 celdas EXACTAS y el primer listado empieza justo
/// después. `Fixed` gana al mínimo del kind, así que este número es el ancho
/// de verdad y no una sugerencia.
#[test]
fn el_sidebar_ocupa_dieciseis_celdas_y_el_listado_empieza_en_la_diecisiete() {
    let app = app_con_sidebar();
    let buf = buffer_de(&app, 100, 30);
    let f = filas(&buf);
    // Fila 1: dentro de los dos bloques, ya sin el borde superior.
    let fila = &f[1];
    let celda = |x: usize| fila.chars().nth(x).expect("la celda está pintada");
    assert_eq!(celda(0), '│', "borde izquierdo del sidebar");
    assert_eq!(celda(15), '│', "borde derecho del sidebar, en la celda 15");
    assert_eq!(
        celda(16),
        '│',
        "borde izquierdo del primer listado, en la 16"
    );
}

/// La pantalla entera con el sidebar abierto.
#[test]
fn snapshot_sidebar_abierto() {
    let app = app_con_sidebar();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    insta::assert_snapshot!(terminal.backend().to_string());
}

/// Un favorito roto se PINTA, marcado y atenuado. Esconderlo sería un fallo
/// de configuración que el lector no puede ver; y el motivo entero no cabe en
/// catorce celdas, así que lo dice la barra de estado (ver `places_activate`).
#[test]
fn el_favorito_roto_se_pinta_marcado_y_atenuado() {
    let app = app_con_sidebar();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let buf = terminal.backend().buffer().clone();
    let f = filas(&buf);
    let y = f
        .iter()
        .position(|fila| fila.chars().take(16).collect::<String>().contains("roto"))
        .expect("la fila del favorito roto está pintada");
    let sidebar: String = f[y].chars().take(16).collect();
    assert!(sidebar.contains('!'), "va marcada: {sidebar:?}");
    // Y ATENUADA: el volcado de texto no lleva estilos, así que celda a celda.
    let x = sidebar.find("roto").expect("el nombre está");
    let estilo = buf[(
        u16::try_from(x).expect("cabe"),
        u16::try_from(y).expect("cabe"),
    )]
        .style();
    assert_eq!(
        estilo.fg,
        app.theme.role(norte_theme::Role::Info).fg,
        "la fila de un favorito roto se pinta con el frente atenuado"
    );
}

/// Cerrado —el default— la pantalla no lleva sidebar ninguno: el criterio de
/// aceptación de L3 es que el usuario no note nada hasta abrirlo, y los
/// snapshots ortodoxos que ya existen lo comprueban celda a celda.
#[test]
fn cerrado_no_pinta_nada() {
    let mut app = app_de_prueba();
    app.render_now_ms = Some(0);
    let buf = buffer_de(&app, 100, 30);
    let f = filas(&buf);
    assert!(
        !f.iter()
            .any(|fila| fila.contains(&norte_i18n::t_in(norte_i18n::Lang::Es, "places-title"))),
        "sin abrirlo, el título del sidebar no aparece"
    );
}

/// Enter sobre un favorito lleva al LISTADO ENFOCADO a ese sitio, y devuelve
/// el teclado. El sidebar es un MANDO, no un panel con directorio propio.
#[test]
fn enter_en_un_favorito_da_el_destino_y_suelta_el_teclado() {
    let mut app = app_con_sidebar();
    // Cabecera Unidades, dos discos, cabecera Favoritos, trabajo.
    for _ in 0..4 {
        app.places_down();
    }
    let destino = app.places_activate().expect("un favorito da destino");
    assert_eq!(destino, vp("file:///trabajo"));
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// Enter sobre una cabecera no hace nada, y el teclado se queda donde está.
#[test]
fn enter_en_una_cabecera_no_hace_nada() {
    let mut app = app_con_sidebar();
    assert!(app.places_activate().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Places);
}

/// Enter sobre un favorito ROTO no navega y la barra dice por qué: es la otra
/// mitad de pintarlo marcado, porque en catorce celdas cabe el aviso y no la
/// explicación.
#[test]
fn enter_en_un_favorito_roto_explica_en_la_barra() {
    let mut app = app_con_sidebar();
    for _ in 0..5 {
        app.places_down();
    }
    assert!(app.places_activate().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Places, "no suelta el teclado");
    assert_eq!(
        app.message.as_deref(),
        Some(norte_i18n::t_in(norte_i18n::Lang::Es, "hotlist-invalid").as_str()),
        "la barra dice el motivo"
    );
}

/// Enter sobre un disco lleva a su punto de montaje.
#[test]
fn enter_en_un_disco_da_su_montaje() {
    let mut app = app_con_sidebar();
    app.places_down();
    assert_eq!(app.places_activate(), Some(vp("file:///")));
}

/// Plegar una sección esconde sus filas sin cerrar nada.
#[test]
fn plegar_desde_la_app_esconde_las_filas() {
    let mut app = app_con_sidebar();
    let id = app.places_slot().expect("abierto");
    let antes = app.panes.places(id).expect("sidebar").rows().len();
    app.places_toggle_fold();
    let despues = app.panes.places(id).expect("sidebar").rows().len();
    assert!(despues < antes);
    assert!(app.places_slot().is_some(), "plegar no cierra el sidebar");
}
