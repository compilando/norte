//! Métodos del protocolo (spec §11): nombres de método JSON-RPC y sus tipos
//! de params/result. M0 cubre la familia `fs.*` y la notificación
//! `task.progress`; el resto de familias llega con sus hitos.
//!
//! Convención: cada método tiene su struct de params y de result — añadir un
//! campo opcional es compatible; quitar o renombrar exige bump de
//! [`PROTOCOL_VERSION`].
//!
//! Flujo típico (request → task → progreso):
//!
//! ```
//! use norte_proto::methods::{FS_COPY, FsCopyParams, FsTaskResult};
//! use norte_proto::VPath;
//!
//! let params = FsCopyParams {
//!     from: VPath::parse("file:///src/a.txt").unwrap(),
//!     to: VPath::parse("file:///dst/a.txt").unwrap(),
//!     on_collision: Default::default(),
//!     symlinks: Default::default(),
//!     resume: Default::default(),
//!     verify: Default::default(),
//! };
//! let wire = serde_json::to_string(&params).unwrap();
//! let back: FsCopyParams = serde_json::from_str(&wire).unwrap();
//! assert_eq!(back, params);
//! assert_eq!(FS_COPY, "fs.copy");
//! // El result lleva la TaskId; el progreso llega por TASK_PROGRESS.
//! let result: FsTaskResult = serde_json::from_str(r#"{"task_id": 7}"#).unwrap();
//! assert_eq!(result.task_id.get(), 7);
//! ```

use serde::{Deserialize, Serialize};

use crate::{
    CollisionPolicy, DeleteMode, Entry, ResumePolicy, SymlinkPolicy, TaskId, VPath, VerifyPolicy,
};

/// Versión del protocolo (semver). El core soporta N y N-1 (spec §11).
///
/// 0.9.0 (fase 8, ADR 0018): capability `READ_ONLY` + schemes compuestos
/// `zip+`/`tar+` con marcador `!` (archivos como directorios). Aditivo
/// sobre 0.8.x; el bump señala que el server entiende paths compuestos.
///
/// 0.10.0 (M3-2): `TaskKind::Undo` + `TaskKind::Unknown` (forward-compat, como
/// `TaskState::Unknown`). Aditivo sobre 0.9.x; el bump señala que el server sabe
/// emitir Tasks de undo de sesión.
///
/// 0.11.0 (M3-3b): policy engine por el protocolo — `InitializeParams.
/// agent_session` (liga la conexión a una sesión de agente) + métodos
/// `policy.request_scope`/`grant_scope`/`decide`/`pending` + notificación
/// `policy.approval_required`. Aditivo sobre 0.10.x.
///
/// 0.12.0 (M3-4): `policy.undo_session` — un humano deshace la sesión completa de
/// un agente (LIFO estricto, corre como Task). Aditivo sobre 0.11.x.
///
/// 0.13.0 (M4-P3): familia `plugin.*` — `plugin.list` (enumera plugins
/// descubiertos + errores de carga, solo lectura), `plugin.set_approval` y
/// `plugin.set_enabled` (un humano aprueba capabilities / activa un plugin;
/// solo conexiones User). Aditivo sobre 0.12.x.
///
/// 0.14.0 (M4-P4): `plugin.run_command` — ejecuta un comando de un plugin
/// APROBADO y ACTIVADO (el plugin corre sandboxeado; devuelve el string del
/// comando o error). Aditivo sobre 0.13.x.
///
/// 0.15.0 (M4-P5): `plugin.preview` — ejecuta el primer plugin previewer
/// APROBADO y ACTIVADO que maneje el mimetype del archivo sobre los bytes que
/// el core lee; todo `None` = ningún previewer aplica. Aditivo sobre 0.14.x.
pub const PROTOCOL_VERSION: &str = "0.15.0";

