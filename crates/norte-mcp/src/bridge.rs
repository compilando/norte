//! El puente: MCP (JSON-RPC 2.0 NDJSON por stdio) ↔ protocolo norte (UDS).
//!
//! Regla 9: aquí NO hay decisiones de policy ni acceso al FS — cada tool es
//! un reenvío 1:1 al daemon, que gobierna (scope, ask, journal, actor)
//! server-side. El puente es un cliente-agente más: comprometerlo no salta
//! la policy. Los tipos del wire MCP se construyen con `serde_json::json!`
//! (NO son los `Response` de norte-proto: solo comparten el framing NDJSON).

use std::sync::Arc;

use base64::Engine as _;
use norte_core::backend::Backend;
use norte_core::backend::remote::RemoteBackend;
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

/// Filas de `compare` por llamada si el caller no pide `limit` (~2 lotes de
/// [`norte_proto::methods::COMPARE_ROWS_MAX_BATCH`]). `fs.compare` no pagina
/// como `fs.list` (no hay `cursor`, y el walk no tiene `max_hits` como
/// `fs.search`): sin este tope, comparar dos árboles grandes metería un
/// millón de filas en un solo resultado de tool y reventaría el contexto del
/// modelo.
const COMPARE_ROWS_DEFAULT: usize = 500;
/// Tope DURO de `limit`, aunque el caller pida más: una `limit` sin techo
/// sería el mismo problema que no tener tope, con un paso extra.
const COMPARE_ROWS_MAX: usize = 5000;

/// Celda del id JSON-RPC de la `fs.*` mutante en vuelo de un tool (#72).
type DaemonIdCell = Arc<std::sync::OnceLock<u64>>;

