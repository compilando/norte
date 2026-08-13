//! E2E del CRITERIO DE SALIDA de M3 (spec §M3): "Claude Code gestiona un
//! directorio real bajo policy `ask`, con undo de sesión completa". Todo
//! in-process (daemon UDS en tempdir + puente MCP + cliente humano): el
//! flujo entero `request_scope` → grant → copy(ask) → approval → decide →
//! completed → undo → revertido, y la frontera de scope que sigue cerrada.
#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use norte_core::daemon::{Client, Daemon, DaemonApprovalResolver, DaemonConfig};
use norte_core::{Engine, PolicyConfig, ScopeRegistry, ScopedPolicy};
use norte_mcp::bridge::Bridge;
use norte_proto::VPath;
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// `tools/call` por el puente → `(texto, is_error)`.
async fn call_tool(b: &Bridge, name: &str, args: serde_json::Value) -> (String, bool) {
    let req = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": args},
    });
    let out = b.handle_line(&req.to_string()).await.expect("respuesta");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let content = v["result"]["content"][0]["text"]
        .as_str()
        .expect("content")
        .to_owned();
    (content, v["result"]["isError"].as_bool().expect("isError"))
}

/// El criterio de salida de M3, extremo a extremo.
/// Daemon REAL con journal in-memory + policy `ask` + approval router, con
/// `mem:///proj/informe.txt` sembrado. Devuelve `(tempdir, socket, mem)`.
async fn spawn_ask_daemon() -> (tempfile::TempDir, std::path::PathBuf, Arc<MemProvider>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("policy");
    let policy = ScopedPolicy::new(scopes.clone(), cfg);
    let approvals = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
    let journal = Arc::new(norte_core::SqliteJournal::new(
        norte_core::Journal::open_in_memory()
            .await
            .expect("journal"),
    ));
    let engine = Arc::new(
        Engine::with_journal(journal).with_policy(Arc::new(policy), Arc::clone(&approvals) as _),
    );
    // El spool de `sync.plan`, bajo el mismo tempdir del socket (ADR 0049):
    // sin él, `sync.plan` responde `Unsupported` (ver `daemon.rs` de
    // norte-core, que documenta el mismo requisito).
    engine.set_spool(norte_core::sync::Spool::new(dir.path()));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&mem, "mem:///proj/informe.txt", b"datos importantes").await;
    let daemon = Daemon::bind_with_policy(
        engine,
        scopes,
        approvals,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: None,
        },
    )
    .await
    .expect("bind");
    tokio::spawn(daemon.run());
    (dir, socket, mem)
}

