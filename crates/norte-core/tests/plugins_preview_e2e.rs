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

// ---------------------------------------------------------------------
// G3a (ADR 0037): `plugin.preview_styled` de punta a punta CON WASM real,
// a través de `Backend` (no del `PluginRuntime` a pelo como arriba). Solo
// unix: el daemon UDS es `#[cfg(unix)]` (ADR 0011), igual que
// `tests/backend_remote.rs`, del que esta sección toma el arnés
// (`RemoteBackend::connect` + `DaemonConfig::plugins_dir`).
//
// NO se ejercita `Backend::Embedded` aquí a propósito: su brazo de plugins
// resuelve el directorio SIEMPRE vía `norte_core::connect::config_dir()`
// (global del proceso, sin parámetro de override) — cambiarlo desde un test
// exigiría `std::env::set_var` (`unsafe` en edition 2024, regla 5 del
// proyecto: PROHIBIDO fuera de `norte-vfs-local`). `Backend::Remote` ejerce
// la MISMA superficie pública (`Backend::plugin_preview_styled`) contra el
// handler REAL del daemon (`daemon::server::handle_plugin_preview_styled`,
// cableado en esta misma task) sin ese problema — el `plugins_dir` del
// daemon SÍ es parametrizable por test (`DaemonConfig`), como ya prueba
// `spawn_daemon_plugins_ok_y_roto` en `tests/daemon.rs`.
#[cfg(unix)]
mod styled {
    use std::sync::Arc;

    use bytes::Bytes;
    use norte_core::Engine;
    use norte_core::backend::Backend;
    use norte_core::backend::remote::RemoteBackend;
    use norte_core::daemon::{Daemon, DaemonConfig};
    use norte_proto::VPath;
    use norte_proto::methods::ClientInfo;
    use norte_testkit::MemProvider;
    use norte_vfs::Provider;

