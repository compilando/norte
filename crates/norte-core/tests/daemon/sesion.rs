use super::*;

#[tokio::test]
async fn initialize_negocia_y_es_obligatorio() {
    let d = spawn_daemon(None).await;
    // Sin initialize: cualquier método es NOT_INITIALIZED.
    let c = Client::connect(&d.socket).await.expect("connect");
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("initialize primero");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::NOT_INITIALIZED),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // Con initialize, funciona.
    let _c2 = connected_client(&d).await;
}

/// La versión N-1 del daemon, DERIVADA de `PROTOCOL_VERSION`.
///
/// Escrita a mano (era `"0.38.2"`), la ventana que este test dice comprobar
/// pasaba a ser «una versión vieja cualquiera» al primer bump y un rojo al
/// segundo, culpando al cambio que pasara por delante. En 0.x el minor es el
/// major efectivo, así que N-1 es minor menos uno.
pub(super) fn n_minus_one() -> String {
    let (major, rest) = methods::PROTOCOL_VERSION.split_once('.').expect("semver");
    let (minor, _) = rest.split_once('.').expect("semver");
    let minor: u64 = minor.parse().expect("minor numérico");
    assert_eq!(major, "0", "fuera de 0.x la ventana N-1 la define el major");
    assert!(minor > 0, "0.0.x no tiene N-1 que pedir");
    format!("{major}.{}.2", minor - 1)
}

#[tokio::test]
async fn initialize_rechaza_version_incompatible() {
    let d = spawn_daemon(None).await;
    let c = Client::connect(&d.socket).await.expect("connect");
    let err = c
        .call::<_, methods::InitializeResult>(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: "0.1.0".into(),
                encodings: vec!["json".into()],
                agent_session: None,
            },
        )
        .await
        .expect_err("0.1.0 no es ni N ni N-1");
    match err {
        ClientError::Rpc(rpc) => {
            // Código PROPIO: la señal de upgrade jamás se parsea de message.
            assert_eq!(rpc.code, codes::VERSION_MISMATCH);
            assert!(norte_core::daemon::is_version_mismatch(&ClientError::Rpc(
                rpc
            )));
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // N-1 SÍ entra.
    let c2 = Client::connect(&d.socket).await.expect("connect");
    let ok: methods::InitializeResult = c2
        .call(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: n_minus_one(),
                encodings: vec![],
                agent_session: None,
            },
        )
        .await
        .expect("N-1 aceptado");
    assert_eq!(ok.protocol_version, methods::PROTOCOL_VERSION);
}

#[tokio::test]
async fn initialize_rechaza_encoding_desconocido() {
    let d = spawn_daemon(None).await;
    let c = Client::connect(&d.socket).await.expect("connect");
    let err = c
        .call::<_, methods::InitializeResult>(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["msgpack".into()],
                agent_session: None,
            },
        )
        .await
        .expect_err("solo json en M2");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// `agent_session` se valida fail-closed en el handshake (encoding-auditor H1
/// de T5): controles, bidi, vacío o kilométrico → `INVALID_PARAMS`. El id
/// viaja a journal, logs y modales de aprobación — jamás lo elige libre el
/// agente.
#[tokio::test]
async fn agent_session_hostil_se_rechaza_en_initialize() {
    let d = spawn_daemon(None).await;
    let hostiles = [
        "s1\nmem:///fake",       // inyección de líneas
        "s1\u{202e}ypoc",        // override RTL
        "s1\u{1b}]0;pwned\u{7}", // OSC/ANSI
        "",                      // vacío
        &"a".repeat(65),         // demasiado largo
        "con espacios",          // fuera de charset
    ];
    for session in hostiles {
        let c = Client::connect(&d.socket).await.expect("connect");
        let err = c
            .call::<_, methods::InitializeResult>(
                methods::INITIALIZE,
                &InitializeParams {
                    client_info: client_info(),
                    protocol_version: methods::PROTOCOL_VERSION.into(),
                    encodings: vec!["json".into()],
                    agent_session: Some(session.into()),
                },
            )
            .await
            .expect_err("sesión hostil rechazada");
        assert!(
            matches!(err, ClientError::Rpc(ref rpc) if rpc.code == codes::INVALID_PARAMS),
            "esperaba INVALID_PARAMS para {session:?}, fue {err:?}"
        );
    }
    // El charset legal completo pasa.
    let c = Client::connect(&d.socket).await.expect("connect");
    let ok: methods::InitializeResult = c
        .call(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
                agent_session: Some("Agente.Claude_01-x".into()),
            },
        )
        .await
        .expect("sesión válida");
    assert_eq!(ok.protocol_version, methods::PROTOCOL_VERSION);
}

/// `connection.trust_host_key` (0.7.0, fase 6e) existe en el dispatch y
/// llega al engine: sin conector configurado responde la taxonomía
/// `Unsupported` por el wire — no `METHOD_NOT_FOUND` (eso significaría que
/// el handler falta).
#[tokio::test]
async fn trust_host_key_llega_al_engine() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::ConnectionTrustHostKeyResult>(
            methods::CONNECTION_TRUST_HOST_KEY,
            &methods::ConnectionTrustHostKeyParams {
                host: "h.example".into(),
                port: Some(22),
                algo: "ssh-ed25519".into(),
                fingerprint: "SHA256:abc".into(),
            },
        )
        .await
        .expect_err("sin conector: Unsupported");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(rpc.data, Some(norte_proto::Error::Unsupported)),
                "taxonomía Unsupported, fue {:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// #325, el gemelo del de arriba: `connection.provide_secret` existe en el
/// dispatch y llega al engine. Sin conector responde `Unsupported` por el
/// wire; un `METHOD_NOT_FOUND` querría decir que el handler falta.
#[tokio::test]
async fn provide_secret_llega_al_engine() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::ConnectionProvideSecretResult>(
            methods::CONNECTION_PROVIDE_SECRET,
            &methods::ConnectionProvideSecretParams {
                conn: "rosetta".into(),
                secret: "s3cr3t".into(),
            },
        )
        .await
        .expect_err("sin conector: Unsupported");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(rpc.data, Some(norte_proto::Error::Unsupported)),
                "taxonomía Unsupported, fue {:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

#[tokio::test]
async fn metodo_desconocido_es_method_not_found() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, serde_json::Value>("fs.inventado", &serde_json::json!({}))
        .await
        .expect_err("no existe el método");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::METHOD_NOT_FOUND));
}

// ---------- lifecycle ----------