/// `initialize` — handshake OBLIGATORIO antes de cualquier otro método
/// (ADR 0011). Rechaza versiones incompatibles (ver
/// [`version_compatible`]) y negocia el encoding (hoy solo `"json"`).
pub const INITIALIZE: &str = "initialize";
/// `daemon.shutdown` — apaga el daemon: `graceful` (default) espera a las
/// tasks vivas; sin graceful las cancela primero. Autenticado como todo.
/// Solo una conexión humana (sin `agent_session`) puede apagar: para una
/// conexión de agente es `INVALID_REQUEST`, como los demás actos de
/// gobierno humano (p. ej. `policy.grant_scope`/`decide`/`undo_session`).
pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";
/// `task.list` — resync de un frontend que (re)conecta (0.5.0, fase 3):
/// las tasks VIVAS más los desenlaces recientes que el server retiene
/// (anillo acotado, mejor esfuerzo); los cambios posteriores llegan por
/// [`TASK_PROGRESS`]. El receptor DEBE deduplicar por `task_id` (una
/// misma task puede venir viva y su terminal en la misma respuesta si
/// caen en la ventana del anillo).
///
/// Visibilidad por actor: una conexión humana ve TODAS las tasks; una
/// conexión de agente (`agent_session` en `initialize`) SOLO las de su
/// propia sesión — `current` lleva paths de otros actores y no se cruza.
/// El mismo criterio enruta la notificación [`TASK_PROGRESS`].
pub const TASK_LIST: &str = "task.list";
/// `fs.read` — UN tramo de un archivo, en base64 (0.5.0). Para lectura de
/// presentación (viewer); las copias JAMÁS pasan por aquí (son tasks del
/// daemon). El tramo devuelto puede ser más corto que el pedido: `eof`
/// dice si el archivo terminó — si es `false`, el caller repite con el
/// offset avanzado.
pub const FS_READ: &str = "fs.read";
/// `fs.capabilities` — capabilities del provider que sirve un path
/// (0.5.0): el frontend decide p. ej. si F8 ofrece papelera (ADR 0009).
pub const FS_CAPABILITIES: &str = "fs.capabilities";

/// Tope de bytes devueltos por UNA llamada a [`FS_READ`] (antes de
/// base64). Pedir más no es error: se recorta y `eof` lo cuenta.
pub const FS_READ_MAX_CHUNK: u64 = 8 * 1024 * 1024;

/// Tope de entradas devueltas por UNA página de [`FS_LIST`] (0.8.0, ADR
/// 0017). Pedir un `limit` mayor no es error: se recorta a este techo (mismo
/// patrón que [`FS_READ_MAX_CHUNK`]), y el resto sigue por el `next_cursor`.
pub const FS_LIST_MAX_PAGE: u32 = 10_000;

/// ¿Acepta un core `server` a un cliente `client`? N y N-1 (spec §11):
/// mismo major; en 0.x el "major efectivo" es el minor — se acepta el
/// mismo minor o el inmediatamente anterior. El patch jamás importa.
///
/// ```
/// use norte_proto::methods::version_compatible;
/// assert!(version_compatible("0.4.0", "0.4.9"));
/// assert!(version_compatible("0.4.0", "0.3.0"));
/// assert!(!version_compatible("0.4.0", "0.2.0"));
/// assert!(!version_compatible("0.4.0", "0.5.0")); // cliente del futuro
/// assert!(!version_compatible("0.4.0", "no-semver"));
/// ```
#[must_use]
pub fn version_compatible(server: &str, client: &str) -> bool {
    fn digits(seg: &str) -> Option<u64> {
        // Estricto: solo dígitos, sin `+`/espacios (que u64::parse tolera)
        // ni ceros a la izquierda (semver los prohíbe). Pre-release y
        // build metadata también quedan fuera — deliberado y pinneado en
        // tests: un daemon de desarrollo con tag raro NO negocia.
        if seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if seg.len() > 1 && seg.starts_with('0') {
            return None;
        }
        seg.parse().ok()
    }
    fn parse(v: &str) -> Option<(u64, u64)> {
        let mut it = v.split('.');
        let major = digits(it.next()?)?;
        let minor = digits(it.next()?)?;
        // El patch debe existir y ser numérico (semver), pero no se compara.
        let _ = digits(it.next()?)?;
        if it.next().is_some() {
            return None;
        }
        Some((major, minor))
    }
    let (Some((sj, sn)), Some((cj, cn))) = (parse(server), parse(client)) else {
        return false;
    };
    if sj != cj {
        return false;
    }
    if sj > 0 {
        // Estabilidad real: mismo major basta; N/N-1 aplica al minor del
        // servidor frente a clientes más nuevos.
        return cn <= sn;
    }
    // 0.x: el minor es el "major efectivo" — N o N-1.
    cn == sn || cn + 1 == sn
}