/// Errores del ciclo de vida del puente (conexión/transporte). Los errores
/// de una TOOL no llegan aquí: viajan como `isError: true` en el result MCP
/// (el agente puede leerlos y reaccionar).
///
/// `non_exhaustive`: la lista crece con cada superficie nueva del puente
/// (`Streams` la estrenó) y ninguna de esas adiciones debe ser un break.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BridgeError {
    /// I/O de stdio.
    #[error("stdio: {0}")]
    Io(#[from] std::io::Error),
    /// Fallo hablando con el daemon (conexión/handshake).
    #[error("daemon: {0}")]
    Daemon(#[from] ClientError),
    /// Fallo abriendo el brazo de streams ([`Bridge::streams`]). Separado de
    /// [`Self::Daemon`] porque llega en taxonomía del protocolo, no como
    /// `ClientError`, y porque distingue «el puente no arrancó» de «una tool
    /// no pudo abrir su segunda conexión».
    #[error("brazo de streams: {0}")]
    Streams(#[source] norte_proto::Error),
}

/// El puente conectado al daemon como SESIÓN DE AGENTE.
pub struct Bridge {
    client: Client,
    session: String,
    /// El socket, guardado para poder abrir [`Bridge::streams`] al vuelo.
    socket: std::path::PathBuf,
    /// La conexión que drena notificaciones, abierta en la PRIMERA tool que
    /// la necesita.
    ///
    /// Perezosa a propósito: un agente que solo lista y lee jamás la abre, y
    /// una segunda conexión al daemon no es gratis. Una sola, cacheada: dos
    /// serían dos `conn_id` sin ninguna ventaja.
    streams: tokio::sync::OnceCell<Backend>,
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
            socket: socket.to_path_buf(),
            streams: tokio::sync::OnceCell::new(),
        })
    }

    /// El brazo que drena notificaciones, abriéndolo si es la primera vez.
    ///
    /// `fs.compare` y `sync.plan` no contestan con su resultado: lo entregan
    /// por notificaciones (`compare.rows`, `sync.steps`), y el [`Client`] de
    /// este puente no las enruta — su canal se toma con `&mut self` y, con
    /// ocho tools en vuelo, habría que demultiplexarlas por `task_id`. Ese
    /// demultiplexor ya existe en [`Backend::Remote`], así que el puente abre
    /// una SEGUNDA conexión al daemon y la usa para esos dos métodos.
    ///
    /// Es el MISMO actor: se abre con `connect_as_agent(self.session)`, y los
    /// scopes de policy están indexados por SESIÓN
    /// (`ScopeRegistry::grant(session, …)`), no por conexión — lo concedido al
    /// agente vale igual aquí. Lo que NO se comparte es el `conn_id`: un plan
    /// retenido en el spool para esta conexión no es redimible desde la de
    /// tools, lo que es exactamente por qué el puente no ofrece `sync_apply`.
    ///
    /// Perezosa y cacheada: se abre una vez y, en el camino normal, muere con
    /// el puente (el `Backend` es un campo, no un `spawn`; al soltarlo, su
    /// bomba de notificaciones ve caer el último `Arc` y sale sola). «Normal»
    /// es literal: `Backend` es `Clone` y todo `TaskRef` que salga de aquí
    /// lleva dentro un clon, así que un clon retenido —o una task viva— la
    /// mantiene abierta más allá del puente. No la retengas.
    ///
    /// **Ninguna operación lógica puede repartirse entre las dos conexiones**
    /// (comprobar por una y actuar por la otra). Entre las dos llamadas puede
    /// cambiar el estado de scopes e incluso el daemon: este brazo
    /// RECONECTA solo y la conexión de tools no, de modo que tras un reinicio
    /// el brazo puede estar hablando con un daemon nuevo —`ScopeRegistry`
    /// vacío— mientras la otra está muerta. Los dos lados fallan cerrados,
    /// pero la carrera existe: cada tool decide por UNA conexión.
    ///
    /// # Errors
    /// [`BridgeError::Streams`] si el daemon no acepta la segunda conexión
    /// (socket caído, sesión rechazada).
    pub async fn streams(&self) -> Result<&Backend, BridgeError> {
        self.streams
            .get_or_try_init(|| async {
                let remote = RemoteBackend::connect_as_agent(
                    self.socket.clone(),
                    methods::ClientInfo {
                        name: "norte-mcp".into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                    self.session.clone(),
                )
                .await
                .map_err(BridgeError::Streams)?;
                let mut backend = Backend::Remote(remote);
                // El daemon difunde a esta conexión el progreso de las tasks
                // de su MISMA sesión —o sea, las de la conexión de tools—, y
                // el backend las encola como «foráneas» en un canal sin tope.
                // El puente no las mira (cada tool sigue su propia task por
                // `task.list`), así que se suelta el receptor: sin él, el
                // `send` es un no-op y la cola no crece durante toda la vida
                // del proceso.
                let _ = backend.take_foreign_tasks();
                Ok(backend)
            })
            .await
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
            "compare" => self.tool_compare(args).await,
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
                    // El puente no expone atributos de provider (ADR 0039,
                    // bloque 1 = solo wire): vacío = no se entrega ninguno.
                    attrs: Vec::new(),
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
            .call(
                methods::FS_STAT,
                &methods::FsStatParams {
                    path,
                    attrs: Vec::new(),
                },
            )
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

    /// `compare`: dos árboles, y qué difiere entre ellos. NO muta nada (sin
    /// journal, sin undo — regla dura 4 no aplica: `Backend::compare` no
    /// escribe).
    ///
    /// Va por [`Self::streams`], no por [`Self::call`]: `fs.compare` entrega
    /// sus filas por notificación (`compare.rows`), y esta conexión de tools
    /// no las enruta (ver la rustdoc de `streams`). `Backend::compare` valida
    /// raíces iguales y `follow_symlinks` ANTES de elegir brazo, así que esa
    /// comprobación llega gratis aquí.
    ///
    /// # El tope de filas
    /// `fs.compare` no pagina (no hay `cursor`) ni tiene `max_hits` como
    /// `fs.search`: sin un tope, comparar dos árboles grandes metería un
    /// millón de filas en un solo resultado de tool. Al llegar a `limit`
    /// ([`COMPARE_ROWS_DEFAULT`], techo [`COMPARE_ROWS_MAX`]) se deja de
    /// drenar, se CANCELA la task (igual que `norte compare` no necesita
    /// hacer: él drena hasta el final salvo que el lector se vaya) y el
    /// payload dice `truncated: true`. Una truncación silenciosa sería peor
    /// que el tope: un modelo que la lea como completa reportaría dos árboles
    /// como iguales cuando no se sabe.
    ///
    /// # `complete`
    /// La misma distinción que el código de salida 2 de `norte compare` (el
    /// comando del CLI, `compare_cmd`): sólo `true` cuando la task llegó a
    /// `Completed` SIN truncar. Cualquier otra
    /// cosa —truncada, cancelada, fallida, o un daemon que se cayó a mitad—
    /// es `false`, para que el agente no confunda "until here it looked
    /// clean" con "esto es todo lo que hay".
    ///
    /// No se cruza `TaskProgress::entries_done` contra las filas recibidas
    /// (la rustdoc de `Backend::compare` documenta que el cierre del canal
    /// tampoco lo garantiza): esa comprobación es para quien vaya a ESCRIBIR
    /// a partir de las filas (el plan de sincronización), y esta tool sólo
    /// lee — la misma paridad que ya tiene `norte compare` en el CLI, que
    /// tampoco la hace.
    async fn tool_compare(&self, args: &Value) -> Result<Value, String> {
        let left = vpath_arg(args, "a")?;
        let right = vpath_arg(args, "b")?;
        let criteria = compare_criteria_arg(args)?;
        let max_depth = opt_u64_arg(args, "max_depth")?
            .map(|d| u32::try_from(d).map_err(|_| format!("arg max_depth too large: {d}")))
            .transpose()?;
        let mtime_tolerance_ms = opt_u64_arg(args, "mtime_tolerance_ms")?
            .map(|t| u32::try_from(t).map_err(|_| format!("arg mtime_tolerance_ms too large: {t}")))
            .transpose()?
            .unwrap_or(2000);
        let limit = opt_u64_arg(args, "limit")?
            .map(|l| usize::try_from(l).map_err(|_| format!("arg limit too large: {l}")))
            .transpose()?
            .unwrap_or(COMPARE_ROWS_DEFAULT)
            .min(COMPARE_ROWS_MAX);
        // `limit: 0` no es "cero filas por decisión del caller", es un
        // argumento sin sentido (encoding-auditor, revisión de la tarea 2):
        // sin este chequeo saldría `truncated: true` con `rows: []` en la
        // PRIMERA fila, indistinguible de un árbol de verdad truncado. El
        // esquema JSON ya dice `"minimum": 1`, pero eso es asesor — un MCP
        // client real puede mandarlo igual.
        if limit == 0 {
            return Err("arg limit must be >= 1".to_owned());
        }

        let params = methods::FsCompareParams {
            left,
            right,
            criteria,
            max_depth,
            mtime_tolerance_ms,
            // El puente no ofrece seguir symlinks (`Backend::compare` lo
            // rechaza de todos modos): ver la rustdoc de `tool_transfer`
            // para el motivo de no exponer opciones que el core no soporta.
            follow_symlinks: false,
            descend_orphans: None,
        };

        let (task, mut rx) = self
            .streams()
            .await
            .map_err(|e| e.to_string())?
            .compare(params)
            .await
            .map_err(map_backend_err)?;

        // Filas fieles al wire: `CompareRow` serializa `verdict`/`criterion`/
        // `confidence` como los valores snake_case del protocolo (nunca una
        // etiqueta traducida) y las rutas de sus `Entry` como `to_wire()` —
        // es la MISMA forma que `norte compare --json` ya expone (regla 1;
        // jamás una cadena lossy). Se decide fila a fila si truncar en vez de
        // colectar el lote entero primero: un lote agotando exactamente el
        // resto del tope no debe arrastrar una fila de más.
        let mut rows: Vec<methods::CompareRow> = Vec::new();
        let mut truncated = false;
        'drain: while let Some(batch) = rx.recv().await {
            for row in batch.rows {
                if rows.len() >= limit {
                    truncated = true;
                    break 'drain;
                }
                rows.push(row);
            }
        }
        if truncated {
            // Cooperativa (regla dura 3): sin esto, una comparación de
            // millones de filas seguiría leyendo (y hasheando) los dos
            // árboles enteros para un `rx` que ya nadie drena.
            task.cancel();
        }
        let state = task.join().await;
        let complete = !truncated && matches!(state, norte_proto::TaskState::Completed);

        let rows = serde_json::to_value(&rows)
            .map_err(|e| format!("tool compare: could not encode rows: {e}"))?;
        Ok(json!({
            "rows": rows,
            "truncated": truncated,
            "complete": complete,
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

/// Traduce un [`norte_proto::Error`] de [`Bridge::tool_compare`] (que habla
/// con [`Bridge::streams`], NO con [`Client`], así que no hay `ClientError`
/// que envolver) al mismo texto ACCIONABLE que [`map_client_err`]: un
/// `PolicyDenied` tiene que decir "pide scope" salga por el brazo que salga.
fn map_backend_err(e: norte_proto::Error) -> String {
    match e {
        norte_proto::Error::PolicyDenied { ref rule } => format!(
            "denied by policy ({rule}). If out-of-scope, call request_scope and ask the human to grant it."
        ),
        other => format!("{other}"),
    }
}

/// Traduce el arg opcional `criteria` (array de strings) de `compare` a
/// [`methods::CompareCriteria`], con el MISMO contrato que `--criteria` del
/// CLI (`parse_compare_criteria`): ausente = el default del wire (tamaño +
/// fecha, sin hash); presente = EXACTAMENTE la lista pedida, nunca sumada al
/// default (`["hash"]` a secas enciende solo `hash`).
fn compare_criteria_arg(args: &Value) -> Result<methods::CompareCriteria, String> {
    let names = match args.get("criteria") {
        None | Some(Value::Null) => return Ok(methods::CompareCriteria::default()),
        Some(v) => v
            .as_array()
            .ok_or("arg criteria must be an array of strings")?,
    };
    let mut criteria = methods::CompareCriteria {
        size: false,
        mtime: false,
        hash: false,
    };
    for name in names {
        match name.as_str() {
            Some("size") => criteria.size = true,
            Some("mtime") => criteria.mtime = true,
            Some("hash") => criteria.hash = true,
            _ => return Err(format!("arg criteria: unknown criterion {name}")),
        }
    }
    Ok(criteria)
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

/// Las 8 tools v1 (ADR 0024, superficie = lo que el wire ya ofrece) más
/// `compare` (spec 3 fase B, tarea 2): la primera que consume el brazo de
/// streams en vez de responder directo.
fn tool_defs() -> Value {
    let mut defs = fixed_tool_defs();
    defs.push(compare_tool_def());
    Value::Array(defs)
}

/// Las 8 tools v1 (ADR 0024): superficie = lo que el wire ya ofrece. Separada
/// de [`tool_defs`] y de [`compare_tool_def`] solo por el lint de longitud
/// (`too_many_lines`): las tres juntas eran una función, esta división no
/// cambia el JSON que sale.
fn fixed_tool_defs() -> Vec<Value> {
    let path_desc =
        "VPath URL: file:///…, sftp://host/…, s3://bucket/…, or composed zip:file:///a.zip!/inside";
    let Value::Array(defs) = json!([
        {
            "name": "list_dir",
            "description": "List a directory managed by norte. Returns entries (path/kind/size/mtime_ms) and next_cursor when paginated. size/mtime_ms may be null (lazy listing); use stat for a specific path.",
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
    ]) else {
        unreachable!("json!([…]) siempre produce Value::Array")
    };
    defs
}

/// La definición de `compare` (spec 3 fase B, tarea 2), separada de
/// [`fixed_tool_defs`] solo por el lint de longitud — ver la rustdoc de
/// [`tool_defs`].
fn compare_tool_def() -> Value {
    let path_desc =
        "VPath URL: file:///…, sftp://host/…, s3://bucket/…, or composed zip:file:///a.zip!/inside";
    json!({
        "name": "compare",
        "description": "Compare two trees (files and directories) and report what differs between them. Read-only: this mutates nothing. Each row carries WIRE values (verdict/criterion/confidence as the protocol spells them, e.g. \"only_left\", never a translated label) and both paths as their wire form. If more rows exist than `limit`, the comparison is CANCELLED partway and `truncated` is true — treat a truncated result as unknown, never as \"these trees match\". `complete` is true only when the run reached its end untruncated; false covers truncated, cancelled, and failed alike, so a false always means \"do not trust this as the whole picture\".",
        "inputSchema": {
            "type": "object",
            "properties": {
                "a": {"type": "string", "description": path_desc},
                "b": {"type": "string", "description": path_desc},
                "criteria": {"type": "array", "items": {"type": "string", "enum": ["size", "mtime", "hash"]}, "description": "which rungs to run; absent = size+mtime (the wire default). hash reads full file contents on both sides and requires content scope."},
                "max_depth": {"type": "integer", "minimum": 0, "description": "root counts as depth 0; absent = unlimited"},
                "mtime_tolerance_ms": {"type": "integer", "minimum": 0, "description": "default 2000 (FAT-safe)"},
                "limit": {"type": "integer", "minimum": 1, "description": "max rows before the comparison is cancelled and truncated:true; default 500"}
            },
            "required": ["a", "b"]
        }
    })
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
