//! La hoja de atributos (fase A): a quién sigue, y sobre todo qué NO lee.
//!
//! Igual que el visor acoplado, se prueba sin daemon: la decisión vive en la
//! lib, así que «un hueco oculto no pide nada» deja de ser una regla escrita
//! en un spec y pasa a ser lo único que el código puede hacer. Y aquí hay una
//! regla más fuerte todavía: la hoja no pide NUNCA, ni visible. Lo que enseña
//! ya está en el listado.

use norte_frontend::layout::Resolved;
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Pane};
use norte_tui::metadata::{Want, want};

const W: u16 = 100;
const H: u16 = 30;

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn entry(dir: &VPath, nombre: &[u8], kind: EntryKind) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir
            .join(Segment::new(nombre.to_vec()).expect("segmento"))
            .clone(),
        kind,
        size: Some(7),
        mtime_ms: Some(1_700_000_000_000),
    }
}

fn app_de_prueba() -> App {
    let left = vp("file:///izq");
    let right = vp("file:///der");
    App::new(
        Pane::new(
            left.clone(),
            vec![
                entry(&left, b"uno.txt", EntryKind::File),
                entry(&left, b"carpeta", EntryKind::Dir),
            ],
        ),
        Pane::new(
            right.clone(),
            vec![entry(&right, b"dos.txt", EntryKind::File)],
        ),
    )
}

fn resolver(app: &mut App) -> Resolved {
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    norte_tui::ui::before_frame(app, area);
    norte_tui::ui::resolved_for(app, area)
}

/// Con la hoja abierta y el cursor sobre algo, hay objetivo, y viaja con su
/// HUECO: una respuesta aplicada por posición aterriza en quien ocupe ese
/// sitio al llegar (la lección de la fase C de P6).
#[test]
fn el_objetivo_lleva_el_hueco_de_la_hoja() {
    let mut app = app_de_prueba();
    app.toggle_metadata();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    let (hueco, w) = want(&app, &res).expect("hay objetivo");
    assert_eq!(Some(hueco), app.metadata_slot());
    let Want::Entry(e) = w else {
        panic!("una entrada")
    };
    assert_eq!(e.path, vp("file:///izq/uno.txt"));
}

/// La entrada que enseña es la que el listado YA tiene: mismo tamaño, misma
/// fecha, mismos bytes. Esto es lo que hace que la hoja no cueste una lectura.
#[test]
fn lo_que_ensena_sale_del_listado_y_no_de_una_peticion() {
    let mut app = app_de_prueba();
    app.toggle_metadata();
    app.panes[0].set_cursor(1);
    let del_listado = app.panes[0].selected().expect("cursor").clone();
    let res = resolver(&mut app);
    let (_, w) = want(&app, &res).expect("objetivo");
    assert_eq!(w, Want::Entry(Box::new(del_listado)));
}

/// Un hueco detrás de una pestaña no produce objetivo. Es el mismo invariante
/// que el preview, y se prueba igual porque una fuga así solo la ve un test.
#[test]
fn una_hoja_oculta_no_produce_objetivo() {
    let mut app = app_de_prueba();
    app.toggle_metadata();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    assert!(want(&app, &res).is_some());

    let hueco = app.metadata_slot().expect("abierta");
    app.layout = app.layout.wrap_in_tabs(hueco);
    app.layout = app.layout.add_tab(
        hueco,
        &norte_frontend::layout::Node::slot(
            norte_frontend::layout::SlotId(900),
            norte_frontend::layout::KindId::new("tasks"),
        ),
    );
    let res = resolver(&mut app);
    assert!(
        !res.placements.iter().any(|(id, _)| *id == hueco),
        "el hueco quedó oculto de verdad"
    );
    assert!(want(&app, &res).is_none(), "lo que no se ve no enseña nada");
}

/// Sin cursor no se queda lo de antes puesto: se dice que no hay nada debajo.
#[test]
fn un_listado_vacio_dice_que_no_hay_nada() {
    let dir = vp("file:///vacio");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.toggle_metadata();
    let res = resolver(&mut app);
    let (_, w) = want(&app, &res).expect("hay respuesta");
    assert_eq!(w, Want::Note("metadata-empty"));
}

