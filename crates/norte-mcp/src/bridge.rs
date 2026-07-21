//! El puente: MCP (JSON-RPC 2.0 NDJSON por stdio) ↔ protocolo norte (UDS).
//!
//! Regla 9: aquí NO hay decisiones de policy ni acceso al FS — cada tool es
//! un reenvío 1:1 al daemon, que gobierna (scope, ask, journal, actor)
//! server-side. El puente es un cliente-agente más: comprometerlo no salta
//! la policy. Los tipos del wire MCP se construyen con `serde_json::json!`
//! (NO son los `Response` de norte-proto: solo comparten el framing NDJSON).

use std::sync::Arc;

use base64::Engine as _;
use norte_core::daemon::{Client, ClientError};
use norte_proto::methods;
use norte_proto::{ByteRange, DeleteMode, VPath};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Versión MCP que respondemos, fija (ADR 0024).
const MCP_VERSION: &str = "2025-06-18";
/// Tope de espera del estado terminal de una Task encolada por un tool
/// mutante. Holgado: una op bajo regla `ask` YA esperó su aprobación DENTRO
/// de la llamada al daemon; esto solo cubre la ejecución.
const TASK_WAIT: std::time::Duration = std::time::Duration::from_mins(10);
/// Intervalo del poll de `task.list` esperando el terminal.
const TASK_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// Celda del id JSON-RPC de la `fs.*` mutante en vuelo de un tool (#72).
type DaemonIdCell = Arc<std::sync::OnceLock<u64>>;