/// `fs.list` — listar un directorio.
pub const FS_LIST: &str = "fs.list";
/// `fs.stat` — metadatos de un nodo.
pub const FS_STAT: &str = "fs.stat";
/// `fs.copy` — copia (recursiva si es dir) como Task.
pub const FS_COPY: &str = "fs.copy";
/// `fs.move` — movimiento como Task (rename atómico si el provider puede).
pub const FS_MOVE: &str = "fs.move";
/// `fs.delete` — borrado como Task: papelera (default) o permanente
/// (recursivo post-order) — ADR 0009.
pub const FS_DELETE: &str = "fs.delete";
/// `task.cancel` — petición de cancelación cooperativa. La respuesta solo
/// confirma la recepción; el estado final (`cancelled`, o `completed` si la
/// Task ganó la carrera) llega por [`TASK_PROGRESS`].
///
/// Alcance por actor: una conexión de agente solo cancela tasks de su
/// propia sesión; sobre una task ajena el ack es idéntico al de una task
/// desconocida (no se filtra existencia) y la task sigue. Una conexión
/// humana cancela cualquiera.
pub const TASK_CANCEL: &str = "task.cancel";
/// `connection.trust_host_key` — registra una host key SSH en el `known_hosts`
/// tras confirmación del usuario (flujo TOFU, ADR 0015 D; 0.7.0, fase 6). Se
/// llama tras un [`Error::HostKeyUnknown`](crate::Error::HostKeyUnknown) y
/// antes de reintentar la conexión. Idempotente. Decisión de confianza
/// HUMANA: para una conexión de agente es `INVALID_REQUEST`, como p. ej.
/// `policy.grant_scope`/`decide`/`undo_session`.
pub const CONNECTION_TRUST_HOST_KEY: &str = "connection.trust_host_key";
/// `task.progress` — notificación server→client, coalescida (≤30 Hz).
pub const TASK_PROGRESS: &str = "task.progress";
/// `policy.request_scope` — un agente pide un scope (rutas+ops+TTL, M3-3b).
pub const POLICY_REQUEST_SCOPE: &str = "policy.request_scope";
/// `policy.grant_scope` — un humano concede un scope pendiente.
pub const POLICY_GRANT_SCOPE: &str = "policy.grant_scope";
/// `policy.decide` — un humano aprueba/deniega una aprobación pendiente.
pub const POLICY_DECIDE: &str = "policy.decide";
/// `policy.pending` — lista de aprobaciones pendientes (resync).
pub const POLICY_PENDING: &str = "policy.pending";
/// `policy.approval_required` — notificación server→client: una op `ask`
/// espera decisión (M3-3b).
pub const POLICY_APPROVAL_REQUIRED: &str = "policy.approval_required";
/// `policy.undo_session` — deshace la sesión de un AGENTE completa (M3-4):
/// un humano revierte en LIFO estricto todo lo que hizo `session`. Solo
/// conexiones User (una sesión de agente no deshace a otras ni a sí misma
/// por esta vía). Vive en `policy.*` (la familia de gobernanza de agentes:
/// scopes, aprobaciones, undo) — `session.*` queda reservado para la sesión
/// de UI (spec §11).
pub const POLICY_UNDO_SESSION: &str = "policy.undo_session";
/// `plugin.list` — enumera los plugins DESCUBIERTOS más los errores de carga
/// (M4-P3). Solo lectura y ABIERTO (cualquier conexión lo consulta): un
/// frontend pinta el catálogo y el estado (aprobado/activo) sin mutar nada.
pub const PLUGIN_LIST: &str = "plugin.list";
/// `plugin.set_approval` — un HUMANO aprueba (o revoca) las capabilities de un
/// plugin (M4-P3). SOLO conexiones User: una sesión de agente jamás se
/// autoconcede capabilities de plugin.
pub const PLUGIN_SET_APPROVAL: &str = "plugin.set_approval";
/// `plugin.set_enabled` — un HUMANO activa o desactiva un plugin (M4-P3). SOLO
/// conexiones User (misma barrera que [`PLUGIN_SET_APPROVAL`]).
pub const PLUGIN_SET_ENABLED: &str = "plugin.set_enabled";
/// `plugin.run_command` — ejecuta un comando de un plugin `command` APROBADO y
/// ACTIVADO (M4-P4); el plugin corre sandboxeado; devuelve el string del
/// comando o error.
pub const PLUGIN_RUN_COMMAND: &str = "plugin.run_command";
/// `plugin.preview` — ejecuta el primer plugin previewer APROBADO y ACTIVADO
/// que maneje el mimetype del archivo (M4-P5) sobre los bytes que el core lee;
/// todo `None` = ningún previewer aplica (el frontend cae a la vista cruda).
pub const PLUGIN_PREVIEW: &str = "plugin.preview";

