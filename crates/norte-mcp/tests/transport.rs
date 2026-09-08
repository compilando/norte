//! #67: el transporte del puente NO es serial — un tool suspendido (ask de
//! policy / task larga) no retiene `ping` ni `notifications/cancelled`.
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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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

/// Daemon `ask` in-process (mismo arnés que `e2e_m3`).
async fn spawn_ask_daemon() -> (tempfile::TempDir, std::path::PathBuf, Arc<MemProvider>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let scopes = ScopeRegistry::new();
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("policy");
    let approvals = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
    let engine = Arc::new(Engine::new().with_policy(
        Arc::new(ScopedPolicy::new(scopes.clone(), cfg)),
        Arc::clone(&approvals) as _,
    ));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&mem, "mem:///proj/a.txt", b"datos").await;
    let daemon = Daemon::bind_with_policy(
        engine,
        scopes,
        approvals,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    tokio::spawn(daemon.run());
    (dir, socket, mem)
}

/// Arranca el transporte del puente sobre un par duplex y devuelve los
/// extremos del "cliente MCP": (escritor de líneas, lector de líneas).
async fn spawn_transport(
    socket: &std::path::Path,
) -> (
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
    BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
) {
    let bridge = Bridge::connect(socket, "claude").await.expect("bridge");
    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (srv_r, srv_w) = tokio::io::split(server_side);
    tokio::spawn(async move {
        let _ = norte_mcp::bridge::serve_transport(bridge, BufReader::new(srv_r), srv_w).await;
    });
    let (cli_r, cli_w) = tokio::io::split(client_side);
    (cli_w, BufReader::new(cli_r))
}

async fn send_line(w: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>, v: &serde_json::Value) {
    w.write_all(v.to_string().as_bytes()).await.expect("write");
    w.write_all(b"\n").await.expect("nl");
}

async fn read_json(
    r: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
) -> serde_json::Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(3), r.read_line(&mut line))
        .await
        .expect("respuesta antes del timeout")
        .expect("io");
    serde_json::from_str(&line).expect("json")
}

/// Concede scope de copy a la sesión `claude` vía el propio transporte + un
/// humano wire-directo.
async fn grant_scope_via_transport(
    w: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
    r: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    socket: &std::path::Path,
) -> Client {
    let mut human = Client::connect(socket).await.expect("human");
    human
        .initialize(norte_proto::methods::ClientInfo {
            name: "tui".into(),
            version: "0".into(),
        })
        .await
        .expect("init humano");
    send_line(
        w,
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"request_scope",
            "arguments":{"roots":["mem:///proj"],"ops":["copy"],"ttl_ms":60_000}}}),
    )
    .await;
    let resp = read_json(r).await;
    let text = resp["result"]["content"][0]["text"].as_str().expect("text");
    let req_id = serde_json::from_str::<serde_json::Value>(text).expect("json")["request_id"]
        .as_u64()
        .expect("request_id");
    let _: norte_proto::methods::GrantScopeResult = human
        .call(
            norte_proto::methods::POLICY_GRANT_SCOPE,
            &norte_proto::methods::GrantScopeParams { request_id: req_id },
        )
        .await
        .expect("grant");
    human
}

/// Espera DETERMINISTA a que el copy esté genuinamente suspendido en su Ask:
/// sondea `policy.pending` hasta verlo no vacío y devuelve el `approval_id`.
/// Jamás un sleep fijo: «el copy ya llegó al Ask» es un estado del daemon, y
/// 100 ms sobre UDS local era una apuesta que bajo carga se perdía (ola W10).
async fn esperar_ask(human: &Client) -> u64 {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let res: norte_proto::methods::PolicyPendingResult = human
            .call(norte_proto::methods::POLICY_PENDING, &serde_json::json!({}))
            .await
            .expect("policy.pending");
        if let Some(p) = res.pending.first() {
            return p.approval_id;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "el Ask nunca apareció en policy.pending"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn copy_call(id: u64) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":"copy",
        "arguments":{"from":"mem:///proj/a.txt","to":"mem:///proj/b.txt"}}})
}

/// #67: con un `copy` SUSPENDIDO en el ask, un `ping` concurrente responde
/// igualmente — el keep-alive del cliente MCP no declara muerto al puente.
#[tokio::test]
async fn ping_responde_con_un_tool_suspendido_en_vuelo() {
    let (_dir, socket, _mem) = spawn_ask_daemon().await;
    let (mut w, mut r) = spawn_transport(&socket).await;
    let human = grant_scope_via_transport(&mut w, &mut r, &socket).await;

    // El copy queda suspendido en el Ask (nadie decide).
    send_line(&mut w, &copy_call(10)).await;
    esperar_ask(&human).await;
    // El ping DEBE responder aunque el copy siga en vuelo.
    send_line(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":11,"method":"ping"}),
    )
    .await;
    let resp = read_json(&mut r).await;
    assert_eq!(
        resp["id"], 11,
        "el ping responde ANTES de que el copy resuelva: {resp}"
    );
}

