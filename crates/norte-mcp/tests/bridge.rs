//! Integración del puente MCP (M3-4 T5) contra un daemon REAL en tempdir:
//! handshake, tools/list, y cada tool reenviada con la gobernanza del daemon
//! (scope + policy) puesta. El puente se conduce por `handle_line` — sin
//! subprocesos ni stdio.
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use norte_core::approval::DenyAll;
use norte_core::daemon::{Daemon, DaemonConfig, DaemonError};
use norte_core::{Engine, OpSet, PolicyConfig, Scope, ScopeRegistry, ScopedPolicy};
use norte_mcp::bridge::Bridge;
use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_vfs::Provider;

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

struct TestDaemon {
    socket: PathBuf,
    _run: tokio::task::JoinHandle<Result<(), DaemonError>>,
    _dir: tempfile::TempDir,
    mem: Arc<MemProvider>,
    /// El MISMO registro que consulta el gate: los tests conceden directo
    /// (el round-trip request+grant por wire ya lo cubren los tests del
    /// daemon y el E2E de T7).
    scopes: ScopeRegistry,
}

/// Daemon con journal in-memory + regla `allow` + scopes compartidos (patrón
/// de `norte-core/tests/daemon.rs`, copiado — un crate no importa helpers de
/// los tests de otro).
async fn spawn_daemon_allow() -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"allow\"").expect("policy cfg");
    let policy = ScopedPolicy::new(scopes.clone(), cfg);
    let journal = Arc::new(norte_core::SqliteJournal::new(
        norte_core::Journal::open_in_memory()
            .await
            .expect("journal"),
    ));
    let engine =
        Arc::new(Engine::with_journal(journal).with_policy(Arc::new(policy), Arc::new(DenyAll)));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind_with_scopes(
        engine,
        scopes.clone(),
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon {
        socket,
        _run: run,
        _dir: dir,
        mem,
        scopes,
    }
}

/// Concede a `session` todo `mem:///proj` (directo al registro compartido).
fn grant_proj(d: &TestDaemon, session: &str) {
    d.scopes.grant(
        session,
        Scope::forever(vec![vp("mem:///proj")], OpSet::all()),
    );
}

/// `tools/call` por el puente; devuelve `(texto, is_error)` del content MCP.
async fn call_tool(b: &Bridge, name: &str, args: serde_json::Value) -> (String, bool) {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/call",
        "params": {"name": name, "arguments": args},
    });
    let out = b
        .handle_line(&req.to_string())
        .await
        .expect("una respuesta por request");
    let v: serde_json::Value = serde_json::from_str(&out).expect("respuesta JSON");
    assert_eq!(v["id"], 42, "el id se ecoa verbatim");
    let content = v["result"]["content"][0]["text"]
        .as_str()
        .expect("content de texto")
        .to_owned();
    let is_error = v["result"]["isError"].as_bool().expect("isError presente");
    (content, is_error)
}

#[tokio::test]
async fn initialize_ping_y_tools_list() {
    let d = spawn_daemon_allow().await;
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    let out = b
        .handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#)
        .await
        .expect("respuesta");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(v["result"]["serverInfo"]["name"], "norte-mcp");

    // Las notificaciones (sin id) JAMÁS producen respuesta (JSON-RPC).
    assert!(
        b.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await
            .is_none()
    );

    let out = b
        .handle_line(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#)
        .await
        .expect("respuesta");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["result"], serde_json::json!({}));

    let out = b
        .handle_line(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#)
        .await
        .expect("respuesta");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let names: Vec<&str> = v["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    for esperado in [
        "list_dir",
        "stat",
        "read_file",
        "copy",
        "move",
        "delete",
        "task_status",
        "request_scope",
    ] {
        assert!(names.contains(&esperado), "falta tool {esperado}");
    }

    // Método desconocido → error JSON-RPC -32601; JSON roto → -32700.
    let out = b
        .handle_line(r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#)
        .await
        .expect("respuesta");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["error"]["code"], -32601);
    let out = b.handle_line("{esto no es json").await.expect("respuesta");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["error"]["code"], -32700);
    assert_eq!(v["id"], serde_json::Value::Null);
}