/// Params de [`FS_LIST`] (paginación por cursor desde 0.8.0, ADR 0017).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsListParams {
    /// Directorio a listar. OBLIGATORIO también al continuar (valida que el
    /// `cursor` corresponde a ESTE listado).
    pub path: VPath,
    /// Máximo de entradas de ESTA página. `None` (o ausente) = sin límite
    /// (drena el resto). Recortado a [`FS_LIST_MAX_PAGE`]; `Some(0)` es error
    /// (`-32602`, evita páginas vacías en bucle). Un cliente 0.7 no lo envía.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor OPACO de la página siguiente (el `next_cursor` de la respuesta
    /// previa). `None` = abre un listado nuevo. Jamás se parsea; desconocido
    /// o expirado → [`Error::CursorExpired`](crate::Error::CursorExpired).
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Result de [`FS_LIST`].
///
/// Cláusula de compatibilidad (ADR 0004/0017): sin `cursor` NI `limit`, el
/// core DEVUELVE el listado COMPLETO con `next_cursor: null` — un cliente
/// 0.7 (N-1) recibe exactamente lo de antes, jamás un truncado en silencio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsListResult {
    /// Entradas de ESTA página (orden: el del provider, sin garantía).
    pub entries: Vec<Entry>,
    /// Cursor de la página siguiente, o `None` si el listado terminó. Un
    /// cliente 0.7 lo ignora (campo desconocido para él).
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// Params de [`FS_STAT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsStatParams {
    /// Nodo a consultar.
    pub path: VPath,
}

/// Result de [`FS_STAT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsStatResult {
    /// Metadatos del nodo.
    pub entry: Entry,
}

/// Params de [`FS_COPY`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCopyParams {
    /// Origen (archivo o directorio).
    pub from: VPath,
    /// Destino EXACTO (con `RenameAuto` el core deriva el nombre libre;
    /// con el resto de políticas jamás inventa nombres).
    pub to: VPath,
    /// Qué hacer si el destino existe. `#[serde(default)]`: un cliente N-1
    /// que no lo envía obtiene `Fail` (el comportamiento de siempre).
    #[serde(default)]
    pub on_collision: CollisionPolicy,
    /// Qué hacer con los symlinks del origen (default `Preserve`).
    #[serde(default)]
    pub symlinks: SymlinkPolicy,
    /// Reanudación (ADR 0012); default `Off` = contrato de M1.
    #[serde(default)]
    pub resume: ResumePolicy,
    /// Verificación del parcial al reanudar; default `Length`.
    #[serde(default)]
    pub verify: VerifyPolicy,
}

