use super::*;

// ---------- plugin.* (M4-P3) ----------

/// Manifiesto válido mínimo (mismo del test de `norte_core::plugins`).
pub(super) const DEMO_MANIFEST: &str = r#"
[plugin]
id = "org.norte.demo"
name = "Demo"
publisher = "norte"
version = "0.1.0"
category = "command"
[capabilities]
fs-read = "scoped"
"#;

/// Daemon con `plugins_dir` apuntando a un tempdir SEMBRADO con un plugin
/// descubrible (`plugins/org.norte.demo/plugin.toml`). JAMÁS toca el
/// `~/.config` real: el `plugins_dir` explícito aísla el estado del test.
pub(super) async fn spawn_daemon_plugins() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    // Raíz de plugins DENTRO del mismo tempdir (se limpia con `_dir`).
    let plugins_root = dir.path().join("cfg");
    let plugin_dir = plugins_root.join("plugins").join("org.norte.demo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin");
    std::fs::write(plugin_dir.join("plugin.toml"), DEMO_MANIFEST).expect("write manifest");

    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(plugins_root),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// `plugin.list` por el socket ve el plugin sembrado, nace sin aprobar/activar.
#[tokio::test]
async fn plugin_list_ve_el_catalogo_sembrado() {
    let d = spawn_daemon_plugins().await;
    let c = connected_client(&d).await;
    let list: methods::PluginListResult = c
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert_eq!(list.plugins.len(), 1, "el plugin sembrado se descubre");
    let p = &list.plugins[0];
    assert_eq!(p.id, "org.norte.demo");
    assert!(!p.approved, "nace sin aprobar");
    assert!(!p.enabled, "nace sin activar");
    assert!(list.errors.is_empty());
}

/// Un HUMANO aprueba por el socket; `plugin.list` lo refleja (y persistió, así
/// que una NUEVA conexión también lo ve aprobado).
#[tokio::test]
async fn plugin_set_approval_humano_se_refleja_y_persiste() {
    let d = spawn_daemon_plugins().await;
    let human = connected_client(&d).await;
    let _: methods::PluginSetApprovalResult = human
        .call(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: true,
                expected_digest: None,
            },
        )
        .await
        .expect("aprobación aceptada");

    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list tras aprobar");
    assert!(list.plugins[0].approved, "la aprobación se refleja");

    // Una conexión NUEVA lee el estado persistido (mismo daemon, mismo dir).
    let otra = connected_client(&d).await;
    let list2: methods::PluginListResult = otra
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list en otra conexión");
    assert!(list2.plugins[0].approved, "la aprobación persistió");
}

/// Y con el ancla BUENA —la que el propio `plugin.list` acaba de dar— sí
/// concede: el campo cierra una ventana, no la puerta.
#[tokio::test]
async fn plugin_set_approval_con_el_ancla_que_se_leyo_concede() {
    let d = spawn_daemon_plugins().await;
    let human = connected_client(&d).await;
    let list: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    let ancla = list.plugins[0]
        .manifest_digest
        .clone()
        .expect("el catálogo trae el ancla que un humano lee");

    let _: methods::PluginSetApprovalResult = human
        .call(
            methods::PLUGIN_SET_APPROVAL,
            &methods::PluginSetApprovalParams {
                id: "org.norte.demo".into(),
                approved: true,
                expected_digest: Some(ancla),
            },
        )
        .await
        .expect("el ancla que se leyó concede");

    let despues: methods::PluginListResult = human
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list tras aprobar");
    assert!(despues.plugins[0].approved);
}

/// `plugin.run_command` de un plugin SIN aprobar es `INVALID_REQUEST` y NO lo
/// ejecuta (fail-closed): el humano no ha consentido, así que el runtime no
/// arranca. El demo sembrado nace sin aprobar/activar (M4-P4). El caso de éxito
/// con un `.wasm` real es E2E de la task siguiente.
#[tokio::test]
async fn plugin_run_command_sin_aprobar_es_invalid_request() {
    let d = spawn_daemon_plugins().await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PluginRunCommandResult>(
            methods::PLUGIN_RUN_COMMAND,
            &methods::PluginRunCommandParams {
                id: "org.norte.demo".into(),
                command: "echo".into(),
                arg: "hola".into(),
            },
        )
        .await
        .expect_err("un plugin sin aprobar jamás se ejecuta");
    assert!(
        matches!(err, ClientError::Rpc(ref rpc) if rpc.code == codes::INVALID_REQUEST),
        "sin aprobar = INVALID_REQUEST, no se ejecuta: {err:?}"
    );
}