#[tokio::test]
async fn list_dir_y_stat_leen_bajo_scope() {
    // Las LECTURAS pasan por el gate de scope igual que las mutaciones (#80):
    // un agente lee SOLO bajo un scope concedido. Aquí se concede antes.
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/a.txt", b"hola").await;
    grant_proj(&d, "claude");
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    let (out, err) = call_tool(&b, "list_dir", serde_json::json!({"path": "mem:///proj"})).await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload json");
    assert_eq!(v["entries"][0]["path"], "mem:///proj/a.txt");
    assert_eq!(v["entries"][0]["kind"], "file");

    let (out, err) = call_tool(&b, "stat", serde_json::json!({"path": "mem:///proj/a.txt"})).await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload json");
    assert_eq!(v["size"], 4);
}

/// #80: un agente SIN scope recibe la denegación ACCIONABLE (menciona
/// `request_scope`) en las lecturas, igual que ya la recibe en `copy`.
#[tokio::test]
async fn lecturas_sin_scope_son_accionables() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/a.txt", b"hola").await;
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    for (tool, args) in [
        ("list_dir", serde_json::json!({"path": "mem:///proj"})),
        ("stat", serde_json::json!({"path": "mem:///proj/a.txt"})),
        (
            "read_file",
            serde_json::json!({"path": "mem:///proj/a.txt"}),
        ),
    ] {
        let (out, err) = call_tool(&b, tool, args).await;
        assert!(err, "{tool}: sin scope debe fallar");
        assert!(
            out.contains("out-of-scope") && out.contains("request_scope"),
            "{tool}: error accionable, fue: {out}"
        );
    }
}

