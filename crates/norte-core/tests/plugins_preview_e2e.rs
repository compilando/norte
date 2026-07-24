//! E2E del previewer de plugins (M4-P5, cierre): la cadena completa
//! descubrir → (denegar sin aprobar) → aprobar → activar → resolver → ejecutar,
//! contra un componente WASM **real** compilado desde
//! `norte-plugin-host/examples-wasm/previewer-demo` (M4-P2) y ejecutado
//! sandboxeado por el runtime.
//!
//! Es el cierre de M4-P5: `PluginRegistry::resolve_previewer` elige, fail-closed,
//! el previewer consentido para un mimetype, y el runtime lo EJECUTA de verdad
//! devolviendo el render (cabecera + primeras 3 líneas del contenido).
//!
//! Si el target `wasm32-wasip2` no está instalado el test hace SKIP (no hay
//! artefacto que ejecutar): pasa en toolchains sin ese target y el resto de la
//! suite queda verde. Con el target presente ejecuta el `.wasm` de verdad.

use std::path::PathBuf;
use std::process::Command;

use norte_core::PluginRegistry;
use norte_plugin_host::PluginRuntime;

/// Manifiesto `previewer` del plugin sembrado: declara `text/*` como su glob de
/// mimetypes y `fs-read=scoped` (el render no toca el FS, pero fija que las
/// capabilities del manifiesto viajan al runtime). El id lleva puntos
/// (reverse-DNS).
const PREV_MANIFEST: &str = r#"
[plugin]
id = "org.norte.prev"
name = "Preview Demo"
publisher = "norte"
version = "0.1.0"
category = "previewer"

[contributions]
previewer = [{ mimetypes = ["text/*"] }]

[capabilities]
fs-read = "scoped"
"#;

/// Contenido de prueba: cuatro líneas — el previewer-demo solo toma las 3
/// primeras, así que "linea cuatro" NO debe aparecer en el render.
const SAMPLE: &[u8] = b"linea uno\nlinea dos\nlinea tres\nlinea cuatro";

/// Igual que [`PREV_MANIFEST`] pero con `[config.banner]` (P2 Task 4a): para
/// probar que `resolve_previewer` + `set_settings` entregan `[config]` al
/// previewer, no solo al `command` (Task 3).
const PREV_MANIFEST_WITH_CONFIG: &str = r#"
[plugin]
id = "org.norte.prev-cfg"
name = "Preview Demo Config"
publisher = "norte"
version = "0.1.0"
category = "previewer"

[contributions]
previewer = [{ mimetypes = ["text/*"] }]

[capabilities]
fs-read = "scoped"

[config.banner]
type = "string"
default = "Default Banner"
"#;

/// La cadena de cierre M4-P5 con un componente WASM REAL.
#[test]
fn plugin_preview_e2e_wasm_real() {
    let Some(wasm) = build_guest("previewer-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado; no hay .wasm que ejecutar");
        return;
    };

    // config_dir/plugins/org.norte.prev/{plugin.toml, plugin.wasm}
    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.prev");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), PREV_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");

    // 1) SIN aprobar: fail-closed. El .wasm ESTÁ presente y el mimetype casa,
    //    pero el consentimiento manda: no se elige previewer alguno.
    assert!(
        reg.resolve_previewer("text/plain").is_none(),
        "un previewer no consentido jamás se elige, ni con .wasm presente"
    );

    // 2) Aprobar + activar (in-memory: uso embebido en el test).
    assert!(
        reg.set_approval_in_memory("org.norte.prev", true),
        "el plugin existe: la aprobación se aplica"
    );
    assert!(
        reg.set_enabled_in_memory("org.norte.prev", true),
        "el plugin existe: la activación se aplica"
    );

    // 3) Ahora sí resuelve para el mimetype que casa el glob `text/*`.
    let (id, name, resolved_wasm, caps, _settings) = reg
        .resolve_previewer("text/plain")
        .expect("text/plain casa text/* con el previewer consentido");
    assert_eq!(id, "org.norte.prev", "id del previewer resuelto");
    assert_eq!(name, "Preview Demo", "name del previewer resuelto");
    assert!(
        resolved_wasm.ends_with("plugin.wasm"),
        "el binario resuelto es <dir>/plugin.wasm"
    );

    // 4) Un mimetype que el previewer NO declara → None (solo declara text/*).
    assert!(
        reg.resolve_previewer("application/json").is_none(),
        "application/json no casa text/*: no hay previewer para él"
    );

    // 5) EJECUTA el componente WASM real: el core leería los bytes acotados y los
    //    pasa al guest (regla 9: el plugin no toca el FS a pelo).
    let render = rt
        .instantiate(&resolved_wasm, caps)
        .expect("instanciar el previewer")
        .render_preview("text/plain", SAMPLE)
        .expect("el previewer-demo debe renderizar el contenido");

    assert!(
        render.contains("[text/plain]"),
        "el render lleva la cabecera con el mimetype: {render:?}"
    );
    assert!(
        render.contains("linea uno"),
        "el render incluye la 1.ª línea: {render:?}"
    );
    assert!(
        render.contains("linea tres"),
        "el render incluye la 3.ª línea: {render:?}"
    );
    assert!(
        !render.contains("linea cuatro"),
        "el previewer-demo solo toma 3 líneas: la 4.ª no aparece: {render:?}"
    );
}