/// `plugin.run_command` de un id DESCONOCIDO es `INVALID_PARAMS` (el cliente
/// pidió un plugin que no existe): no se ejecuta ni se filtra nada.
#[tokio::test]
async fn plugin_run_command_id_desconocido_es_invalid_params() {
    let d = spawn_daemon_plugins().await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PluginRunCommandResult>(
            methods::PLUGIN_RUN_COMMAND,
            &methods::PluginRunCommandParams {
                id: "org.norte.fantasma".into(),
                command: "echo".into(),
                arg: String::new(),
            },
        )
        .await
        .expect_err("id desconocido");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

// ---------- plugin.help (H3e) ----------

/// Daemon sembrado con el plugin demo, dando al test la oportunidad de escribir
/// su propio `help.md` (H3e). `seed` recibe `(raiz_del_tempdir, dir_del_plugin)`
/// — la raíz para poder dejar ficheros FUERA del directorio del plugin, que es
/// justo lo que el caso del enlace escapado necesita.
pub(super) async fn spawn_daemon_help_plugin(
    seed: impl FnOnce(&std::path::Path, &std::path::Path),
) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let plugins_root = dir.path().join("cfg");
    let plugin_dir = plugins_root.join("plugins").join("org.norte.demo");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin");
    std::fs::write(plugin_dir.join("plugin.toml"), DEMO_MANIFEST).expect("write manifest");
    seed(dir.path(), &plugin_dir);

    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(plugins_root),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// `plugin.help` por el socket devuelve el `help.md` del plugin, ya acotado por
/// el host: cuerpo íntegro, sin recorte ni pérdida.
#[tokio::test]
async fn plugin_help_devuelve_la_pagina_acotada_del_plugin() {
    let d = spawn_daemon_help_plugin(|_root, plugin_dir| {
        std::fs::write(
            plugin_dir.join("help.md"),
            "# Demo\n\nLa página del plugin demo.\n",
        )
        .expect("write help.md");
    })
    .await;
    let c = connected_client(&d).await;
    let help: methods::PluginHelpResult = c
        .call(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "org.norte.demo".into(),
            },
        )
        .await
        .expect("plugin.help responde");
    assert!(help.markdown.contains("demo"), "llega el cuerpo: {help:?}");
    assert!(
        !help.truncated && !help.lossy,
        "nada que recortar: {help:?}"
    );

    // Y `plugin.list` lo anuncia, para que el frontend no pida en vano.
    let list: methods::PluginListResult = c
        .call(methods::PLUGIN_LIST, &methods::PluginListParams {})
        .await
        .expect("plugin.list");
    assert!(list.plugins[0].has_help, "has_help lo anuncia");
}