/// Errores del ciclo de vida del puente (conexión/transporte). Los errores
/// de una TOOL no llegan aquí: viajan como `isError: true` en el result MCP
/// (el agente puede leerlos y reaccionar).
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// I/O de stdio.
    #[error("stdio: {0}")]
    Io(#[from] std::io::Error),
    /// Fallo hablando con el daemon (conexión/handshake).
    #[error("daemon: {0}")]
    Daemon(#[from] ClientError),
}

/// El puente conectado al daemon como SESIÓN DE AGENTE.
pub struct Bridge {
    client: Client,
    session: String,
}

impl Bridge {
    /// Conecta al daemon por `socket` y negocia el handshake declarando
    /// `agent_session = session`: todas las mutaciones de este puente quedan
    /// ligadas a ese actor server-side.
    ///
    /// # Errors
    /// [`BridgeError::Daemon`]: socket inalcanzable, versión incompatible o
    /// sesión rechazada (charset `[A-Za-z0-9._-]`, 1..=64).
    pub async fn connect(socket: &std::path::Path, session: &str) -> Result<Self, BridgeError> {
        let client = Client::connect(socket).await?;
        let _init: methods::InitializeResult = client
            .call(
                methods::INITIALIZE,
                &methods::InitializeParams {
                    client_info: methods::ClientInfo {
                        name: "norte-mcp".into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                    protocol_version: methods::PROTOCOL_VERSION.into(),
                    encodings: vec!["json".into()],
                    agent_session: Some(session.to_owned()),
                },
            )
            .await?;
        Ok(Self {
            client,
            session: session.to_owned(),
        })
    }

    /// Procesa UNA línea del transporte MCP y devuelve la respuesta ya
    /// serializada (`None` para notificaciones — MCP 2025-06-18 no tiene
    /// batches, así que un request produce EXACTAMENTE una respuesta).
    /// Separado de stdio para que los tests lo conduzcan sin proceso.
    pub async fn handle_line(&self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(rpc_error(
                    &Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                ));
            }
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        // Notificaciones MCP (sin id): initialized/cancelled/… — sin respuesta.
        let id = id?;
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let out = match method {
            "initialize" => rpc_result(
                &id,
                &json!({
                    "protocolVersion": MCP_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {
                        "name": "norte-mcp",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            ),
            "ping" => rpc_result(&id, &json!({})),
            "tools/list" => rpc_result(&id, &json!({"tools": tool_defs()})),
            "tools/call" => self.tools_call(&id, &params, &Arc::default()).await,
            other => rpc_error(&id, -32601, &format!("unknown method: {other}")),
        };
        Some(out)
    }

    /// `tools/call` completo: ejecuta la tool y devuelve la respuesta MCP
    /// serializada. Es la unidad que el transporte concurrente (#67) despacha
    /// a su propia task.
    pub async fn tools_call(&self, id: &Value, params: &Value, daemon_id: &DaemonIdCell) -> String {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        match self.call_tool(name, &args, daemon_id).await {
            Ok(v) => rpc_result(id, &tool_content(&v, false)),
            // Error de TOOL: el agente lo LEE (isError) y reacciona —
            // p. ej. pedir scope tras un out-of-scope.
            Err(text) => rpc_result(id, &tool_content(&json!(text), true)),
        }
    }

    /// Despacha una tool a su método de wire. `Err(texto)` = fallo de tool
    /// (viaja como `isError`, jamás rompe el transporte).
    async fn call_tool(
        &self,
        name: &str,
        args: &Value,
        daemon_id: &DaemonIdCell,
    ) -> Result<Value, String> {
        match name {
            "list_dir" => self.tool_list_dir(args).await,
            "stat" => self.tool_stat(args).await,
            "read_file" => self.tool_read_file(args).await,
            "copy" | "move" => self.tool_transfer(name, args, daemon_id).await,
            "delete" => self.tool_delete(args, daemon_id).await,
            "task_status" => self.tool_task_status(args).await,
            "request_scope" => self.tool_request_scope(args).await,
            other => Err(format!("unknown tool: {other}")),
        }
    }

    async fn tool_list_dir(&self, args: &Value) -> Result<Value, String> {
        let path = vpath_arg(args, "path")?;
        let limit = opt_u64_arg(args, "limit")?
            .map(|l| u32::try_from(l).map_err(|_| format!("arg limit too large: {l}")))
            .transpose()?;
        let cursor = args
            .get("cursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let r: methods::FsListResult = self
            .call(
                methods::FS_LIST,
                &methods::FsListParams {
                    path,
                    limit,
                    cursor,
                },
            )
            .await?;
        let entries: Vec<Value> = r
            .entries
            .iter()
            .map(|e| {
                json!({
                    "path": e.path.to_wire(),
                    "kind": e.kind,
                    "size": e.size,
                    "mtime_ms": e.mtime_ms,
                })
            })
            .collect();
        Ok(json!({"entries": entries, "next_cursor": r.next_cursor}))
    }

    async fn tool_stat(&self, args: &Value) -> Result<Value, String> {
        let path = vpath_arg(args, "path")?;
        let r: methods::FsStatResult = self
            .call(methods::FS_STAT, &methods::FsStatParams { path })
            .await?;
        Ok(json!({
            "path": r.entry.path.to_wire(),
            "kind": r.entry.kind,
            "size": r.entry.size,
            "mtime_ms": r.entry.mtime_ms,
        }))
    }

    async fn tool_read_file(&self, args: &Value) -> Result<Value, String> {
        let path = vpath_arg(args, "path")?;
        let offset = opt_u64_arg(args, "offset")?.unwrap_or(0);
        let len = opt_u64_arg(args, "len")?;
        let r: methods::FsReadResult = self
            .call(
                methods::FS_READ,
                &methods::FsReadParams {
                    path,
                    range: Some(ByteRange { offset, len }),
                },
            )
            .await?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&r.content_b64)
            .map_err(|e| format!("daemon sent invalid base64: {e}"))?;
        // Texto si LO ES; si no, lossy marcado + los bytes fieles en base64
        // (el agente elige qué mirar; nada se pierde).
        match String::from_utf8(bytes) {
            Ok(text) => Ok(json!({"text": text, "eof": r.eof})),
            Err(e) => {
                let bytes = e.into_bytes();
                Ok(json!({
                    "text": String::from_utf8_lossy(&bytes),
                    "base64": r.content_b64,
                    "eof": r.eof,
                }))
            }
        }
    }

    /// `copy` y `move` comparten cuerpo pero cada uno serializa SU tipo de
    /// params (M3 del rust-reviewer: reutilizar `FsCopyParams` para `fs.move`
    /// funcionaba por coincidencia de shape — divergencia futura invisible).
    async fn tool_transfer(
        &self,
        name: &str,
        args: &Value,
        daemon_id: &DaemonIdCell,
    ) -> Result<Value, String> {
        let from = vpath_arg(args, "from")?;
        let to = vpath_arg(args, "to")?;
        let r: methods::FsTaskResult = if name == "copy" {
            self.call_tracked(
                methods::FS_COPY,
                &methods::FsCopyParams {
                    from,
                    to,
                    on_collision: norte_proto::CollisionPolicy::default(),
                    symlinks: norte_proto::SymlinkPolicy::default(),
                    resume: norte_proto::ResumePolicy::default(),
                    verify: norte_proto::VerifyPolicy::default(),
                },
                daemon_id,
            )
            .await?
        } else {
            self.call_tracked(
                methods::FS_MOVE,
                &methods::FsMoveParams {
                    from,
                    to,
                    on_collision: norte_proto::CollisionPolicy::default(),
                    symlinks: norte_proto::SymlinkPolicy::default(),
                    resume: norte_proto::ResumePolicy::default(),
                    verify: norte_proto::VerifyPolicy::default(),
                },
                daemon_id,
            )
            .await?
        };
        self.wait_terminal(r.task_id).await
    }

    async fn tool_delete(&self, args: &Value, daemon_id: &DaemonIdCell) -> Result<Value, String> {
        let path = vpath_arg(args, "path")?;
        let mode = match args.get("mode") {
            // Default TRASH (spec §10): un borrado agéntico es SIEMPRE
            // recuperable salvo petición explícita (que la policy puede
            // seguir denegando). Un `mode` PRESENTE con tipo/valor ilegal es
            // error — jamás degradar en silencio (sec MINOR-1).
            None | Some(Value::Null) => DeleteMode::Trash,
            Some(Value::String(s)) if s == "trash" => DeleteMode::Trash,
            Some(Value::String(s)) if s == "permanent" => DeleteMode::Permanent,
            Some(other) => {
                return Err(format!("invalid mode {other}: use \"trash\"|\"permanent\""));
            }
        };
        let r: methods::FsTaskResult = self
            .call_tracked(
                methods::FS_DELETE,
                &methods::FsDeleteParams { path, mode },
                daemon_id,
            )
            .await?;
        self.wait_terminal(r.task_id).await
    }

    async fn tool_task_status(&self, args: &Value) -> Result<Value, String> {
        let id = args
            .get("task_id")
            .and_then(Value::as_u64)
            .ok_or("missing integer arg: task_id")?;
        let r: methods::TaskListResult = self
            .call(methods::TASK_LIST, &methods::TaskListParams {})
            .await?;
        match r.tasks.iter().find(|t| t.task_id.get() == id) {
            Some(t) => Ok(task_json(t)),
            None => Err(format!("unknown task {id} (too old or never existed)")),
        }
    }

    async fn tool_request_scope(&self, args: &Value) -> Result<Value, String> {
        let roots = args
            .get("roots")
            .and_then(Value::as_array)
            .ok_or("missing array arg: roots")?
            .iter()
            .map(|v| {
                let s = v.as_str().ok_or("roots must be strings")?;
                VPath::parse(s).map_err(|e| format!("invalid VPath {s:?}: {e}"))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let ops = args
            .get("ops")
            .and_then(Value::as_array)
            .ok_or("missing array arg: ops")?
            .iter()
            .map(|v| v.as_str().map(str::to_owned).ok_or("ops must be strings"))
            .collect::<Result<Vec<_>, _>>()?;
        let ttl_ms = args
            .get("ttl_ms")
            .and_then(Value::as_u64)
            .ok_or("missing integer arg: ttl_ms")?;
        let r: methods::RequestScopeResult = self
            .call(
                methods::POLICY_REQUEST_SCOPE,
                &methods::RequestScopeParams {
                    // SIEMPRE la sesión del puente: la identidad no es un
                    // argumento (el daemon lo re-valida igualmente).
                    session: self.session.clone(),
                    roots,
                    ops,
                    ttl_ms,
                },
            )
            .await?;
        Ok(json!({
            "request_id": r.request_id,
            "status": "pending human grant",
            "hint": format!(
                "a human must run `norte policy grant {}` (or grant from the TUI); retry the operation afterwards",
                r.request_id
            ),
        }))
    }

    /// `call` del wire con los errores RPC convertidos a texto de tool. La
    /// taxonomía viaja en `data`: un `PolicyDenied` sale ACCIONABLE.
    async fn call<P, R>(&self, method: &str, params: &P) -> Result<R, String>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        self.client
            .call(method, params)
            .await
            .map_err(map_client_err)
    }

    /// Como [`Self::call`], pero registra el id JSON-RPC asignado en
    /// `daemon_id` (una celda `OnceLock`) ANTES de suspenderse — para que el
    /// handler de `notifications/cancelled` pueda reenviar un `rpc.cancel` de
    /// esa request mientras sigue suspendida en un Ask (#72).
    async fn call_tracked<P, R>(
        &self,
        method: &str,
        params: &P,
        daemon_id: &DaemonIdCell,
    ) -> Result<R, String>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        self.client
            .call_tracked(method, params, |id| {
                // OnceLock: el PRIMER `fs.*` mutante de este tool fija el id;
                // los polls de task.list posteriores NO lo pisan.
                let _ = daemon_id.set(id);
            })
            .await
            .map_err(map_client_err)
    }

    /// Espera el estado terminal de `task_id` por poll de `task.list`. La
    /// task ACABA de ser ack-eada por el daemon, así que existe: si un poll
    /// no la encuentra NI viva NI en los desenlaces recientes, es que
    /// terminó y su desenlace fue EVICTADO del buffer de recientes (64,
    /// global) — se devuelve un error HONESTO inmediato en vez de agotar el
    /// deadline afirmando "still running" (M1 del rust-reviewer).
    async fn wait_terminal(&self, task_id: norte_proto::TaskId) -> Result<Value, String> {
        let deadline = tokio::time::Instant::now() + TASK_WAIT;
        loop {
            let r: methods::TaskListResult = self
                .call(methods::TASK_LIST, &methods::TaskListParams {})
                .await?;
            match r.tasks.iter().find(|t| t.task_id == task_id) {
                Some(t) if t.state.is_terminal() => return Ok(task_json(t)),
                Some(_) => {}
                None => {
                    return Err(format!(
                        "task {} finished but its outcome was evicted (busy daemon); \
                         verify the result with stat/list_dir",
                        task_id.get()
                    ));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "task {} not observed terminal within {}s; poll it with task_status",
                    task_id.get(),
                    TASK_WAIT.as_secs()
                ));
            }
            tokio::time::sleep(TASK_POLL).await;
        }
    }

    /// Reenvía al daemon un `rpc.cancel` de la request `daemon_id` (#72): si
    /// esa `fs.*` sigue suspendida en un Ask, el daemon la retira (fail-closed).
    /// Best-effort: un id ya resuelto es no-op en el daemon; un canal muerto se
    /// descarta. El daemon gobierna: comprometer el puente NO salta la policy.
    pub fn cancel_daemon_request(&self, daemon_id: u64) {
        let _ = self.client.notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(daemon_id),
            },
        );
    }
}

/// Traduce un error del `Client` al texto de tool (la taxonomía en `data`;
/// un `PolicyDenied` sale ACCIONABLE). Compartido por `call` y `call_tracked`.
fn map_client_err(e: ClientError) -> String {
    match e {
        ClientError::Rpc(rpc) => match rpc.data {
            Some(norte_proto::Error::PolicyDenied { ref rule }) => format!(
                "denied by policy ({rule}). If out-of-scope, call request_scope and ask the human to grant it."
            ),
            Some(err) => format!("{err}"),
            None => format!("rpc error {}: {}", rpc.code, rpc.message),
        },
        other => format!("daemon unreachable: {other}"),
    }
}

/// Snapshot de una task como JSON de tool (estado + error por categoría).
fn task_json(t: &norte_proto::TaskProgress) -> Value {
    use norte_proto::TaskState;
    let (state, error) = match &t.state {
        TaskState::Completed => ("completed", None),
        TaskState::Cancelled => ("cancelled", None),
        TaskState::Failed { error } => ("failed", Some(error.to_string())),
        _ => ("running", None),
    };
    json!({
        "task_id": t.task_id.get(),
        "state": state,
        "error": error,
        "bytes_done": t.bytes_done,
        "bytes_total": t.bytes_total,
    })
}

/// Argumento entero OPCIONAL con criterio único (sec MINOR-1 / enc H3):
/// ausente = `None`, presente con tipo malo (float, string, negativo) =
/// error de tool — jamás una degradación silenciosa que confunda a un
/// agente en bucle (un `offset: 2.0` ignorado parecería aplicado).
fn opt_u64_arg(args: &Value, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("arg {key} must be a non-negative integer, got {v}")),
    }
}