#[tokio::test]
async fn daemon_shutdown_graceful_espera_y_apaga() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                ..Default::default()
            },
        )
        .await
        .expect("shutdown aceptado");
    let joined = tokio::time::timeout(Duration::from_secs(5), d.run)
        .await
        .expect("run() termina")
        .expect("join limpio");
    joined.expect("apagado sin error");
    assert!(!d.socket.exists(), "el socket se retira del FS");
}

/// Cuando `run()` vuelve, las conexiones ya se CERRARON — con lo que tenían
/// que decir ya escrito.
///
/// Quien llama a `run()` suele ser un `main` que retorna justo después, y al
/// soltar el runtime se lleva por delante toda task que siga viva. La que
/// escribe la respuesta a `daemon.shutdown` era una de ellas: `norte daemon
/// stop` fallaba de vez en cuando con «conexión cerrada con la request en
/// vuelo» sobre un daemon que sí se había parado. Se mira en otra conexión
/// abierta porque es observable sin esperar: si `run()` ya drenó, su lectura
/// da EOF en el acto; si no, todavía no hay nada que leer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_vuelve_con_las_conexiones_ya_cerradas() {
    for _ in 0..30 {
        run_vuelve_con_las_conexiones_ya_cerradas_una_vez().await;
    }
}

async fn run_vuelve_con_las_conexiones_ya_cerradas_una_vez() {
    let d = spawn_daemon(None).await;
    let otra = tokio::net::UnixStream::connect(&d.socket)
        .await
        .expect("connect");
    let c = connected_client(&d).await;
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                ..Default::default()
            },
        )
        .await
        .expect("shutdown aceptado");
    tokio::time::timeout(Duration::from_secs(5), d.run)
        .await
        .expect("run() termina")
        .expect("join limpio")
        .expect("apagado sin error");
    let mut buf = [0u8; 16];
    match otra.try_read(&mut buf) {
        Ok(0) => {}
        leido => panic!("la conexión sigue abierta al volver run(): {leido:?}"),
    }
}

/// Espera una `daemon.going_away` y devuelve si dice que vuelvas.
pub(super) async fn going_away(c: &mut Client) -> bool {
    loop {
        let n = tokio::time::timeout(Duration::from_secs(5), c.notification())
            .await
            .expect("notificación antes del timeout")
            .expect("conexión viva");
        if n.method == methods::DAEMON_GOING_AWAY {
            let p: methods::DaemonGoingAway =
                serde_json::from_value(n.params.expect("params")).expect("DaemonGoingAway");
            return p.reconnect;
        }
    }
}

/// Un RELEVO avisa de que vuelvas, y avisa ANTES de dejar de aceptar.
#[tokio::test]
async fn un_relevo_avisa_de_que_vuelvas() {
    let d = spawn_daemon(None).await;
    let mut c = connected_client(&d).await;
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Handover,
            },
        )
        .await
        .expect("relevo aceptado");

    assert!(going_away(&mut c).await, "un relevo dice que vuelvas");
}

/// Y una parada corriente avisa de lo CONTRARIO. Es lo que separa «el daemon se
/// paró» de «se cayó la conexión», que para quien lo lee no son lo mismo — y es
/// lo que impide que un cliente resucite lo que el usuario acaba de parar.
#[tokio::test]
async fn una_parada_avisa_de_que_no_vuelvas() {
    let d = spawn_daemon(None).await;
    let mut c = connected_client(&d).await;
    let _: DaemonShutdownResult = c
        .call(methods::DAEMON_SHUTDOWN, &DaemonShutdownParams::default())
        .await
        .expect("parada aceptada");

    assert!(!going_away(&mut c).await, "una parada dice que no vuelvas");
}

/// Un relevo con una task VIVA se rehúsa, en la respuesta, mientras todavía hay
/// alguien a quien contestar — y no toca nada: el daemon sigue aceptando.
///
/// La negativa va DELANTE y no después de esperar a las tasks porque la
/// respuesta de `daemon.shutdown` sale en el acto: una negativa decidida
/// minutos más tarde no tendría a quién decírsela, y para entonces el listener
/// ya habría dejado de aceptar — «rehusar» significaría volver a aceptar, que
/// es una máquina de estados que nadie pidió.
#[tokio::test]
async fn un_relevo_con_una_task_viva_se_rehusa_y_no_toca_nada() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///src.bin", &vec![7u8; 256 * 1024]).await;
    // Latencia por operación: la copia sigue viva mientras se pide el relevo,
    // de forma determinista y sin dormir a ciegas.
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(50)));
    let d = spawn_daemon_mem(None, Duration::from_mins(2), mem).await;
    let c = connected_client(&d).await;
    let _: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("copia lanzada");

    let err = c
        .call::<_, DaemonShutdownResult>(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Handover,
            },
        )
        .await
        .expect_err("con una copia viva, no");
    assert!(
        matches!(&err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST),
        "{err:?}"
    );

    // Y no tocó nada: el daemon sigue en pie y sirviendo.
    let _: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("el daemon sigue aceptando tras rehusar el relevo");
}

/// **El socket se retira ANTES de drenar, no después**, y eso solo importa
/// desde que existe el relevo.
///
/// Al apagar, el daemon suelta el listener y espera a sus tasks. Si el fichero
/// del socket sigue ahí durante esa espera, un cliente que reconecte recibe
/// `ECONNREFUSED`, arranca el reemplazo —cosa que ANTES de esta fase no hacía
/// nunca—, el reemplazo borra la ruta rancia y enlaza la suya… y el daemon
/// viejo, al terminar de drenar, borra el socket DEL REEMPLAZO. Éste se queda
/// escuchando en un inodo sin nombre, y como el permiso de arranque es de un
/// solo uso, nadie lo vuelve a levantar.
///
/// El test fija el orden: con una task viva —o sea, en pleno drenaje— la ruta
/// ya no existe.
#[tokio::test]
async fn el_socket_se_retira_antes_de_drenar() {
    let mem = MemProvider::new();
    write_file(&mem, "mem:///src.bin", &vec![7u8; 4 * 1024 * 1024]).await;
    // Latencia ALTA por operación: el drenaje dura segundos, así que «el
    // socket se fue» y «el daemon terminó» no pueden confundirse.
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(200)));
    let mut d = spawn_daemon_mem(None, Duration::from_mins(2), mem).await;
    let c = connected_client(&d).await;
    let _: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("copia lanzada");

    // Parada graceful: entra en el drenaje con la copia viva.
    let _: DaemonShutdownResult = c
        .call(methods::DAEMON_SHUTDOWN, &DaemonShutdownParams::default())
        .await
        .expect("parada aceptada");

    // La ruta tiene que desaparecer MIENTRAS todavía se drena. Las dos mitades
    // son la aserción: sin la segunda, un drenaje que acabara rápido haría pasar
    // el test con el borrado al final, que es justo lo que rompe el relevo.
    let mut retirado = false;
    for _ in 0..50 {
        if !d.socket.exists() {
            retirado = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(retirado, "el socket sigue ahí durante el drenaje");
    assert!(
        futures::FutureExt::now_or_never(&mut d.run).is_none(),
        "y el daemon TODAVÍA no ha terminado: si ya terminó, este test no          distingue el borrado temprano del tardío"
    );
}

pub(super) async fn spawn_daemon_at(socket: PathBuf) -> TestDaemon {
    // El tempdir padre lo posee el caller; aquí un guard vacío.
    let dir = tempfile::tempdir().expect("tempdir guard");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: std::time::Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        _dir: dir,
        mem,
    }
}

