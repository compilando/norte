//! Las columnas que el usuario configuró son las que el host recibe.
//!
//! El spike las enseñó vacías —solo pintaba `kind`— y el backend de tabla no
//! lo veía. Esto reproduce el camino de verdad: un `norte.toml` en disco, la
//! misma resolución que usa el TUI, y la lista tal como se le pasa al host.

use norte_config::{Layer, Layers};

fn ajustes(toml: &str) -> norte_frontend::columns::ColumnsSettings {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("norte.toml"), toml).expect("escribe config");
    let capas = Layers {
        dirs: vec![(dir.path().to_path_buf(), Layer::User)],
    };
    let cfg = norte_frontend::config::load(&capas).expect("config válida");
    norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns)
}

/// Las cuatro columnas configuradas llegan, en orden y con `name` incluido
/// (el host es quien lo filtra, porque el nombre lo pinta aparte).
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

/// Sin `[ui.columns]`, las de siempre.
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