/// Espera el terminal de `task_id` por `task.list` (con tope).
async fn wait_undo_terminal(human: &Client, task_id: norte_proto::TaskId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let listed: norte_proto::methods::TaskListResult = human
            .call(
                norte_proto::methods::TASK_LIST,
                &norte_proto::methods::TaskListParams {},
            )
            .await
            .expect("task.list");
        if let Some(t) = listed.tasks.iter().find(|t| t.task_id == task_id)
            && t.state.is_terminal()
        {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "el undo no terminó");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Espera el terminal del undo y comprueba su informe (#71): 1 revertida,
/// nada saltado ni bloqueado — el «done» deja de ser a ciegas.
async fn assert_undo_report_clean(human: &Client, task_id: norte_proto::TaskId) {
    wait_undo_terminal(human, task_id).await;
    let report: norte_proto::methods::PolicyUndoReportResult = human
        .call(
            norte_proto::methods::POLICY_UNDO_REPORT,
            &norte_proto::methods::PolicyUndoReportParams { task_id },
        )
        .await
        .expect("undo_report");
    assert_eq!(report.undone, 1, "la copia revertida se cuenta");
    assert_eq!(report.skipped_created_no_trash, 0);
    assert!(report.blocked.is_none(), "sin bloqueo: {report:?}");
}

/// El criterio de salida de M3, extremo a extremo.
#[tokio::test]
async fn criterio_de_salida_m3_agente_bajo_ask_con_undo() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;

    // --- El HUMANO: conexión User que concede, aprueba y deshace ---
    let mut human = Client::connect(&socket).await.expect("connect humano");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("initialize humano");

    // --- El AGENTE: puente MCP declarando la sesión ---
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("connect agente");

    // 1) El agente pide scope para su proyecto.
    let (out, err) = call_tool(
        &agent,
        "request_scope",
        serde_json::json!({"roots": ["mem:///proj"], "ops": ["copy"], "ttl_ms": 60000}),
    )
    .await;
    assert!(!err, "{out}");
    let req_id = serde_json::from_str::<serde_json::Value>(&out).expect("json")["request_id"]
        .as_u64()
        .expect("request_id");

    // 2) El humano lo concede.
    let _: norte_proto::methods::GrantScopeResult = human
        .call(
            norte_proto::methods::POLICY_GRANT_SCOPE,
            &norte_proto::methods::GrantScopeParams { request_id: req_id },
        )
        .await
        .expect("grant");

    // 3) El agente copia — queda SUSPENDIDA en el `ask` (spawn: el humano debe
    //    seguir atendido para aprobar).
    let copy = tokio::spawn(async move {
        let r = call_tool(
            &agent,
            "copy",
            serde_json::json!({"from": "mem:///proj/informe.txt", "to": "mem:///proj/copia.txt"}),
        )
        .await;
        (agent, r)
    });

    // 4) El humano recibe `policy.approval_required` y aprueba.
    let approval_id = loop {
        let n = human.notification().await.expect("canal vivo");
        if n.method == norte_proto::methods::POLICY_APPROVAL_REQUIRED {
            let req: norte_proto::methods::PolicyApprovalRequired =
                serde_json::from_value(n.params.expect("params")).expect("shape");
            assert_eq!(req.op, "copy");
            assert_eq!(req.session.as_deref(), Some("claude"));
            break req.approval_id;
        }
    };
    let _: norte_proto::methods::PolicyDecideResult = human
        .call(
            norte_proto::methods::POLICY_DECIDE,
            &norte_proto::methods::PolicyDecideParams {
                approval_id,
                approve: true,
            },
        )
        .await
        .expect("decide approve");

    // 5) La copia procede; el destino existe.
    let (agent, (out, err)) = tokio::time::timeout(Duration::from_secs(5), copy)
        .await
        .expect("no cuelga")
        .expect("join");
    assert!(!err, "aprobada debe completar: {out}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out).expect("json")["state"],
        "completed"
    );
    assert!(mem.stat(&vp("mem:///proj/copia.txt")).await.is_ok());

    // 6) El humano deshace la sesión COMPLETA del agente → la copia se revierte,
    //    el original sigue intacto.
    let undone: norte_proto::methods::PolicyUndoSessionResult = human
        .call(
            norte_proto::methods::POLICY_UNDO_SESSION,
            &norte_proto::methods::PolicyUndoSessionParams {
                session: "claude".into(),
            },
        )
        .await
        .expect("undo_session");
    // 6b) Terminal + informe (#71): 1 revertida, nada saltado ni bloqueado.
    assert_undo_report_clean(&human, undone.task_id).await;
    assert!(
        matches!(
            mem.stat(&vp("mem:///proj/copia.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "la copia del agente se revirtió"
    );
    assert!(
        mem.stat(&vp("mem:///proj/informe.txt")).await.is_ok(),
        "el original jamás se tocó"
    );

    // 7) Fuera de scope SIGUE cerrado: la concesión no fue un cheque en blanco.
    let (out, err) = call_tool(
        &agent,
        "copy",
        serde_json::json!({"from": "mem:///proj/informe.txt", "to": "mem:///fuera.txt"}),
    )
    .await;
    assert!(err, "fuera del scope debe fallar");
    assert!(
        out.contains("out-of-scope") && out.contains("request_scope"),
        "denegación accionable: {out}"
    );
}

/// #155 en miniatura: el puente puede ABRIR el brazo que drena
/// notificaciones, y lo abre UNA vez. Sin esto, `compare` y `sync_plan` no
/// tienen por dónde recibir sus filas.
///
/// Y el brazo es del AGENTE, no de un usuario: se comprueba con el gate de
/// lectura, que para `Actor::Agent` exige un scope vivo y para `Actor::User`
/// no exige nada. Si el brazo se abriera con el `connect` pelado, la lista de
/// abajo saldría bien — y el puente habría blanqueado el actor.
#[tokio::test]
async fn el_puente_abre_su_brazo_de_streams_una_sola_vez() {
    let (_dir, socket, _mem) = spawn_ask_daemon().await;
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("connect agente");

    let primero = agent.streams().await.expect("primer brazo");
    let segundo = agent.streams().await.expect("segundo brazo");
    assert!(
        std::ptr::eq(primero, segundo),
        "el brazo se abre perezosamente pero UNA vez: dos conexiones por sesión \
         serían dos conn_id y ningún beneficio"
    );

    // El brazo declara `agent_session`: sin scope concedido, el gate de
    // lectura del daemon lo veda. Un `connect` pelado (actor User) listaría.
    let denegado = primero.list(&vp("mem:///proj")).await;
    assert!(
        matches!(denegado, Err(norte_proto::Error::PolicyDenied { .. })),
        "el brazo tiene que ser una conexión de AGENTE (sin scope, vedada), fue {denegado:?}"
    );
}

/// Pide y concede scope de LECTURA para `roots` (helper de test): `compare`
/// pasa por el mismo `read_gate` que `list`/`stat` (ver
/// `el_puente_abre_su_brazo_de_streams_una_sola_vez`), así que una tool que
/// solo lee necesita scope concedido igual que una que muta — la op elegida
/// (`copy`) es irrelevante para `covers_read`, que solo mira la raíz.
async fn grant_read_scope(agent: &Bridge, human: &Client, roots: &[&str]) {
    let (out, err) = call_tool(
        agent,
        "request_scope",
        serde_json::json!({"roots": roots, "ops": ["copy"], "ttl_ms": 60000}),
    )
    .await;
    assert!(!err, "{out}");
    let req_id = serde_json::from_str::<serde_json::Value>(&out).expect("json")["request_id"]
        .as_u64()
        .expect("request_id");
    let _: norte_proto::methods::GrantScopeResult = human
        .call(
            norte_proto::methods::POLICY_GRANT_SCOPE,
            &norte_proto::methods::GrantScopeParams { request_id: req_id },
        )
        .await
        .expect("grant");
}

/// El agente ve QUÉ difiere, con el vocabulario del wire y no con etiquetas
/// traducidas: un resultado de tool que cambia con el idioma del operador no
/// es un contrato.
#[tokio::test]
async fn compare_devuelve_las_filas_con_valores_de_wire() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///a")).await.expect("mkdir a");
    mem.mkdir(&vp("mem:///b")).await.expect("mkdir b");
    write_file(&mem, "mem:///a/x.txt", b"hola").await;

    let mut human = Client::connect(&socket).await.expect("connect humano");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("initialize humano");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("connect agente");
    grant_read_scope(&agent, &human, &["mem:///a", "mem:///b"]).await;

    let (out, err) = call_tool(
        &agent,
        "compare",
        serde_json::json!({"a": "mem:///a", "b": "mem:///b"}),
    )
    .await;
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    let filas = payload["rows"].as_array().expect("rows");
    assert!(!filas.is_empty(), "x.txt solo está en un lado");
    // El veredicto viaja como valor de WIRE (`only_left`, verificado contra
    // el serde de `CompareVerdict`), no como `left_only` — el plan lo
    // adivinaba mal.
    assert!(
        filas.iter().any(|f| f["verdict"] == "only_left"),
        "el veredicto viaja como valor de wire: {payload}"
    );
    assert_eq!(payload["truncated"], false, "{payload}");
    assert_eq!(payload["complete"], true, "{payload}");
}

/// El tope de filas corta la comparación, la CANCELA, y lo DICE: una
/// truncación silenciosa sería peor que el tope — un modelo que la lea como
/// completa reportaría dos árboles como iguales sin haberlos visto enteros.
#[tokio::test]
async fn compare_con_limit_bajo_trunca_y_cancela() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///a")).await.expect("mkdir a");
    mem.mkdir(&vp("mem:///b")).await.expect("mkdir b");
    for i in 0..5 {
        write_file(&mem, &format!("mem:///a/f{i}.txt"), b"x").await;
    }

    let mut human = Client::connect(&socket).await.expect("connect humano");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("initialize humano");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("connect agente");
    grant_read_scope(&agent, &human, &["mem:///a", "mem:///b"]).await;

    let (out, err) = call_tool(
        &agent,
        "compare",
        serde_json::json!({"a": "mem:///a", "b": "mem:///b", "limit": 2}),
    )
    .await;
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        payload["rows"].as_array().expect("rows").len(),
        2,
        "{payload}"
    );
    assert_eq!(payload["truncated"], true, "{payload}");
    assert_eq!(
        payload["complete"], false,
        "truncado nunca es completo: {payload}"
    );
}

/// Regla 1 en `compare` (encoding-auditor, revisión de la tarea 2): un nombre
/// hostil que nace como BYTES en el provider viaja hasta la fila de tool como
/// `to_wire()` fiel, jamás lossy — corpus completo de norte-testkit, igual
/// que `nombre_hostil_round_trip_byte_fiel_por_el_puente` cubre `list_dir`.
#[tokio::test]
async fn compare_nombre_hostil_viaja_como_wire_fiel() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///a")).await.expect("mkdir a");
    mem.mkdir(&vp("mem:///b")).await.expect("mkdir b");

    let corpus = norte_testkit::corpus::hostile_names();
    for n in &corpus {
        let seg = norte_proto::Segment::new(n.bytes.clone()).expect("segmento del corpus");
        let src = vp("mem:///a").join(seg);
        let mut sink = mem.write(&src).await.expect("write");
        sink.write(Bytes::from_static(b"x")).await.expect("chunk");
        sink.commit().await.expect("commit");
    }

    let mut human = Client::connect(&socket).await.expect("connect humano");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("initialize humano");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("connect agente");
    grant_read_scope(&agent, &human, &["mem:///a", "mem:///b"]).await;

    // Tope holgado: el corpus no se acerca al default (500), y esto prueba
    // el camino SIN truncar (el truncado ya tiene su propio test).
    let (out, err) = call_tool(
        &agent,
        "compare",
        serde_json::json!({"a": "mem:///a", "b": "mem:///b", "limit": 1000}),
    )
    .await;
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(payload["truncated"], false, "{payload}");
    let filas = payload["rows"].as_array().expect("rows");
    assert_eq!(filas.len(), corpus.len(), "una fila por nombre: {payload}");

    for n in &corpus {
        let seg = norte_proto::Segment::new(n.bytes.clone()).expect("segmento del corpus");
        let src = vp("mem:///a").join(seg);
        let fila = filas
            .iter()
            .find(|f| {
                f["left"]["path"]
                    .as_str()
                    .is_some_and(|w| VPath::parse(w).is_ok_and(|p| p == src))
            })
            .unwrap_or_else(|| panic!("{}: compare no trae la fila fiel: {payload}", n.id));
        // Solo existe a la izquierda: el veredicto de wire lo dice (`only_left`,
        // no `left_only` — el plan lo adivinaba mal). SALVO que dos entradas
        // del corpus colapsen a la misma clave de emparejamiento en ESTE lado
        // (p. ej. NFC/NFD, `nfd_e_acute` contra su forma compuesta): ahí el
        // veredicto es `ambiguous` con `side: left` — sigue siendo UNA fila
        // fiel por nombre, que es lo que este test comprueba.
        let verdict = fila["verdict"].as_str().expect("verdict");
        assert!(
            verdict == "only_left" || verdict == "ambiguous",
            "{}: veredicto inesperado: {fila}",
            n.id
        );
        if verdict == "ambiguous" {
            assert_eq!(fila["side"], "left", "{}: {fila}", n.id);
        }
        let wire = fila["left"]["path"].as_str().expect("path");
        assert!(
            !wire.contains('\u{FFFD}'),
            "{}: lossy en el wire: {wire}",
            n.id
        );
    }
}