// ---------- protocolo crudo (frames hostiles) ----------

/// Lee UNA línea completa del stream (UDS es stream: un read puede ser
/// parcial — m4 del rust-reviewer).
pub(super) async fn read_frame(s: &mut tokio::net::UnixStream) -> serde_json::Value {
    use tokio::io::AsyncReadExt;
    let mut decoder = norte_proto::wire::FrameDecoder::new();
    let mut buf = vec![0u8; 4096];
    loop {
        if let Some(frame) = decoder.next_frame() {
            return serde_json::from_slice(&frame).expect("respuesta JSON");
        }
        let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
            .await
            .expect("respuesta antes del timeout")
            .expect("read");
        assert_ne!(n, 0, "conexión cerrada esperando respuesta");
        decoder.push(&buf[..n]).expect("frame razonable");
    }
}

/// Frames hostiles directamente sobre el socket, sin el Client: JSON roto
/// = -32700; JSON válido que no es envelope = -32600; request con id de
/// tipo ilegal = -32600 (JAMÁS silencio); params null en daemon.shutdown
/// (la forma canónica del golden) funciona.
#[tokio::test]
async fn frames_hostiles_y_formas_canonicas_crudas() {
    use tokio::io::AsyncWriteExt;
    let d = spawn_daemon(None).await;

    let mut s = tokio::net::UnixStream::connect(&d.socket)
        .await
        .expect("connect crudo");

    s.write_all(b"esto no es json\n").await.expect("write");
    let resp = read_frame(&mut s).await;
    assert_eq!(resp["error"]["code"], serde_json::json!(-32700));
    assert_eq!(resp["id"], serde_json::Value::Null);

    // JSON válido, envelope inválido: -32600, no -32700 (M2 del guardian).
    s.write_all(b"{\"foo\":1}\n").await.expect("write");
    let resp = read_frame(&mut s).await;
    assert_eq!(resp["error"]["code"], serde_json::json!(-32600));

    // id ilegal (negativo): -32600, jamás tragado como notification (M3).
    s.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":-1,\"method\":\"fs.stat\",\"params\":null}\n")
        .await
        .expect("write");
    let resp = read_frame(&mut s).await;
    assert_eq!(resp["error"]["code"], serde_json::json!(-32600));

    // initialize + daemon.shutdown con params null (golden canónico, M1).
    //
    // La versión se INTERPOLA desde `PROTOCOL_VERSION` y no se escribe a mano:
    // clavada aquí (era `"0.38.0"`), el frame envejecía sin que nadie lo
    // tocara y este test se ponía rojo dos bumps después, culpando al cambio
    // que pasara por delante. Lo que prueba es el marco crudo, no la ventana
    // N/N-1 —de eso se ocupa `version_compatible` en `norte-proto`—.
    let hello = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"client_info\":{{\"name\":\"raw\",\"version\":\"0\"}},\"protocol_version\":\"{}\",\"encodings\":[\"json\"]}}}}\n",
        methods::PROTOCOL_VERSION
    );
    s.write_all(hello.as_bytes()).await.expect("write");
    let resp = read_frame(&mut s).await;
    assert!(resp["result"]["protocol_version"].is_string(), "{resp}");
    s.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"daemon.shutdown\",\"params\":null}\n")
        .await
        .expect("write");
    let resp = read_frame(&mut s).await;
    assert!(resp["error"].is_null(), "params null aceptado: {resp}");
}

/// initialize repetido = error de protocolo (decisión pinneada, m4 del
/// guardian) — y la conexión sigue viva y usable.
#[tokio::test]
async fn initialize_repetido_es_invalid_request() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::InitializeResult>(
            methods::INITIALIZE,
            &InitializeParams {
                client_info: client_info(),
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
                agent_session: None,
            },
        )
        .await
        .expect_err("re-initialize rechazado");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_REQUEST));
    let _: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect("la conexión sigue viva");
}

// ---------- L2: la sesión de UI por el socket ----------

/// La sesión va y vuelve, y la revisión sube. La primera conexión humana que
/// pregunta se la queda.
#[tokio::test]
async fn session_get_y_put_por_el_socket() {
    // Con `state_dir`, porque `owner` significa «esto se guarda»: un daemon
    // sin dónde escribir contesta que no, y con razón.
    let estado = tempfile::tempdir().expect("tempdir");
    let d = spawn_daemon_estado(estado.path()).await;
    let c = connected_client(&d).await;
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g.session.revision, 0);
    assert_eq!(
        g.session.version, 0,
        "sin esquema hasta que alguien escriba"
    );
    assert!(g.owner, "la primera conexión humana se la queda");

    let cuerpo = serde_json::json!({ "version": 1, "slots": {} });
    let p: methods::SessionPutResult = c
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: cuerpo.clone(),
            },
        )
        .await
        .expect("session.put");
    assert_eq!(p.revision, 1);

    let g2: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g2.session.body, cuerpo, "vuelve el mismo documento");
    assert_eq!(g2.session.version, 1);
    assert_eq!(g2.session.revision, 1);
}

