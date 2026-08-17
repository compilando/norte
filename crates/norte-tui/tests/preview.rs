//! El visor ACOPLADO (L3): qué decide leer, y sobre todo qué decide NO leer.
//!
//! Todo esto se prueba sin daemon a propósito. `main.rs` es un binario y un
//! test de integración no lo alcanza, así que la decisión vive en la lib: si
//! `preview::want` no devuelve objetivo, no existe petición que contar. La
//! suspensión de un hueco oculto deja de ser una regla escrita en un spec y
//! pasa a ser lo único que el código puede hacer.

use norte_frontend::layout::{LayoutDiagnostic, Resolved};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Pane};
use norte_tui::preview::{Want, want};

const W: u16 = 100;
const H: u16 = 30;

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn entrada(dir: &VPath, nombre: &str, kind: EntryKind) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir
            .join(Segment::new(nombre.as_bytes().to_vec()).expect("segmento"))
            .clone(),
        kind,
        size: Some(1),
        mtime_ms: None,
    }
}

/// Dos listados, cada uno con un fichero DISTINTO y un directorio.
///
/// `Pane::new` ORDENA, y el orden pone los directorios primero: el cursor
/// arranca sobre `carpeta`, no sobre el fichero. Los tests lo colocan a mano.
fn app_de_prueba() -> App {
    let izq = vp("file:///izq");
    let der = vp("file:///der");
    App::new(
        Pane::new(
            izq.clone(),
            vec![
                entrada(&izq, "uno.txt", EntryKind::File),
                entrada(&izq, "carpeta", EntryKind::Dir),
            ],
        ),
        Pane::new(der.clone(), vec![entrada(&der, "dos.txt", EntryKind::File)]),
    )
}

/// Resuelve el frame como lo hace el run loop: `before_frame` reconcilia los
/// roles, y sin eso el vínculo `follows: Role(Active)` no apunta a nadie.
fn resolver(app: &mut App) -> Resolved {
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    norte_tui::ui::before_frame(app, area);
    norte_tui::ui::resolved_for(app, area)
}

/// Con el preview abierto y el cursor sobre un fichero, hay objetivo, y viaja
/// con su HUECO. Es la lección de la fase C de P6: por posición, una respuesta
/// en vuelo se aplica a quien ocupe ese sitio al llegar.
#[test]
fn el_objetivo_lleva_el_hueco_del_preview() {
    let mut app = app_de_prueba();
    app.toggle_preview();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    let (hueco, w) = want(&app, &res).expect("hay objetivo");
    assert_eq!(Some(hueco), app.preview_slot());
    assert_eq!(w, Want::File(vp("file:///izq/uno.txt")));
}

/// Un preview detrás de una pestaña no produce objetivo: no hay petición que
/// contar. En L1b una fuga igual solo la vio un test, así que aquí está.
#[test]
fn un_preview_oculto_no_produce_objetivo() {
    let mut app = app_de_prueba();
    app.toggle_preview();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    assert!(want(&app, &res).is_some());

    // Se esconde metiéndolo en una pestaña con otro panel delante.
    let hueco = app.preview_slot().expect("abierto");
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
    assert!(
        want(&app, &res).is_none(),
        "un hueco que no se pinta no pide nada"
    );
}

/// Un directorio bajo el cursor no se lee: se dice lo que es.
#[test]
fn un_directorio_bajo_el_cursor_no_se_lee() {
    let mut app = app_de_prueba();
    app.toggle_preview();
    app.panes[0].set_cursor(0);
    let res = resolver(&mut app);
    let (_, w) = want(&app, &res).expect("hay respuesta");
    assert_eq!(w, Want::Note("preview-directory"));
}

/// El preview sigue al rol `active`: cambiar de listado cambia lo que enseña,
/// sin tocar el layout. Es el primer consumidor de `follows` que existe.
#[test]
fn cambiar_de_listado_cambia_el_objetivo() {
    let mut app = app_de_prueba();
    app.toggle_preview();
    app.panes[0].set_cursor(1);
    let res = resolver(&mut app);
    let (_, a) = want(&app, &res).expect("objetivo");
    app.set_focus(1);
    let res = resolver(&mut app);
    let (_, b) = want(&app, &res).expect("objetivo");
    assert_eq!(a, Want::File(vp("file:///izq/uno.txt")));
    assert_eq!(b, Want::File(vp("file:///der/dos.txt")));
}