/// Extrae y parsea un argumento `VPath` obligatorio.
fn vpath_arg(args: &Value, key: &str) -> Result<VPath, String> {
    let s = args
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string arg: {key}"))?;
    VPath::parse(s).map_err(|e| format!("invalid VPath {s:?}: {e}"))
}

/// Result MCP de `tools/call`: el payload va como texto JSON en `content`.
fn tool_content(payload: &Value, is_error: bool) -> Value {
    let text = if let Value::String(s) = payload {
        s.clone()
    } else {
        payload.to_string()
    };
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

fn rpc_result(id: &Value, result: &Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn rpc_error(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// Las 8 tools v1 (ADR 0024): superficie = lo que el wire ya ofrece.
fn tool_defs() -> Value {
    let path_desc =
        "VPath URL: file:///…, sftp://host/…, s3://bucket/…, or composed zip:file:///a.zip!/inside";
    json!([
        {
            "name": "list_dir",
            "description": "List a directory managed by norte. Returns entries (path/kind/size/mtime_ms) and next_cursor when paginated.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": path_desc},
                    "limit": {"type": "integer", "minimum": 1, "description": "page size"},
                    "cursor": {"type": "string", "description": "next_cursor from the previous page"}
                },
                "required": ["path"]
            }
        },
        {
            "name": "stat",
            "description": "Metadata of one node (kind/size/mtime_ms).",
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string", "description": path_desc}},
                "required": ["path"]
            }
        },
        {
            "name": "read_file",
            "description": "Read a byte range of a file. Returns text (UTF-8; lossy if not) plus base64 of the exact bytes when the content is not valid UTF-8, and eof. If the byte range splits a multibyte character, text will contain U+FFFD at the edges and base64 carries the exact bytes: reassemble multi-chunk reads from base64, never by concatenating text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": path_desc},
                    "offset": {"type": "integer", "minimum": 0},
                    "len": {"type": "integer", "minimum": 1, "description": "max bytes (capped server-side)"}
                },
                "required": ["path"]
            }
        },
        {
            "name": "copy",
            "description": "Copy a file or directory (recursive). Runs as a cancellable task and this call waits for its outcome. Requires a granted scope; an `ask` policy suspends until a human approves.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": {"type": "string", "description": path_desc},
                    "to": {"type": "string", "description": "exact destination path (existing destination = conflict)"}
                },
                "required": ["from", "to"]
            }
        },
        {
            "name": "move",
            "description": "Move/rename a file or directory. Same governance as copy (scope + policy).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": {"type": "string", "description": path_desc},
                    "to": {"type": "string", "description": "exact destination path"}
                },
                "required": ["from", "to"]
            }
        },
        {
            "name": "delete",
            "description": "Delete a file or directory (recursive). Default mode is trash (recoverable); permanent requires explicit mode and policy approval.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": path_desc},
                    "mode": {"type": "string", "enum": ["trash", "permanent"], "description": "default trash"}
                },
                "required": ["path"]
            }
        },
        {
            "name": "task_status",
            "description": "Current state of a norte task by id (running/completed/failed/cancelled).",
            "inputSchema": {
                "type": "object",
                "properties": {"task_id": {"type": "integer", "minimum": 1}},
                "required": ["task_id"]
            }
        },
        {
            "name": "request_scope",
            "description": "Request access to path subtrees with specific operations (copy/move/delete/mkdir) for a TTL. The request stays PENDING until a human grants it (`norte policy grant <request_id>` or from the TUI); retry your operation after the grant.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "roots": {"type": "array", "items": {"type": "string"}, "description": "subtree roots (VPath URLs)"},
                    "ops": {"type": "array", "items": {"type": "string", "enum": ["copy", "move", "delete", "mkdir"]}},
                    "ttl_ms": {"type": "integer", "minimum": 1, "description": "time-to-live in milliseconds (capped server-side at 24h)"}
                },
                "required": ["roots", "ops", "ttl_ms"]
            }
        }
    ])
}