/// `session.release` (fase 9) suelta la propiedad SIN desconectar, y lo que
/// suelta es la propiedad y no el contenido.
///
/// Las tres cosas que tiene que demostrar, y ninguna es obvia:
///
/// - la dueña recibe `released: true` y deja de serlo;
/// - el CUERPO sigue donde estaba —es justo lo que el otro frontend va a
///   leer—, y con su revisión;
/// - la siguiente conexión humana se la lleva, que es lo que hace posible el
///   relevo.
#[tokio::test]
async fn session_release_suelta_la_propiedad_y_conserva_el_cuerpo() {
    let estado = tempfile::tempdir().expect("tempdir");
    let daemon = spawn_daemon_estado(estado.path()).await;
    let cliente = connected_client(&daemon).await;
    let g: methods::SessionGetResult = cliente
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g.owner, "la primera conexión humana se la queda");

    let cuerpo = serde_json::json!({ "version": 1, "slots": {} });
    let p: methods::SessionPutResult = cliente
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: cuerpo.clone(),
            },
        )
        .await
        .expect("session.put");
    assert_eq!(p.revision, 1);

    let r: methods::SessionReleaseResult = cliente
        .call(methods::SESSION_RELEASE, &serde_json::json!({}))
        .await
        .expect("session.release");
    assert!(r.released, "era la dueña");

    // Soltar DOS veces contesta `false` la segunda: ya no era ella, y eso hay
    // que poder distinguirlo de un fallo.
    let r2: methods::SessionReleaseResult = cliente
        .call(methods::SESSION_RELEASE, &serde_json::json!({}))
        .await
        .expect("session.release");
    assert!(!r2.released, "ya no era la dueña");

    // Y otra conexión se la lleva, con el cuerpo intacto: es el relevo.
    let cliente2 = connected_client(&daemon).await;
    let g2: methods::SessionGetResult = cliente2
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g2.owner, "la sesión estaba libre y se la lleva");
    assert_eq!(g2.session.body, cuerpo, "lo que se soltó fue la propiedad");
    assert_eq!(g2.session.revision, 1);
}

/// Un AGENTE no suelta la sesión de nadie: no tiene pantalla, y la respuesta
/// es la misma que a `session.get` — `INVALID_REQUEST` antes de mirar nada.
#[tokio::test]
async fn session_release_se_le_niega_a_un_agente() {
    let estado = tempfile::tempdir().expect("tempdir");
    let daemon = spawn_daemon_estado(estado.path()).await;
    let humano = connected_client(&daemon).await;
    let g: methods::SessionGetResult = humano
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g.owner);

    let agente = connected_agent(&daemon, "claude").await;
    let err = agente
        .call::<_, methods::SessionReleaseResult>(methods::SESSION_RELEASE, &serde_json::json!({}))
        .await
        .expect_err("un agente no tiene sesión");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_REQUEST),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    // Y la del humano SIGUE siendo suya: el intento del agente no la tocó.
    let r: methods::SessionReleaseResult = humano
        .call(methods::SESSION_RELEASE, &serde_json::json!({}))
        .await
        .expect("session.release");
    assert!(r.released, "el humano seguía siendo el dueño");
}

/// Una revisión rancia por el wire es la taxonomía `Conflict` en `data`, no un
/// error de transporte: el cliente distingue «vuelve a leer» de «el daemon se
/// rompió».
#[tokio::test]
async fn session_put_rancio_es_conflict() {
    // CON `state_dir`: desde la revisión de #237 un core que no persiste
    // rehúsa el `put` entero, así que un conflicto de revisión solo se puede
    // provocar donde de verdad se escribe.
    let estado = tempfile::tempdir().expect("tmp");
    let d = spawn_daemon_estado(estado.path()).await;
    let c = connected_client(&d).await;
    let _: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    let params = methods::SessionPutParams {
        version: 1,
        revision: 0,
        body: serde_json::json!({}),
    };
    let _: methods::SessionPutResult = c
        .call(methods::SESSION_PUT, &params)
        .await
        .expect("el primero entra");
    let err = c
        .call::<_, methods::SessionPutResult>(methods::SESSION_PUT, &params)
        .await
        .expect_err("la revisión ya no es la vigente");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(
                rpc.data,
                Some(Error::Conflict {
                    conflict: norte_proto::ConflictKind::StaleRevision
                }),
                "{:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Por encima del tope: `LimitExceeded` con SU token, y la sesión almacenada
/// se queda como estaba.
#[tokio::test]
async fn session_put_sobre_el_tope_es_limit_exceeded() {
    // CON `state_dir`, por lo mismo que el test de arriba: el tope se
    // comprueba después de la propiedad, y sin escritor no se llega.
    let estado = tempfile::tempdir().expect("tmp");
    let d = spawn_daemon_estado(estado.path()).await;
    let c = connected_client(&d).await;
    let _: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    let gordo = serde_json::json!({ "x": "y".repeat(methods::SESSION_BODY_MAX + 1) });
    let err = c
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: gordo,
            },
        )
        .await
        .expect_err("no cabe");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert!(
                matches!(
                    rpc.data,
                    Some(Error::LimitExceeded { ref limit }) if limit == Error::LIMIT_SESSION_BODY
                ),
                "LimitExceeded session-body, fue {:?}",
                rpc.data
            );
        }
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g.session.revision, 0, "no se escribió nada");
}

/// El segundo cliente del MISMO daemon recibe una copia y corre suelto: su
/// `put` se rehúsa y la sesión de la dueña se queda intacta.
#[tokio::test]
async fn el_segundo_cliente_recibe_copia_y_no_escribe() {
    let estado = tempfile::tempdir().expect("tempdir");
    let d = spawn_daemon_estado(estado.path()).await;
    let uno = connected_client(&d).await;
    let g1: methods::SessionGetResult = uno
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g1.owner);
    let _: methods::SessionPutResult = uno
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "quien": "uno" }),
            },
        )
        .await
        .expect("la dueña escribe");

    let dos = connected_client(&d).await;
    let g2: methods::SessionGetResult = dos
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g2.owner, "la segunda corre suelta");
    assert_eq!(
        g2.session.body["quien"],
        serde_json::json!("uno"),
        "recibe COPIA"
    );
    let err = dos
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 1,
                body: serde_json::json!({ "quien": "dos" }),
            },
        )
        .await
        .expect_err("quien no es dueña no escribe");
    // Con taxonomía y no con prosa: el cliente distingue «no mandas» de «tus
    // params están mal» sin leer inglés — y es la misma negativa que da el
    // brazo embebido.
    match err {
        ClientError::Rpc(ref rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(Error::PermissionDenied), "{:?}", rpc.data);
        }
        ref other => panic!("esperaba Rpc, fue {other:?}"),
    }
    let g3: methods::SessionGetResult = uno
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g3.session.body["quien"], serde_json::json!("uno"));
}