/// El agente ve QUÉ haría una sincronización, y la descripción de la tool le
/// dice que el hash NO le sirve a nadie más.
#[tokio::test]
async fn sync_plan_devuelve_los_pasos_y_no_aplica_nada() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///src")).await.expect("mkdir src");
    mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");
    write_file(&mem, "mem:///src/nuevo.txt", b"hola").await;

    let mut human = Client::connect(&socket).await.expect("connect humano");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("initialize humano");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("connect agente");
    grant_read_scope(&agent, &human, &["mem:///src", "mem:///dst"]).await;

    let (out, err) = call_tool(
        &agent,
        "sync_plan",
        serde_json::json!({"source": "mem:///src", "dest": "mem:///dst", "mode": "update"}),
    )
    .await;
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        payload["steps"].as_array().expect("steps").len(),
        1,
        "{payload}"
    );
    assert_eq!(payload["truncated"], false, "{payload}");
    assert_eq!(payload["complete"], true, "{payload}");
    assert!(
        payload["counts"].is_object(),
        "los totales que el core sí conoce"
    );
    assert!(payload.get("dest_trash").is_some(), "{payload}");
    assert!(payload["blockers"].is_array(), "{payload}");
    // El hash NO viaja: no le sirve a nadie fuera de la conexión de streams.
    assert!(payload.get("plan_hash").is_none(), "{payload}");
    // `task_id` tampoco: sin el hash no sirve para correlacionar nada aquí.
    assert!(payload.get("task_id").is_none(), "{payload}");

    // Y el destino sigue vacío: planear no escribe.
    assert!(
        mem.stat(&vp("mem:///dst/nuevo.txt")).await.is_err(),
        "sync_plan no aplica nada"
    );
}

