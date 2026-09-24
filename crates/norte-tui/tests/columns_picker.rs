//! The columns picker (#108 7a): overlay behavior and persistence. Same
//! driving style as `theme_picker.rs` + `theme_persist.rs`: the logic
//! lives in `App`/`ColumnsPicker` (tested with no event loop) and disk
//! through the explicit-dir function (`config::persist_columns`) —
//! `apply_picked_columns` (main.rs) is private to the binary, so each test
//! reproduces both of its halves (session: `apply_picked` + re-sort; disk:
//! `persist_columns`), which is exactly the seam the theme tests
//! (`persist_ui_theme_to`) already use.

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

/// The IN-SESSION half of `apply_picked_columns` (main.rs): in-memory
/// settings (list+sort and cycled formats, #108 7b) + re-sort of both panes.
fn apply(app: &mut App, picked: &norte_frontend::columns_picker::Picked) {
    app.columns.apply_picked(
        picked.scheme_target.as_deref(),
        &picked.ids,
        picked.sort.clone(),
    );
    for (id, fmt) in &picked.formats {
        app.columns.apply_format(id, fmt);
    }
    for i in 0..2 {
        app.apply_scheme_sort(i);
    }
}

/// The DISK half of `apply_picked_columns` (main.rs): the same
/// `SortSpec` → `PersistSort` mapping the binary does.
fn persistir(dir: &std::path::Path, picked: &norte_frontend::columns_picker::Picked) {
    persist_columns(
        dir,
        picked.scheme_target.as_deref(),
        &picked.ids,
        // A per-attribute sort is not saved to the file (ADR 0144): `None`
        // leaves the `sort` key as it was.
        match picked.sort.column {
            SortColumn::Name => Some("name"),
            SortColumn::Size => Some("size"),
            SortColumn::Mtime => Some("mtime"),
            SortColumn::Extension => Some("extension"),
            SortColumn::Attr(_) => None,
        }
        .map(|column| PersistSort {
            column,
            descending: picked.sort.dir == SortDir::Desc,
            dirs_first: picked.sort.dirs_first,
        }),
    )
    .expect("persistence");
    // #108 7b: the cycled formats, like the binary — after the list.
    for (id, fmt) in &picked.formats {
        norte_tui::config::persist_column_format(dir, id, fmt).expect("format persistence");
    }
}

/// The effective layout's visible builtins, in order (#117: the layout
/// already speaks `ColumnId`; here there are only configured builtins).
fn builtins(app: &App) -> Vec<Builtin> {
    app.columns
        .layout_items_for("file")
        .iter()
        .filter_map(|(id, _)| match id {
            norte_frontend::columns::ColumnId::Builtin(b) => Some(*b),
            _ => None,
        })
        .collect()
}

/// Open → scroll down to mtime → sort → confirm: the pane ends up sorted by
/// mtime asc and the hermetic dir's `norte.toml` carries `[ui.columns]`
/// with `column = "mtime"`.
#[test]
fn picker_sorts_and_persists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app();
    app.open_columns_picker(&[]);
    {
        let p = app.columns_picker.as_mut().expect("picker open");
        p.down(); // size
        p.down(); // mtime
        p.sort_current();
        assert_eq!(p.sort().column, SortColumn::Mtime, "the row's sort");
        assert_eq!(p.sort().dir, SortDir::Asc, "first click = asc");
    }
    let picked = app.columns_picker.as_ref().expect("picker open").finish();
    app.columns_picker = None; // confirm closes the overlay
    apply(&mut app, &picked);
    let sort = app.focused().sort();
    assert_eq!(
        sort.column,
        SortColumn::Mtime,
        "the pane re-sorts in session"
    );
    assert_eq!(sort.dir, SortDir::Asc);
    persistir(dir.path(), &picked);
    let s = std::fs::read_to_string(dir.path().join("norte.toml")).expect("read");
    assert!(s.contains("[ui.columns]"), "persisted section: {s}");
    assert!(s.contains(r#"column = "mtime""#), "persisted sort: {s}");
}

/// Open → turn off size → confirm: the scheme's `layout_items_for` no
/// longer carries `Size` and the file persists `default` with no `"size"`.
#[test]
fn picker_toggle_persists_the_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app();
    app.open_columns_picker(&[]);
    {
        let p = app.columns_picker.as_mut().expect("picker open");
        p.down(); // size
        p.toggle(); // off
    }
    let picked = app.columns_picker.as_ref().expect("picker open").finish();
    app.columns_picker = None;
    assert_eq!(
        picked.ids,
        vec!["name".to_owned(), "mtime".to_owned()],
        "only the enabled ones travel (kind was already off)"
    );
    assert_eq!(picked.scheme_target, None, "no override → default");
    apply(&mut app, &picked);
    assert_eq!(
        builtins(&app),
        vec![Builtin::Name, Builtin::Mtime],
        "size out of the layout in session"
    );
    persistir(dir.path(), &picked);
    let s = std::fs::read_to_string(dir.path().join("norte.toml")).expect("read");
    assert!(s.contains("default = ["), "persisted list: {s}");
    assert!(s.contains(r#""mtime""#), "mtime in the list: {s}");
    assert!(!s.contains(r#""size""#), "size must not be persisted: {s}");
}

/// #108 7b: `f` over size cycles iec→si; confirming applies it IN SESSION
/// (`style_for` sees it instantly) and persists a `[[ui.columns.spec]]`
/// with `id = "size"`, `format = "si"` that the real `load` rereads.
#[test]
fn picker_cycles_format_and_persists_the_spec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app();
    app.open_columns_picker(&[]);
    {
        let p = app.columns_picker.as_mut().expect("picker open");
        p.down(); // size
        p.cycle_format(); // iec → si
        assert_eq!(p.format_of_cursor().as_deref(), Some("si"));
    }
    let picked = app.columns_picker.as_ref().expect("picker open").finish();
    app.columns_picker = None;
    assert_eq!(picked.formats, vec![("size".to_owned(), "si".to_owned())]);
    apply(&mut app, &picked);
    assert_eq!(
        app.columns.style_for("file", Builtin::Size).size_format,
        norte_frontend::columns::SizeFormat::Si,
        "the session sees the format instantly"
    );
    persistir(dir.path(), &picked);
    let s = std::fs::read_to_string(dir.path().join("norte.toml")).expect("read");
    assert!(s.contains(r#"id = "size""#), "persisted spec: {s}");
    assert!(s.contains(r#"format = "si""#), "persisted format: {s}");
    // Round-trip through the frontend's REAL loader (TUI layers).
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
        "the written file reloads with the spec"
    );
}

/// Esc discards: neither the session settings nor disk change —
/// `dialog.cancel` in `on_columns_key` drops the overlay with NO `finish`.
#[test]
fn picker_cancel_touches_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = app();
    let before = builtins(&app);
    let sort_before = app.focused().sort();
    app.open_columns_picker(&[]);
    {
        let p = app.columns_picker.as_mut().expect("picker open");
        p.down();
        p.toggle();
        p.sort_current();
    }
    app.columns_picker = None; // cancel: discards with no finish/apply/persist
    assert_eq!(builtins(&app), before, "cancel does not touch the layout");
    assert_eq!(
        app.focused().sort(),
        sort_before,
        "cancel does not touch the sort"
    );
    assert!(
        !dir.path().join("norte.toml").exists(),
        "cancel never writes config"
    );
}