#[tokio::test]
async fn read_file_texto_y_binario_fiel() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/texto.txt", b"hola \xc3\xb1").await;
    write_file(&d.mem, "mem:///proj/crudo.bin", &[0x68, 0xE9, 0x00, 0xFF]).await;
    grant_proj(&d, "claude");
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    let (out, err) = call_tool(
        &b,
        "read_file",
        serde_json::json!({"path": "mem:///proj/texto.txt"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["text"], "hola ñ");
    assert!(v.get("base64").is_none(), "UTF-8 válido: sin base64");
    assert_eq!(v["eof"], true);

    let (out, err) = call_tool(
        &b,
        "read_file",
        serde_json::json!({"path": "mem:///proj/crudo.bin"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    let fieles = base64::engine::general_purpose::STANDARD
        .decode(v["base64"].as_str().expect("base64 presente"))
        .expect("base64 válido");
    assert_eq!(fieles, vec![0x68, 0xE9, 0x00, 0xFF], "bytes exactos");
    assert!(v["text"].as_str().expect("text").contains('\u{FFFD}'));
}

#[tokio::test]
async fn copy_con_scope_completa_y_fuera_de_scope_es_accionable() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/src.txt", b"hola").await;
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    // Sin scope: el error de tool es ACCIONABLE (menciona request_scope).
    let (out, err) = call_tool(
        &b,
        "copy",
        serde_json::json!({"from": "mem:///proj/src.txt", "to": "mem:///proj/dst.txt"}),
    )
    .await;
    assert!(err, "sin scope debe fallar");
    assert!(
        out.contains("out-of-scope") && out.contains("request_scope"),
        "accionable para el agente: {out}"
    );

    // Con scope: completa y el archivo existe.
    grant_proj(&d, "claude");
    let (out, err) = call_tool(
        &b,
        "copy",
        serde_json::json!({"from": "mem:///proj/src.txt", "to": "mem:///proj/dst.txt"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["state"], "completed");
    assert!(d.mem.stat(&vp("mem:///proj/dst.txt")).await.is_ok());

    // task_status del task recién terminado (retenido en recientes).
    let task_id = v["task_id"].as_u64().expect("task_id");
    let (out, err) = call_tool(&b, "task_status", serde_json::json!({"task_id": task_id})).await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["state"], "completed");
}

#[tokio::test]
async fn delete_default_trash_y_permanent_explicito() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/victima.txt", b"x").await;
    grant_proj(&d, "claude");
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    // Default trash: la papelera lógica del testkit lo acepta — completa y
    // la víctima desaparece de la vista (RECUPERABLE: el journal registra
    // Trashed con destino; el undo de M3-2 restaura desde ahí).
    let (out, err) = call_tool(
        &b,
        "delete",
        serde_json::json!({"path": "mem:///proj/victima.txt"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["state"], "completed");
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/victima.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));

    // Permanent explícito: borra (la policy allow del harness lo permite;
    // con la policy de ejemplo esto sería un ask/deny).
    write_file(&d.mem, "mem:///proj/victima2.txt", b"x").await;
    let (out, err) = call_tool(
        &b,
        "delete",
        serde_json::json!({"path": "mem:///proj/victima2.txt", "mode": "permanent"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["state"], "completed");
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/victima2.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));

    // Modo inventado → error de tool, sin llamar al daemon.
    let (out, err) = call_tool(
        &b,
        "delete",
        serde_json::json!({"path": "mem:///proj/x", "mode": "shred"}),
    )
    .await;
    assert!(err);
    assert!(out.contains("invalid mode"), "{out}");
}

#[tokio::test]
async fn request_scope_devuelve_id_y_hint_humano() {
    let d = spawn_daemon_allow().await;
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");
    let (out, err) = call_tool(
        &b,
        "request_scope",
        serde_json::json!({"roots": ["mem:///proj"], "ops": ["copy"], "ttl_ms": 60000}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert!(v["request_id"].is_u64());
    assert!(
        v["hint"]
            .as_str()
            .expect("hint")
            .contains("norte policy grant"),
        "el agente sabe qué pedirle al humano: {out}"
    );
}

#[tokio::test]
async fn vpath_invalido_es_error_de_tool_local() {
    let d = spawn_daemon_allow().await;
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");
    let (out, err) = call_tool(&b, "stat", serde_json::json!({"path": "no-es-un-vpath"})).await;
    assert!(err);
    assert!(out.contains("invalid VPath"), "{out}");
    // Tool desconocida → error de tool (no de protocolo).
    let (out, err) = call_tool(&b, "write_file", serde_json::json!({})).await;
    assert!(err);
    assert!(out.contains("unknown tool"), "{out}");
}

#[tokio::test]
async fn sesion_ilegal_rechazada_en_connect() {
    let d = spawn_daemon_allow().await;
    let Err(err) = Bridge::connect(&d.socket, "con espacios").await else {
        panic!("el daemon valida el charset en el handshake");
    };
    assert!(matches!(err, norte_mcp::bridge::BridgeError::Daemon(_)));
}

/// La tool `move` (rust M3: única sin cobertura, y ahora serializa SU
/// `FsMoveParams`): mueve dentro del scope y el origen desaparece.
#[tokio::test]
async fn move_renombra_dentro_del_scope() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/viejo.txt", b"hola").await;
    grant_proj(&d, "claude");
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    let (out, err) = call_tool(
        &b,
        "move",
        serde_json::json!({"from": "mem:///proj/viejo.txt", "to": "mem:///proj/nuevo.txt"}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    assert_eq!(v["state"], "completed");
    assert!(d.mem.stat(&vp("mem:///proj/nuevo.txt")).await.is_ok());
    assert!(matches!(
        d.mem.stat(&vp("mem:///proj/viejo.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
}

/// Regla 1 en la superficie NUEVA (encoding H1): un nombre hostil que NACE
/// como bytes en el provider viaja al agente como wire form (%XX, jamás
/// lossy); el agente lo ECOA en copy y el destino tiene LOS MISMOS bytes.
/// Corpus completo de norte-testkit.
#[tokio::test]
async fn nombre_hostil_round_trip_byte_fiel_por_el_puente() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    d.mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");
    d.scopes.grant(
        "claude",
        Scope::forever(vec![vp("mem:///proj"), vp("mem:///dst")], OpSet::all()),
    );
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    for n in norte_testkit::corpus::hostile_names() {
        // Origen = BYTES, sin pasar por String en ningún momento.
        let seg = norte_proto::Segment::new(n.bytes.clone()).expect("segmento del corpus");
        let src = vp("mem:///proj").join(seg);
        let mut sink = d.mem.write(&src).await.expect("write");
        sink.write(Bytes::from_static(b"payload"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");

        // 1) list_dir emite el wire form fiel (nunca U+FFFD).
        let (out, err) =
            call_tool(&b, "list_dir", serde_json::json!({"path": "mem:///proj"})).await;
        assert!(!err, "{}: {out}", n.id);
        let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
        let wire = v["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .filter_map(|e| e["path"].as_str())
            .find(|w| VPath::parse(w).is_ok_and(|p| p == src))
            .unwrap_or_else(|| panic!("{}: el listado no trae el wire fiel", n.id))
            .to_owned();
        assert!(
            !wire.contains('\u{FFFD}'),
            "{}: lossy en el wire: {wire}",
            n.id
        );

        // 2) El agente ECOA ese string en copy → mismos bytes en el destino.
        let dst_wire = format!(
            "mem:///dst/{}",
            wire.strip_prefix("mem:///proj/").expect("hijo de proj")
        );
        let (out, err) = call_tool(
            &b,
            "copy",
            serde_json::json!({"from": wire, "to": dst_wire}),
        )
        .await;
        assert!(!err, "{}: {out}", n.id);
        let dst = VPath::parse(&dst_wire).expect("dst wire");
        let entry = d
            .mem
            .stat(&dst)
            .await
            .unwrap_or_else(|e| panic!("{}: destino no existe: {e}", n.id));
        assert_eq!(
            entry.path.file_name().expect("nombre").as_bytes(),
            n.bytes.as_slice(),
            "{}: los bytes del nombre difieren tras el round-trip",
            n.id
        );
        d.mem.remove(&src).await.expect("limpia src");
        d.mem.remove(&dst).await.expect("limpia dst");
    }
}

/// Encoding H2: un rango que PARTE un carácter multibyte hace parecer
/// binario un texto — el chunk cae a lossy MARCADO + base64 byte-exacto
/// (jamás pérdida; la description de la tool manda reensamblar por base64).
#[tokio::test]
async fn read_file_frontera_multibyte_cae_a_base64_fiel() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    // "año…": offset=1,len=1 corta la ñ (0xC3 0xB1) → chunk [0xC3].
    write_file(&d.mem, "mem:///proj/texto.txt", "año 2026\n".as_bytes()).await;
    grant_proj(&d, "claude"); // #80: leer exige scope
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");
    let (out, err) = call_tool(
        &b,
        "read_file",
        serde_json::json!({"path": "mem:///proj/texto.txt", "offset": 1, "len": 1}),
    )
    .await;
    assert!(!err, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("payload");
    let fieles = base64::engine::general_purpose::STANDARD
        .decode(v["base64"].as_str().expect("base64 presente"))
        .expect("base64 válido");
    assert_eq!(fieles, [0xC3], "byte exacto de la ñ partida");
    assert_eq!(v["text"], "\u{FFFD}");
}

/// Criterio único de args (sec MINOR-1 / enc H3): un argumento PRESENTE con
/// tipo ilegal es error de tool — jamás degradación silenciosa.
#[tokio::test]
async fn args_mal_tipados_son_error_no_degradacion() {
    let d = spawn_daemon_allow().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/f.txt", b"x").await;
    grant_proj(&d, "claude");
    let b = Bridge::connect(&d.socket, "claude").await.expect("connect");

    // mode numérico: NO cae a trash en silencio.
    let (out, err) = call_tool(
        &b,
        "delete",
        serde_json::json!({"path": "mem:///proj/f.txt", "mode": 123}),
    )
    .await;
    assert!(err);
    assert!(out.contains("invalid mode"), "{out}");
    assert!(
        d.mem.stat(&vp("mem:///proj/f.txt")).await.is_ok(),
        "intacto"
    );

    // offset float: NO lee desde 0 fingiendo aplicarlo.
    let (out, err) = call_tool(
        &b,
        "read_file",
        serde_json::json!({"path": "mem:///proj/f.txt", "offset": 2.5}),
    )
    .await;
    assert!(err);
    assert!(out.contains("offset"), "{out}");

    // limit string: error, no ignorado.
    let (out, err) = call_tool(
        &b,
        "list_dir",
        serde_json::json!({"path": "mem:///proj", "limit": "muchos"}),
    )
    .await;
    assert!(err);
    assert!(out.contains("limit"), "{out}");
}