/// No hay tool de aplicar, y eso es la decisión, no un olvido.
#[tokio::test]
async fn no_existe_una_tool_de_aplicar() {
    let (_dir, socket, _mem) = spawn_ask_daemon().await;
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("connect agente");
    let out = agent
        .handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .await
        .expect("respuesta");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let defs = v["result"]["tools"].as_array().expect("tools");
    assert!(
        defs.iter().any(|t| t["name"] == "sync_plan"),
        "sync_plan tiene que estar: {v}"
    );
    assert!(
        !defs.iter().any(|t| t["name"] == "sync_apply"),
        "aplicar es acción de un humano en su propio cliente (spec 3 §2.1): {v}"
    );
}

/// `mode` ausente o de tipo/valor ilegal es error, NUNCA un default silencioso
/// — el mismo precedente que `tool_delete::mode` (spec 3, tarea 3): el wire
/// tampoco tiene un valor neutro entre `update` y `mirror`.
#[tokio::test]
async fn sync_plan_mode_malformado_o_ausente_es_error_no_default() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///src")).await.expect("mkdir src");
    mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");

    let mut human = Client::connect(&socket).await.expect("connect humano");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("initialize humano");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("connect agente");
    grant_read_scope(&agent, &human, &["mem:///src", "mem:///dst"]).await;

    // Ausente: NO cae a `update` en silencio.
    let (out, err) = call_tool(
        &agent,
        "sync_plan",
        serde_json::json!({"source": "mem:///src", "dest": "mem:///dst"}),
    )
    .await;
    assert!(err, "{out}");
    assert!(out.contains("mode"), "{out}");

    // Tipo ilegal.
    let (out, err) = call_tool(
        &agent,
        "sync_plan",
        serde_json::json!({"source": "mem:///src", "dest": "mem:///dst", "mode": 7}),
    )
    .await;
    assert!(err, "{out}");
    assert!(out.contains("invalid mode"), "{out}");

    // Valor de string ilegal (ninguno de los dos del wire).
    let (out, err) = call_tool(
        &agent,
        "sync_plan",
        serde_json::json!({"source": "mem:///src", "dest": "mem:///dst", "mode": "obliterate"}),
    )
    .await;
    assert!(err, "{out}");
    assert!(out.contains("invalid mode"), "{out}");
}

