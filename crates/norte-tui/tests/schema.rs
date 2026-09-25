//! Golden of the published JSON Schemas (ADR 0007, spec §13): generated
//! from the SAME serde structs that parse them — if the code changes the
//! format, this test forces republishing the schema.

use std::path::Path;

#[test]
fn published_schemas_do_not_diverge() {
    let cases = [
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
    for (name, schema) in cases {
        let json = format!("{}\n", serde_json::to_string_pretty(&schema).unwrap());
        let path = base.join(name);
        if std::env::var_os("NORTE_UPDATE_SCHEMA").is_some() {
            std::fs::write(&path, &json).expect("write schema");
            continue;
        }
        let published = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| {
                panic!("missing docs/schema/{name}: regenerate with NORTE_UPDATE_SCHEMA=1")
            })
            // Belt in addition to .gitattributes: a checkout with CRLF must
            // not break the comparison (caught in Windows CI).
            .replace("\r\n", "\n");
        assert_eq!(
            published, json,
            "docs/schema/{name} diverged from the code: regenerate with \
             NORTE_UPDATE_SCHEMA=1 cargo nextest run -p norte-tui schemas"
        );
    }
}
