//! Tests del runtime wasmtime que NO requieren un componente WASM real: que el
//! motor se construye y que un artefacto basura falla con un error claro.

use norte_plugin_host::{Capabilities, PluginRuntime, RuntimeError};

mod support;

#[test]
fn runtime_se_construye() {
    let _rt = PluginRuntime::new().expect("engine");
}

#[test]
fn cargar_un_no_componente_falla_claro() {
    let rt = PluginRuntime::new().expect("engine");
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("no.wasm");
    std::fs::write(&fake, b"esto no es un componente wasm").unwrap();
    let err = rt
        .instantiate(&fake, Capabilities::default())
        .expect_err("bytes basura");
    assert!(matches!(err, RuntimeError::Component(_)), "fue {err:?}");
}

#[test]
fn artefacto_demasiado_grande_se_rechaza_antes_de_compilar() {
    // Un `.wasm` que supera el tope (issue #68) se rechaza sin llegar a
    // `Component::from_file`. Se crea un fichero DISPERSO (`set_len`) para no
    // escribir de verdad decenas de MiB: `metadata().len()` devuelve el tamaño
    // lógico, que es lo que mira el cap.
    let rt = PluginRuntime::new().expect("engine");
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("gigante.wasm");
    let f = std::fs::File::create(&fake).unwrap();
    // 64 MiB + 1: justo por encima de MAX_ARTIFACT_BYTES.
    f.set_len(64 * 1024 * 1024 + 1).unwrap();
    drop(f);
    let err = rt
        .instantiate(&fake, Capabilities::default())
        .expect_err("artefacto sobredimensionado");
    assert!(
        matches!(err, RuntimeError::ArtifactTooLarge { .. }),
        "fue {err:?}"
    );
}

#[test]
fn previewer_demo_renderiza_y_loguea() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");
    let mut inst = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instancia");
    let out = inst
        .render_preview(
            "text/plain",
            b"linea uno\nlinea dos\nlinea tres\nlinea cuatro",
        )
        .expect("render");
    assert!(out.contains("text/plain"), "cabecera: {out}");
    assert!(out.contains("linea uno") && out.contains("linea tres"));
    assert!(!out.contains("linea cuatro"), "solo 3 líneas");
    assert!(
        inst.logs().iter().any(|l| l.contains("previewer-demo")),
        "host-log: {:?}",
        inst.logs()
    );
}

#[test]
fn command_demo_ejecuta_y_reporta_error_de_comando() {
    let Some(wasm) = support::build_guest("command-demo") else {
        return;
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");
    let mut inst = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instancia");
    assert_eq!(inst.run_command("echo", "hola").expect("echo"), "hola");
    assert_eq!(inst.run_command("shout", "hola").expect("shout"), "HOLA");
    let err = inst.run_command("nope", "").expect_err("desconocido");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Guest(ref m) if m.contains("desconocido")),
        "fue {err:?}"
    );
}

#[test]
fn guest_en_bucle_trapea_por_deadline_no_cuelga_el_host() {
    let Some(wasm) = support::build_guest("command-demo") else {
        return;
    };
    // Deadline corto SOLO para el test (~1 s: 20 ticks × 50 ms) para no esperar
    // los ~10 s del default de producción. El ticker corta el bucle → trap.
    let rt = norte_plugin_host::PluginRuntime::with_epoch_deadline(20).expect("engine");
    let mut inst = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instancia");
    let err = inst
        .run_command("spin", "")
        .expect_err("un guest en bucle debe trapear, no colgar");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Trap(_)),
        "fue {err:?}"
    );
}

#[test]
fn fs_read_scoped_gatea_la_puerta_en_el_host() {
    let Some(wasm) = support::build_guest("command-demo") else {
        return;
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");
    // SIN fs-read: la puerta se cierra en el host aunque el guest la llame.
    let mut sin = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instancia");
    sin.preload_scoped("demo", b"secreto".to_vec());
    let err = sin.run_command("read", "").expect_err("sin capability");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Guest(ref m) if m.contains("fs-read")),
        "fue {err:?}"
    );
    // CON fs-read=scoped: la puerta se abre y devuelve el recurso sembrado.
    let mut con = rt
        .instantiate(
            &wasm,
            norte_plugin_host::Capabilities::scoped_read_for_test(),
        )
        .expect("instancia");
    con.preload_scoped("demo", b"contenido".to_vec());
    assert_eq!(con.run_command("read", "").expect("read"), "contenido");
}

#[test]
fn host_config_entrega_settings_al_guest_real() {
    // P2 Task 3: `set_settings` + el comando `config` del guest real
    // (`host_config::get` bajo el capó) — end-to-end sin pasar por el
    // catálogo/registro (eso lo cubre `plugins_config_e2e.rs` en norte-core).
    let Some(wasm) = support::build_guest("command-demo") else {
        return;
    };
    let rt = norte_plugin_host::PluginRuntime::new().expect("engine");

    // Sin `set_settings`: el mapa por defecto está vacío, `get` no encuentra
    // nada (mismo comportamiento que un plugin sin `[config]`).
    let mut sin = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instancia");
    let err = sin
        .run_command("config", "greeting")
        .expect_err("sin set_settings no hay nada que leer");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Guest(ref m) if m.contains("greeting")),
        "fue {err:?}"
    );

    // Con `set_settings`: el guest lee el valor instalado tal cual.
    let mut con = rt
        .instantiate(&wasm, norte_plugin_host::Capabilities::default())
        .expect("instancia");
    con.set_settings(std::collections::BTreeMap::from([(
        "greeting".to_string(),
        "hola mundo".to_string(),
    )]));
    assert_eq!(
        con.run_command("config", "greeting").expect("config"),
        "hola mundo"
    );
    // Una clave NO instalada sigue sin encontrarse, aunque el mapa no esté
    // vacío (no es "todo o nada": es por-clave).
    let err = con
        .run_command("config", "no-declarada")
        .expect_err("clave ausente del mapa instalado");
    assert!(
        matches!(err, norte_plugin_host::RuntimeError::Guest(ref m) if m.contains("no-declarada")),
        "fue {err:?}"
    );
}