/// Tope de una línea del transporte (16 MiB, como `MAX_FRAME_BYTES` del
/// wire de norte): una "línea" sin `\n` jamás acumula memoria sin límite
/// (MINOR-2 del security-reviewer) — se descarta y se responde `-32700`.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Tools/call CONCURRENTES en vuelo por transporte (#67): un cliente MCP
/// razonable lleva 1-2; el tope corta a un cliente desbocado con un error
/// de respuesta, jamás acumulando tasks sin límite.
pub const MAX_INFLIGHT_TOOLS: usize = 8;

/// Un tool en vuelo (#67 + #72): su token de cancelación local y la celda con
/// el id JSON-RPC de su `fs.*` mutante (para reenviar `rpc.cancel` al daemon).
#[derive(Clone)]
struct InflightTool {
    token: CancellationToken,
    daemon_id: DaemonIdCell,
}

/// Sirve MCP por stdio hasta EOF (el agente cierra el pipe al terminar). Un
/// mensaje por línea (NDJSON, el transporte stdio de MCP; tope
/// [`MAX_LINE_BYTES`]). stdout es EXCLUSIVO del transporte: cualquier
/// diagnóstico va por tracing (el binario debe fijar el subscriber a
/// stderr).
///
/// # Errors
/// I/O de stdio o la conexión/handshake inicial con el daemon.
pub async fn serve_stdio(socket: &std::path::Path, session: &str) -> Result<(), BridgeError> {
    let bridge = Bridge::connect(socket, session).await?;
    tracing::info!(session, "puente MCP conectado al daemon");
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    serve_transport(bridge, stdin, stdout).await
}

