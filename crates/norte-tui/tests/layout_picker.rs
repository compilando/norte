//! El selector de disposición: qué se carga, quién gana y qué se dice.
//!
//! La regla que estos tests fijan es una sola y es la que sorprende: un
//! fichero tuyo en `layouts/<nombre>.toml` GANA al preset de fábrica con ese
//! nombre. Es lo que hacen las demás capas de configuración, y el preset se
//! recupera borrando el fichero.

use std::ffi::{OsStr, OsString};

use norte_frontend::layout::{Dir, KindId, Node, SlotId, config::to_toml};
use norte_frontend::layout_picker::{LayoutPicker, UserLayout};
use norte_proto::VPath;
use norte_tui::app::{App, Pane};

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn app_de_prueba() -> App {
    App::new(
        Pane::new(vp("file:///izq"), Vec::new()),
        Pane::new(vp("file:///der"), Vec::new()),
    )
}

/// Un layout de usuario: dos listados y nada más, para distinguirlo de
/// cualquier preset por el número de huecos.
fn arbol_mio() -> Node {
    Node::split(
        Dir::Vertical,
        vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(2), KindId::browser()),
        ],
    )
}

/// Cargar y aplicar, que es lo que hace el binario en dos pasos: el fichero
/// se lee FUERA del bucle de eventos (regla 2, #244 M2) y el `App` solo
/// decide qué hacer con lo leído.
fn aplicar(app: &mut App, nombre: &str, dir: &std::path::Path) -> bool {
    let n = OsStr::new(nombre);
    app.apply_loaded_layout(n, norte_frontend::layout::config::load(dir, n))
}

/// Lo que el binario le pasa al selector: cada fichero del directorio, ya
/// leído.
fn del_usuario(dir: &std::path::Path) -> Vec<UserLayout> {
    norte_frontend::layout::config::list(dir)
        .into_iter()
        .map(|name| UserLayout {
            tree: norte_frontend::layout::config::load(dir, &name).map_err(|e| e.to_string()),
            name,
        })
        .collect()
}

fn escribir(dir: &std::path::Path, nombre: &str, arbol: &Node) {
    let layouts = dir.join("layouts");
    std::fs::create_dir_all(&layouts).expect("mkdir");
    std::fs::write(
        layouts.join(format!("{nombre}.toml")),
        to_toml(arbol).expect("toml"),
    )
    .expect("write");
}

#[test]
fn un_preset_de_fabrica_se_aplica_sin_fichero_ninguno() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut app = app_de_prueba();
    assert!(aplicar(&mut app, "simple", dir.path()));
    assert_eq!(
        app.layout,
        norte_frontend::layout::presets::tree("simple").expect("de fábrica")
    );
    assert!(app.message.is_none(), "sin fichero no hay nada que avisar");
}

/// Un preset con sidebar, visor, procesos y atributos deja TODOS esos huecos
/// con su estado puesto. Sin esto el panel se pinta vacío para siempre: el
/// toggle que lo habría creado no se va a pulsar, porque ya está ahí.
#[test]
fn un_preset_completo_siembra_el_estado_de_cada_hueco() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut app = app_de_prueba();
    assert!(aplicar(&mut app, "full", dir.path()));
    for id in app.layout.slot_ids() {
        let kind = app
            .layout
            .kind_of(id)
            .map(|k| k.as_str().to_owned())
            .expect("kind");
        let sembrado = match kind.as_str() {
            "browser" => app.panes.browser(id).is_some(),
            "places" => app.panes.places(id).is_some(),
            "viewer" => app.panes.preview(id).is_some(),
            "processes" => app.panes.processes(id).is_some(),
            "metadata" => app.panes.metadata(id).is_some(),
            // `tasks` y `status` no tienen estado propio.
            _ => true,
        };
        assert!(sembrado, "el hueco {id:?} de kind {kind} se quedó vacío");
    }
}

/// Gana el fichero del usuario, y con el mismo nombre que uno de fábrica.
#[test]
fn un_fichero_del_usuario_gana_al_preset_del_mismo_nombre() {
    let dir = tempfile::tempdir().expect("tmp");
    escribir(dir.path(), "simple", &arbol_mio());
    let mut app = app_de_prueba();
    assert!(aplicar(&mut app, "simple", dir.path()));
    assert_eq!(app.layout, arbol_mio(), "se cargó el del usuario");
    assert!(app.message.is_none());
}