/// P2 Task 4a: el previewer recibe `[config]` YA resuelto vía `host-config`,
/// igual que `command` (Task 3) — este test es el análogo de
/// `plugins_config_e2e.rs` pero para la ruta `resolve_previewer` +
/// `set_settings` + `render_preview`. Sin `config.toml`, el guest ve el
/// DEFAULT del esquema.
#[test]
fn plugin_preview_e2e_wasm_real_config_banner_default() {
    let Some(wasm) = build_guest("previewer-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado; no hay .wasm que ejecutar");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.prev-cfg");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), PREV_MANIFEST_WITH_CONFIG)
        .expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");
    // Deliberadamente SIN config.toml.

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(reg.set_approval_in_memory("org.norte.prev-cfg", true));
    assert!(reg.set_enabled_in_memory("org.norte.prev-cfg", true));

    let (_id, _name, resolved_wasm, caps, settings) = reg
        .resolve_previewer("text/plain")
        .expect("text/plain casa text/*");
    let mut inst = rt.instantiate(&resolved_wasm, caps).expect("instanciar");
    inst.set_settings(settings);
    let render = inst
        .render_preview("text/plain", SAMPLE)
        .expect("render con settings");
    assert!(
        render.starts_with("Default Banner\n"),
        "sin config.toml, el guest ve el default del esquema: {render:?}"
    );
}

/// Como el anterior, pero CON `config.toml` — el guest debe ver el OVERRIDE
/// validado, no el default.
#[test]
fn plugin_preview_e2e_wasm_real_config_banner_override() {
    let Some(wasm) = build_guest("previewer-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado; no hay .wasm que ejecutar");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.prev-cfg");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), PREV_MANIFEST_WITH_CONFIG)
        .expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");
    std::fs::write(
        plugin_dir.join("config.toml"),
        "banner = \"Hola desde config\"\n",
    )
    .expect("write config.toml");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(reg.set_approval_in_memory("org.norte.prev-cfg", true));
    assert!(reg.set_enabled_in_memory("org.norte.prev-cfg", true));

    let (_id, _name, resolved_wasm, caps, settings) = reg
        .resolve_previewer("text/plain")
        .expect("text/plain casa text/*");
    let mut inst = rt.instantiate(&resolved_wasm, caps).expect("instanciar");
    inst.set_settings(settings);
    let render = inst
        .render_preview("text/plain", SAMPLE)
        .expect("render con settings");
    assert!(
        render.starts_with("Hola desde config\n"),
        "con config.toml, el guest ve el override validado: {render:?}"
    );
}

/// Compila el guest `examples-wasm/<name>/` de `norte-plugin-host` a
/// `wasm32-wasip2` (release) y devuelve la ruta del `.wasm`.
///
/// Réplica del helper de `plugins_run_e2e.rs` / `norte-plugin-host/tests/support`
/// (no accesible entre árboles de tests). Devuelve `None` (SKIP) si el target
/// `wasm32-wasip2` no está instalado; si el target está pero el guest no compila,
/// es un fallo real y aborta.
fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return None;
    }

    // `CARGO_MANIFEST_DIR` = .../crates/norte-core; el guest vive en el crate
    // hermano norte-plugin-host.
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
    assert!(
        status.success(),
        "el guest {name} no compiló (target wasm32-wasip2 presente)"
    );

    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(
        wasm.exists(),
        "no se encontró el artefacto {}",
        wasm.display()
    );
    Some(wasm)
}

/// `true` si `rustup` reporta `target` entre los instalados.
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
