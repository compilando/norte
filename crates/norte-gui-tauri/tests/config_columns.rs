//! The columns the user configured are the ones the host receives.
//!
//! The spike showed them empty — it only painted `kind` — and the table
//! backend did not see it. This reproduces the real path: a `norte.toml` on
//! disk, the same resolution the TUI uses, and the list exactly as it is
//! handed to the host.

use norte_config::{Layer, Layers};

fn ajustes(toml: &str) -> norte_frontend::columns::ColumnsSettings {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("norte.toml"), toml).expect("writes config");
    let capas = Layers {
        dirs: vec![(dir.path().to_path_buf(), Layer::User)],
    };
    let cfg = norte_frontend::config::load(&capas).expect("valid config");
    norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns)
}

/// The four configured columns arrive, in order and with `name` included
/// (the host is the one that filters it out, because the name is painted
/// separately).
#[test]
fn las_columnas_configuradas_llegan_en_orden() {
    let st = ajustes(
        r#"
[ui.columns]
default = ["name", "size", "mtime", "kind"]
"#,
    );
    let ids: Vec<String> = st
        .layout_items_for("file")
        .into_iter()
        .map(|(id, _)| id.to_string())
        .collect();
    assert_eq!(ids, vec!["name", "size", "mtime", "kind"]);
}

/// Without `[ui.columns]`, the usual ones.
#[test]
fn sin_configuracion_quedan_las_de_fabrica() {
    let st = ajustes("[ui]\nlang = \"es\"\n");
    let ids: Vec<String> = st
        .layout_items_for("file")
        .into_iter()
        .map(|(id, _)| id.to_string())
        .collect();
    assert!(ids.contains(&"name".to_owned()), "{ids:?}");
    assert!(ids.contains(&"size".to_owned()), "{ids:?}");
}
