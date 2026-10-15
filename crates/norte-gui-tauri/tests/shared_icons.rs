//! One drawing, two frontends: every chrome icon the terminal rasterises
//! (`norte_frontend::panelbar::CHROME_ICONS`) is the file the window
//! imports, never a second hand-drawn copy in `icons.ts`.

#[test]
fn the_window_imports_every_shared_chrome_icon() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/src/render/icons.ts"),
    )
    .expect("icons.ts");
    for id in norte_frontend::panelbar::CHROME_ICONS {
        // Places' star, one file for both.
        if *id == "fs:favorite" {
            assert!(src.contains("\"fs:favorite\": places"));
            continue;
        }
        let file = format!("assets/ui-icons/{}.svg?raw", id.replace(':', "-"));
        assert!(src.contains(&file), "icons.ts does not import {file}");
        assert!(
            src.contains(&format!("\"{id}\": ")),
            "icons.ts does not map {id} to its file"
        );
    }
}