/// Params de [`FS_MOVE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsMoveParams {
    /// Origen.
    pub from: VPath,
    /// Destino exacto (ver [`FsCopyParams::to`]).
    pub to: VPath,
    /// Qué hacer si el destino existe (ver [`FsCopyParams::on_collision`]).
    #[serde(default)]
    pub on_collision: CollisionPolicy,
    /// Qué hacer con los symlinks (solo aplica al camino copy+delete; el
    /// rename same-provider mueve el link tal cual).
    #[serde(default)]
    pub symlinks: SymlinkPolicy,
    /// Reanudación del camino copy+delete (ADR 0012); default `Off`.
    #[serde(default)]
    pub resume: ResumePolicy,
    /// Verificación del parcial al reanudar; default `Length`.
    #[serde(default)]
    pub verify: VerifyPolicy,
}

/// Params de [`FS_DELETE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsDeleteParams {
    /// Nodo a borrar (recursivo si es dir).
    pub path: VPath,
    /// Papelera o permanente. `#[serde(default)]` = Trash: el default del
    /// wire es el SEGURO (ADR 0009).
    #[serde(default)]
    pub mode: DeleteMode,
}

/// Result de [`FS_COPY`], [`FS_MOVE`] y [`FS_DELETE`]: la Task creada.
/// El progreso llega por [`TASK_PROGRESS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsTaskResult {
    /// Id de la Task encolada.
    pub task_id: TaskId,
}

/// Identidad de un cliente (va en [`InitializeParams`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Nombre del frontend (`norte-tui`, `norte-cli`, un tercero…).
    pub name: String,
    /// Versión del frontend (informativa, jamás se compara).
    pub version: String,
}

/// Identidad del servidor (va en [`InitializeResult`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Nombre del servidor (`norte-core`).
    pub name: String,
    /// Versión del binario del daemon (informativa).
    pub version: String,
}

/// Params de [`INITIALIZE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeParams {
    /// Quién se conecta.
    pub client_info: ClientInfo,
    /// Versión del protocolo del cliente; incompatible = error y cierre.
    pub protocol_version: String,
    /// Encodings que el cliente sabe hablar, por preferencia. Vacío o
    /// ausente = `["json"]` implícito (el único de M2, decisión 4 del
    /// kickoff: negociado-pero-solo-JSON).
    #[serde(default)]
    pub encodings: Vec<String>,
    /// Si presente, la conexión actúa como SESIÓN DE AGENTE con este id: sus
    /// mutaciones se evalúan por el policy engine (M3-3). Ausente = frontend
    /// humano (`User`, sin sandbox). El servidor liga el actor a la conexión;
    /// un cliente no puede declararse `User` por otra vía.
    ///
    /// El id se valida server-side fail-closed: 1..=64 chars de
    /// `[A-Za-z0-9._-]`, si no `INVALID_PARAMS` — viaja a journal, logs y
    /// modales de aprobación de los frontends, jamás debe ser un vector de
    /// inyección (controles/bidi) elegido por el agente.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<String>,
}

/// Result de [`INITIALIZE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    /// Quién responde.
    pub server_info: ServerInfo,
    /// Versión del protocolo del core.
    pub protocol_version: String,
    /// Encodings aceptados (hoy siempre `["json"]`).
    pub encodings: Vec<String>,
}

/// Params de [`DAEMON_SHUTDOWN`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonShutdownParams {
    /// `true` (default): terminar las tasks vivas antes de salir.
    /// `false`: cancelarlas primero (estado limpio garantizado igual).
    #[serde(default = "default_graceful")]
    pub graceful: bool,
}

impl Default for DaemonShutdownParams {
    fn default() -> Self {
        Self { graceful: true }
    }
}

fn default_graceful() -> bool {
    true
}