/// Un id que NO está en el catálogo es `INVALID_PARAMS` — mismo trato que
/// `plugin.set_approval` da a un plugin fantasma. El id nunca se compone en una
/// ruta, así que un `../` solo falla el lookup.
#[tokio::test]
async fn plugin_help_de_un_id_desconocido_es_invalid_params() {
    let d = spawn_daemon_help_plugin(|_root, plugin_dir| {
        std::fs::write(plugin_dir.join("help.md"), "# Demo\n").expect("write help.md");
    })
    .await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PluginHelpResult>(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "no.existe".into(),
            },
        )
        .await
        .expect_err("un plugin fantasma no tiene página");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));

    let err2 = c
        .call::<_, methods::PluginHelpResult>(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "../../etc/passwd".into(),
            },
        )
        .await
        .expect_err("un id con travesía es solo un id desconocido");
    assert!(matches!(err2, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// El agujero que cierra la guarda del host, comprobado en el punto donde un
/// AGENTE llega: `plugin.help` no puede convertirse en una lectura de fichero
/// arbitrario que rodee el motor de policy. Un `help.md` que es un enlace a algo
/// de FUERA del directorio del plugin se sirve como página en blanco.
#[cfg(unix)]
#[tokio::test]
async fn plugin_help_no_sirve_un_help_md_que_escapa_del_directorio() {
    let d = spawn_daemon_help_plugin(|root, plugin_dir| {
        let secreto = root.join("secreto.md");
        std::fs::write(&secreto, "CLAVE-PRIVADA-QUE-NO-DEBE-CRUZAR-EL-WIRE")
            .expect("write secreto");
        std::os::unix::fs::symlink(&secreto, plugin_dir.join("help.md")).expect("symlink");
    })
    .await;
    let agent = connected_agent(&d, "claude-01").await;
    let help: methods::PluginHelpResult = agent
        .call(
            methods::PLUGIN_HELP,
            &methods::PluginHelpParams {
                id: "org.norte.demo".into(),
            },
        )
        .await
        .expect("un plugin conocido siempre responde");
    assert_eq!(
        help.markdown, "",
        "un enlace que sale del directorio no se sirve"
    );
}

/// Manifiesto con `[config]` (G3c): tres claves de tipos distintos, para
/// ejercitar `plugin.get_config`/`plugin.set_config` de punta a punta por
/// el socket.
pub(super) const CONFIG_MANIFEST: &str = r#"
[plugin]
id = "org.norte.cfg"
name = "Cfg Demo"
publisher = "norte"
version = "0.1.0"
category = "command"

[config.greeting]
type = "string"
default = "hola"

[config.retries]
type = "int"
default = 3
min = 0
max = 10

[config.mode]
type = "enum"
default = "fast"
values = ["fast", "thorough"]
"#;

/// Daemon sembrado con [`CONFIG_MANIFEST`] (G3c) — espejo de
/// `spawn_daemon_plugins`, distinto manifiesto.
pub(super) async fn spawn_daemon_config_plugin() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let plugins_root = dir.path().join("cfg");
    let plugin_dir = plugins_root.join("plugins").join("org.norte.cfg");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir plugin");
    std::fs::write(plugin_dir.join("plugin.toml"), CONFIG_MANIFEST).expect("write manifest");

    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(plugins_root),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        dir,
        mem,
    }
}

/// `plugin.get_config` por el socket: esquema + valor efectivo de las TRES
/// claves, ABIERTO a cualquier conexión (leer no consiente nada) — incluso
/// SIN aprobar/activar el plugin (mismo criterio que `plugin.list`).
#[tokio::test]
async fn plugin_get_config_ve_el_esquema_y_los_defaults() {
    let d = spawn_daemon_config_plugin().await;
    let c = connected_client(&d).await;
    let res: methods::PluginGetConfigResult = c
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("plugin.get_config");
    assert_eq!(res.keys.len(), 3);
    let greeting = res.keys.iter().find(|k| k.key == "greeting").unwrap();
    assert_eq!(greeting.kind, "string");
    assert_eq!(greeting.value, "hola");
    let retries = res.keys.iter().find(|k| k.key == "retries").unwrap();
    assert_eq!(retries.kind, "int");
    assert_eq!(retries.min, Some(0));
    assert_eq!(retries.max, Some(10));
    let mode = res.keys.iter().find(|k| k.key == "mode").unwrap();
    assert_eq!(mode.kind, "enum");
    assert_eq!(
        mode.values,
        vec!["fast".to_string(), "thorough".to_string()]
    );
}

/// `plugin.get_config` de un id DESCONOCIDO responde `keys: []` — nunca un
/// error (mismo criterio indulgente que `plugin.list` con un catálogo
/// vacío).
#[tokio::test]
async fn plugin_get_config_id_desconocido_es_keys_vacio() {
    let d = spawn_daemon_config_plugin().await;
    let c = connected_client(&d).await;
    let res: methods::PluginGetConfigResult = c
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.fantasma".into(),
            },
        )
        .await
        .expect("plugin.get_config no es error con id desconocido");
    assert!(res.keys.is_empty());
}

