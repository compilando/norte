//! Partir un panel y abrir una pestaña HEREDAN el listado del panel de al
//! lado, y eso no puede traerse la fila `..`.
//!
//! Lo que copiaban era `entries()`, que la lleva dentro: el panel nuevo se
//! ponía la SUYA encima y la heredada quedaba en medio del listado — con el
//! nombre del directorio padre, ordenada entre los directorios, y marcable.
//! Cada partición añadía una más, y `Ctrl+A` metía al PADRE en lo que se copia
//! o se borra.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Pane};

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn entradas(dir: &VPath, n: usize) -> Vec<Entry> {
    (0..n)
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

/// Dos panes sobre `file:///casa/hija` —un directorio con padre, que es donde
/// existe la fila `..`— y la fila encendida.
fn app_con_fila_de_subir() -> App {
    let dir = vp("file:///casa/hija");
    let mut app = App::new(
        Pane::new(dir.clone(), entradas(&dir, 3)),
        Pane::new(dir.clone(), entradas(&dir, 3)),
    );
    app.set_parent_row(true);
    app
}

/// Cuántas entradas tiene cada listado, y cuántas de ellas apuntan al padre.
fn foto(app: &App) -> (Vec<usize>, usize) {
    let padre = vp("file:///casa");
    let cuentas = (0..app.panes.len())
        .map(|i| app.panes[i].entries().len())
        .collect();
    let padres = (0..app.panes.len())
        .map(|i| {
            app.panes[i]
                .entries()
                .iter()
                .filter(|e| e.path == padre)
                .count()
        })
        .sum();
    (cuentas, padres)
}

/// Partir tres veces deja los cuatro paneles con el MISMO listado, y una sola
/// fila de subir en cada uno.
#[test]
fn partir_varias_veces_no_acumula_filas_de_subir() {
    let mut app = app_con_fila_de_subir();
    let (antes, _) = foto(&app);
    assert_eq!(antes[0], 4, "tres entradas y la de subir");

    for _ in 0..3 {
        app.layout_split(norte_frontend::layout::Dir::Horizontal);
    }
    let (cuentas, padres) = foto(&app);
    assert_eq!(cuentas.len(), 5, "los dos de partida y los tres nuevos");
    assert!(
        cuentas.iter().all(|n| *n == antes[0]),
        "todos los paneles listan lo mismo: {cuentas:?}"
    );
    assert_eq!(
        padres,
        cuentas.len(),
        "una fila de subir por panel, ni una más"
    );
}

/// Y una pestaña nueva, igual: hereda el listado por el mismo camino.
#[test]
fn una_pestana_nueva_no_hereda_la_fila_de_subir_como_entrada() {
    let mut app = app_con_fila_de_subir();
    let (antes, _) = foto(&app);
    app.tab_new();
    app.tab_new();
    let (cuentas, padres) = foto(&app);
    assert!(
        cuentas.iter().all(|n| *n == antes[0]),
        "cada pestaña lista lo mismo: {cuentas:?}"
    );
    assert_eq!(padres, cuentas.len());
}

/// La consecuencia que importa: tras partir, marcar TODO marca lo mismo que
/// antes de partir — y el padre no está entre lo marcado. Si lo estuviera, F8
/// borraría el directorio de arriba.
#[test]
fn marcar_todo_en_un_panel_partido_no_marca_al_padre() {
    let mut app = app_con_fila_de_subir();
    app.panes[0].mark_all();
    let esperado = app.panes[0].marks_len();
    assert_eq!(esperado, 3, "las tres entradas de verdad, no la de subir");
    app.panes[0].clear_marks();

    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    app.layout_split(norte_frontend::layout::Dir::Horizontal);
    let nuevo = app.focus();
    app.panes[nuevo].mark_all();
    assert_eq!(
        app.panes[nuevo].marks_len(),
        esperado,
        "el panel partido marca lo mismo que el de partida"
    );
    assert!(
        !app.panes[nuevo]
            .marked_paths()
            .contains(&vp("file:///casa")),
        "el PADRE jamás entra en lo marcado: {:?}",
        app.panes[nuevo].marked_paths()
    );
}