/// Transporte del puente sobre CUALQUIER par lectura/escritura (#67): las
/// `tools/call` se despachan a tasks CONCURRENTES (tope
/// [`MAX_INFLIGHT_TOOLS`]) y las respuestas salen por un canal único hacia
/// el writer — jamás dos líneas entrelazadas. Un tool suspendido (ask de
/// policy, `wait_terminal` de una task larga) ya no retiene `ping` ni
/// `notifications/cancelled`. OJO: concurrentes EN EL PUENTE — el daemon
/// sirve su conexión en SERIE, así que dos tools que lo toquen se encolan
/// allí; lo que queda siempre vivo es lo que no toca el daemon
/// (ping/initialize/tools\/list/cancelled). `notifications/cancelled {requestId}` aborta
/// el tool en vuelo SIN respuesta (spec MCP); la Task del daemon subyacente
/// sigue viva y GOBERNADA (journal + undo) — solo se abandona la espera.
///
/// # Errors
/// I/O del transporte.
///
/// # Panics
/// Nunca: los `expect` de los locks documentan la invariante de poisoning
/// (nada paniquea con ellos tomados).
pub async fn serve_transport<R, W>(
    bridge: Bridge,
    mut reader: R,
    writer: W,
) -> Result<(), BridgeError>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let bridge = Arc::new(bridge);
    // Salida única: respuestas inline y de tasks compiten por el canal, el
    // writer serializa líneas completas.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(64);
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(out) = out_rx.recv().await {
            if writer.write_all(out.as_bytes()).await.is_err()
                || writer.write_all(b"\n").await.is_err()
                || writer.flush().await.is_err()
            {
                break;
            }
        }
    });
    // Tools en vuelo, por id serializado: `notifications/cancelled` cancela
    // su token; el guard del task retira la entrada al terminar.
    let inflight: Arc<std::sync::Mutex<std::collections::HashMap<String, InflightTool>>> =
        Arc::default();

    let mut line: Vec<u8> = Vec::new();
    // `true` = la línea actual ya excedió el tope: se drena hasta el `\n`
    // sin acumular y se responde -32700 al cerrarse.
    let mut overflow = false;
    let result: Result<(), BridgeError> = loop {
        // fill_buf/consume a mano: `read_until` acumularía sin tope. La
        // lectura se racea contra la muerte del WRITER (stdout roto): sin
        // salida no se despachan más tools con efectos (m3 del review).
        let (nl_at, used) = {
            let chunk = tokio::select! {
                r = reader.fill_buf() => match r {
                    Ok(c) => c,
                    // Un error de lectura TAMBIÉN pasa por el teardown común
                    // (cancelar in-flight, drenar writer) — B3 del review.
                    Err(e) => break Err(e.into()),
                },
                () = out_tx.closed() => break Ok(()),
            };
            if chunk.is_empty() {
                break Ok(()); // EOF
            }
            let nl_at = chunk.iter().position(|&b| b == b'\n');
            let take = nl_at.unwrap_or(chunk.len());
            if !overflow {
                if line.len() + take > MAX_LINE_BYTES {
                    overflow = true;
                    line.clear();
                } else {
                    line.extend_from_slice(&chunk[..take]);
                }
            }
            (nl_at, nl_at.map_or(chunk.len(), |i| i + 1))
        };
        reader.consume(used);
        if nl_at.is_none() {
            continue;
        }
        // Línea completa.
        if overflow {
            overflow = false;
            let _ = out_tx
                .send(rpc_error(&Value::Null, -32700, "line too long"))
                .await;
            continue;
        }
        let text = String::from_utf8_lossy(&line).into_owned();
        line.clear();
        if text.trim().is_empty() {
            continue;
        }
        dispatch_line(&bridge, &text, &out_tx, &inflight).await;
    };
    tracing::info!("fin del transporte (EOF/errores): puente terminado");
    // Teardown COMÚN a todos los caminos: los tools en vuelo se abandonan
    // (el peer ya no leerá sus respuestas) y el writer se drena.
    for (_, tool) in inflight.lock().expect("inflight lock sano").drain() {
        tool.token.cancel();
        // #72: si el tool tenía una fs.* mutante en vuelo, reenvía su
        // rpc.cancel — retira el Ask suspendido en vez de esperar al TTL.
        if let Some(&daemon_id) = tool.daemon_id.get() {
            bridge.cancel_daemon_request(daemon_id);
        }
    }
    drop(out_tx);
    let _ = writer_task.await;
    result
}