/// La dueña que se va SUELTA la sesión: la siguiente conexión humana la toma.
/// Sin esto, un cliente que muere deja la pantalla de rehén hasta el relevo.
#[tokio::test]
async fn al_morir_la_duena_la_sesion_queda_libre() {
    let estado = tempfile::tempdir().expect("tempdir");
    let d = spawn_daemon_estado(estado.path()).await;
    let uno = connected_client(&d).await;
    let g1: methods::SessionGetResult = uno
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g1.owner);
    drop(uno);

    // La desconexión se procesa en el servidor; se reintenta hasta verla. El
    // límite es de TIEMPO y no un número de vueltas: bajo carga, «50 yields»
    // es una carrera que se pierde y un rojo intermitente.
    let libre = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let dos = connected_client(&d).await;
            let g: methods::SessionGetResult = dos
                .call(methods::SESSION_GET, &serde_json::json!({}))
                .await
                .expect("session.get");
            if g.owner {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        libre.is_ok(),
        "la sesión quedó de rehén de una conexión muerta"
    );
}

/// Un daemon SIN dónde escribir no dice que manda: `owner: false`, y el
/// cliente se ve suelto en vez de escribir cada segundo una pantalla que no
/// va a llegar a ningún disco.
#[tokio::test]
async fn sin_state_dir_nadie_es_duena() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g.owner, "sin escritor no hay dueña que prometer");
}

/// Daemon con `state_dir` propio: el que persiste la sesión de UI (L2). El
/// directorio lo pone el test, y por eso ningún test toca el estado real.
pub(super) async fn spawn_daemon_estado(state: &std::path::Path) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: Some(state.to_path_buf()),
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        run,
        _dir: dir,
        mem,
    }
}

/// El relevo es el evento por el que esto existe: un daemon se va, y lo que el
/// cliente había puesto está en disco cuando arranca el siguiente.
#[tokio::test]
async fn la_sesion_sobrevive_a_un_relevo() {
    let estado = tempfile::tempdir().expect("tmp");
    let d = spawn_daemon_estado(estado.path()).await;
    let c = connected_client(&d).await;
    let _: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    let _: methods::SessionPutResult = c
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "dir": "file:///casa" }),
            },
        )
        .await
        .expect("session.put");
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Handover,
            },
        )
        .await
        .expect("daemon.shutdown");
    drop(c);
    // El volcado y la suelta del lock ocurren DENTRO de `run`: esperarlo es
    // esperar exactamente a lo que el sucesor necesita encontrar hecho.
    d.run.await.expect("join").expect("apagado limpio");

    let d2 = spawn_daemon_estado(estado.path()).await;
    let c2 = connected_client(&d2).await;
    let g: methods::SessionGetResult = c2
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert_eq!(g.session.body["dir"], serde_json::json!("file:///casa"));
    assert_eq!(g.session.revision, 1, "la revisión también sobrevive");
    assert_eq!(g.session.version, 1);
}

/// #237: un daemon que arranca mientras OTRO core tiene el lock de la sesión
/// no lo volvía a intentar jamás.
///
/// `session_persists` se calculaba una vez en el bind, así que contestaba
/// `owner: false` durante toda su vida — también horas después de que el otro
/// proceso se hubiera ido y el fichero llevara libre desde entonces. El brazo
/// embebido ya reintentaba (#234); éste es el del daemon.
///
/// Y al tomarlo tarde ADOPTA el documento de disco: lo que el otro core
/// escribió después de que éste arrancara es lo vigente, y servir la copia
/// vieja con el número nuevo sería perderlo sin que nada lo notara.
#[tokio::test]
async fn un_daemon_suelto_toma_la_sesion_cuando_queda_libre() {
    let estado = tempfile::tempdir().expect("tmp");
    let uno = spawn_daemon_estado(estado.path()).await;
    let c1 = connected_client(&uno).await;
    let g1: methods::SessionGetResult = c1
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(g1.owner, "el primero tiene el lock");

    // El segundo arranca CON el lock tomado: corre suelto.
    let dos = spawn_daemon_estado(estado.path()).await;
    let c2 = connected_client(&dos).await;
    let g2: methods::SessionGetResult = c2
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g2.owner, "con el lock de otro, suelto");

    // El primero escribe DESPUÉS de que el segundo haya arrancado: esto es lo
    // que el segundo tiene que adoptar, y no puede haberlo leído al nacer.
    let _: methods::SessionPutResult = c1
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "quien": "el primero" }),
            },
        )
        .await
        .expect("la dueña escribe");
    let _: DaemonShutdownResult = c1
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Stop,
            },
        )
        .await
        .expect("daemon.shutdown");
    drop(c1);
    uno.run.await.expect("join").expect("apagado limpio");

    // El límite es de TIEMPO y no un número de vueltas: el escritor reintenta
    // en su tick, y bajo carga contar vueltas es un rojo intermitente.
    let tomada = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let g: methods::SessionGetResult = c2
                .call(methods::SESSION_GET, &serde_json::json!({}))
                .await
                .expect("session.get");
            if g.owner {
                return g;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("el segundo nunca tomó una sesión que llevaba libre");

    assert_eq!(
        tomada.session.body["quien"],
        serde_json::json!("el primero"),
        "y adopta el documento que dejó el otro, no el que tenía al nacer"
    );
    assert_eq!(tomada.session.revision, 1, "con su revisión");
}