/// Result de [`DAEMON_SHUTDOWN`]: objeto vacío, reservado para extensión.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonShutdownResult {}

/// Params de [`TASK_LIST`]: objeto vacío, reservado para extensión
/// (filtros por estado/kind llegarán aquí como campos opcionales).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListParams {}

/// Result de [`TASK_LIST`].
///
/// ```
/// use norte_proto::methods::TaskListResult;
/// let r: TaskListResult = serde_json::from_str(r#"{"tasks":[]}"#).unwrap();
/// assert!(r.tasks.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListResult {
    /// Snapshots de las tasks vivas + los desenlaces recientes retenidos
    /// por el server (mejor esfuerzo; ver [`TASK_LIST`]). Puede repetir
    /// `task_id` — el receptor deduplica.
    pub tasks: Vec<crate::TaskProgress>,
}

/// Params de [`FS_READ`].
///
/// ```
/// use norte_proto::methods::FsReadParams;
/// use norte_proto::VPath;
/// let p = FsReadParams { path: VPath::parse("file:///x").unwrap(), range: None };
/// // El emisor canónico escribe `range: null` explícito (ADR 0004).
/// assert!(serde_json::to_string(&p).unwrap().contains(r#""range":null"#));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadParams {
    /// Archivo a leer.
    pub path: VPath,
    /// Tramo pedido; ausente/`null` = desde 0, tope del server.
    #[serde(default)]
    pub range: Option<crate::ByteRange>,
}

/// Result de [`FS_READ`].
///
/// ```
/// use norte_proto::methods::FsReadResult;
/// let r: FsReadResult = serde_json::from_str(r#"{"content_b64":"aGk=","eof":true}"#).unwrap();
/// assert!(r.eof);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadResult {
    /// Bytes del tramo, en base64 estándar (los bytes de un archivo no
    /// son texto: JSON no puede llevarlos crudos).
    pub content_b64: String,
    /// `true` si el tramo termina EN el fin del archivo.
    pub eof: bool,
}

/// Params de [`FS_CAPABILITIES`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCapabilitiesParams {
    /// Un path del provider a consultar.
    pub path: VPath,
}

/// Result de [`FS_CAPABILITIES`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCapabilitiesResult {
    /// Capabilities declaradas por el provider.
    pub capabilities: crate::Capabilities,
}

/// Params de [`TASK_CANCEL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCancelParams {
    /// Task a cancelar. Cancelar una Task terminal o inexistente no es error:
    /// la respuesta llega igual y el estado real viaja por [`TASK_PROGRESS`].
    pub task_id: TaskId,
}

/// Result de [`TASK_CANCEL`]: objeto vacío, reservado para extensión.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCancelResult {}

/// Params de [`CONNECTION_TRUST_HOST_KEY`] (flujo TOFU, ADR 0015 D). Lleva el
/// fingerprint que el usuario VERIFICÓ; el core lo compara con la clave que
/// vuelve a presentar el servidor al reintentar, y solo registra si coincide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionTrustHostKeyParams {
    /// Host al que se conecta (`host`; el puerto aparte).
    pub host: String,
    /// Puerto (ausente = default del scheme).
    #[serde(default)]
    pub port: Option<u16>,
    /// Algoritmo de la clave (p. ej. `ssh-ed25519`).
    pub algo: String,
    /// Fingerprint en formato OpenSSH `SHA256:<base64>` que el usuario
    /// confirmó (la misma cadena que trae el `Error::HostKeyUnknown`).
    pub fingerprint: String,
}

/// Result de [`CONNECTION_TRUST_HOST_KEY`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionTrustHostKeyResult {
    /// `true` si la clave quedó registrada (idempotente: `true` también si ya
    /// estaba). `false` reservado para un futuro rechazo por política.
    pub trusted: bool,
}

