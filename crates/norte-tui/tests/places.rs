//! El sidebar de sitios dentro de la `App` (L3): abrirlo, enfocarlo, cerrarlo
//! y, sobre todo, NO tocar los listados al hacerlo.
//!
//! La regla que estos tests protegen es la 7 del spec: `app.panes[i]` sigue
//! queriendo decir «el i-ésimo LISTADO». Un sidebar no es un lado, y el día
//! que lo fuera, una copia podría tener por destino una lista de discos.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Pane};

fn vp(wire: &str) -> VPath {
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