/// Clave de correlación MCP: el `id` JSON serializado (número o string).
fn id_key(id: &Value) -> String {
    id.to_string()
}

/// Retira la entrada de `inflight` a CUALQUIER salida de la task del tool
/// (respuesta, cancel o panic) — mismo patrón RAII que `PendingGuard`.
struct InflightGuard {
    key: String,
    map: Arc<std::sync::Mutex<std::collections::HashMap<String, InflightTool>>>,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.map
            .lock()
            .expect("inflight lock sano")
            .remove(&self.key);
    }
}

/// Clasifica y despacha UNA línea (#67): lo barato responde inline; un
/// `tools/call` se va a su task con token de cancelación.
async fn dispatch_line(
    bridge: &Arc<Bridge>,
    text: &str,
    out_tx: &tokio::sync::mpsc::Sender<String>,
    inflight: &Arc<std::sync::Mutex<std::collections::HashMap<String, InflightTool>>>,
) {
    let msg: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            let _ = out_tx
                .send(rpc_error(
                    &Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                ))
                .await;
            return;
        }
    };
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let Some(id) = msg.get("id").cloned() else {
        // Notificación: `cancelled` aborta el tool en vuelo; el resto
        // (initialized…) se ignora sin respuesta (JSON-RPC).
        if method == "notifications/cancelled"
            && let Some(req_id) = msg.pointer("/params/requestId")
            && let Some(tool) = inflight
                .lock()
                .expect("inflight lock sano")
                .remove(&id_key(req_id))
        {
            tool.token.cancel();
            // #72: si el tool había lanzado una fs.* mutante contra el daemon,
            // reenvía un rpc.cancel de ESA request — retira su Ask suspendido en
            // vez de dejarlo zombi hasta el TTL. Un id aún sin fijar (tool que no
            // llegó a llamar al daemon) = nada que cancelar.
            if let Some(&daemon_id) = tool.daemon_id.get() {
                bridge.cancel_daemon_request(daemon_id);
            }
        }
        return;
    };
    if method == "tools/call" {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let token = CancellationToken::new();
        // Celda del id JSON-RPC de la fs.* mutante del tool (#72): se comparte
        // entre la task del tool (que lo fija) y el handler de cancelled (que
        // lo lee para reenviar rpc.cancel).
        let daemon_id: DaemonIdCell = Arc::default();
        // El lock vive en su propio scope SIN awaits (Send del future). La
        // admisión es por ENTRY VACANTE: un id repetido en vuelo NO
        // sobrescribe (sobrescribir dejaría el token anterior huérfano y el
        // tope seria bypasseable reutilizando el mismo id — A1 del
        // security-reviewer).
        let admission = {
            let mut map = inflight.lock().expect("inflight lock sano");
            if map.len() >= MAX_INFLIGHT_TOOLS {
                Err("too many concurrent tool calls")
            } else {
                match map.entry(id_key(&id)) {
                    std::collections::hash_map::Entry::Occupied(_) => {
                        Err("duplicate request id already in flight")
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(InflightTool {
                            token: token.clone(),
                            daemon_id: Arc::clone(&daemon_id),
                        });
                        Ok(())
                    }
                }
            }
        };
        if let Err(msg) = admission {
            let _ = out_tx.send(rpc_error(&id, -32000, msg)).await;
            return;
        }
        let bridge = Arc::clone(bridge);
        let out_tx = out_tx.clone();
        let guard = InflightGuard {
            key: id_key(&id),
            map: Arc::clone(inflight),
        };
        tokio::spawn(async move {
            // El guard retira la entrada a CUALQUIER salida — incluido un
            // panic de la tool (sin él, 8 panics agotarían el transporte
            // para siempre; M1 del rust-reviewer).
            let _guard = guard;
            tokio::select! {
                biased;
                out = bridge.tools_call(&id, &params, &daemon_id) => {
                    let _ = out_tx.send(out).await;
                }
                // Cancelado: SIN respuesta (spec MCP) — la espera se
                // abandona; la Task del daemon sigue, gobernada.
                () = token.cancelled() => {}
            }
        });
        return;
    }
    // Lo barato (initialize/ping/tools/list/desconocido) responde inline:
    // jamás bloquea (no toca el daemon).
    if let Some(out) = bridge.handle_line(text).await {
        let _ = out_tx.send(out).await;
    }
}