/// Y si el hueco SEGUIDO muere, el motor degrada al rol `active` y lo DICE. El
/// diagnóstico existe desde L1a y hasta L3 no lo ejercitaba nadie.
#[test]
fn si_muere_el_hueco_seguido_se_degrada_al_activo_y_lo_dice() {
    use norte_frontend::layout::{Bindings, Edge, Follow, KindId, Node, Size, SlotId};
    let mut app = app_de_prueba();
    app.toggle_preview();
    let hueco = app.preview_slot().expect("abierto");
    // Se vuelve a acoplar el MISMO hueco atado a uno CONCRETO que no existe:
    // es el estado en que queda un `follows: Slot(id)` cuyo panel se cerró.
    let foco = app.focused_slot();
    app.layout = app.layout.close_slot(hueco).expect("se puede cerrar").dock(
        foco,
        Edge::Right,
        Size::Weight(1),
        &Node::slot_bound(
            hueco,
            KindId::new("viewer"),
            Bindings {
                follows: Some(Follow::Slot(SlotId(777))),
            },
        ),
    );
    let res = resolver(&mut app);
    assert!(
        res.diagnostics.is_empty(),
        "el reparto en sí no tiene nada que arreglar"
    );
    let mut diags = Vec::new();
    let destino =
        norte_frontend::layout::resolve_follow(&app.layout, hueco, &app.roles, &mut diags);
    assert_eq!(
        destino,
        app.roles.get(norte_frontend::layout::RoleId::Active)
    );
    assert!(
        diags
            .iter()
            .any(|d| matches!(d, LayoutDiagnostic::FollowRetargeted { .. })),
        "y lo dice en vez de quedarse mirando al vacío"
    );
    assert!(
        want(&app, &res).is_some(),
        "el preview sigue enseñando algo"
    );
}

/// Abrir el preview no cambia cuántos LISTADOS hay, ni cuál está enfocado, ni
/// —y esto es lo que importa— quién tiene el teclado.
///
/// Un preview que se lleva las flechas apaga lo único que hace: seguir a un
/// cursor que ya no se puede mover. Lo enseñó tmux en la primera pulsación.
#[test]
fn abrir_el_preview_no_se_lleva_el_teclado() {
    let mut app = app_de_prueba();
    let antes = (app.panes.len(), app.focus());
    app.toggle_preview();
    assert_eq!((app.panes.len(), app.focus()), antes);
    assert_eq!(app.key_owner(), KeyOwner::Panes);
    assert!(app.preview_slot().is_some());
}

/// La segunda pulsación SÍ se lo lleva: es como se llega a `viewer.hex` y a
/// los encodings sin inventar teclas nuevas.
#[test]
fn la_segunda_pulsacion_enfoca_el_preview() {
    let mut app = app_de_prueba();
    app.toggle_preview();
    app.toggle_preview();
    assert_eq!(app.key_owner(), KeyOwner::Preview);
    assert!(app.preview_slot().is_some(), "enfocar no cierra");
}

/// Y la tercera lo cierra, dejando el árbol como estaba.
#[test]
fn la_tercera_pulsacion_cierra_y_devuelve_el_arbol_de_antes() {
    let mut app = app_de_prueba();
    let antes = app.layout.clone();
    app.toggle_preview();
    app.toggle_preview();
    app.toggle_preview();
    assert_eq!(app.layout, antes);
    assert!(app.preview_slot().is_none());
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// Una lectura denegada se PINTA en el hueco, y no abre ningún modal.
///
/// El preview sigue al cursor: un diálogo por pulsación convertiría bajar por
/// un directorio en una ráfaga de modales que nadie pidió.
#[test]
fn una_lectura_denegada_pinta_el_motivo_y_no_abre_modal() {
    let mut app = app_de_prueba();
    app.toggle_preview();
    let hueco = app.preview_slot().expect("abierto");
    app.preview_failed(hueco, "err-permission-denied");
    assert!(app.modal.is_none(), "no se pregunta nada");

    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).expect("terminal");
    app.render_now_ms = Some(0);
    norte_tui::ui::before_frame(&mut app, ratatui::layout::Rect::new(0, 0, W, H));
    terminal
        .draw(|f| norte_tui::ui::draw(f, &app))
        .expect("draw");
    let texto = terminal.backend().to_string();
    let motivo = norte_i18n::t_in(norte_i18n::Lang::Es, "err-permission-denied");
    assert!(
        texto.contains(&motivo),
        "el motivo se lee dentro del hueco:\n{texto}"
    );
}

/// `layout.preview` está atado en los siete presets y en las dos pantallas que
/// lo necesitan: la de navegar y la del VISOR, que es la que resuelve mientras
/// el teclado está dentro del preview. Misma trampa que destapó `alt+b` en
/// tmux, cerrada aquí antes de que muerda.
#[test]
fn layout_preview_esta_atado_en_los_siete_presets_y_en_las_dos_pantallas() {
    use norte_frontend::keymap::{CATALOGUE, Effective, Screen, parse_keymap, presets};
    let conocidos: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
    for nombre in presets::NAMES {
        let src = presets::source(nombre).expect("el preset existe");
        let kf = parse_keymap(src).expect("el preset parsea");
        for pantalla in [Screen::Browse, Screen::Viewer] {
            let eff =
                Effective::build_for(&kf, &[], &conocidos, pantalla).expect("el preset fusiona");
            assert!(
                eff.bindings()
                    .iter()
                    .any(|(_, cmd)| *cmd == "layout.preview"),
                "{nombre} no ata layout.preview en {pantalla:?}"
            );
        }
    }
}
