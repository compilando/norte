//! E2E de ejecución de plugins (M4-P4, cierre): la cadena completa
//! descubrir → (denegar sin aprobar) → aprobar → activar → ejecutar, contra un
//! componente WASM **real** compilado desde
//! `norte-plugin-host/examples-wasm/command-demo` y ejecutado sandboxeado por el
//! runtime de M4-P2.
//!
//! Si el target `wasm32-wasip2` no está instalado el test hace SKIP (no hay
//! artefacto que ejecutar): pasa en toolchains sin ese target y el resto de la
//! suite queda verde. Con el target presente ejecuta el `.wasm` de verdad y
//! comprueba la salida (`echo`/`shout`) y el error del guest.

use std::path::PathBuf;
use std::process::Command;

use norte_core::{PluginRegistry, PluginRunError};
use norte_plugin_host::PluginRuntime;

/// Manifiesto `command` mínimo del plugin sembrado — SIN capabilities
/// especiales (echo/shout no tocan el FS). El id lleva puntos (reverse-DNS).
const CMD_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cmd"
name = "Command Demo"
publisher = "norte"
version = "0.1.0"
category = "command"
"#;

/// La cadena de cierre M4-P4 con un componente WASM REAL.
#[test]
fn plugin_run_command_e2e_wasm_real() {
    let Some(wasm) = build_guest("command-demo") else {
        eprintln!("SKIP: target wasm32-wasip2 no instalado; no hay .wasm que ejecutar");
        return;
    };

    // config_dir/plugins/org.norte.cmd/{plugin.toml, plugin.wasm}
    let cfg = tempfile::tempdir().expect("tempdir");
    let plugin_dir = cfg.path().join("plugins").join("org.norte.cmd");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
    std::fs::write(plugin_dir.join("plugin.toml"), CMD_MANIFEST).expect("write manifest");
    std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");

    let rt = PluginRuntime::new().expect("PluginRuntime::new");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");

    // 1) SIN aprobar: fail-closed. El .wasm ESTÁ presente, pero el consentimiento
    //    manda: no se ejecuta nada.
    let err = reg
        .run_command(&rt, "org.norte.cmd", "echo", "hola")
        .expect_err("un plugin sin aprobar jamás se ejecuta, ni con .wasm presente");
    assert!(
        matches!(err, PluginRunError::NotApproved(ref id) if id == "org.norte.cmd"),
        "sin aprobar = NotApproved: {err:?}"
    );

    // 2) Aprobar + activar (persiste en plugins-state.toml, uso embebido).
    assert!(
        reg.set_approval("org.norte.cmd", true)
            .expect("set_approval"),
        "el plugin existe: la aprobación se aplica"
    );
    assert!(
        reg.set_enabled("org.norte.cmd", true).expect("set_enabled"),
        "el plugin existe: la activación se aplica"
    );

    // 3) Ejecutar de verdad el componente WASM sandboxeado.
    let out = reg
        .run_command(&rt, "org.norte.cmd", "echo", "hola")
        .expect("echo debe ejecutar el guest real y devolver el arg");
    assert_eq!(out, "hola", "el guest `echo` devuelve el arg tal cual");

    let out = reg
        .run_command(&rt, "org.norte.cmd", "shout", "hola")
        .expect("shout debe ejecutar el guest real");
    assert_eq!(
        out, "HOLA",
        "el guest `shout` devuelve el arg en mayúsculas"
    );

    // 4) Un comando que el guest no conoce → Err del guest → Runtime(Guest).
    let err = reg
        .run_command(&rt, "org.norte.cmd", "desconocido", "")
        .expect_err("un comando desconocido devuelve Err desde el guest");
    assert!(
        matches!(err, PluginRunError::Runtime(_)),
        "el Err de lógica del guest se mapea a Runtime: {err:?}"
    );
}

/// Compila el guest `examples-wasm/<name>/` de `norte-plugin-host` a
/// `wasm32-wasip2` (release) y devuelve la ruta del `.wasm`.
///
/// Réplica del helper de `norte-plugin-host/tests/support/mod.rs` (no accesible
/// desde el árbol de tests de `norte-core`). Devuelve `None` (SKIP) si el target
/// `wasm32-wasip2` no está instalado; si el target está pero el guest no
/// compila, es un fallo real y aborta.
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
