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

/// Pasos de `sync_plan` por llamada si el caller no pide `limit`. MISMO valor
/// que [`COMPARE_ROWS_DEFAULT`] y el mismo motivo: `sync.plan` tampoco pagina
/// (no hay `cursor`), y un plan sobre dos árboles grandes metería cientos de
/// miles de pasos en un solo resultado de tool. El nombre y el vocabulario del
/// payload (`limit`/`truncated`/`complete`) son deliberadamente los mismos que
/// en `compare`: un modelo que lea las dos tools no debe aprender dos
/// vocabularios para la misma idea.
const SYNC_STEPS_DEFAULT: usize = 500;
/// Tope DURO de `limit` de `sync_plan`, por el mismo motivo que
/// [`COMPARE_ROWS_MAX`].
const SYNC_STEPS_MAX: usize = 5000;

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
        let mut client = Client::connect(socket).await?;
        // El MISMO handshake que usa el brazo de streams
        // (`RemoteBackend::connect_as_agent` llama aquí también). Aquí hubo un
        // `InitializeParams` literal, con un comentario que lo justificaba
        // diciendo que el puente guarda el `InitializeResult`: no lo guardaba
        // —lo tiraba a `_init`—, y el motivo real era que el método estaba
        // `pub(crate)`. Dos literales para las dos mitades de UNA sesión de
        // agente es como un `encodings` ampliado llega a una y no a la otra.
        let _init: methods::InitializeResult = client
            .initialize_as_agent(client_info(), session.to_owned())
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
    /// # Solo para tests
    /// `pub` únicamente porque el E2E de `norte-mcp` necesita comprobar que el
    /// brazo se abre UNA vez; no es API estable (`doc(hidden)`, puede cambiar
    /// sin bump), y el precedente en el árbol es
    /// `norte_core::backend::TaskRef::synthetic_for_tests`. Un llamante que la
    /// use tiene el [`Backend`] ENTERO de la conexión autenticada del agente —
    /// `sync_apply` incluido, cuya ausencia es justo lo que decide ADR 0050.
    /// Las tools pasan por aquí; nadie más debería.
    ///
    /// # Errors
    /// [`BridgeError::Streams`] si el daemon no acepta la segunda conexión
    /// (socket caído, sesión rechazada).
    #[doc(hidden)]
    pub async fn streams(&self) -> Result<&Backend, BridgeError> {
        self.streams
            .get_or_try_init(|| async {
                let remote = RemoteBackend::connect_as_agent(
                    self.socket.clone(),
                    client_info(),
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
            "sync_plan" => self.tool_sync_plan(args).await,
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
    /// drenar, se CANCELA la task y el payload dice `truncated: true`. Una
    /// truncación silenciosa sería peor que el tope: un modelo que la lea como
    /// completa reportaría dos árboles como iguales cuando no se sabe.
    ///
    /// # El deadline
    /// Mismo tope que el resto de tools que esperan una task
    /// ([`TASK_WAIT`]): con el rung de `hash` encendido una comparación puede
    /// durar horas —lo dice la propia rustdoc de `fs.compare`— y ocuparía uno
    /// de los [`MAX_INFLIGHT_TOOLS`] huecos sin contestar jamás. Pasado el
    /// deadline se devuelve lo drenado con `timed_out: true` y `complete:
    /// false`, y la task del daemon se cancela al soltar el guard.
    ///
    /// # `complete`, y por qué no basta el estado terminal
    /// La misma distinción que el código de salida 2 de `norte compare` (el
    /// comando del CLI, `compare_cmd`), MÁS la cuenta de filas. El feed de
    /// `compare.rows` se enruta con `OnFull::DropBatch`: un lote descartado
    /// —buffer del cliente lleno, carrera de teardown del route, evicción del
    /// outbox— solo deja un `warn!`, el canal se cierra LIMPIO y la task acaba
    /// `Completed`. O sea que «terminal y sin truncar» no demuestra que las
    /// filas estén todas. `TaskProgress::entries_done` cuenta las filas
    /// ENVIADAS y es —por contrato de `norte_core::compare`— la única señal
    /// con la que un cliente detecta que se le perdió una notificación, así
    /// que `complete` exige además que cuadre con las filas recibidas, y
    /// `rows_total` la publica para que el modelo VEA el hueco en vez de
    /// tener que deducirlo.
    ///
    /// Esto se apartó una vez por paridad con el CLI. La paridad no aplica: un
    /// humano lee un pane de diferencias, y un booleano llamado `complete` en
    /// un esquema le dice a un modelo que la comparación llegó al final.
    async fn tool_compare(&self, args: &Value) -> Result<Value, String> {
        let left = vpath_arg(args, "left")?;
        let right = vpath_arg(args, "right")?;
        let criteria = compare_criteria_arg(args)?;
        let max_depth = opt_u64_arg(args, "max_depth")?
            .map(|d| u32::try_from(d).map_err(|_| format!("arg max_depth too large: {d}")))
            .transpose()?;
        let mtime_tolerance_ms = opt_u64_arg(args, "mtime_tolerance_ms")?
            .map(|t| u32::try_from(t).map_err(|_| format!("arg mtime_tolerance_ms too large: {t}")))
            .transpose()?
            .unwrap_or_else(default_mtime_tolerance_ms);
        let limit = stream_limit_arg(args, COMPARE_ROWS_DEFAULT, COMPARE_ROWS_MAX)?;

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
        let task_id = task.id();
        let progress = task.progress();
        // Armado ANTES del primer await sobre el stream: si el agente manda
        // `notifications/cancelled` (o el transporte muere), el future de esta
        // tool se SUELTA y el walk del daemon seguiría leyendo —y hasheando—
        // los dos árboles enteros. Ver la rustdoc del guard.
        let mut guard = CancelOnAbandon::new(task.canceller());

        // Filas fieles al wire: `CompareRow` serializa `verdict`/`criterion`/
        // `confidence` como los valores snake_case del protocolo (nunca una
        // etiqueta traducida) y las rutas de sus `Entry` como `to_wire()` —
        // es la MISMA forma que `norte compare --json` ya expone (regla 1;
        // jamás una cadena lossy). Se decide fila a fila si truncar en vez de
        // colectar el lote entero primero: un lote agotando exactamente el
        // resto del tope no debe arrastrar una fila de más.
        let mut rows: Vec<methods::CompareRow> = Vec::new();
        let mut truncated = false;
        let drained = tokio::time::timeout(TASK_WAIT, async {
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
            task.join().await
        })
        .await;
        // Solo el camino que VIO el terminal desarma: el del deadline deja que
        // el guard cancele al volver.
        let state = match drained {
            Ok(state) => {
                guard.disarm();
                Some(state)
            }
            Err(_) => None,
        };
        let emitted = progress.borrow().entries_done;
        let complete = stream_is_complete(truncated, state.as_ref(), rows.len(), emitted);

        let encoded = serde_json::to_value(&rows)
            .map_err(|e| format!("tool compare: could not encode rows: {e}"))?;
        Ok(json!({
            "task_id": task_id.get(),
            "rows": encoded,
            "rows_total": emitted,
            "truncated": truncated,
            "timed_out": state.is_none(),
            "state": state_label(state.as_ref()),
            "error": state_error(state.as_ref()),
            "complete": complete,
        }))
    }

    /// `sync_plan`: qué HARÍA una sincronización de un lado al otro. NO aplica
    /// nada — no hay tool `sync_apply` (spec 3 §2.1): aplicar es una acción de
    /// un HUMANO en su propio cliente, y el puente no la ofrece.
    ///
    /// Va por [`Self::streams`], igual que [`Self::tool_compare`]: `sync.plan`
    /// entrega sus pasos por notificación (`sync.steps`* y un
    /// `sync.plan_done`) y esta conexión de tools no las enruta (ver la
    /// rustdoc de [`Self::streams`]).
    ///
    /// # El hash NO viaja
    /// [`methods::SyncPlanDone::plan_hash`] queda retenido SOLO para la
    /// conexión de [`Self::streams`] — un plan aprobado por esta llamada no es
    /// redimible desde la conexión de tools (no hay tool que lo intente), así
    /// que el hash es un valor que NADIE fuera de esta llamada puede usar. Se
    /// omite del payload a propósito: un valor que no sirve para nada es una
    /// invitación a intentarlo de todos modos.
    ///
    /// El `task_id` sí viaja, y no por el hash: las dos conexiones del puente
    /// son el MISMO `Actor::Agent { session }`, y el criterio de visibilidad
    /// del daemon es igualdad de actor, así que la conexión de tools puede
    /// observar (`task_status`) y cancelar la task que abrió el brazo de
    /// streams. Quitarlo dejaba a un agente sin nada que hacer con un plan que
    /// tardaba.
    ///
    /// # El tope de pasos, y el deadline
    /// Mismo contrato que [`Self::tool_compare`] (mismos nombres de campo, a
    /// propósito): al llegar a `limit` ([`SYNC_STEPS_DEFAULT`], techo
    /// [`SYNC_STEPS_MAX`]) se deja de drenar, se CANCELA la task y el payload
    /// dice `truncated: true`; pasado [`TASK_WAIT`] se devuelve lo drenado con
    /// `timed_out: true`. Cancelada la task, `sync.plan_done` JAMÁS llega
    /// (`run_sync_plan` no lo emite en el camino de error: ver
    /// `norte_core::sync::run_sync_plan`), así que `counts`/`dest_trash`/
    /// `blockers`/`blockers_total`/`executable` quedan AUSENTES del payload —
    /// nunca puestos a cero ni inventados — y es exactamente lo que
    /// `complete: false` avisa.
    ///
    /// # `complete` cuadra los pasos contra `counts`
    /// El `Done` es un detector de pérdida más fuerte que el de
    /// [`Self::tool_compare`] —el feed de `sync.steps` se enruta con
    /// `OnFull::CloseFeed`, así que un lote perdido cierra el canal y el
    /// `Done` no llega— pero no lo cubre todo: un lote que llega ANTES de que
    /// el route esté registrado y encuentra el `pending` lleno se descarta con
    /// una traza y sin cerrar nada, de modo que el `Done` puede aparecer con
    /// una lista de pasos que no es la que `counts` cuenta. `counts` se rehace
    /// paso a paso en el spool (es lo que el ejecutor mira para pedir sus
    /// puertas de policy), así que la suma de sus clases ES el total de pasos
    /// del plan: si no cuadra con los recibidos, esto no está completo. Va
    /// también en el payload como `steps_total`.
    async fn tool_sync_plan(&self, args: &Value) -> Result<Value, String> {
        let (params, limit) = sync_plan_args(args)?;

        let (task, mut rx) = self
            .streams()
            .await
            .map_err(|e| e.to_string())?
            .sync_plan(params)
            .await
            .map_err(map_plan_err)?;
        let task_id = task.id();
        let mut guard = CancelOnAbandon::new(task.canceller());

        // Pasos fieles al wire: `SyncStep` serializa `kind`/`criterion`/
        // `confidence`/`reversal`/`reason` como los valores snake_case del
        // protocolo (regla 1; jamás una etiqueta traducida) y `rel`/`dest_rel`
        // como sus bytes de wire.
        let mut steps: Vec<methods::SyncStep> = Vec::new();
        let mut truncated = false;
        let mut done: Option<methods::SyncPlanDone> = None;
        let drained = tokio::time::timeout(TASK_WAIT, async {
            'drain: while let Some(event) = rx.recv().await {
                match event {
                    norte_core::sync::SyncPlanEvent::Steps(batch) => {
                        for step in batch.steps {
                            if steps.len() >= limit {
                                truncated = true;
                                break 'drain;
                            }
                            steps.push(step);
                        }
                    }
                    norte_core::sync::SyncPlanEvent::Done(d) => {
                        // Como mucho UNO por Task, y siempre el último — nada que
                        // seguir drenando después.
                        done = Some(d);
                        break;
                    }
                }
            }
            if truncated {
                // Cooperativa (regla dura 3): igual que en `tool_compare`, sin
                // esto el planificador seguiría recorriendo (y comparando) los dos
                // árboles enteros para un `rx` que ya nadie drena.
                task.cancel();
            }
            task.join().await
        })
        .await;
        let state = match drained {
            Ok(state) => {
                guard.disarm();
                Some(state)
            }
            Err(_) => None,
        };
        let steps_total = done.as_ref().map(|d| plan_steps_total(&d.counts));
        let complete = done.is_some()
            && stream_is_complete(
                truncated,
                state.as_ref(),
                steps.len(),
                steps_total.unwrap_or_default(),
            );

        let encoded = serde_json::to_value(&steps)
            .map_err(|e| format!("tool sync_plan: could not encode steps: {e}"))?;
        let mut payload = json!({
            "task_id": task_id.get(),
            "steps": encoded,
            "truncated": truncated,
            "timed_out": state.is_none(),
            "state": state_label(state.as_ref()),
            "error": state_error(state.as_ref()),
            "complete": complete,
        });
        if let (Some(d), Some(total)) = (done, steps_total) {
            merge_plan_done(&mut payload, &d, total)?;
        }
        Ok(payload)
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

/// Los args de `sync_plan` → `(params, limit)`. Aparte de la tool solo por el
/// lint de longitud, y el reparto es el natural: aquí no se toca el daemon.
fn sync_plan_args(args: &Value) -> Result<(methods::SyncPlanParams, usize), String> {
    let source = vpath_arg(args, "source")?;
    let dest = vpath_arg(args, "dest")?;
    // `mode` no tiene valor neutro entre copiar y borrar (igual que en el
    // wire, `SyncPlanParams::mode` no lleva `#[serde(default)]`): AUSENTE es
    // tan error como MALFORMADO — jamás una degradación silenciosa (mismo
    // criterio que `tool_delete::mode`, con el matiz de que aquí no hay
    // default que ofrecer).
    let mode = match args.get("mode") {
        Some(Value::String(s)) if s == "update" => methods::SyncMode::Update,
        Some(Value::String(s)) if s == "mirror" => methods::SyncMode::Mirror,
        None | Some(Value::Null) => {
            return Err("missing arg mode: use \"update\"|\"mirror\" (no default)".to_owned());
        }
        Some(other) => return Err(format!("invalid mode {other}: use \"update\"|\"mirror\"")),
    };
    let criteria = compare_criteria_arg(args)?;
    let on_unknown = match args.get("on_unknown") {
        None | Some(Value::Null) => methods::OnUnknown::Copy,
        Some(Value::String(s)) if s == "copy" => methods::OnUnknown::Copy,
        Some(Value::String(s)) if s == "skip" => methods::OnUnknown::Skip,
        Some(other) => return Err(format!("invalid on_unknown {other}: use \"copy\"|\"skip\"")),
    };
    let limit = stream_limit_arg(args, SYNC_STEPS_DEFAULT, SYNC_STEPS_MAX)?;
    let params = methods::SyncPlanParams {
        source,
        dest,
        mode,
        compare: methods::SyncCompareOptions {
            criteria,
            // `max_depth`/`mtime_tolerance_ms` no son argumentos de esta tool
            // (spec 3 §4): el default del wire alcanza para un plan agéntico,
            // y `follow_symlinks`/`descend_orphans` NO son del llamante en
            // `sync.plan` — pedirlos es `-32602` server-side. La descripción
            // de la tool lo DICE, porque un agente que acaba de comparar con
            // `max_depth: 2` recibiría si no un plan sobre el árbol entero sin
            // ninguna señal.
            ..methods::SyncCompareOptions::default()
        },
        on_unknown,
        // El puente no ofrece seleccionar un subárbol del plan: es una
        // superficie del panel de diferencias (spec 3 §4), no de un agente que
        // aún no ha visto las filas.
        include: None,
    };
    Ok((params, limit))
}

/// Funde el `sync.plan_done` en el payload de la tool.
///
/// Serializado del wire y NO reconstruido campo a campo: así la ortografía de
/// `counts`/`dest_trash`/`blockers`/`executable` es EXACTAMENTE la del
/// protocolo sin copiarla a mano dos veces. Se quitan antes `plan_hash` (no le
/// sirve a nadie fuera de la conexión de streams) y el `task_id` del wire, que
/// es EL MISMO que el puente ya puso.
///
/// **La fusión no PISA.** `serde_json::Map::extend` sobrescribe, y extender el
/// payload CON el `Done` significa que un campo futuro del wire llamado
/// `complete` —o `truncated`, o `state`— reemplazaría en silencio la bandera
/// de honestidad que este puente calcula. Latente hoy; una clave nueva del
/// wire no debería poder romperlo.
///
/// # Errors
/// Si el `SyncPlanDone` no serializa (no puede: struct plano).
fn merge_plan_done(
    payload: &mut Value,
    done: &methods::SyncPlanDone,
    steps_total: u64,
) -> Result<(), String> {
    let done_json = serde_json::to_value(done)
        .map_err(|e| format!("tool sync_plan: could not encode plan_done: {e}"))?;
    let (Value::Object(mut obj), Some(target)) = (done_json, payload.as_object_mut()) else {
        return Ok(());
    };
    obj.remove("plan_hash");
    obj.remove("task_id");
    for (k, v) in obj {
        match target.entry(k) {
            serde_json::map::Entry::Vacant(e) => {
                e.insert(v);
            }
            serde_json::map::Entry::Occupied(e) => tracing::warn!(
                key = %e.key(),
                "sync.plan_done trae una clave que el puente ya fija: se conserva la del puente"
            ),
        }
    }
    target.insert("steps_total".to_owned(), json!(steps_total));
    Ok(())
}

/// Como [`map_backend_err`], pero para `sync.plan`, cuyo error más probable en
/// uso normal llega como «internal error».
///
/// El daemon retiene como mucho 16 planes POR CONEXIÓN (10 min de TTL) y
/// rehúsa el 17.º con `OVERLOADED` y un mensaje que dice exactamente qué pasa.
/// Ese mensaje se pierde: `RpcError::protocol` no lleva `data`, y
/// `RemoteBackend::to_taxonomy` colapsa un `Rpc` sin `data` en
/// `Error::Internal { panic: false }`, que se imprime «internal error (panic:
/// false)». El puente tiene UNA conexión de streams para todo el proceso, no
/// puede aplicar y no hay método para descartar, así que el tope es alcanzable
/// sin hacer nada raro — y «internal error» es justo el texto que hace que un
/// agente reintente en bucle, que es lo que llenó el tope.
///
/// Se nombra la causa probable sin afirmarla: el mensaje del daemon no ha
/// llegado hasta aquí y decir que fue el tope cuando fue otra cosa sería la
/// misma clase de mentira. Que el texto del RPC viaje entero es
/// <https://github.com/compilando/norte/issues/182>.
fn map_plan_err(e: norte_proto::Error) -> String {
    match e {
        norte_proto::Error::Internal { panic: false } => format!(
            "sync.plan was refused and the daemon's reason did not survive the trip \
             (issue #182). The cause reachable in normal use is the cap of \
             {MAX_RETAINED_SYNC_PLANS_HINT} retained plans per connection, which expire on \
             their own after ~{SYNC_PLAN_TTL_MIN_HINT} minutes. Do NOT retry in a loop: \
             report the plans you already have and let the older ones expire."
        ),
        other => map_backend_err(other),
    }
}

/// Los dos números que [`map_plan_err`] nombra, y que el daemon no exporta:
/// `MAX_RETAINED_SYNC_PLANS` es privado de `norte_core::daemon::server`. Si
/// alguno cambia allí, este texto miente — por eso están aquí arriba y con
/// nombre, y no incrustados en un `format!`.
const MAX_RETAINED_SYNC_PLANS_HINT: usize = 16;
/// El TTL del plan retenido, en minutos ([`methods::SYNC_PLAN_TTL_MS`]).
const SYNC_PLAN_TTL_MIN_HINT: u64 = methods::SYNC_PLAN_TTL_MS / 60_000;

/// Traduce el arg opcional `criteria` (array de strings) de `compare` y de
/// `sync_plan` a [`methods::CompareCriteria`]: ausente o `null` = el default
/// del wire (tamaño + fecha, sin hash); presente = EXACTAMENTE la lista
/// pedida, nunca sumada al default (`["hash"]` a secas enciende solo `hash`).
///
/// Una lista VACÍA es un ERROR, y es aquí donde este contrato **no** es el del
/// `--criteria` del CLI. `parse_compare_criteria` trata la lista vacía como
/// ausente porque clap no distingue «no lo dijo» de «lo dijo vacío»; JSON sí
/// los distingue, y tratarlos igual costaría lo siguiente:
///
/// - en `compare`, ningún rung decide, así que `norte_compare` da
///   `same`/`presence`/`unknown` a TODA pareja presente en los dos lados. Con
///   `complete: true`. El agente reporta dos árboles idénticos sin haber
///   comparado nada.
/// - en `sync_plan`, peor: `Same` + `Unknown` con el `on_unknown: copy` que es
///   el default de esta tool produce un `Overwrite` POR FICHERO. `criteria:
///   []` convertiría «planifica una actualización» en «reescribe el destino
///   entero», con `executable: true`.
///
/// Un argumento vacío no puede ser la forma más corta de pedir eso. Se rechaza
/// con el mismo criterio que `mode`, que tampoco tiene valor neutro, y los dos
/// esquemas llevan además `"minItems": 1` — que es asesor, así que el chequeo
/// vive aquí.
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
    if !(criteria.size || criteria.mtime || criteria.hash) {
        return Err(
            "arg criteria: name at least one of \"size\", \"mtime\", \"hash\" \
                    (an empty list runs no comparison at all and would report the two \
                    trees as equal); omit criteria for the wire default (size+mtime)"
                .to_owned(),
        );
    }
    Ok(criteria)
}

/// Tolerancia de mtime por defecto, LEÍDA del wire en vez de copiada: el
/// literal vive en `norte_proto`'s `default_mtime_tolerance_ms` (privado) y
/// [`methods::SyncCompareOptions::default`] es su única salida pública. Copiar
/// el `2000` aquí es como el día que el wire lo cambie el puente se quede con
/// el viejo.
fn default_mtime_tolerance_ms() -> u32 {
    methods::SyncCompareOptions::default().mtime_tolerance_ms
}

/// El `limit` de una tool de stream (`compare`, `sync_plan`): ausente = su
/// default, recortado al techo duro, y `0` es ERROR.
///
/// `limit: 0` no es «cero filas por decisión del caller», es un argumento sin
/// sentido (encoding-auditor, revisión de la tarea 2): sin este chequeo saldría
/// `truncated: true` con la lista vacía en el PRIMER elemento, indistinguible
/// de un árbol de verdad truncado. El esquema JSON ya dice `"minimum": 1`, pero
/// eso es asesor — un MCP client real puede mandarlo igual.
fn stream_limit_arg(args: &Value, default: usize, max: usize) -> Result<usize, String> {
    let limit = opt_u64_arg(args, "limit")?
        .map(|l| usize::try_from(l).map_err(|_| format!("arg limit too large: {l}")))
        .transpose()?
        .unwrap_or(default)
        .min(max);
    if limit == 0 {
        return Err("arg limit must be >= 1".to_owned());
    }
    Ok(limit)
}

/// El total de PASOS de un plan según sus contadores: la suma de las clases
/// (`unknown_kind` incluida, que existe justo para que no falte ninguna).
///
/// Es lo que [`Bridge::tool_sync_plan`] cuadra contra los pasos recibidos.
/// Saturante, como las sumas de [`methods::SyncCounts::add`]: un contador
/// desbordado es un número raro, un pánico en el camino de un plan de medio
/// millón de pasos es una tool muerta. `irreversible` NO se suma — es
/// transversal a las clases, lo dice su rustdoc — ni `unmeasured_steps`, que
/// es un subconjunto de `copy`+`overwrite`.
fn plan_steps_total(counts: &methods::SyncCounts) -> u64 {
    counts
        .create_dir
        .saturating_add(counts.copy)
        .saturating_add(counts.overwrite)
        .saturating_add(counts.delete_tree)
        .saturating_add(counts.skip)
        .saturating_add(counts.unknown_kind)
}

/// ¿Se puede afirmar que este resultado de stream es TODO lo que había?
///
/// Cuatro condiciones, y ninguna sobra:
///
/// 1. no se truncó (el caller puso el tope);
/// 2. se vio el estado terminal (`None` = venció [`TASK_WAIT`]);
/// 3. ese estado es `Completed` — `Cancelled` y `Failed` no son un final
///    limpio, y devolver una lista vacía sin decirlo se lee como «no hay
///    diferencias»;
/// 4. lo recibido cuadra con lo que el daemon dice haber emitido. Es la
///    condición que faltaba: los lotes se pueden perder DEJANDO la task en
///    `Completed` (ver la rustdoc de [`Bridge::tool_compare`]).
fn stream_is_complete(
    truncated: bool,
    state: Option<&norte_proto::TaskState>,
    got: usize,
    emitted: u64,
) -> bool {
    !truncated
        && matches!(state, Some(norte_proto::TaskState::Completed))
        && u64::try_from(got).is_ok_and(|got| got == emitted)
}

/// Nombre del estado terminal para el payload de una tool de stream. `None`
/// (venció el deadline sin verlo) es `"running"`: la task del daemon seguía
/// viva cuando esta tool dejó de mirarla.
fn state_label(state: Option<&norte_proto::TaskState>) -> &'static str {
    match state {
        Some(norte_proto::TaskState::Completed) => "completed",
        Some(norte_proto::TaskState::Cancelled) => "cancelled",
        Some(norte_proto::TaskState::Failed { .. }) => "failed",
        _ => "running",
    }
}

/// El porqué de un `"failed"`, o `null`. Va al lado de [`state_label`]: sin él,
/// una comparación que reventó y una que no encontró diferencias son la misma
/// lista vacía.
fn state_error(state: Option<&norte_proto::TaskState>) -> Value {
    match state {
        Some(norte_proto::TaskState::Failed { error }) => json!(error.to_string()),
        _ => Value::Null,
    }
}

/// `ClientInfo` del puente, uno solo para las DOS conexiones de la sesión.
fn client_info() -> methods::ClientInfo {
    methods::ClientInfo {
        name: "norte-mcp".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// Cancela la Task de una tool de stream al SOLTARSE, salvo que se haya
/// desarmado.
///
/// El puente ya cancelaba al truncar. Faltaba el otro motivo, que es el mismo
/// hecho: `notifications/cancelled` (y la muerte del transporte) sueltan el
/// future de la tool, y `TaskRef` no tiene `Drop` — soltar el `rx` local solo
/// quita el route del cliente, mientras la bomba del daemon sigue mandando
/// lotes por una conexión viva, cobra `true` y no ve jamás un `ReceiverGone`.
/// El walk (y con `criteria: ["hash"]`, la lectura ENTERA de los dos árboles)
/// seguía hasta el final para nadie. Cancelar porque dejamos de leer pero no
/// porque el agente dejó de querer no es una regla coherente; y el hueco era
/// explotable dentro de un scope legítimo: abrir comparaciones con hash y
/// cancelarlas al momento devuelve el hueco de [`MAX_INFLIGHT_TOOLS`] al
/// instante y deja el walk corriendo, hasta `MAX_LIVE_TASKS_AGENTS`.
///
/// Mismo patrón que `CancelOnAbandon` de `norte_core::backend::remote`
/// (drop-guard armado que se desarma en el camino normal).
struct CancelOnAbandon {
    /// `None` = desarmado (se vio el terminal: no hay nada que cancelar).
    canceller: Option<norte_core::backend::TaskCanceller>,
}

impl CancelOnAbandon {
    fn new(canceller: norte_core::backend::TaskCanceller) -> Self {
        Self {
            canceller: Some(canceller),
        }
    }

    /// El camino normal: la Task ya es terminal.
    fn disarm(&mut self) {
        self.canceller = None;
    }
}

impl Drop for CancelOnAbandon {
    fn drop(&mut self) {
        if let Some(canceller) = self.canceller.take() {
            canceller.cancel();
        }
    }
}

/// Snapshot de una task como JSON de tool (estado + error por categoría).
fn task_json(t: &norte_proto::TaskProgress) -> Value {
    json!({
        "task_id": t.task_id.get(),
        // Mismo vocabulario que las tools de stream: un modelo que lea
        // `task_status` y `compare` no aprende dos nombres para un estado.
        "state": state_label(Some(&t.state)),
        "error": state_error(Some(&t.state)),
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

/// Descripción del argumento `path` de toda tool que toma uno. Una sola vez:
/// estaba copiada en tres sitios, y tres copias de una descripción de esquema
/// divergen igual que dos copias de un handshake.
const PATH_DESC: &str =
    "VPath URL: file:///…, sftp://host/…, s3://bucket/…, or composed zip:file:///a.zip!/inside";

/// Las diez tools del puente: las ocho v1 (ADR 0024, superficie = lo que el
/// wire ya ofrece) más `compare` y `sync_plan` (spec 3 fase B), las primeras
/// que consumen el brazo de streams en vez de responder directo.
///
/// **Una función por tool, y esta lista es solo el orden.** Antes eran ocho
/// dentro de un `json!([…])` y dos aparte, sin nada que dijera cuál de las dos
/// convenciones estrena la undécima; y ese `json!([…])` obligaba a un
/// `unreachable!` para desempaquetar el `Value::Array`, o sea un camino de
/// pánico en código que no es de test.
fn tool_defs() -> Value {
    Value::Array(vec![
        list_dir_tool_def(),
        stat_tool_def(),
        read_file_tool_def(),
        transfer_tool_def("copy"),
        transfer_tool_def("move"),
        delete_tool_def(),
        task_status_tool_def(),
        request_scope_tool_def(),
        compare_tool_def(),
        sync_plan_tool_def(),
    ])
}

fn list_dir_tool_def() -> Value {
    json!({
        "name": "list_dir",
        "description": "List a directory managed by norte. Returns entries (path/kind/size/mtime_ms) and next_cursor when paginated. size/mtime_ms may be null (lazy listing); use stat for a specific path.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "limit": {"type": "integer", "minimum": 1, "description": "page size"},
                "cursor": {"type": "string", "description": "next_cursor from the previous page"}
            },
            "required": ["path"]
        }
    })
}

fn stat_tool_def() -> Value {
    json!({
        "name": "stat",
        "description": "Metadata of one node (kind/size/mtime_ms).",
        "inputSchema": {
            "type": "object",
            "properties": {"path": {"type": "string", "description": PATH_DESC}},
            "required": ["path"]
        }
    })
}

fn read_file_tool_def() -> Value {
    json!({
        "name": "read_file",
        "description": "Read a byte range of a file. Returns text (UTF-8; lossy if not) plus base64 of the exact bytes when the content is not valid UTF-8, and eof. If the byte range splits a multibyte character, text will contain U+FFFD at the edges and base64 carries the exact bytes: reassemble multi-chunk reads from base64, never by concatenating text.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "offset": {"type": "integer", "minimum": 0},
                "len": {"type": "integer", "minimum": 1, "description": "max bytes (capped server-side)"}
            },
            "required": ["path"]
        }
    })
}

/// `copy` y `move`: MISMO esquema y misma gobernanza, así que una función con
/// el nombre por parámetro en vez de dos copias que se separen.
fn transfer_tool_def(name: &str) -> Value {
    let description = if name == "copy" {
        "Copy a file or directory (recursive). Runs as a cancellable task and this call waits for its outcome. Requires a granted scope; an `ask` policy suspends until a human approves."
    } else {
        "Move/rename a file or directory. Same governance as copy (scope + policy)."
    };
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "from": {"type": "string", "description": PATH_DESC},
                "to": {"type": "string", "description": "exact destination path (existing destination = conflict)"}
            },
            "required": ["from", "to"]
        }
    })
}