/// Params de [`POLICY_REQUEST_SCOPE`] (M3-3b): un agente pide un scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestScopeParams {
    /// Sesión de agente que pide (debe coincidir con la de la conexión).
    pub session: String,
    /// Raíces solicitadas (contención por subtree-prefix).
    pub roots: Vec<VPath>,
    /// Op-kinds solicitados (`copy|move|delete|mkdir`).
    pub ops: Vec<String>,
    /// TTL solicitado en milisegundos.
    pub ttl_ms: u64,
}

/// Result de [`POLICY_REQUEST_SCOPE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestScopeResult {
    /// Id de la petición, para que un humano la conceda con `policy.grant_scope`.
    pub request_id: u64,
}

/// Params de [`POLICY_GRANT_SCOPE`] (un humano concede una petición pendiente).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantScopeParams {
    /// Id devuelto por `policy.request_scope`.
    pub request_id: u64,
}

/// Result de [`POLICY_GRANT_SCOPE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantScopeResult {}

/// Notificación [`POLICY_APPROVAL_REQUIRED`] (server→client): una op `ask`
/// espera decisión. Las rutas van REDACTADAS si llevan userinfo (regla 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyApprovalRequired {
    /// Id para responder con `policy.decide`.
    pub approval_id: u64,
    /// Sesión de agente que pidió la op (si aplica).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Op-kind (`copy|move|delete|mkdir`).
    pub op: String,
    /// Rutas implicadas (wire, redactadas). SOLO display: jamás se reparsan a
    /// una operación — la op real va ligada server-side por `approval_id`.
    pub paths: Vec<String>,
    /// TTL de la aprobación en milisegundos. `0` = DESCONOCIDO (p. ej. una
    /// pendiente reconstruida del resync de `policy.pending`, que no
    /// transporta el TTL restante): el frontend no pinta cuenta atrás.
    pub ttl_ms: u64,
}

/// Params de [`POLICY_DECIDE`] (un humano aprueba/deniega).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecideParams {
    /// Id de la aprobación pendiente.
    pub approval_id: u64,
    /// `true` = aprobar, `false` = denegar.
    pub approve: bool,
}

/// Result de [`POLICY_DECIDE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecideResult {}

/// Una aprobación pendiente (elemento de [`PolicyPendingResult`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    /// Id para responder con `policy.decide`.
    pub approval_id: u64,
    /// Sesión de agente que pidió la op (si aplica).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Op-kind.
    pub op: String,
    /// Rutas implicadas (wire, redactadas). SOLO display: jamás se reparsan a
    /// una operación — la op real va ligada server-side por `approval_id`.
    pub paths: Vec<String>,
}

/// Result de [`POLICY_PENDING`] (resync de aprobaciones pendientes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyPendingResult {
    /// Aprobaciones pendientes.
    pub pending: Vec<PendingApproval>,
}

/// Params de [`POLICY_UNDO_SESSION`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUndoSessionParams {
    /// Sesión de agente cuyas mutaciones se deshacen (mismo formato que
    /// `agent_session` del initialize: `[A-Za-z0-9._-]`, 1..=64).
    pub session: String,
}

/// Result de [`POLICY_UNDO_SESSION`]: el undo corre como Task (progreso por
/// `task.progress`, cancelable con `task.cancel`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUndoSessionResult {
    /// Task del undo.
    pub task_id: TaskId,
}

/// Un plugin descubierto (elemento de [`PluginListResult::plugins`], M4-P3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// Id estable del plugin (namespace inverso, p. ej. `org.norte.demo`).
    pub id: String,
    /// Nombre legible para mostrar.
    pub name: String,
    /// Publicador declarado en el manifiesto.
    pub publisher: String,
    /// Versión del plugin (informativa).
    pub version: String,
    /// Categoría (`previewer`, `indexer`…): qué papel juega en el core.
    pub category: String,
    /// Capabilities que el plugin solicita (p. ej. `fs-read`). Un humano las
    /// aprueba con [`PLUGIN_SET_APPROVAL`] antes de que surtan efecto.
    pub capabilities: Vec<String>,
    /// `true` si un humano ya aprobó sus capabilities.
    pub approved: bool,
    /// `true` si un humano lo tiene activado.
    pub enabled: bool,
}