/// Sigue al rol `active`: cambiar de listado cambia lo que enseña, sin tocar
/// el layout.
#[test]
fn cambiar_de_listado_cambia_lo_que_ensena() {
    let mut app = app_de_prueba();
    app.toggle_metadata();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    let (_, a) = want(&app, &res).expect("objetivo");
    app.set_focus(1);
    let res = resolver(&mut app);
    let (_, b) = want(&app, &res).expect("objetivo");
    let (Want::Entry(a), Want::Entry(b)) = (a, b) else {
        panic!("dos entradas")
    };
    assert_eq!(a.path, vp("file:///izq/uno.txt"));
    assert_eq!(b.path, vp("file:///der/dos.txt"));
}

/// Un nombre que no es UTF-8 llega byte a byte: la hoja no lo reinterpreta ni
/// lo pierde por el camino.
#[test]
fn un_nombre_no_utf8_llega_entero() {
    let dir = vp("file:///izq");
    let hostile = entry(&dir, b"m\xffl.txt", EntryKind::File);
    let mut app = App::new(
        Pane::new(dir.clone(), vec![hostile.clone()]),
        Pane::new(vp("file:///der"), Vec::new()),
    );
    app.toggle_metadata();
    let res = resolver(&mut app);
    let (_, w) = want(&app, &res).expect("objetivo");
    assert_eq!(w, Want::Entry(Box::new(hostile)));
}

/// La hoja NO se lleva el teclado NUNCA: sigue al cursor, y con las flechas
/// dentro dejaría de seguir a nada. Dos estados, abrir y cerrar.
///
/// Tenía tres, y el del medio era falso: ponía un `KeyOwner` que no consumía
/// nadie, así que la hoja cogía el borde de foco mientras las flechas seguían
/// moviendo el listado de al lado, y hacía falta una tercera pulsación para
/// cerrar lo que la segunda no había enfocado (#243).
#[test]
fn la_hoja_no_se_lleva_el_teclado_nunca() {
    let mut app = app_de_prueba();
    app.toggle_metadata();
    assert!(app.metadata_slot().is_some(), "abierta");
    assert_eq!(
        app.key_owner(),
        KeyOwner::Panes,
        "el cursor sigue siendo tuyo"
    );

    app.toggle_metadata();
    assert!(app.metadata_slot().is_none(), "la segunda cierra");
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// El panel de procesos hace lo contrario, y a propósito: se abre para actuar
/// sobre una tarea, así que toma el teclado de entrada y la segunda pulsación
/// cierra. La franja de tareas no se toca en ningún momento.
#[test]
fn el_panel_de_procesos_toma_el_teclado_al_abrir() {
    let mut app = app_de_prueba();
    let tasks_antes = app
        .layout
        .slot_ids()
        .into_iter()
        .filter(|id| {
            app.layout
                .kind_of(*id)
                .is_some_and(|k| k.as_str() == "tasks")
        })
        .count();

    app.toggle_processes();
    assert!(app.processes_slot().is_some(), "abierto");
    assert_eq!(app.key_owner(), KeyOwner::Processes);

    app.toggle_processes();
    assert!(app.processes_slot().is_none(), "cerrado");
    assert_eq!(app.key_owner(), KeyOwner::Panes);

    let tasks_despues = app
        .layout
        .slot_ids()
        .into_iter()
        .filter(|id| {
            app.layout
                .kind_of(*id)
                .is_some_and(|k| k.as_str() == "tasks")
        })
        .count();
    assert_eq!(tasks_antes, tasks_despues, "la franja sigue donde estaba");
}

/// Cerrar cualquiera de los dos devuelve el árbol de antes, sin dejar un
/// `Split` degenerado detrás.
#[test]
fn cerrar_devuelve_el_arbol_de_antes() {
    let mut app = app_de_prueba();
    let antes = app.layout.clone();

    app.toggle_metadata();
    app.toggle_metadata();
    assert_eq!(app.layout, antes, "la hoja no dejó rastro");

    app.toggle_processes();
    app.toggle_processes();
    assert_eq!(app.layout, antes, "el panel de procesos tampoco");
}