fn delete_tool_def() -> Value {
    json!({
        "name": "delete",
        "description": "Delete a file or directory (recursive). Default mode is trash (recoverable); permanent requires explicit mode and policy approval.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "mode": {"type": "string", "enum": ["trash", "permanent"], "description": "default trash"}
            },
            "required": ["path"]
        }
    })
}

fn task_status_tool_def() -> Value {
    json!({
        "name": "task_status",
        "description": "Current state of a norte task by id (running/completed/failed/cancelled). Works for the task_id returned by compare and sync_plan too: both connections of this bridge are the same agent actor.",
        "inputSchema": {
            "type": "object",
            "properties": {"task_id": {"type": "integer", "minimum": 1}},
            "required": ["task_id"]
        }
    })
}

fn request_scope_tool_def() -> Value {
    json!({
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
    })
}

/// La definición de `compare` (spec 3 fase B, tarea 2).
///
/// Los argumentos se llaman `left`/`right` y no `a`/`b` porque las filas
/// contestan en `left`/`right`: un modelo no debería tener que deducir cuál de
/// los dos era `a`.
fn compare_tool_def() -> Value {
    json!({
        "name": "compare",
        "description": "Compare two trees (files and directories) and report what differs between them. Read-only: this mutates nothing. Each row carries WIRE values (verdict/criterion/confidence as the protocol spells them, e.g. \"only_left\", never a translated label) and both paths as their wire form. `complete` is the ONLY field to trust before saying anything about the two trees: it is true just when the run reached its end, was not truncated, and the rows received match the count the daemon says it emitted (`rows_total`). It is false when the run was truncated at `limit` (and then the comparison was CANCELLED), when the deadline expired (`timed_out`), when the task ended `cancelled` or `failed` (`state` says which, `error` says why), and when rows went missing in transit. A false `complete` means UNKNOWN — never \"these trees match\".",
        "inputSchema": {
            "type": "object",
            "properties": {
                "left": {"type": "string", "description": PATH_DESC},
                "right": {"type": "string", "description": PATH_DESC},
                "criteria": {"type": "array", "minItems": 1, "items": {"type": "string", "enum": ["size", "mtime", "hash"]}, "description": "which rungs to run; absent = size+mtime (the wire default). An EMPTY list is rejected: it would run no rung at all and call every pair equal. hash reads full file contents on both sides and requires content scope."},
                "max_depth": {"type": "integer", "minimum": 0, "description": "root counts as depth 0; absent = unlimited"},
                "mtime_tolerance_ms": {"type": "integer", "minimum": 0, "description": "absent = the wire default (2000 ms, FAT-safe)"},
                "limit": {"type": "integer", "minimum": 1, "description": "max rows before the comparison is cancelled and truncated:true; default 500"}
            },
            "required": ["left", "right"]
        }
    })
}

