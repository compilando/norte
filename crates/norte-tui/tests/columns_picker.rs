//! El picker de columnas (#108 7a): comportamiento del overlay y
//! persistencia. Mismo estilo de driving que `theme_picker.rs` +
//! `theme_persist.rs`: la lógica vive en `App`/`ColumnsPicker` (se testea
//! sin bucle de eventos) y el disco por la función de dir explícito
//! (`config::persist_columns`) — `apply_picked_columns` (main.rs) es
//! privada del binario, así que cada test reproduce sus DOS mitades
//! (sesión: `apply_picked` + re-sort; disco: `persist_columns`), que es
//! exactamente el seam que ya usan los tests de tema
//! (`persist_ui_theme_to`).

use norte_frontend::columns::Builtin;
use norte_frontend::{SortColumn, SortDir};
use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::config::{PersistSort, persist_columns};

fn vp(w: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(w).expect("wire")
}

fn app() -> App {
    let d = vp("file:///x");
    App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
}

/// La mitad EN SESIÓN de `apply_picked_columns` (main.rs): settings en
/// memoria (lista+sort y formatos ciclados, #108 7b) + re-sort de ambos
/// panes.
fn aplicar(app: &mut App, picked: &norte_frontend::columns_picker::Picked) {
    app.columns
        .apply_picked(picked.scheme_target.as_deref(), &picked.ids, picked.sort);
    for (id, fmt) in &picked.formats {
        app.columns.apply_format(id, fmt);
    }
    for i in 0..2 {
        app.apply_scheme_sort(i);
    }
}

/// La mitad de DISCO de `apply_picked_columns` (main.rs): el mismo mapeo
/// `SortSpec` → `PersistSort` que hace el binario.
fn persistir(dir: &std::path::Path, picked: &norte_frontend::columns_picker::Picked) {
    persist_columns(
        dir,
        picked.scheme_target.as_deref(),
        &picked.ids,
        PersistSort {
            column: match picked.sort.column {
                SortColumn::Name => "name",
                SortColumn::Size => "size",
                SortColumn::Mtime => "mtime",
            },
            descending: picked.sort.dir == SortDir::Desc,
            dirs_first: picked.sort.dirs_first,
        },
    )
    .expect("persistencia");
    // #108 7b: los formatos ciclados, como en el binario — tras la lista.
    for (id, fmt) in &picked.formats {
        norte_tui::config::persist_column_format(dir, id, fmt).expect("persistencia de formato");
    }
}

/// Los builtin visibles del layout efectivo, en orden.
fn builtins(app: &App) -> Vec<Builtin> {
    app.columns
        .layout_items_for("file")
        .iter()
        .map(|(b, _)| *b)
        .collect()
}