/// Un fichero roto AVISA y cae al preset: un layout que no parsea no puede
/// dejar a norte sin pantalla.
#[test]
fn un_fichero_roto_avisa_y_cae_al_preset() {
    let dir = tempfile::tempdir().expect("tmp");
    let layouts = dir.path().join("layouts");
    std::fs::create_dir_all(&layouts).expect("mkdir");
    std::fs::write(layouts.join("simple.toml"), "esto no es un layout").expect("write");

    let mut app = app_de_prueba();
    assert!(aplicar(&mut app, "simple", dir.path()));
    assert_eq!(
        app.layout,
        norte_frontend::layout::presets::tree("simple").expect("de fábrica"),
        "cayó al preset"
    );
    let aviso = app.message.as_deref().expect("avisa");
    assert!(aviso.contains("simple"), "nombra el layout: {aviso}");
}

/// Un nombre que no es de nadie no deja la pantalla a medias: se dice y el
/// árbol de antes sigue en pie.
#[test]
fn un_nombre_desconocido_no_cambia_el_arbol() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut app = app_de_prueba();
    let antes = app.layout.clone();
    assert!(!aplicar(&mut app, "no-existe", dir.path()));
    assert_eq!(app.layout, antes);
    assert!(app.message.is_some(), "lo dice");
}

/// Cambiar de disposición no borra la navegación: el listado que ya estaba
/// en un hueco sigue donde estaba.
#[test]
fn cambiar_de_layout_conserva_los_listados_que_ya_habia() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut app = app_de_prueba();
    let izq = app.panes[0].dir().clone();
    assert!(aplicar(&mut app, "full", dir.path()));
    assert_eq!(app.panes[0].dir(), &izq, "el listado no se reinició");
}

/// El selector: cinco de fábrica más lo del directorio, y el aviso de que el
/// nombre coincide con un preset de teclas.
#[test]
fn el_selector_lista_las_de_fabrica_y_las_del_usuario() {
    let dir = tempfile::tempdir().expect("tmp");
    escribir(dir.path(), "mio", &arbol_mio());
    let mios = del_usuario(dir.path());
    assert_eq!(
        mios.iter().map(|u| u.name.clone()).collect::<Vec<_>>(),
        vec![OsString::from("mio")]
    );

    let p = LayoutPicker::open(mios);
    let nombres: Vec<OsString> = p.rows().iter().map(|r| r.name.clone()).collect();
    assert_eq!(
        nombres,
        ["orthodox", "simple", "krusader", "explorer", "full", "mio"]
            .map(OsString::from)
            .to_vec()
    );
    assert!(
        p.rows()
            .iter()
            .find(|r| r.name == OsStr::new("krusader"))
            .expect("krusader")
            .shares_keymap_name,
        "el diálogo avisa de que el nombre es también de keymap"
    );
    assert!(
        p.rows().last().expect("mio").tree.as_ref() == Some(&arbol_mio()),
        "la fila del usuario trae SU árbol para la vista previa (#244 M3)"
    );
    assert!(
        !p.rows().last().expect("mio").factory,
        "el del usuario no es de fábrica"
    );
}

/// Elegir en el selector aplica lo elegido, y cancelar no toca nada.
#[test]
fn confirmar_aplica_y_cancelar_no_toca_nada() {
    use norte_tui::app::PickerAction;
    let dir = tempfile::tempdir().expect("tmp");
    let mut app = app_de_prueba();
    let antes = app.layout.clone();

    app.open_layout_picker(del_usuario(dir.path()));
    app.layout_picker_input(PickerAction::Cancel);
    assert!(app.layout_picker.is_none(), "cerrado");
    assert_eq!(app.layout, antes, "cancelar no aplica");

    app.open_layout_picker(del_usuario(dir.path()));
    app.layout_picker_input(PickerAction::Down); // simple
    app.layout_picker_input(PickerAction::Confirm);
    assert!(app.layout_picker.is_none(), "confirmar cierra");
    assert_eq!(
        app.layout,
        norte_frontend::layout::presets::tree("simple").expect("de fábrica")
    );
    assert!(
        app.message.as_deref().is_some_and(|m| m.contains("simple")),
        "dice cuál aplicó: {:?}",
        app.message
    );
}
