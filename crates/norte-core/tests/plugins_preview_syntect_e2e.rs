//! E2E del previewer syntect (#29): compila el guest REAL
//! `norte-plugin-host/examples-wasm/previewer-syntect` a `wasm32-wasip2`, lo
//! siembra bajo consentimiento, lo resuelve para un mimetype de código y lo
//! EJECUTA — verificando que devuelve la sintaxis resaltada como ANSI de 24
//! bits (que el frontend sanea a color de pane, ver `norte-frontend::ansi`).
//!
//! SKIP si el target `wasm32-wasip2` no está instalado (igual que el E2E del
//! previewer-demo): la suite queda verde en toolchains sin ese target.

use std::path::PathBuf;
use std::process::Command;

use norte_core::PluginRegistry;
use norte_plugin_host::PluginRuntime;

/// Manifiesto del previewer syntect: declara `text/*` y `application/json`
/// (para que un JSON resuelva contra un syntax real) + `fs-read=scoped`.
const MANIFEST: &str = r#"
[plugin]
id = "org.norte.syntect"
name = "Syntect Highlighter"
publisher = "norte"
version = "0.1.0"
category = "previewer"

[contributions]
previewer = [{ mimetypes = ["text/*", "application/json"] }]

[capabilities]
fs-read = "scoped"
"#;

/// JSON de prueba: syntect tiene un syntax `JSON`, así que el resaltado es
/// determinista (claves/valores en colores distintos).
const SAMPLE: &[u8] = b"{\n  \"name\": \"norte\",\n  \"count\": 42\n}\n";

#[test]
fn plugin_preview_syntect_e2e_wasm_real() {
    let Some(wasm) = build_guest("previewer-syntect") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado; no hay .wasm que ejecutar");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.syntect");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");

    // Fail-closed: sin aprobar no se elige aunque el .wasm esté y el mime case.
    assert!(
        reg.resolve_previewer("application/json").is_none(),
        "un previewer no consentido jamás se elige"
    );

    assert!(reg.set_approval_in_memory("org.norte.syntect", true));
    assert!(reg.set_enabled_in_memory("org.norte.syntect", true));

    let (id, _name, resolved_wasm, caps, _settings) = reg
        .resolve_previewer("application/json")
        .expect("application/json casa el glob del previewer consentido");
    assert_eq!(id, "org.norte.syntect");

    // EJECUTA el WASM real: el core pasaría el TEXTO ya decodificado (§6.2); el
    // guest devuelve la sintaxis resaltada como ANSI de 24 bits.
    let render = rt
        .instantiate(&resolved_wasm, caps)
        .expect("instanciar el previewer")
        .render_preview("application/json", SAMPLE)
        .expect("el previewer-syntect debe renderizar");

    // Color REAL: al menos una secuencia SGR de 24 bits (`ESC[38;2;r;g;bm`).
    assert!(
        render.contains("\x1b[38;2;"),
        "el render lleva color ANSI de 24 bits (syntect): {render:?}"
    );
    // El contenido sobrevive: la clave del JSON aparece en el texto resaltado.
    assert!(
        render.contains("name"),
        "el render incluye el texto del contenido: {render:?}"
    );
    assert!(
        render.contains("42"),
        "el render incluye el valor numérico: {render:?}"
    );
}

/// Compila `examples-wasm/<name>/` a `wasm32-wasip2` (release). `None` (SKIP)
/// si el target no está instalado; si está pero no compila, es fallo real.
fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return None;
    }
    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("norte-plugin-host")
        .join("examples-wasm")
        .join(name);
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(&guest_dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("no se pudo lanzar cargo para compilar el guest");
    assert!(status.success(), "el guest {name} no compiló");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(wasm.exists(), "no se encontró {}", wasm.display());
    Some(wasm)
}

fn target_installed(target: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l == target)
        })
}