/// #67: `notifications/cancelled` abandona el tool en vuelo SIN respuesta
/// (spec MCP); el transporte sigue vivo para lo siguiente.
#[tokio::test]
async fn cancelled_abandona_el_tool_en_vuelo_sin_respuesta() {
    let (_dir, socket, _mem) = spawn_ask_daemon().await;
    let (mut w, mut r) = spawn_transport(&socket).await;
    let human = grant_scope_via_transport(&mut w, &mut r, &socket).await;

    send_line(&mut w, &copy_call(10)).await;
    esperar_ask(&human).await;
    send_line(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":10}}),
    )
    .await;
    // Tras cancelar, un ping responde y NADA llega para el id 10 antes.
    send_line(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":12,"method":"ping"}),
    )
    .await;
    let resp = read_json(&mut r).await;
    assert_eq!(
        resp["id"], 12,
        "lo primero tras el cancel es el ping — el id 10 no responde: {resp}"
    );
}

/// #72 (e2e): el agente cancela su tools/call suspendido en un Ask → el puente
/// reenvía rpc.cancel → el daemon RETIRA el Ask. El humano ya NO puede
/// aprobar-para-ejecutar: policy.pending queda vacío y un policy.decide tardío
/// no crea el destino. (≠ #67, que solo abandonaba la espera local dejando el
/// Ask zombi hasta el TTL.)
#[tokio::test]
async fn cancel_del_agente_retira_el_ask_el_humano_no_ejecuta() {
    let (_dir, socket, mem) = spawn_ask_daemon().await;
    let (mut w, mut r) = spawn_transport(&socket).await;
    let human = grant_scope_via_transport(&mut w, &mut r, &socket).await;

    // El agente lanza el copy: queda suspendido en el Ask (nadie decide aún).
    send_line(&mut w, &copy_call(10)).await;

    let approval_id = esperar_ask(&human).await;

    // El agente CANCELA su tools/call en vuelo.
    send_line(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":10}}),
    )
    .await;

    // El Ask debe RETIRARSE: policy.pending vuelve a vacío. Sondeo con deadline.
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let res: norte_proto::methods::PolicyPendingResult = human
                .call(norte_proto::methods::POLICY_PENDING, &serde_json::json!({}))
                .await
                .expect("policy.pending");
            if res.pending.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "el Ask quedó ZOMBI: el rpc.cancel no se reenvió (#72)"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // Un policy.decide TARDÍO no debe ejecutar nada (la pending ya no existe):
    // Err u Ok ambos aceptables — lo que importa es el efecto en el FS.
    let _ = human
        .call::<_, norte_proto::methods::PolicyDecideResult>(
            norte_proto::methods::POLICY_DECIDE,
            &norte_proto::methods::PolicyDecideParams {
                approval_id,
                approve: true,
            },
        )
        .await;

    // Deja aflorar cualquier ejecución errónea antes de comprobar el FS.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        matches!(
            mem.stat(&vp("mem:///proj/b.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "b.txt no debe existir tras la retirada del Ask"
    );

    // El transporte sigue vivo tras todo el intercambio.
    send_line(
        &mut w,
        &serde_json::json!({"jsonrpc":"2.0","id":12,"method":"ping"}),
    )
    .await;
    let resp = read_json(&mut r).await;
    assert_eq!(
        resp["id"], 12,
        "el transporte sigue vivo tras el cancel: {resp}"
    );
}

/// #67 (regla 3): EOF del peer con un tool SUSPENDIDO termina el transporte
/// limpio — cancela lo en vuelo, drena el writer, retorna dentro de un
/// timeout (jamás cuelga en el join del writer).
#[tokio::test]
async fn eof_con_tool_en_vuelo_termina_limpio() {
    let (_dir, socket, _mem) = spawn_ask_daemon().await;
    let bridge = Bridge::connect(&socket, "claude").await.expect("bridge");
    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (srv_r, srv_w) = tokio::io::split(server_side);
    let served = tokio::spawn(async move {
        norte_mcp::bridge::serve_transport(bridge, BufReader::new(srv_r), srv_w).await
    });
    let (cli_r, mut cli_w) = tokio::io::split(client_side);
    let mut r = BufReader::new(cli_r);

    // Concede scope y lanza un copy que queda suspendido en el Ask.
    let human = grant_scope_via_transport(&mut cli_w, &mut r, &socket).await;
    send_line(&mut cli_w, &copy_call(10)).await;
    esperar_ask(&human).await;

    // El peer MUERE (dropea su extremo) con el copy en vuelo.
    drop(cli_w);
    drop(r);

    // serve_transport debe RETORNAR pronto — no colgarse esperando al Ask.
    let out = tokio::time::timeout(Duration::from_secs(3), served)
        .await
        .expect("serve_transport no cuelga tras EOF con tool en vuelo")
        .expect("join");
    assert!(out.is_ok(), "teardown limpio: {out:?}");
}