/// **La promesa de ADR 0059, de punta a punta**: una sesión que escribió un
/// binario MÁS NUEVO no se lee y —lo que importa— no se pisa.
///
/// Sin el gate en el escritor, esto se rompía en un segundo y en silencio: el
/// fichero del futuro no se cargaba, el core arrancaba en la revisión 0, el
/// primer `put` del cliente la aceptaba, y el volcado siguiente publicaba
/// encima. Perder la sesión de un binario nuevo contra uno viejo no se
/// recupera, así que la afirmación es sobre los BYTES del fichero.
#[tokio::test]
async fn una_sesion_del_futuro_no_se_pisa_por_el_socket() {
    let estado = tempfile::tempdir().expect("tmp");
    let futura = methods::Session {
        version: norte_core::ui_session::disk::SCHEMA_VERSION + 1,
        revision: 7,
        body: serde_json::json!({ "de": "un binario más nuevo" }),
    };
    norte_core::ui_session::disk::write(estado.path(), &futura).expect("escribe la del futuro");
    let fichero = norte_core::ui_session::disk::path(estado.path());
    let antes = std::fs::read(&fichero).expect("lee");

    let d = spawn_daemon_estado(estado.path()).await;
    let c = connected_client(&d).await;
    let g: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g.owner, "no se lee, así que tampoco se escribe");
    assert_eq!(g.session.revision, 0, "arranca desde la configuración");
    // Y un cliente que IGNORE `owner` tampoco la pisa. Antes se le aceptaba en
    // memoria y el cuerpo moría con el proceso; desde la revisión de #237 se
    // rehúsa de plano, que es lo que ya hacía el brazo embebido — y lo que hay
    // que hacer en cuanto el escritor puede tomar el lock tarde.
    let err = c
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "yo": "el binario viejo" }),
            },
        )
        .await
        .expect_err("sobre una sesión del futuro no se escribe ni en memoria");
    match err {
        ClientError::Rpc(ref rpc) => {
            assert_eq!(rpc.data, Some(Error::PermissionDenied), "{:?}", rpc.data);
        }
        ref other => panic!("esperaba Rpc, fue {other:?}"),
    }
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Stop,
            },
        )
        .await
        .expect("daemon.shutdown");
    drop(c);
    d.run.await.expect("join").expect("apagado limpio");

    assert_eq!(
        std::fs::read(&fichero).expect("lee"),
        antes,
        "el fichero del futuro tiene que seguir byte a byte como estaba"
    );
}

/// El core que no tiene el lock sirve la pantalla y NO la escribe: dos cores
/// sobre un mismo estado no se pisan.
#[tokio::test]
async fn un_core_suelto_no_escribe_el_estado_ajeno() {
    let estado = tempfile::tempdir().expect("tmp");
    let duena = spawn_daemon_estado(estado.path()).await;
    let c = connected_client(&duena).await;
    let _: methods::SessionGetResult = c
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    let _: methods::SessionPutResult = c
        .call(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: 0,
                body: serde_json::json!({ "quien": "la dueña" }),
            },
        )
        .await
        .expect("session.put");

    // El segundo core arranca CON la pantalla —el lock decide quién escribe,
    // no quién lee— aunque todavía no esté en disco.
    let suelto = spawn_daemon_estado(estado.path()).await;
    let c2 = connected_client(&suelto).await;
    let g2: methods::SessionGetResult = c2
        .call(methods::SESSION_GET, &serde_json::json!({}))
        .await
        .expect("session.get");
    assert!(!g2.owner, "el segundo core corre suelto");
    // La revisión sale de SU `get` y no se da por cero: el core suelto carga lo
    // que haya en disco, y para cuando arranca, la dueña puede haber volcado ya
    // —el primer tick de su escritor es inmediato—. Fijar el cero aquí era
    // afirmar quién ganaba esa carrera, y bajo carga la perdía: rojo
    // intermitente en un test que no habla de revisiones.
    // Y desde la revisión de #237 el `put` de un core suelto se REHÚSA, igual
    // que en el brazo embebido: aceptarlo en memoria dejó de ser inocuo cuando
    // el escritor pudo tomar el lock tarde —el cuerpo aceptado suelto
    // sobrevivía a la adopción y se publicaba encima de la pantalla ajena—.
    let err = c2
        .call::<_, methods::SessionPutResult>(
            methods::SESSION_PUT,
            &methods::SessionPutParams {
                version: 1,
                revision: g2.session.revision,
                body: serde_json::json!({ "quien": "el suelto" }),
            },
        )
        .await
        .expect_err("un core suelto no escribe NI en memoria");
    match err {
        ClientError::Rpc(ref rpc) => {
            assert_eq!(rpc.data, Some(Error::PermissionDenied), "{:?}", rpc.data);
        }
        ref other => panic!("esperaba Rpc, fue {other:?}"),
    }
    let _: DaemonShutdownResult = c2
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Stop,
            },
        )
        .await
        .expect("daemon.shutdown");
    drop(c2);
    suelto.run.await.expect("join").expect("apagado limpio");

    // En disco no ha dejado NADA suyo. El fichero puede existir ya —la dueña
    // vuelca cada segundo, y este test no compite con ese reloj— pero lo que
    // diga es de ELLA. La afirmación no es «no hay fichero», que dependería
    // del tick, sino «el fichero no es del suelto», que no depende de nada.
    let fichero = norte_core::ui_session::disk::path(estado.path());
    let quien = |ruta: &std::path::Path| -> Option<String> {
        let raw = std::fs::read(ruta).ok()?;
        let s: methods::Session = serde_json::from_slice(&raw).ok()?;
        Some(s.body["quien"].to_string())
    };
    if let Some(q) = quien(&fichero) {
        assert_eq!(
            q, "\"la dueña\"",
            "un core suelto escribió el estado de otro"
        );
    }

    // Y al apagarse la dueña, el fichero es suyo sin ambigüedad: su volcado
    // final es el que manda.
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                mode: methods::ShutdownMode::Stop,
            },
        )
        .await
        .expect("daemon.shutdown");
    drop(c);
    duena.run.await.expect("join").expect("apagado limpio");
    assert_eq!(
        quien(&fichero).as_deref(),
        Some("\"la dueña\""),
        "el volcado final es el de la dueña"
    );
}

// ---------------------------------------------------------------------------
// El registro del daemon por el cable (#328, ADR 0092).
// ---------------------------------------------------------------------------

/// Un daemon con anillo de registro montado, y el anillo.
///
/// El anillo es EL MISMO objeto de los dos lados —el daemon lo sirve, el
/// subscriber del test escribe en él— porque en el proceso de verdad también
/// lo es: quien monta el registro es el binario, y el daemon solo lo sirve.
pub(super) async fn spawn_daemon_con_anillo() -> (TestDaemon, norte_config::logring::LogRing) {
    spawn_daemon_con_anillo_de(norte_config::logring::RING_DEFAULT).await
}

/// El mismo, con el anillo del tamaño que pida el test.
///
/// Un anillo PEQUEÑO es la única forma de llegar al desbordamiento sin emitir
/// dos mil líneas, y el desbordamiento es lo que hace comprobable el `lost`.
pub(super) async fn spawn_daemon_con_anillo_de(
    cap: usize,
) -> (TestDaemon, norte_config::logring::LogRing) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let anillo = norte_config::logring::LogRing::new(cap);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind")
    .with_log_ring(anillo.clone());
    let run = tokio::spawn(daemon.run());
    (
        TestDaemon {
            socket,
            run,
            _dir: dir,
            mem,
        },
        anillo,
    )
}