/// El tope de pasos corta el plan, lo CANCELA, y el payload lo dice sin
/// inventar nada: `sync.plan_done` no llega tras cancelar (`run_sync_plan` no
/// lo emite en el camino de error), así que `counts`/`dest_trash`/`blockers`
/// tienen que quedar AUSENTES — nunca en cero, que un modelo leería como "sin
/// bloqueos" cuando en realidad no se sabe.
#[tokio::test]
async fn sync_plan_con_limit_bajo_trunca_y_no_trae_lo_que_no_supo() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    mem.mkdir(&vp("mem:///src")).await.expect("mkdir src");
    mem.mkdir(&vp("mem:///dst")).await.expect("mkdir dst");
    for i in 0..5 {
        write_file(&mem, &format!("mem:///src/f{i}.txt"), b"x").await;
    }

    let mut human = Client::connect(&socket).await.expect("connect humano");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("initialize humano");
    let agent = Bridge::connect(&socket, "claude")
        .await
        .expect("connect agente");
    grant_read_scope(&agent, &human, &["mem:///src", "mem:///dst"]).await;

    let (out, err) = call_tool(
        &agent,
        "sync_plan",
        serde_json::json!({"source": "mem:///src", "dest": "mem:///dst", "mode": "update", "limit": 2}),
    )
    .await;
    assert!(!err, "{out}");
    let payload: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(
        payload["steps"].as_array().expect("steps").len(),
        2,
        "{payload}"
    );
    assert_eq!(payload["truncated"], true, "{payload}");
    assert_eq!(
        payload["complete"], false,
        "truncado nunca es completo: {payload}"
    );
    for ausente in [
        "counts",
        "dest_trash",
        "blockers",
        "blockers_total",
        "executable",
    ] {
        assert!(
            payload.get(ausente).is_none(),
            "{ausente} no puede inventarse tras truncar: {payload}"
        );
    }
    assert!(payload.get("plan_hash").is_none(), "{payload}");

    // Y el destino sigue vacío: truncado tampoco aplica nada.
    for i in 0..5 {
        assert!(
            mem.stat(&vp(&format!("mem:///dst/f{i}.txt")))
                .await
                .is_err(),
            "sync_plan no aplica nada, ni truncado"
        );
    }
}
