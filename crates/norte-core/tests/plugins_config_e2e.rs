//! E2E de entrega de `[config]` (P2 Task 3): manifiesto con `[config]` +
//! `config.toml` (override o ausente) → el guest COMMAND **real** (compilado
//! desde `norte-plugin-host/examples-wasm/command-demo`) lee el valor
//! resuelto vía `host-config::get` y lo hace eco en su salida. Cierra la
//! cadena completa: `resolve_settings` (Task 2, catálogo) →
//! `PluginRegistry::run_command` (Task 3, este fichero) → `host-config`
//! (WIT, runtime) → el guest.
//!
//! Mismo criterio SKIP que `plugins_run_e2e.rs`: sin el target
//! `wasm32-wasip2` no hay artefacto que ejecutar, y el resto de la suite
//! queda verde.

use std::path::PathBuf;
use std::process::Command;

use norte_core::PluginRegistry;
use norte_plugin_host::PluginRuntime;

/// Manifiesto `command` con UN `[config]` de tipo `string` — igual de mínimo
/// que `CMD_MANIFEST` de `plugins_run_e2e.rs`, pero con un esquema de config
/// para ejercitar la entrega end-to-end.
const CONFIG_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cfgdemo"
name = "Config Demo"
publisher = "norte"
version = "0.1.0"
category = "command"

[config.greeting]
type = "string"
default = "hola"
"#;

/// Sin `config.toml`: el guest lee el DEFAULT declarado en el esquema.
#[test]
fn plugin_config_e2e_wasm_real_sin_fichero_usa_el_default() {
    let Some(wasm) = build_guest("command-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado; no hay .wasm que ejecutar");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.cfgdemo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");
    // Deliberadamente SIN escribir config.toml.

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(
        reg.set_approval("org.norte.cfgdemo", true)
            .expect("set_approval")
    );
    assert!(
        reg.set_enabled("org.norte.cfgdemo", true)
            .expect("set_enabled")
    );

    let out = reg
        .run_command(&rt, "org.norte.cfgdemo", "config", "greeting")
        .expect("el guest debe leer el setting vía host-config");
    assert_eq!(
        out, "hola",
        "sin config.toml, el guest ve el default del esquema"
    );
}

/// Con `config.toml`: el guest lee el OVERRIDE validado, no el default.
#[test]
fn plugin_config_e2e_wasm_real_con_override_lo_refleja() {
    let Some(wasm) = build_guest("command-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado; no hay .wasm que ejecutar");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.cfgdemo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");
    std::fs::write(
        plugin_dir.join("config.toml"),
        "greeting = \"hola mundo\"\n",
    )
    .expect("write config.toml");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(
        reg.set_approval("org.norte.cfgdemo", true)
            .expect("set_approval")
    );
    assert!(
        reg.set_enabled("org.norte.cfgdemo", true)
            .expect("set_enabled")
    );

    let out = reg
        .run_command(&rt, "org.norte.cfgdemo", "config", "greeting")
        .expect("el guest debe leer el override vía host-config");
    assert_eq!(
        out, "hola mundo",
        "con config.toml, el guest ve el valor validado, no el default"
    );
}

/// G3c (`plugin.set_config` wire): `PluginRegistry::set_config` persiste UN
/// valor nuevo y RE-RESUELVE `settings` en memoria — el siguiente
/// `run_command` (sin volver a `discover`) debe ver el valor recién
/// escrito, no el default ni el `config.toml` original. Cierra la cadena
/// completa que `plugin.get_config`/`plugin.set_config` exponen por el
/// wire: `set_config` (validar contra el esquema → persistir →
/// re-`resolve_settings`) → `run_command` → `host-config` → el guest REAL.
#[test]
fn plugin_config_e2e_wasm_real_set_config_actualiza_lo_que_ve_el_guest() {
    let Some(wasm) = build_guest("command-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado; no hay .wasm que ejecutar");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.cfgdemo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");
    // Sin config.toml: arranca en el default ("hola").

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(
        reg.set_approval("org.norte.cfgdemo", true)
            .expect("set_approval")
    );
    assert!(
        reg.set_enabled("org.norte.cfgdemo", true)
            .expect("set_enabled")
    );

    // Antes de `set_config`: el guest sigue viendo el default.
    let before = reg
        .run_command(&rt, "org.norte.cfgdemo", "config", "greeting")
        .expect("guest lee el default");
    assert_eq!(before, "hola");

    // `plugin.set_config` (vía el método del registro, mismo camino que el
    // handler del daemon): valida contra el esquema, persiste, re-resuelve.
    reg.set_config("org.norte.cfgdemo", "greeting", "hola G3c")
        .expect("set_config con un string válido debe aplicar");

    // Tras `set_config`, SIN volver a `discover`: el guest ve el valor
    // nuevo — prueba que `set_config` re-resolvió `settings` EN MEMORIA,
    // no solo en disco.
    let after = reg
        .run_command(&rt, "org.norte.cfgdemo", "config", "greeting")
        .expect("guest lee el valor recién fijado");
    assert_eq!(after, "hola G3c");

    // Persistencia real: un `discover` FRESCO (simula un reinicio del
    // daemon/proceso embebido) también ve el valor nuevo desde
    // `config.toml`.
    let reg2 = PluginRegistry::discover(cfg.path()).expect("discover tras set_config");
    assert_eq!(
        reg2.config_keys("org.norte.cfgdemo")
            .expect("plugin conocido")
            .iter()
            .find(|(k, _, _)| k == "greeting")
            .map(|(_, _, v)| v.as_str()),
        Some("hola G3c"),
    );
}

/// G3c: un valor que NO valida contra el esquema (aquí, una clave
/// desconocida) se rechaza SIN persistir — `config.toml` sigue sin existir
/// tras el intento fallido, y el guest sigue viendo el default.
#[test]
fn plugin_config_e2e_wasm_real_set_config_invalido_no_persiste() {
    let Some(wasm) = build_guest("command-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado; no hay .wasm que ejecutar");
        return;
    };

    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.cfgdemo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
    assert!(
        reg.set_approval("org.norte.cfgdemo", true)
            .expect("set_approval")
    );
    assert!(
        reg.set_enabled("org.norte.cfgdemo", true)
            .expect("set_enabled")
    );

    let err = reg
        .set_config("org.norte.cfgdemo", "no-such-key", "x")
        .expect_err("clave desconocida debe rechazarse");
    assert!(matches!(
        err,
        norte_core::plugins::PluginConfigSetError::UnknownKey(_)
    ));
    assert!(
        !plugin_dir.join("config.toml").exists(),
        "un set_config rechazado no debe crear config.toml"
    );

    let out = reg
        .run_command(&rt, "org.norte.cfgdemo", "config", "greeting")
        .expect("guest sigue viendo el default tras el rechazo");
    assert_eq!(out, "hola");
}

/// Compila el guest `examples-wasm/<name>/` de `norte-plugin-host` a
/// `wasm32-wasip2` (release) y devuelve la ruta del `.wasm`.
///
/// Réplica del helper de `norte-plugin-host/tests/support/mod.rs` (no accesible
/// desde el árbol de tests de `norte-core`) y de `plugins_run_e2e.rs` (mismo
/// crate, mismo criterio SKIP) — mismo texto a propósito, no una tercera
/// variante.
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