/// Encamina las líneas de ESTE hilo al anillo mientras viva el guard.
///
/// Un subscriber GLOBAL solo se puede instalar una vez por proceso, y estos
/// tests necesitan el suyo; el de ámbito lo resuelve, igual que `con_lineas`
/// en `norte-ui-host`. El daemon corre en el mismo hilo (el runtime de
/// `#[tokio::test]` es de un hilo), así que sus líneas entran también — que es
/// exactamente lo que pasa en el proceso de verdad.
///
/// Por la capa de `tracing` y no metiendo líneas a mano: el filtro por el que
/// pasa esa capa es donde vive la cota de `suppaftp`, y un atajo que se la
/// saltara probaría un camino que no existe.
pub(super) fn hacia_el_anillo(
    anillo: &norte_config::logring::LogRing,
) -> tracing::subscriber::DefaultGuard {
    use tracing_subscriber::layer::SubscriberExt as _;
    let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(anillo));
    tracing::subscriber::set_default(s)
}

/// LA prueba de este trabajo: la cota sigue viva al otro lado del socket.
///
/// `suppaftp` escribe `PASS <contraseña>` en TRACE (#43, regla 10), y el nivel
/// del anillo se sube DESDE la interfaz. Si subir el nivel por `log.level`
/// dejara pasar un target de terceros, una pulsación en un panel pondría una
/// contraseña en pantalla.
///
/// **Cubre el camino de `tracing`, no el del puente `log`.** `suppaftp` no
/// emite eventos de `tracing`: emite `log::trace!`, y `tracing-log` los
/// despacha con el `target` estático `"log"`. Ese otro camino ya está fijado
/// en `norte-config`
/// (`logring::tests::la_contrasena_no_entra_ni_por_el_puente_de_log`), y la
/// cota es la MISMA función para los dos, así que repetirlo por el socket
/// probaría dos veces lo mismo. Lo que este test añade es que subir el nivel
/// POR EL CABLE no la levanta; el nombre `suppaftp` está aquí porque es el
/// target que la lista blanca nombra, no porque éste sea su camino real.
#[tokio::test]
async fn subir_el_nivel_por_el_cable_no_levanta_la_cota() {
    let (d, anillo) = spawn_daemon_con_anillo().await;
    let _guard = hacia_el_anillo(&anillo);
    let c = connected_client(&d).await;

    let nivel: methods::LogLevelResult = c
        .call(methods::LOG_LEVEL, &serde_json::json!({ "level": "trace" }))
        .await
        .expect("el humano sube el nivel");
    assert_eq!(nivel.level, "trace", "el daemon contesta el que QUEDÓ");

    tracing::trace!(target: "suppaftp", "PASS secreto-de-verdad");
    tracing::trace!(target: "hyper::proto", "cabecera cruda");
    tracing::trace!(target: "norte_core::connect", "esto sí");

    let r: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 500 }),
        )
        .await
        .expect("el humano lee el registro");
    let mensajes: Vec<_> = r.lines.iter().map(|l| l.message.as_str()).collect();
    assert!(mensajes.iter().any(|m| m.contains("esto sí")));
    assert!(!mensajes.iter().any(|m| m.contains("secreto-de-verdad")));
    assert!(!mensajes.iter().any(|m| m.contains("cabecera cruda")));
}

/// El cursor sobrevive dos llamadas y no repite ni se salta líneas.
#[tokio::test]
async fn el_cursor_encadena_dos_llamadas() {
    let (d, anillo) = spawn_daemon_con_anillo().await;
    let _guard = hacia_el_anillo(&anillo);
    let c = connected_client(&d).await;

    tracing::info!(target: "norte_core::prueba", "primera");
    let a: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 500 }),
        )
        .await
        .expect("primera vuelta");
    assert!(a.lines.iter().any(|l| l.message.contains("primera")));
    tracing::info!(target: "norte_core::prueba", "segunda");
    let b: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": a.next, "max": 500 }),
        )
        .await
        .expect("segunda vuelta");
    assert!(b.lines.iter().any(|l| l.message.contains("segunda")));
    assert!(!b.lines.iter().any(|l| l.message.contains("primera")));
    // Sin cursor no se afirma ningún hueco: nadie perdió lo que nunca esperó.
    assert_eq!(a.lost, 0);
    assert_eq!(b.lost, 0, "un cursor al día no se perdió nada");
    // El nivel y el fondo de la historia viajan con las líneas, para que el
    // panel pueda decir «esto es todo lo que hay» y a qué nivel se capturó.
    assert_eq!(a.level, "info", "el anillo arranca en INFO");
    assert_eq!(
        a.capacity,
        u32::try_from(norte_config::logring::RING_DEFAULT).expect("cabe"),
    );
}

/// Un cursor que se quedó atrás recibe el hueco CONTADO, y no un cero.
///
/// Es lo único que hace honesto el sondeo, y es el caso que ninguno de los
/// otros tests toca: todos preguntan al día o sin cursor, así que el `lost` que
/// viaja por el cable siempre valía cero — sustituir esa cuenta por un `0`
/// literal en el daemon los habría dejado a todos verdes. El fallo que esto
/// impide es concreto: un panel sondea, la máquina se atasca treinta segundos
/// con el daemon a tope, el panel vuelve a preguntar y se le contesta un
/// registro con un salto y ninguna explicación — una línea que falta es
/// indistinguible de un suceso que no ocurrió.
///
/// Se afirma el número EXACTO, no `> 0`: un `lost` que solo tiene que ser
/// positivo lo cumple cualquier cuenta mal hecha, y este número alimenta una
/// marca de hueco que dice cuántas.
#[tokio::test]
async fn un_cursor_que_se_quedo_atras_recibe_el_hueco_contado() {
    const CAP: usize = 64;
    const EMITIDAS: u64 = 100;

    let (d, anillo) = spawn_daemon_con_anillo_de(CAP).await;
    let _guard = hacia_el_anillo(&anillo);
    let c = connected_client(&d).await;

    tracing::info!(target: "norte_core::prueba", "la última que este cliente vio");
    let a: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 500 }),
        )
        .await
        .expect("primera vuelta");
    assert_eq!(a.lost, 0, "sin cursor no se afirma hueco");

    // El cliente se queda parado mientras el daemon sigue trabajando, y el
    // anillo da la vuelta por debajo de su cursor.
    for i in 0..EMITIDAS {
        tracing::info!(target: "norte_core::prueba", "mientras no mirabas: {i}");
    }

    let b: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": a.next, "max": 500 }),
        )
        .await
        .expect("segunda vuelta");

    // Que entraron exactamente las mías y nada más es lo que hace legible el
    // número de abajo: sin esto, un `lost` distinto no diría si falla la
    // cuenta o si el daemon logueó por su cuenta.
    assert_eq!(
        b.next - a.next,
        EMITIDAS,
        "entre las dos vueltas entraron solo las líneas del test"
    );
    // De las 100 que entraron, el anillo solo conserva 64: las 36 primeras
    // —justo las que este cursor esperaba— se cayeron por detrás.
    assert_eq!(
        b.lost,
        EMITIDAS - u64::try_from(CAP).expect("cabe"),
        "el hueco se cuenta, no se calla"
    );
    assert_eq!(b.lines.len(), CAP, "y llega el anillo entero");
    assert!(
        !b.lines
            .iter()
            .any(|l| l.message.contains("la última que este cliente vio")),
        "esa ya se había caído: es de lo que el hueco cuenta"
    );
    // Y la vuelta siguiente, con el cursor al día, no arrastra el hueco de la
    // anterior: `lost` es de ESTE cursor, no de todo lo que el anillo tiró.
    let c2: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": b.next, "max": 500 }),
        )
        .await
        .expect("tercera vuelta");
    assert_eq!(c2.lost, 0, "un cursor al día no perdió nada");
}

