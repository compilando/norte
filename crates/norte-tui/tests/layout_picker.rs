//! The layout picker: what loads, who wins and what gets said.
//!
//! The rule these tests pin is a single one and it is the surprising one:
//! a file of yours at `layouts/<name>.toml` WINS over the factory preset of
//! that name. It is what every other config layer does, and the preset
//! comes back by deleting the file.

use std::ffi::{OsStr, OsString};

use norte_frontend::layout::{Dir, KindId, Node, SlotId, config::to_toml};
use norte_frontend::layout_picker::{LayoutPicker, UserLayout};
use norte_proto::VPath;
use norte_tui::app::{App, Pane};

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn app_de_prueba() -> App {
    App::new(
        Pane::new(vp("file:///izq"), Vec::new()),
        Pane::new(vp("file:///der"), Vec::new()),
    )
}

/// A user layout: two listings and nothing else, to tell it apart from any
/// preset by its number of slots.
fn arbol_mio() -> Node {
    Node::split(
        Dir::Vertical,
        vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(2), KindId::browser()),
        ],
    )
}

/// Load and apply, which is what the binary does in two steps: the file is
/// read OUTSIDE the event loop (rule 2, #244 M2) and `App` only decides
/// what to do with what was read.
fn aplicar(app: &mut App, nombre: &str, dir: &std::path::Path) -> bool {
    let n = OsStr::new(nombre);
    app.apply_loaded_layout(n, norte_frontend::layout::config::load(dir, n))
}

/// What the binary hands the picker: every file in the directory, already
/// read.
fn user(dir: &std::path::Path) -> Vec<UserLayout> {
    norte_frontend::layout::config::list(dir)
        .into_iter()
        .map(|name| UserLayout {
            tree: norte_frontend::layout::config::load(dir, &name).map_err(|e| e.to_string()),
            name,
        })
        .collect()
}

fn escribir(dir: &std::path::Path, nombre: &str, tree: &Node) {
    let layouts = dir.join("layouts");
    std::fs::create_dir_all(&layouts).expect("mkdir");
    std::fs::write(
        layouts.join(format!("{nombre}.toml")),
        to_toml(tree).expect("toml"),
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
        norte_frontend::layout::presets::tree("simple").expect("factory")
    );
    assert!(
        app.message.is_none(),
        "with no file there is nothing to warn about"
    );
}

/// A preset with a sidebar, viewer, processes and attributes leaves ALL
/// those slots with their state set. Without this the panel paints empty
/// forever: the toggle that would have created it is not going to be
/// pressed, because it is already there.
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
            // `tasks` and `status` have no state of their own.
            _ => true,
        };
        assert!(sembrado, "slot {id:?} of kind {kind} was left empty");
    }
}

/// The user's file wins, even under the same name as a factory one.
#[test]
fn un_fichero_del_usuario_gana_al_preset_del_mismo_nombre() {
    let dir = tempfile::tempdir().expect("tmp");
    escribir(dir.path(), "simple", &arbol_mio());
    let mut app = app_de_prueba();
    assert!(aplicar(&mut app, "simple", dir.path()));
    assert_eq!(app.layout, arbol_mio(), "the user's was loaded");
    assert!(app.message.is_none());
}

/// A broken file WARNS and falls back to the preset: a layout that does not
/// parse cannot leave norte with no screen.
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
        norte_frontend::layout::presets::tree("simple").expect("factory"),
        "it fell back to the preset"
    );
    let notice = app.message.as_deref().expect("it warns");
    assert!(notice.contains("simple"), "it names the layout: {notice}");
}

/// A name that belongs to nobody does not leave the screen half-done: it is
/// said and the earlier tree stays standing.
#[test]
fn un_nombre_desconocido_no_cambia_el_arbol() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut app = app_de_prueba();
    let before = app.layout.clone();
    assert!(!aplicar(&mut app, "no-existe", dir.path()));
    assert_eq!(app.layout, before);
    assert!(app.message.is_some(), "it says so");
}

/// Switching layouts does not erase navigation: the listing that was
/// already in a slot stays where it was.
#[test]
fn cambiar_de_layout_conserva_los_listados_que_ya_habia() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut app = app_de_prueba();
    let left = app.panes[0].dir().clone();
    assert!(aplicar(&mut app, "full", dir.path()));
    assert_eq!(app.panes[0].dir(), &left, "the listing was not reset");
}

/// The picker: five factory ones plus the directory's, and the warning that
/// the name matches a keymap preset.
#[test]
fn el_selector_lista_las_de_fabrica_y_las_del_usuario() {
    let dir = tempfile::tempdir().expect("tmp");
    escribir(dir.path(), "mio", &arbol_mio());
    let mine = user(dir.path());
    assert_eq!(
        mine.iter().map(|u| u.name.clone()).collect::<Vec<_>>(),
        vec![OsString::from("mio")]
    );

    let p = LayoutPicker::open(mine);
    let names: Vec<OsString> = p.rows().iter().map(|r| r.name.clone()).collect();
    assert_eq!(
        names,
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
        "the dialog warns that the name is also a keymap's"
    );
    assert!(
        p.rows().last().expect("mio").tree.as_ref() == Some(&arbol_mio()),
        "the user's row carries ITS OWN tree for the preview (#244 M3)"
    );
    assert!(
        !p.rows().last().expect("mio").factory,
        "the user's is not a factory one"
    );
}

/// Choosing from the picker applies the choice, and cancelling touches nothing.
#[test]
fn confirmar_aplica_y_cancelar_no_toca_nada() {
    use norte_tui::app::PickerAction;
    let dir = tempfile::tempdir().expect("tmp");
    let mut app = app_de_prueba();
    let before = app.layout.clone();

    app.open_layout_picker(user(dir.path()));
    app.layout_picker_input(PickerAction::Cancel);
    assert!(app.layout_picker.is_none(), "closed");
    assert_eq!(app.layout, before, "cancelling applies nothing");

    app.open_layout_picker(user(dir.path()));
    app.layout_picker_input(PickerAction::Down); // simple
    app.layout_picker_input(PickerAction::Confirm);
    assert!(app.layout_picker.is_none(), "confirming closes it");
    assert_eq!(
        app.layout,
        norte_frontend::layout::presets::tree("simple").expect("factory")
    );
    assert!(
        app.message.as_deref().is_some_and(|m| m.contains("simple")),
        "it says which one it applied: {:?}",
        app.message
    );
}