/// La definición de `sync_plan` (spec 3 fase B, tarea 3).
fn sync_plan_tool_def() -> Value {
    json!({
        "name": "sync_plan",
        "description": "Returns what a one-way synchronisation would do. It does NOT apply anything, and there is no tool that does: applying is a human action in their own client. The plan's hash is retained for THIS connection only, so it cannot be handed to a user or another tool — report what would change and let the human plan it again in their client. `task_id` IS usable: poll it with task_status. `complete` is false whenever the run did not reach a clean end for ANY reason (the `limit` was hit and the plan was cancelled and `truncated` is true; the deadline expired and `timed_out` is true; a human cancelled the underlying task; planning failed; or steps went missing in transit, which is what `steps_total` lets you see) — `truncated` only names one of those causes, so check `complete`, not `truncated`, before trusting anything. Whenever the plan did not close, `counts`/`dest_trash`/`blockers`/`blockers_total`/`executable`/`steps_total` are ABSENT from the result (never zero, never invented) — treat their absence exactly like a truncated `compare`: unknown, not clean. `mode: \"mirror\"` deletes from the destination what the source does not have; `dest_trash` says whether that (and any overwrite) is recoverable at all — \"restorable\" (undo can bring it back), \"opaque\" (a trash exists but norte cannot name what it buried) or \"absent\" (nothing comes back). `executable` is the only field that says whether a human could actually run this plan as-is; a non-empty `blockers` (or `blockers_total` bigger than the list) means they could not, whatever the steps look like. This tool takes NEITHER max_depth NOR mtime_tolerance_ms: it always plans over the whole tree with the wire's default tolerance, so a plan does not inherit the narrowing of a compare you ran first. Approved plans are retained server-side per connection with a cap and a TTL of several minutes; a run that fails outright (not `truncated`) rather than returning a `complete: false` result may mean that cap was hit — wait for older plans to expire rather than retrying sync_plan in a tight loop.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "source": {"type": "string", "description": "Where the bytes come from. VPath URL: file:///…, sftp://host/…, s3://bucket/…, or composed zip:file:///a.zip!/inside"},
                "dest": {"type": "string", "description": "Where they would go. VPath URL: file:///…, sftp://host/…, s3://bucket/…, or composed zip:file:///a.zip!/inside"},
                "mode": {"type": "string", "enum": ["update", "mirror"], "description": "REQUIRED, no default: \"update\" only copies/overwrites what differs; \"mirror\" does that plus deletes from dest what source does not have."},
                "criteria": {"type": "array", "minItems": 1, "items": {"type": "string", "enum": ["size", "mtime", "hash"]}, "description": "which rungs to run; absent = size+mtime (the wire default). An EMPTY list is rejected: with no rung the planner would call every pair equal-but-unknown and, under the default on_unknown:copy, plan an Overwrite for every single file. hash reads full file contents on both sides and requires content scope."},
                "on_unknown": {"type": "string", "enum": ["copy", "skip"], "description": "what to do with a row the provider could not compare with certainty; default copy."},
                "limit": {"type": "integer", "minimum": 1, "description": "max steps before the plan is cancelled and truncated:true; default 500"}
            },
            "required": ["source", "dest", "mode"]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// BLOCKER: `criteria: []` no es "el default", es apagar los TRES rungs.
    /// En `compare` deja `same/presence/unknown` en toda pareja presente en los
    /// dos lados —dos árboles "idénticos" sin comparar nada— y en `sync_plan`,
    /// con el `on_unknown: copy` que es su default, un `Overwrite` por fichero.
    #[test]
    fn criteria_vacia_es_error_y_no_el_default_del_wire() {
        let err = compare_criteria_arg(&json!({"criteria": []})).expect_err("lista vacía");
        assert!(err.contains("criteria"), "{err}");
        // Ausente sí es el default del wire (tamaño + fecha, sin hash).
        let d = compare_criteria_arg(&json!({})).expect("ausente");
        assert_eq!(d, methods::CompareCriteria::default());
        assert!(d.size && d.mtime && !d.hash, "{d:?}");
        // Y `null` también: es "no lo dijo", no "lo dijo vacío".
        assert_eq!(
            compare_criteria_arg(&json!({"criteria": Value::Null})).expect("null"),
            methods::CompareCriteria::default()
        );
        // Presente = exactamente lo pedido, jamás sumado al default.
        let solo_hash = compare_criteria_arg(&json!({"criteria": ["hash"]})).expect("hash");
        assert!(!solo_hash.size && !solo_hash.mtime && solo_hash.hash);
    }

    /// Los dos esquemas lo dicen además en el contrato que el modelo lee.
    #[test]
    fn los_esquemas_prohiben_la_lista_vacia_de_criteria() {
        for def in [compare_tool_def(), sync_plan_tool_def()] {
            assert_eq!(
                def["inputSchema"]["properties"]["criteria"]["minItems"],
                json!(1),
                "{def}"
            );
        }
    }

    /// BLOCKER: el estado terminal NO demuestra que estén todas las filas. El
    /// feed de `compare.rows` se enruta con `OnFull::DropBatch`, así que un
    /// lote perdido deja la Task en `Completed` y el canal cerrado limpio.
    #[test]
    fn completo_exige_que_las_filas_cuadren_con_las_emitidas() {
        let completed = norte_proto::TaskState::Completed;
        assert!(stream_is_complete(false, Some(&completed), 7, 7));
        assert!(
            !stream_is_complete(false, Some(&completed), 6, 7),
            "una fila perdida en vuelo con la task en Completed NO es completo"
        );
        assert!(
            !stream_is_complete(true, Some(&completed), 7, 7),
            "truncado"
        );
        assert!(!stream_is_complete(false, None, 7, 7), "sin terminal visto");
        assert!(
            !stream_is_complete(false, Some(&norte_proto::TaskState::Cancelled), 7, 7),
            "cancelada"
        );
        assert!(
            !stream_is_complete(
                false,
                Some(&norte_proto::TaskState::Failed {
                    error: norte_proto::Error::Internal { panic: false }
                }),
                7,
                7
            ),
            "fallida"
        );
    }

    /// El total de pasos de un plan es la suma de las CLASES; `irreversible`
    /// es transversal y `unmeasured_steps` un subconjunto, así que ninguno
    /// suma.
    #[test]
    fn el_total_de_pasos_suma_las_clases_y_solo_las_clases() {
        let counts = methods::SyncCounts {
            create_dir: 1,
            copy: 2,
            overwrite: 3,
            delete_tree: 4,
            skip: 5,
            unknown_kind: 6,
            irreversible: 99,
            bytes: 12345,
            unmeasured_steps: 2,
        };
        assert_eq!(plan_steps_total(&counts), 21);
        assert_eq!(plan_steps_total(&methods::SyncCounts::default()), 0);
    }

    /// El deadline y el estado terminal se cuentan aparte en el payload: `None`
    /// es "seguía viva cuando dejamos de mirar", no "acabó".
    #[test]
    fn el_estado_viaja_con_nombre_y_el_fallo_con_su_causa() {
        assert_eq!(state_label(None), "running");
        assert_eq!(
            state_label(Some(&norte_proto::TaskState::Completed)),
            "completed"
        );
        assert_eq!(
            state_label(Some(&norte_proto::TaskState::Cancelled)),
            "cancelled"
        );
        let failed = norte_proto::TaskState::Failed {
            error: norte_proto::Error::PermissionDenied,
        };
        assert_eq!(state_label(Some(&failed)), "failed");
        assert!(state_error(Some(&failed)).is_string(), "el porqué viaja");
        assert!(state_error(Some(&norte_proto::TaskState::Completed)).is_null());
    }

    /// `limit: 0` no es "cero filas a propósito": sin el rechazo saldría
    /// `truncated: true` con la lista vacía, indistinguible de un árbol de
    /// verdad truncado.
    #[test]
    fn el_limit_cero_es_error_y_el_ausente_es_el_default() {
        assert!(stream_limit_arg(&json!({"limit": 0}), 500, 5000).is_err());
        assert_eq!(
            stream_limit_arg(&json!({}), 500, 5000).expect("ausente"),
            500
        );
        assert_eq!(
            stream_limit_arg(&json!({"limit": 9_000_000}), 500, 5000).expect("techo"),
            5000,
            "el techo duro manda sobre lo que pida el caller"
        );
    }

    /// El default de tolerancia se LEE del wire, no se copia.
    #[test]
    fn la_tolerancia_por_defecto_es_la_del_wire() {
        assert_eq!(
            default_mtime_tolerance_ms(),
            methods::SyncCompareOptions::default().mtime_tolerance_ms
        );
    }

    /// El guard cancela al soltarse, y NO cancela si se desarmó (camino
    /// normal: se vio el terminal).
    #[test]
    fn el_guard_cancela_al_abandonar_y_calla_si_se_desarma() {
        let token = CancellationToken::new();
        drop(CancelOnAbandon::new(
            norte_core::backend::TaskCanceller::Embedded(token.clone()),
        ));
        assert!(token.is_cancelled(), "soltar el future cancela el walk");

        let token = CancellationToken::new();
        let mut guard =
            CancelOnAbandon::new(norte_core::backend::TaskCanceller::Embedded(token.clone()));
        guard.disarm();
        drop(guard);
        assert!(!token.is_cancelled(), "el camino normal no cancela nada");
    }

    /// MAJOR: el 17.º plan de una conexión llega como `Internal`, y "internal
    /// error" es justo el texto que hace reintentar a un agente.
    #[test]
    fn el_error_de_plan_nombra_el_tope_en_vez_de_decir_internal() {
        let texto = map_plan_err(norte_proto::Error::Internal { panic: false });
        assert!(texto.contains("retained plans"), "{texto}");
        assert!(texto.contains("#182"), "el issue del arreglo real: {texto}");
        assert!(
            !texto.contains("internal error"),
            "ni siquiera nombrándolo: es LA cadena que hace reintentar: {texto}"
        );
        // Lo demás sigue saliendo con el texto accionable de siempre.
        let denegado = map_plan_err(norte_proto::Error::PolicyDenied {
            rule: "r".to_owned(),
        });
        assert!(denegado.contains("request_scope"), "{denegado}");
    }

    /// Las diez tools, cada una con su función, y ninguna se llama `sync_apply`.
    #[test]
    fn el_catalogo_de_tools_no_tiene_dos_convenciones() {
        let Value::Array(defs) = tool_defs() else {
            panic!("tool_defs devuelve un array")
        };
        let nombres: Vec<&str> = defs
            .iter()
            .map(|d| d["name"].as_str().expect("nombre"))
            .collect();
        assert_eq!(
            nombres,
            [
                "list_dir",
                "stat",
                "read_file",
                "copy",
                "move",
                "delete",
                "task_status",
                "request_scope",
                "compare",
                "sync_plan"
            ]
        );
        // `path_desc` estaba copiado tres veces; ahora es una constante.
        for def in &defs {
            for prop in ["path", "from", "left", "right"] {
                let d = &def["inputSchema"]["properties"][prop]["description"];
                if let Some(text) = d.as_str() {
                    assert_eq!(text, PATH_DESC, "{def}");
                }
            }
        }
    }

    /// `compare` pregunta por los mismos nombres con los que contesta.
    #[test]
    fn compare_pregunta_en_left_y_right_como_contesta() {
        let def = compare_tool_def();
        assert_eq!(def["inputSchema"]["required"], json!(["left", "right"]));
    }
}