    use super::{PREV_MANIFEST, SAMPLE, build_guest};

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire válido de test")
    }

    async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
        let mut sink = mem.write(&vp(wire)).await.expect("write abre");
        sink.write(Bytes::copy_from_slice(content))
            .await
            .expect("chunk entra");
        sink.commit().await.expect("commit publica");
    }

    /// La cadena de cierre G3a con un componente WASM REAL, de punta a
    /// punta A TRAVÉS DE `Backend::Remote` (daemon UDS real): descubrir →
    /// aprobar → activar (por el WIRE, `plugin.set_approval`/
    /// `plugin.set_enabled` — no `_in_memory`, a diferencia del test
    /// síncrono de arriba) → `Backend::plugin_preview_styled` → roles/fg
    /// REALES del mini-highlighter de `previewer-demo` (ver su rustdoc:
    /// dígitos → `role: "number"`, `TODO`/`FIXME`/`norte` → `role:
    /// "keyword"` + `fg` fijo).
    #[tokio::test]
    #[allow(clippy::too_many_lines)] // e2e de punta a punta: setup+wire+assert, sin trocear
    async fn plugin_preview_styled_e2e_wasm_real_a_traves_del_backend() {
        let Some(wasm) = build_guest("previewer-demo") else {
            eprintln!("SKIP: target wasm32-wasip2 no instalado; no hay .wasm que ejecutar");
            return;
        };

        let cfg = tempfile::tempdir().expect("tempdir cfg");
        let plugin_dir = cfg.path().join("plugins").join("org.norte.prev");
        std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin dir");
        std::fs::write(plugin_dir.join("plugin.toml"), PREV_MANIFEST).expect("write manifest");
        std::fs::copy(&wasm, plugin_dir.join("plugin.wasm")).expect("copy .wasm");

        let dir = tempfile::tempdir().expect("tempdir daemon");
        let socket = dir.path().join("d.sock");
        let engine = Arc::new(Engine::new());
        let mem = Arc::new(MemProvider::new());
        engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
        let daemon = Daemon::bind(
            engine,
            DaemonConfig {
                socket_path: Some(socket.clone()),
                idle_timeout: None,
                listing_ttl: std::time::Duration::from_mins(2),
                plugins_dir: Some(cfg.path().to_path_buf()),
                state_dir: None,
            },
        )
        .await
        .expect("bind");
        let _run = tokio::spawn(daemon.run());

        write_file(&mem, "mem:///doc.txt", SAMPLE).await;

        let remote = RemoteBackend::connect(
            socket,
            None,
            ClientInfo {
                name: "plugins-preview-styled-e2e".into(),
                version: "0.0.0".into(),
            },
        )
        .await
        .expect("connect");
        let backend = Backend::Remote(remote.clone());

        // SIN aprobar todavía: el consentimiento manda, `plugin.preview`
        // clásico ya lo prueba (arriba, in-memory); aquí basta confirmar que
        // el WIRE respeta el mismo fail-closed antes de aprobar.
        let none_yet = backend
            .plugin_preview_styled(&vp("mem:///doc.txt"))
            .await
            .expect("plugin.preview_styled no es error sin aprobar");
        assert!(
            none_yet.is_none(),
            "sin aprobar, ningún previewer consentido casa: None"
        );

        backend
            .plugins_set_approval("org.norte.prev", true)
            .await
            .expect("aprobar por el wire");
        backend
            .plugins_set_enabled("org.norte.prev", true)
            .await
            .expect("activar por el wire");

        let preview = backend
            .plugin_preview_styled(&vp("mem:///doc.txt"))
            .await
            .expect("plugin.preview_styled no es error")
            .expect("aprobado+activado: el previewer aplica");
        assert_eq!(preview.plugin_id, "org.norte.prev");
        assert_eq!(preview.plugin_name, "Preview Demo");

        // SAMPLE = "linea uno\nlinea dos\nlinea tres\nlinea cuatro": la
        // cabecera (1 línea plana) + 3 líneas de contenido resaltado.
        assert_eq!(
            preview.lines.len(),
            4,
            "cabecera + 3 líneas: {:?}",
            preview.lines
        );
        let header_text: String = preview.lines[0].iter().map(|s| s.text.as_str()).collect();
        assert!(
            header_text.contains("[text/plain]"),
            "cabecera con el mimetype: {header_text:?}"
        );
        assert!(
            preview.lines[0]
                .iter()
                .all(|s| s.role.is_none() && s.fg.is_none()),
            "la cabecera es un único span plano: {:?}",
            preview.lines[0]
        );

        // "linea uno" no tiene dígitos ni keywords: todo plano.
        assert!(
            preview.lines[1].iter().all(|s| s.role.is_none()),
            "línea sin dígitos ni keywords: sin roles: {:?}",
            preview.lines[1]
        );

        // El contenido de SAMPLE no lleva dígitos/keywords reales en las 3
        // primeras líneas ("linea uno/dos/tres"); se prueba la conversión
        // exacta (role sin validar en el wire) con un archivo dedicado.
        let mem2 = &mem;
        write_file(mem2, "mem:///code.txt", b"TODO 42 norte plano\nsegunda").await;
        let preview2 = backend
            .plugin_preview_styled(&vp("mem:///code.txt"))
            .await
            .expect("preview_styled ok")
            .expect("previewer sigue aprobado+activado");
        // lines[1] = primera línea de contenido: "TODO 42 norte plano".
        let spans = &preview2.lines[1];
        let by_text = |t: &str| spans.iter().find(|s| s.text == t);
        assert_eq!(
            by_text("TODO").and_then(|s| s.role.as_deref()),
            Some("keyword"),
            "TODO es keyword del guest (SIN validar contra norte_theme::Role en el wire): {spans:?}"
        );
        assert_eq!(
            by_text("TODO").and_then(|s| s.fg),
            Some([255, 200, 0]),
            "keyword además lleva fg fijo: {spans:?}"
        );
        assert_eq!(
            by_text("42").and_then(|s| s.role.as_deref()),
            Some("number"),
            "42 es number: {spans:?}"
        );
        assert_eq!(
            by_text("norte").and_then(|s| s.role.as_deref()),
            Some("keyword"),
            "norte es keyword: {spans:?}"
        );
        assert_eq!(
            by_text("plano").and_then(|s| s.role.as_deref()),
            None,
            "plano no casa ninguna regla del highlighter: {spans:?}"
        );

        // #101 (paridad daemon↔embebido): la decodificación host-side ocurre
        // en el HANDLER DEL DAEMON y su señal `lossy` viaja por el WIRE. Un
        // archivo válido no es lossy...
        assert!(!preview.lossy, "SAMPLE UTF-8 válido: no lossy");
        assert!(!preview2.lossy, "código ASCII: no lossy");
        // ...y uno detectado como texto (BOM UTF-8) con un byte inválido SÍ:
        // prueba que el daemon DECODIFICA (no pasa bytes crudos al guest) y
        // marca la pérdida.
        let mut bad = vec![0xEF, 0xBB, 0xBF];
        bad.extend_from_slice(b"linea\xFFmala\n");
        write_file(&mem, "mem:///bad.txt", &bad).await;
        let preview_lossy = backend
            .plugin_preview_styled(&vp("mem:///bad.txt"))
            .await
            .expect("preview_styled ok")
            .expect("previewer sigue aprobado+activado");
        assert!(
            preview_lossy.lossy,
            "el daemon decodificó texto y marcó la pérdida por el wire: {preview_lossy:?}"
        );
    }
}
