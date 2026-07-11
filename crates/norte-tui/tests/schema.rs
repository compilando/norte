//! Golden de los JSON Schema publicados (ADR 0007, spec §13): se generan
//! desde los MISMOS structs serde que parsean — si el código cambia el
//! formato, este test obliga a republicar el schema.

use std::path::Path;

#[test]
fn los_schemas_publicados_no_divergen() {
    let casos = [
        (
            "norte.schema.json",
            serde_json::to_value(schemars::schema_for!(norte_tui::config::NorteToml)).unwrap(),
        ),
        (
            "keymap.schema.json",
            serde_json::to_value(schemars::schema_for!(norte_tui::keymap::KeymapFile)).unwrap(),
        ),
    ];
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schema");
    for (name, schema) in casos {
        let json = format!("{}\n", serde_json::to_string_pretty(&schema).unwrap());
        let path = base.join(name);
        if std::env::var_os("NORTE_UPDATE_SCHEMA").is_some() {
            std::fs::write(&path, &json).expect("escribir schema");
            continue;
        }
        let publicado = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| {
                panic!("falta docs/schema/{name}: regenera con NORTE_UPDATE_SCHEMA=1")
            })
            // Cinturón además del .gitattributes: un checkout con CRLF no
            // debe romper la comparación (cazado en CI de Windows).
            .replace(
                "
", "
",
            );
        assert_eq!(
            publicado, json,
            "docs/schema/{name} divergió del código: regenera con \
             NORTE_UPDATE_SCHEMA=1 cargo nextest run -p norte-tui schemas"
        );
    }
}