/// Un HUMANO fija un valor válido; `plugin.get_config` lo refleja Y
/// persistió (una NUEVA conexión también lo ve).
#[tokio::test]
async fn plugin_set_config_humano_se_refleja_y_persiste() {
    let d = spawn_daemon_config_plugin().await;
    let human = connected_client(&d).await;
    let _: methods::PluginSetConfigResult = human
        .call(
            methods::PLUGIN_SET_CONFIG,
            &methods::PluginSetConfigParams {
                id: "org.norte.cfg".into(),
                key: "greeting".into(),
                value: "hola mundo".into(),
            },
        )
        .await
        .expect("set_config con un valor válido");

    let res: methods::PluginGetConfigResult = human
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("get_config tras set_config");
    assert_eq!(
        res.keys.iter().find(|k| k.key == "greeting").unwrap().value,
        "hola mundo"
    );

    let otra = connected_client(&d).await;
    let res2: methods::PluginGetConfigResult = otra
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("get_config en otra conexión");
    assert_eq!(
        res2.keys
            .iter()
            .find(|k| k.key == "greeting")
            .unwrap()
            .value,
        "hola mundo",
        "el valor persistió"
    );
}

/// Un valor INVÁLIDO (fuera de `[min,max]`) es `INVALID_PARAMS` y NO se
/// persiste — `plugin.get_config` sigue viendo el default.
#[tokio::test]
async fn plugin_set_config_valor_invalido_no_persiste() {
    let d = spawn_daemon_config_plugin().await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, methods::PluginSetConfigResult>(
            methods::PLUGIN_SET_CONFIG,
            &methods::PluginSetConfigParams {
                id: "org.norte.cfg".into(),
                key: "retries".into(),
                value: "999".into(),
            },
        )
        .await
        .expect_err("999 fuera de [0,10]");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));

    let res: methods::PluginGetConfigResult = human
        .call(
            methods::PLUGIN_GET_CONFIG,
            &methods::PluginGetConfigParams {
                id: "org.norte.cfg".into(),
            },
        )
        .await
        .expect("get_config tras el rechazo");
    assert_eq!(
        res.keys.iter().find(|k| k.key == "retries").unwrap().value,
        "3",
        "el rechazo no debe haber tocado el default"
    );
}

/// Una clave DESCONOCIDA es `INVALID_PARAMS` (no se ensucia `config.toml`
/// con claves que el esquema no declara).
#[tokio::test]
async fn plugin_set_config_clave_desconocida_es_invalid_params() {
    let d = spawn_daemon_config_plugin().await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, methods::PluginSetConfigResult>(
            methods::PLUGIN_SET_CONFIG,
            &methods::PluginSetConfigParams {
                id: "org.norte.cfg".into(),
                key: "no-such-key".into(),
                value: "x".into(),
            },
        )
        .await
        .expect_err("clave desconocida");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// `plugin.preview` de un archivo cuando NO hay ningún previewer instalado
/// (registro vacío, `plugins_dir: None`) devuelve `preview: None` — NO un
/// error: ningún previewer consentido casa el mimetype, así que el frontend cae
/// a la vista cruda. Ni siquiera se leen los bytes del archivo (la resolución
/// falla antes). El caso con un previewer `.wasm` real es E2E de la task
/// siguiente.
#[tokio::test]
async fn plugin_preview_sin_previewer_es_none() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///nota.txt", b"hola mundo").await;
    let c = connected_client(&d).await;
    let res = c
        .call::<_, methods::PluginPreviewResult>(
            methods::PLUGIN_PREVIEW,
            &methods::PluginPreviewParams {
                path: vp("mem:///nota.txt"),
            },
        )
        .await
        .expect("plugin.preview no es error cuando no hay previewer");
    assert!(
        res.preview.is_none(),
        "sin previewer instalado la preview es None (vista cruda), no un error: {res:?}"
    );
}

/// G3a (ADR 0037): `plugin.preview_styled` sin ningún previewer instalado
/// devuelve `preview: None` — MISMO criterio que su gemelo plano, no un
/// error. El client `Backend::plugin_preview_styled` embebido tiene su
/// propio test para el caso `Ok(None)`; este cubre el handler DAEMON contra
/// un socket real.
#[tokio::test]
async fn plugin_preview_styled_sin_previewer_es_none() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///nota.txt", b"hola mundo").await;
    let c = connected_client(&d).await;
    let res = c
        .call::<_, methods::PluginPreviewStyledResult>(
            methods::PLUGIN_PREVIEW_STYLED,
            &methods::PluginPreviewStyledParams {
                path: vp("mem:///nota.txt"),
                columns: None,
            },
        )
        .await
        .expect("plugin.preview_styled no es error cuando no hay previewer");
    assert!(
        res.preview.is_none(),
        "sin previewer instalado la preview con estilo es None: {res:?}"
    );
}