/// `max` se acota en el servidor: pedir un millón no manda un millón.
#[tokio::test]
async fn el_servidor_acota_max() {
    let (d, anillo) = spawn_daemon_con_anillo().await;
    let _guard = hacia_el_anillo(&anillo);
    let c = connected_client(&d).await;

    for i in 0..1200 {
        tracing::info!(target: "norte_core::prueba", "l{i}");
    }
    let r: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": 0, "max": 100_000 }),
        )
        .await
        .expect("pedir de más no es un error");
    // EXACTAMENTE mil, que aquí es determinista: 1200 líneas emitidas en un
    // anillo de 2000, así que ninguna se cayó y el recorte es lo único que
    // limita. Un `<=` habría pasado igual con un servidor que contestara una
    // sola línea, o ninguna.
    assert_eq!(
        r.lines.len(),
        1000,
        "el servidor recorta a su tope, ni más ni menos"
    );
    // Y lo que no cupo NO se pierde: sigue después de `next`.
    let siguiente: methods::LogTailResult = c
        .call(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": r.next, "max": 500 }),
        )
        .await
        .expect("la vuelta siguiente recoge el resto");
    assert!(
        !siguiente.lines.is_empty(),
        "quedaban líneas después del recorte"
    );
}

/// `max: 0` es un error de params, no una página vacía.
///
/// Un panel que sondeara con cero recibiría una lista vacía cada vuelta con el
/// cursor parado, y en pantalla eso se lee como «no está pasando nada» en vez
/// de como el error de programación que es. Mismo criterio que
/// `FsListParams::limit`.
#[tokio::test]
async fn max_cero_no_es_una_pagina_vacia() {
    let (d, anillo) = spawn_daemon_con_anillo().await;
    let _guard = hacia_el_anillo(&anillo);
    let c = connected_client(&d).await;

    let err = c
        .call::<_, methods::LogTailResult>(
            methods::LOG_TAIL,
            &serde_json::json!({ "cursor": null, "max": 0 }),
        )
        .await
        .expect_err("cero no es una página");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

/// Un daemon SIN anillo dice que no lo tiene, y no contesta una lista vacía.
///
/// Es la diferencia que hace posible que el panel degrade a su registro local
/// DICIENDO por qué: un registro vacío y un registro ausente no pueden leerse
/// igual (#326). Es también el daemon compilado sin la feature `logging`, y el
/// que arrancó cuando ya había otro subscriber instalado.
#[tokio::test]
async fn sin_anillo_el_registro_no_existe_en_vez_de_estar_vacio() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;

    for (metodo, params) in [
        (
            methods::LOG_TAIL,
            serde_json::json!({ "cursor": null, "max": 10 }),
        ),
        (methods::LOG_LEVEL, serde_json::json!({ "level": "debug" })),
    ] {
        let err = c
            .call::<_, serde_json::Value>(metodo, &params)
            .await
            .expect_err("este daemon no tiene registro que servir");
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(rpc.data, Some(norte_proto::Error::Unsupported)),
                "{metodo}: se esperaba Unsupported, fue {:?}",
                rpc.data
            ),
            other => panic!("esperaba Rpc, fue {other:?}"),
        }
    }
}

/// Un nivel que no está en el vocabulario NO se degrada al de por defecto.
///
/// Aceptar lo que no se entiende y poner otra cosa dejaría al lector creyendo
/// que pidió algo que nadie hizo.
///
/// Y se rechaza con `INVALID_PARAMS`, que es OTRO error que el del daemon sin
/// anillo (`Unsupported`, ver
/// `sin_anillo_el_registro_no_existe_en_vez_de_estar_vacio`). Con un solo
/// código, un cliente no podría distinguir «este daemon no tiene registro» de
/// «mandé una errata», y las dos cosas piden respuestas distintas: la primera
/// degrada al anillo local para siempre, la segunda se corrige y se reintenta.
#[tokio::test]
async fn un_nivel_desconocido_se_rechaza_y_el_anillo_no_se_mueve() {
    let (d, anillo) = spawn_daemon_con_anillo().await;
    let _guard = hacia_el_anillo(&anillo);
    let c = connected_client(&d).await;

    let err = c
        .call::<_, methods::LogLevelResult>(
            methods::LOG_LEVEL,
            &serde_json::json!({ "level": "verboso-del-todo" }),
        )
        .await
        .expect_err("ese nivel no existe");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(
            rpc.code,
            codes::INVALID_PARAMS,
            "una errata no es una capacidad que falte"
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
    assert_eq!(
        anillo.level(),
        norte_config::logline::LogLevel::Info,
        "el anillo se queda donde estaba"
    );

    // Y bajar no baja: pedir menos verbosidad que la vigente contesta la
    // vigente, que es la respuesta honesta y no un fallo.
    let _: methods::LogLevelResult = c
        .call(methods::LOG_LEVEL, &serde_json::json!({ "level": "debug" }))
        .await
        .expect("sube a debug");
    let r: methods::LogLevelResult = c
        .call(methods::LOG_LEVEL, &serde_json::json!({ "level": "error" }))
        .await
        .expect("pedir menos no es un error");
    assert_eq!(r.level, "debug", "el anillo NUNCA baja");
}