/// Abrir → bajar a mtime → sort → confirmar: el pane queda ordenado por
/// mtime asc y el `norte.toml` del dir hermético lleva `[ui.columns]` con
/// `column = "mtime"`.
#[test]
fn picker_ordena_y_persiste() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app();
    app.open_columns_picker();
    {
        let p = app.columns_picker.as_mut().expect("picker abierto");
        p.down(); // size
        p.down(); // mtime
        p.sort_current();
        assert_eq!(p.sort().column, SortColumn::Mtime, "sort de la fila");
        assert_eq!(p.sort().dir, SortDir::Asc, "primer click = asc");
    }
    let picked = app
        .columns_picker
        .as_ref()
        .expect("picker abierto")
        .finish();
    app.columns_picker = None; // confirm cierra el overlay
    aplicar(&mut app, &picked);
    let sort = app.focused().sort();
    assert_eq!(
        sort.column,
        SortColumn::Mtime,
        "el pane re-sortea en sesión"
    );
    assert_eq!(sort.dir, SortDir::Asc);
    persistir(dir.path(), &picked);
    let s = std::fs::read_to_string(dir.path().join("norte.toml")).expect("leer");
    assert!(s.contains("[ui.columns]"), "sección persistida: {s}");
    assert!(s.contains(r#"column = "mtime""#), "sort persistido: {s}");
}

/// Abrir → apagar size → confirmar: `layout_items_for` del scheme ya no
/// trae `Size` y el fichero persiste `default` sin `"size"`.
#[test]
fn picker_toggle_persiste_la_lista() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app();
    app.open_columns_picker();
    {
        let p = app.columns_picker.as_mut().expect("picker abierto");
        p.down(); // size
        p.toggle(); // fuera
    }
    let picked = app
        .columns_picker
        .as_ref()
        .expect("picker abierto")
        .finish();
    app.columns_picker = None;
    assert_eq!(
        picked.ids,
        vec!["name".to_owned(), "mtime".to_owned()],
        "solo las habilitadas viajan (kind ya venía apagada)"
    );
    assert_eq!(picked.scheme_target, None, "sin override → default");
    aplicar(&mut app, &picked);
    assert_eq!(
        builtins(&app),
        vec![Builtin::Name, Builtin::Mtime],
        "size fuera del layout en sesión"
    );
    persistir(dir.path(), &picked);
    let s = std::fs::read_to_string(dir.path().join("norte.toml")).expect("leer");
    assert!(s.contains("default = ["), "lista persistida: {s}");
    assert!(s.contains(r#""mtime""#), "mtime en la lista: {s}");
    assert!(!s.contains(r#""size""#), "size no debe persistirse: {s}");
}

/// #108 7b: `f` sobre size cicla iec→si; confirmar lo aplica EN SESIÓN
/// (`style_for` lo ve al instante) y persiste un `[[ui.columns.spec]]` con
/// `id = "size"`, `format = "si"` que el `load` real relee.
#[test]
fn picker_cicla_formato_y_persiste_spec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app();
    app.open_columns_picker();
    {
        let p = app.columns_picker.as_mut().expect("picker abierto");
        p.down(); // size
        p.cycle_format(); // iec → si
        assert_eq!(p.format_of_cursor().as_deref(), Some("si"));
    }
    let picked = app
        .columns_picker
        .as_ref()
        .expect("picker abierto")
        .finish();
    app.columns_picker = None;
    assert_eq!(picked.formats, vec![("size".to_owned(), "si".to_owned())]);
    aplicar(&mut app, &picked);
    assert_eq!(
        app.columns.style_for("file", Builtin::Size).size_format,
        norte_frontend::columns::SizeFormat::Si,
        "la sesión ve el formato al instante"
    );
    persistir(dir.path(), &picked);
    let s = std::fs::read_to_string(dir.path().join("norte.toml")).expect("leer");
    assert!(s.contains(r#"id = "size""#), "spec persistido: {s}");
    assert!(s.contains(r#"format = "si""#), "formato persistido: {s}");
    // Round-trip por el loader REAL del frontend (capas del TUI).
    let layers = norte_tui::config::Layers {
        dirs: vec![(dir.path().to_path_buf(), norte_tui::config::Layer::User)],
    };
    let cfg = norte_tui::config::load(&layers).expect("load");
    assert_eq!(
        cfg.common
            .ui_columns
            .specs
            .get("size")
            .and_then(|sp| sp.format.as_deref()),
        Some("si"),
        "el fichero escrito re-carga con el spec"
    );
}

/// Esc descarta: ni los settings de la sesión ni el disco cambian —
/// `dialog.cancel` en `on_columns_key` tira el overlay SIN `finish`.
#[test]
fn picker_cancel_no_toca_nada() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app();
    let antes = builtins(&app);
    let sort_antes = app.focused().sort();
    app.open_columns_picker();
    {
        let p = app.columns_picker.as_mut().expect("picker abierto");
        p.down();
        p.toggle();
        p.sort_current();
    }
    app.columns_picker = None; // cancel: descarta sin finish/apply/persist
    assert_eq!(builtins(&app), antes, "cancel no toca el layout");
    assert_eq!(app.focused().sort(), sort_antes, "cancel no toca el sort");
    assert!(
        !dir.path().join("norte.toml").exists(),
        "cancel jamás escribe config"
    );
}