/// Un directorio de plugin que NO se pudo cargar (elemento de
/// [`PluginListResult::errors`], M4-P3): se reporta para diagnóstico, sin
/// tumbar el resto del catálogo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginLoadError {
    /// Directorio del plugin que falló (display; puede llevar bytes lossy).
    pub dir: String,
    /// Motivo legible del fallo (manifiesto inválido, versión no soportada…).
    pub reason: String,
}

/// Params de [`PLUGIN_LIST`]: objeto vacío, reservado para extensión
/// (filtros por categoría/estado llegarán aquí como campos opcionales).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginListParams {}

/// Result de [`PLUGIN_LIST`]: el catálogo descubierto y los fallos de carga.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginListResult {
    /// Plugins descubiertos y cargados (con su estado aprobado/activo).
    pub plugins: Vec<PluginInfo>,
    /// Directorios que fallaron al cargar (mejor esfuerzo; ver
    /// [`PluginLoadError`]).
    pub errors: Vec<PluginLoadError>,
}

/// Params de [`PLUGIN_SET_APPROVAL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetApprovalParams {
    /// Id del plugin a (des)aprobar.
    pub id: String,
    /// `true` = aprobar las capabilities, `false` = revocar.
    pub approved: bool,
}

/// Result de [`PLUGIN_SET_APPROVAL`]: objeto vacío, reservado para extensión.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetApprovalResult {}

/// Params de [`PLUGIN_SET_ENABLED`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetEnabledParams {
    /// Id del plugin a activar/desactivar.
    pub id: String,
    /// `true` = activar, `false` = desactivar.
    pub enabled: bool,
}

/// Result de [`PLUGIN_SET_ENABLED`]: objeto vacío, reservado para extensión.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetEnabledResult {}

/// Params de [`PLUGIN_RUN_COMMAND`] (M4-P4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRunCommandParams {
    /// Id del plugin que expone el comando.
    pub id: String,
    /// Nombre del comando a ejecutar (declarado por el plugin).
    pub command: String,
    /// Argumento del comando. Ausente = `""` (el default del wire): un cliente
    /// que no lo envía ejecuta el comando sin argumento.
    #[serde(default)]
    pub arg: String,
}

/// Result de [`PLUGIN_RUN_COMMAND`]: la salida del comando del plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRunCommandResult {
    /// Salida (string) que devuelve el comando del plugin.
    pub output: String,
}

/// Params de [`PLUGIN_PREVIEW`] (M4-P5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewParams {
    /// Ruta del archivo a previsualizar (el core lee sus bytes).
    pub path: VPath,
}

/// La preview producida por un plugin previewer: los tres campos van JUNTOS
/// (all-or-nothing). Ver [`PluginPreviewResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreview {
    /// Id del plugin previewer que produjo la salida.
    pub plugin_id: String,
    /// Nombre legible del plugin previewer (para el indicador «via …»).
    pub plugin_name: String,
    /// Salida (texto) de la preview.
    pub output: String,
}

/// Result de [`PLUGIN_PREVIEW`] (M4-P5): la preview del primer previewer que
/// aplica, o NADA. El `flatten` sobre un `Option` hace que el wire sea
/// `{plugin_id,plugin_name,output}` (aplicó) o `{}` (ninguno); el TIPO Rust hace
/// INCONSTRUIBLE un estado parcial (los tres campos van juntos en
/// [`PluginPreview`]), y un objeto parcial del wire colapsa a `None` (sin
/// preview, seguro) — jamás un `plugin_id` sin `output` (protocol-guardian
/// M4-P5). `None` = ningún previewer maneja el mimetype; el frontend cae a la
/// vista cruda.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewResult {
    /// La preview, o `None` si ningún previewer aplicó.
    #[serde(flatten)]
    pub preview: Option<PluginPreview>,
}
