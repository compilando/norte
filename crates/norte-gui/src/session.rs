//! Backend de SESIÓN persistente: UN `RemoteBackend` vivo toda la sesión, en
//! un hilo con runtime tokio propio. Reemplaza el reconnect-per-cd del spike
//! (GUI-a deuda T4): la conexión se establece una vez y todos los listados
//! (y en tasks posteriores las mutaciones) van por ella — condición NECESARIA
//! para seguir el progreso de una task (`task.progress` llega por la BOMBA del
//! `RemoteBackend`, que exige conexión viva).
//!
//! # Puente GPUI ↔ tokio
//! GPUI (executor propio) manda [`SessionCmd`] por un `mpsc` sin bloquear; el
//! hilo tokio ejecuta y devuelve [`SessionEvent`] por otro `mpsc` que la GUI
//! drena en UN `cx.spawn` (los `mpsc` de tokio son awaitables en cualquier
//! executor: solo registran un waker). Un daemon caído en `connect` emite
//! [`SessionEvent::ConnectFailed`], jamás panic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use norte_core::backend::{Backend, TaskCanceller, TaskRef, remote::RemoteBackend};
use norte_proto::methods::{ClientInfo, PluginInfo, PluginLoadError};
use norte_proto::{Entry, Error, TaskId, TaskProgress, TaskState, VPath};
use tokio::sync::mpsc;

use crate::modal::{PendingOp, TransferKind};

/// Config resuelta del entorno (socket + dir inicial).
pub struct LoadConfig {
    /// Socket UDS del daemon.
    pub socket: PathBuf,
    /// Directorio inicial de ambos panes.
    pub dir: VPath,
}

impl LoadConfig {
    /// Resuelve el arranque de la sesión con precedencia LÍNEA DE COMANDOS
    /// → entorno → default: `dir`/`--socket` ganan a `NORTE_DIR`/
    /// `NORTE_SOCKET`, que ganan al `cwd` y al socket propio del daemon.
    /// Punto ÚNICO de esta resolución: el `main` no se cuela por
    /// `std::env::set_var` (que además es `unsafe`, prohibido en el crate).
    ///
    /// # Errors
    /// `NORTE_DIR` no parsea como `VPath`, o el `cwd` no se puede leer.
    pub fn resolve(dir: Option<VPath>, socket: Option<PathBuf>) -> anyhow::Result<Self> {
        let socket = match socket {
            Some(s) => s,
            None => match std::env::var_os("NORTE_SOCKET") {
                Some(s) => PathBuf::from(s),
                None => norte_core::daemon::default_socket_path(None),
            },
        };
        if let Some(dir) = dir {
            return Ok(Self { socket, dir });
        }
        let dir = match std::env::var("NORTE_DIR") {
            Ok(wire) => VPath::parse(&wire)
                .map_err(|e| anyhow::anyhow!("NORTE_DIR no es un VPath válido: {e}"))?,
            Err(_) => {
                let cwd = std::env::current_dir()?;
                norte_vfs_local::vpath_from_native(&cwd)
                    .map_err(|e| anyhow::anyhow!("cwd → VPath: {e}"))?
            }
        };
        Ok(Self { socket, dir })
    }
}

/// Comando de la GUI hacia el hilo de sesión.
pub enum SessionCmd {
    /// Lista `dir` en `pane`; `generation` es el guard anti-stale de GUI-a.
    List {
        /// Pane destino (0|1).
        pane: usize,
        /// Generación del cd (ver `main::generation_is_current`).
        generation: u64,
        /// Directorio a listar.
        dir: VPath,
        /// Ids attr CONFIGURADOS del scheme (#117): los valores solo llegan
        /// pidiéndolos en `fs.list` — sin ellos las celdas attr pintan
        /// blanco (ausencia).
        attrs: Vec<String>,
        /// Pide el [`norte_proto::AttrCatalog`] del scheme ANTES de listar
        /// (#117): una vez por scheme y sesión (la GUI lo decide mirando su
        /// caché); un fallo del catálogo JAMÁS tumba el listado.
        fetch_catalog: bool,
    },
    /// Los dirs que el watcher debe vigilar ahora (#106), uno por pane;
    /// `None` = ese pane no es vigilable (remoto, archivo, virtual).
    ///
    /// La vigilancia vive en ESTE hilo y no en la vista porque necesita un
    /// runtime tokio, y el runtime es de aquí: la vista corre en el hilo de
    /// GPUI, donde un `tokio::spawn` no tiene a quién pedírselo.
    Watch([Option<PathBuf>; 2]),
    /// Lanza una operación mutante (copy/move/delete) como task.
    Submit(PendingOp),
    /// Cancela una task en curso (`task.cancel`, fire-and-forget).
    Cancel(TaskId),
    /// Abre el visor de `path`: intenta un previewer de plugin, si no lee bytes.
    OpenViewer {
        /// Archivo a abrir en el visor.
        path: VPath,
        /// Generación del open (guard anti-stale, como `List`): un
        /// `ViewerOpened`/`ViewerFailed` con una generación vieja se
        /// descarta en `apply_event` (un F3 tardío no reabre por sorpresa
        /// encima de lo que el usuario esté haciendo).
        generation: u64,
    },
    /// Decora `paths` (G3b, ADR 0037): pide `plugin.decorate` para la
    /// página VISIBLE que acaba de listar `pane` — mismo guard anti-stale
    /// (`generation`/`dir`) que `List`/`OpenViewer`, así el `apply_event` de
    /// la GUI descarta una respuesta tardía de un cd ya superado.
    Decorate {
        /// Pane destino (0|1).
        pane: usize,
        /// Generación del cd que lo pidió.
        generation: u64,
        /// Directorio listado (para el guard anti-stale al aplicar).
        dir: VPath,
        /// Rutas visibles a decorar, en el orden del listado.
        paths: Vec<VPath>,
    },
    /// Hidrata `size`/`mtime` de `paths` (#52/#123): el listado local llega
    /// LAZY (`size: None` — `norte-vfs-local` no statea por entrada) y sin
    /// esto las columnas Tamaño/Fecha se pintan en blanco para siempre. Se
    /// pide solo para las filas VISIBLES (`uniform_list` da el rango exacto)
    /// y con el mismo guard anti-stale (`generation`/`dir`) que `Decorate`.
    StatBatch {
        /// Pane destino (0|1).
        pane: usize,
        /// Generación del cd que lo pidió.
        generation: u64,
        /// Directorio listado (para el guard anti-stale al aplicar).
        dir: VPath,
        /// Rutas visibles sin `size`, en el orden del listado.
        paths: Vec<VPath>,
    },
    /// Valores de columna (G3c; #117-follow-up: CONFIG-driven): pide
    /// `plugin.column_values` para cada par (plugin, columna) CONFIGURADO
    /// en `[ui.columns]` (`requested`), validando pertenencia contra el
    /// catálogo vivo (`plugin.list`: aprobado + habilitado + columna
    /// declarada por ESE plugin). La GUI recibe UN solo evento con los
    /// valores YA saneados, clave = id Display (`plugin:<p>/<c>`) — las
    /// cabeceras las pinta el funnel (`header_label`), ya no viajan aquí.
    /// Mismo guard anti-stale que `Decorate`.
    Columns {
        /// Pane destino (0|1).
        pane: usize,
        /// Generación del cd que lo pidió.
        generation: u64,
        /// Directorio listado (guard anti-stale al aplicar).
        dir: VPath,
        /// Rutas visibles a valorar, en el orden del listado.
        paths: Vec<VPath>,
        /// Pares (plugin, columna) configurados para el scheme del pane
        /// (`ColumnsSettings::plugin_ids_for` — cap ya aplicado).
        requested: Vec<(String, String)>,
    },
    /// Catálogo de plugins (G3c): alimenta la paleta (filas de comando de
    /// plugin) y el gestor de extensiones.
    PluginsList,
    /// Esquema `[config]` + valores efectivos de UN plugin (G3c,
    /// `plugin.get_config`) — usado por el drill-down del gestor de
    /// extensiones.
    PluginGetConfig {
        /// Id del plugin.
        id: String,
    },
    /// El `help.md` de UN plugin, BAJO DEMANDA (H3f, `plugin.help`, 0.34.0):
    /// 64 KiB por plugin no pueden viajar en cada `plugin.list`, así que la
    /// página se pide cuando el lector abre su nodo en la ayuda.
    PluginHelp {
        /// Id del plugin cuya página acaba de abrirse.
        id: String,
    },
    /// Resúmenes de `[config]` de TODOS los plugins aprobados+activados
    /// (G3c) — alimenta la sección Plugins del overlay de ajustes
    /// (`plugins_list` + un `plugin.get_config` por plugin, resuelto aquí
    /// para no encadenar N idas-y-vueltas por el puente GPUI↔tokio).
    PluginConfigSummaries,
    /// Persiste UN valor de `[config]` (G3c, `plugin.set_config`).
    PluginSetConfig {
        /// Id del plugin.
        id: String,
        /// Clave `[config.<key>]`.
        key: String,
        /// Valor nuevo, codificado canónicamente.
        value: String,
    },
    /// Aprueba/revoca un plugin (G3c GUI wiring — `Backend::plugins_set_approval`
    /// ya existía, la GUI simplemente nunca lo llamaba).
    PluginSetApproval {
        /// Id del plugin.
        id: String,
        /// Nuevo estado.
        approved: bool,
    },
    /// Activa/desactiva un plugin (G3c GUI wiring).
    PluginSetEnabled {
        /// Id del plugin.
        id: String,
        /// Nuevo estado.
        enabled: bool,
    },
    /// Ejecuta un comando de plugin (G3c, la paleta lo dispara).
    PluginRunCommand {
        /// Id del plugin.
        id: String,
        /// Comando declarado por el plugin.
        command: String,
        /// Argumento (vacío si ninguno).
        arg: String,
    },
    /// Pide un plan de rename IA (`ai.rename_plan`, M4-IA): el daemon manda
    /// los basenames de `dir` al proveedor (tras su gate de IA) y devuelve
    /// parejas from→to REVISABLES — este método jamás muta; aplicar es
    /// N `Submit` normales tras la confirmación del humano.
    AiRenamePlan {
        /// Directorio objetivo del plan.
        dir: VPath,
        /// Instrucción del usuario (ya recortada y no vacía).
        instruction: String,
    },
    /// Búsqueda semántica sobre el índice (`index.search_semantic`,
    /// M4-IA-2): el daemon embebe la query con el proveedor de IA (tras su
    /// gate) y devuelve hasta [`norte_frontend::SEMANTIC_K`] hits
    /// path+score sobre TODOS los roots (`root = None`, paridad TUI — el
    /// `k` es compartido para que la MISMA consulta devuelva lo mismo en
    /// ambos frontends). No muta nada.
    SemanticSearch {
        /// Consulta del usuario (ya recortada y no vacía).
        query: String,
    },
    /// Pide el listado de volúmenes del host (`host.volumes`, 2026-08-10-
    /// volumes.md task V4, design §D): `pane.select-drive`/`-left`/`-right`
    /// y el toggle "mostrar todo" DENTRO del picker piden esto por igual —
    /// una snapshot fresca para el `include_pseudo` pedido. No muta nada;
    /// gateado a `User` en el daemon (design §C), transparente para esta GUI
    /// porque solo habla como humano.
    Volumes {
        /// El pane que la respuesta debe navegar — el foco para
        /// `pane.select-drive`, un LADO fijo para `-left`/`-right`. Viaja de
        /// ida y vuelta (la respuesta lo repite) porque el foco pudo moverse
        /// mientras la petición estaba en vuelo.
        pane: usize,
        /// El modo pedido — el filtro por defecto o "mostrar todo".
        include_pseudo: bool,
    },
    /// Compara dos directorios (`fs.compare`, 0.39.0, ADR 0048 — #158, spec 3
    /// fase C1): arranca la Task y BOMBEA sus lotes de filas hasta que el
    /// canal se cierre ([`SessionEvent::CompareRows`]), y entonces manda UN
    /// [`SessionEvent::CompareDone`] con el snapshot de progreso. No muta
    /// nada; la Task queda registrada en el mapa de cancellers, así que
    /// [`SessionCmd::Cancel`] la cancela como a cualquier otra.
    Compare {
        /// El pane que la lanzó, o sea el lado IZQUIERDO del panel (que no
        /// tiene por qué ser `panes[0]`). Viaja de ida y vuelta, como
        /// [`SessionCmd::Volumes::pane`], porque el foco pudo moverse
        /// mientras la petición estaba en vuelo y el lado izquierdo se
        /// congela al abrir el panel.
        left_pane: usize,
        /// Generación de ESTA petición (guard anti-stale, como `List`): dos
        /// comparaciones seguidas son dos RPC concurrentes, y la respuesta
        /// de la primera puede llegar después de la segunda. La GUI descarta
        /// —y cancela— cualquier arranque que no sea el de la generación
        /// vigente.
        generation: u64,
        /// Params ya resueltos y validados por la GUI (`NorteGui::start_compare`).
        params: Box<norte_proto::methods::FsCompareParams>,
    },
    /// Planifica una sincronización (`sync.plan`, 0.40.0, ADR 0049 — #161,
    /// spec 3 fase C2): arranca la Task y BOMBEA sus eventos
    /// ([`SessionEvent::SyncSteps`] y, si el plan llega a cerrarse, UN
    /// [`SessionEvent::SyncPlanDone`]); cuando el canal se cierra manda UN
    /// [`SessionEvent::SyncPlanEnded`] con el estado de la Task.
    ///
    /// **Planificar no muta nada**: lee los dos árboles y retiene el plan en
    /// el spool del daemon. Lo que muta es `sync.apply`, que es otro comando
    /// y otra tarea de este plan (la 4). La Task queda registrada en el mapa
    /// de cancellers, así que [`SessionCmd::Cancel`] la para como a cualquier
    /// otra (regla 3).
    SyncPlan {
        /// El pane que lo pidió, o sea el lado ORIGEN. Viaja de ida y vuelta
        /// como el `left_pane` de [`SessionCmd::Compare`], y por lo mismo: el
        /// «planificando…» se retira donde se puso, aunque el foco se haya
        /// movido mientras el RPC iba y venía.
        source_pane: usize,
        /// Generación de ESTA petición (guard anti-stale, como `Compare`):
        /// dos planes seguidos son dos RPC concurrentes y pueden contestar en
        /// orden inverso.
        generation: u64,
        /// Params ya resueltos y validados por la GUI (`NorteGui::start_sync`).
        params: Box<norte_proto::methods::SyncPlanParams>,
    },
    /// **Aplica el plan que un humano APROBÓ** (`sync.apply`, 0.40.0, ADR
    /// 0049 — #161, spec 3 fase C2 tarea 4): la mitad DESTRUCTIVA de esta
    /// pantalla — reescribe y, en `Mirror`, borra subárboles del destino.
    ///
    /// Arranca la Task, la registra en el mapa de cancellers (regla 3: es lo
    /// que hace que el `Esc` del panel y su cierre la puedan parar), espera su
    /// desenlace y pide `sync.report` — SIEMPRE, cancelación incluida: lo
    /// aplicado hasta el corte se queda journalizado y media sincronización es
    /// un estado real que el lector tiene que poder ver. Un `DeleteTree`
    /// cortado a medio borrar deja su fila por lo que llegó a quitar (#186) —
    /// fila que es `Irreversible`, o sea que el undo lo nombra y no lo
    /// devuelve. Lo que el INFORME tampoco cuenta lo detalla
    /// `sync_view::close`.
    SyncApply {
        /// La generación del panel que aprobó, ecoada en
        /// [`SessionEvent::SyncApplyStarted`] y en
        /// [`SessionEvent::SyncApplyFailed`]: si el lector pidió OTRO plan
        /// mientras tanto, esa Task no tiene panel que la mire y hay que
        /// cancelarla en vez de dejarla borrando a ciegas.
        generation: u64,
        /// El hash del plan aprobado. Es lo ÚNICO que viaja (ADR 0049): no hay
        /// forma de pedir que se ejecute algo distinto de lo que el panel
        /// enseñó.
        plan_hash: Box<norte_proto::methods::PlanHash>,
    },
}

/// Contenido del viewer que cruza a la GUI (GUI-d T3).
pub enum ViewerContent {
    /// Salida de un previewer de plugin (texto + nombre).
    Plugin {
        /// Nombre legible del plugin previewer (para el indicador «via …»).
        plugin_name: String,
        /// Salida del plugin (texto), sin sanear todavía — la sanea el
        /// `Viewer` core al construirse (`with_plugin_preview`).
        output: String,
        /// La decodificación host-side fue LOSSY (#101).
        lossy: bool,
    },
    /// Salida CON ESTILO de un previewer de plugin (G3a, ADR 0037): el
    /// gemelo estructurado de [`Self::Plugin`] — líneas de spans (`role`
    /// SIN VALIDAR aún, se valida al construir el `Viewer` core,
    /// `with_plugin_preview_styled`) en vez de una cadena plana.
    PluginStyled {
        /// Nombre legible del plugin previewer (para el indicador «via …»).
        plugin_name: String,
        /// Líneas de spans, sin sanear/validar todavía — lo hace el
        /// `Viewer` core al construirse.
        lines: Vec<Vec<norte_proto::methods::SpanWire>>,
        /// La decodificación host-side fue LOSSY (#101).
        lossy: bool,
    },
    /// Bytes crudos (posiblemente truncados al presupuesto).
    Raw {
        /// Los bytes leídos (acotados a `FS_READ_MAX_CHUNK`).
        bytes: Vec<u8>,
        /// `true` si se alcanzó el tope de lectura: puede haber más archivo.
        truncated: bool,
    },
}

/// Evento del hilo de sesión hacia la GUI.
pub enum SessionEvent {
    /// Algo cambió en un dir vigilado (#106), ya coalescido por el debouncer:
    /// la vista re-lista sus panes vigilables. Sin payload a propósito — el
    /// watcher dice QUE hubo cambio, y quién sabe en qué dir está cada pane es
    /// la vista.
    DirsChanged,
    /// El watcher nativo no arrancó y se degradó a sondeo (pitfall de
    /// inotify): se avisa UNA vez por sesión.
    WatchDegraded,
    /// Resultado de un `List` (etiquetado con pane/generación/dir).
    Listed {
        /// Pane destino.
        pane: usize,
        /// Generación del cd que lo pidió.
        generation: u64,
        /// Directorio listado.
        dir: VPath,
        /// Entradas + omitidas del contenedor (#93/#96) o error aplanado a
        /// String (ya renderizable).
        outcome: Result<(Vec<Entry>, Option<u64>), String>,
    },
    /// Catálogo de attrs del scheme (#117): una vez por scheme y sesión.
    AttrCatalog {
        /// Scheme al que pertenece (clave de la caché de la GUI).
        scheme: String,
        /// El catálogo YA saneado (lo sanea el deserializador del wire).
        catalog: norte_proto::AttrCatalog,
    },
    /// La conexión inicial con el daemon falló (mensaje ya renderizable).
    ConnectFailed(String),
    /// La conexión inicial tuvo éxito: `journalled` es
    /// `Backend::Remote(remote).is_journalled()`, preguntado UNA VEZ aquí
    /// (síncrono, sobre el `RemoteBackend` recién conectado) porque esta GUI
    /// no guarda un `Backend` en el hilo de UI (#161). No cambia durante la
    /// sesión: la GUI no reconecta, y hoy solo construye `Backend::Remote`
    /// —jamás `Embedded`— así que la respuesta es estable mientras dure esta
    /// conexión.
    Connected {
        /// Si este backend registra sus mutaciones en un journal.
        journalled: bool,
    },
    /// La op fue aceptada; su task corre con este id (para mapear progreso →
    /// operación en la GUI).
    Submitted {
        /// Id de la task recién creada.
        task_id: TaskId,
        /// La operación que la originó (para read-after-write y reintento).
        op: PendingOp,
    },
    /// La op fue RECHAZADA antes de crear task (path inválido, unsupported…).
    SubmitFailed {
        /// La operación rechazada.
        op: PendingOp,
        /// El error tipado (la GUI decide: banner o modal de conflicto).
        error: Error,
    },
    /// Snapshot de progreso de una task (incl. estado terminal).
    Task(TaskProgress),
    /// El visor de `path` está listo con su contenido.
    ViewerOpened {
        /// Archivo mostrado.
        path: VPath,
        /// Preview de plugin o bytes crudos.
        content: ViewerContent,
        /// Imagen YA decodificada en el hilo de sesión (#92: el decode caro
        /// jamás corre en el hilo de UI). `None` = el contenido no es una
        /// imagen reconocida (o vino de un previewer plugin).
        image: Option<ImageDecode>,
        /// Generación del `OpenViewer` que lo pidió (guard anti-stale).
        generation: u64,
    },
    /// No se pudo abrir el visor de `path` (error ya renderizable).
    ViewerFailed {
        /// Archivo que se intentó abrir.
        path: VPath,
        /// Mensaje ya renderizable.
        error: String,
        /// Generación del `OpenViewer` que lo pidió (guard anti-stale).
        generation: u64,
    },
    /// Resultado de un `Decorate` (G3b, ADR 0037): el mapa YA aplanado y
    /// SANEADO (`norte_frontend::merge_decorations`/`sanitize_decoration` —
    /// el mismo criterio que la TUI, un solo lugar de saneado compartido
    /// por ambos frontends). Un daemon sin decoradores consentidos (o un
    /// `MethodNotFound` de un daemon viejo, ya absorbido por
    /// `Backend::plugin_decorate`) da un mapa vacío, nunca un error — el
    /// listado se pinta igual, sin badges.
    Decorated {
        /// Pane destino.
        pane: usize,
        /// Generación del cd que lo pidió (guard anti-stale).
        generation: u64,
        /// Directorio listado (guard anti-stale: debe casar el `dir`
        /// vigente del pane al aplicar).
        dir: VPath,
        /// Decoraciones ya saneadas, por ruta.
        decorations: HashMap<VPath, norte_frontend::Decoration>,
    },
    /// Resultado de un `StatBatch` (#52/#123): `(ruta, size, mtime_ms)` de
    /// los stats que RESPONDIERON. Un stat fallido o vencido simplemente no
    /// viaja — la fila se queda lazy y la dedup del caller evita el
    /// reintento en bucle (mismo criterio indulgente que `Decorated`: una
    /// celda en blanco, jamás un error de listado).
    Hydrated {
        /// Pane destino.
        pane: usize,
        /// Generación del cd que lo pidió (guard anti-stale).
        generation: u64,
        /// Directorio listado (guard anti-stale).
        dir: VPath,
        /// Entradas hidratadas: ruta, tamaño, mtime.
        entries: Vec<(VPath, Option<u64>, Option<i64>)>,
    },
    /// Resultado de `Columns` (#117-follow-up): los valores de las columnas
    /// `plugin:` CONFIGURADAS, YA saneados y validados contra el catálogo.
    /// Columna no consentida/no declarada = ausente (celdas en blanco,
    /// nunca error — mismo criterio indulgente que `Decorated`).
    ColumnsReady {
        /// Pane destino.
        pane: usize,
        /// Generación del cd que lo pidió (guard anti-stale).
        generation: u64,
        /// Directorio listado (guard anti-stale).
        dir: VPath,
        /// Valores por id Display (`plugin:<p>/<c>`) → ruta → celda YA
        /// saneada — el shape de `PaneState::set_plugin_columns`.
        values: HashMap<String, HashMap<VPath, String>>,
    },
    /// Catálogo de plugins (G3c): respuesta a `PluginsList`.
    PluginsListed(Result<(Vec<PluginInfo>, Vec<PluginLoadError>), String>),
    /// Esquema `[config]` de UN plugin (G3c): respuesta a `PluginGetConfig`.
    PluginConfigReady {
        /// Id del plugin consultado.
        id: String,
        /// Claves YA saneadas (`norte_frontend::plugin_config::sanitize_config_keys`).
        rows: Vec<norte_frontend::plugin_config::ConfigKeyRow>,
    },
    /// Fallo al pedir `plugin.get_config` (G3c) — mensaje ya renderizable.
    PluginConfigFailed(String),
    /// Respuesta a [`SessionCmd::PluginHelp`] (H3f).
    ///
    /// El markdown NO está enmascarado: lleva verbatim lo que el plugin
    /// escribiera. Se parsea con `norte_help::parse_untrusted`, que enmascara
    /// al construir el modelo; nunca se pinta ni se loguea en crudo. NO hay
    /// variante de fallo: una página que no llega deja la página vacía (ver el
    /// brazo del comando), que es mejor respuesta que un error encima de la
    /// ayuda.
    PluginHelpReady {
        /// Plugin al que pertenece la página.
        id: String,
        /// El resultado ya acotado que vino del wire.
        result: norte_proto::methods::PluginHelpResult,
    },
    /// Resúmenes de `[config]` para la sección Plugins del overlay de
    /// ajustes (G3c): respuesta a `PluginConfigSummaries`.
    PluginConfigSummariesReady(Vec<norte_frontend::settings::PluginConfigSummary>),
    /// `plugin.set_config` tuvo éxito (G3c): `key`/`value` para el mensaje
    /// de confirmación (`key` charset-safe, `value` ya validado
    /// client-side).
    PluginConfigSaved {
        /// Clave fijada.
        key: String,
        /// Valor nuevo, como texto de display.
        value: String,
    },
    /// `plugin.set_config` falló (G3c) — mensaje ya renderizable.
    PluginConfigSaveFailed(String),
    /// `plugin.set_approval`/`plugin.set_enabled` tuvieron éxito (G3c GUI
    /// wiring): `approved`/`enabled` es el nuevo estado LOCAL (feedback
    /// optimista, mismo criterio que la TUI's `set_local_approved`).
    PluginGovernanceSet {
        /// Id del plugin afectado.
        id: String,
        /// `Some(approved)` si fue una aprobación; `None` si fue
        /// activación.
        approved: Option<bool>,
        /// `Some(enabled)` si fue una activación; `None` si fue aprobación.
        enabled: Option<bool>,
    },
    /// `plugin.set_approval`/`plugin.set_enabled` fallaron (G3c) — mensaje
    /// ya renderizable.
    PluginGovernanceFailed(String),
    /// `plugin.run_command` tuvo éxito (G3c, disparado desde la paleta):
    /// salida NO confiable del plugin, sin sanear todavía.
    PluginRunResult(String),
    /// `plugin.run_command` falló (G3c) — mensaje ya renderizable.
    PluginRunFailed(String),
    /// Resultado de `AiRenamePlan` (M4-IA): las parejas del plan (posiblemente
    /// vacías = el modelo no propuso cambios) o el error aplanado a String
    /// (mismo helper que los arms vecinos). `dir` viaja de vuelta para abrir
    /// el modal del plan sobre el dir que lo PIDIÓ, aunque el pane ya haya
    /// navegado a otro sitio.
    AiRenamePlan {
        /// Directorio objetivo del plan (el del prompt, no el vigente).
        dir: VPath,
        /// Parejas from→to o error ya renderizable.
        result: Result<Vec<norte_proto::methods::AiRenameEntry>, String>,
        /// El plan del LOTE (`fs.rename_batch_plan`, §17) de las MISMAS
        /// parejas, pedido en el MISMO viaje que el plan IA.
        ///
        /// Va aquí, y no en un evento aparte, para que llegue ATÓMICAMENTE
        /// con las parejas que describe: dos eventos podrían cruzarse con
        /// una segunda petición y pegar un veredicto a un plan que no es el
        /// suyo.
        ///
        /// `None` = ni se intentó (plan IA vacío, desbordado o con un
        /// segmento inválido: el cinturón de la GUI lo rechaza igual);
        /// `Some(Err)` = el core no pudo planificar.
        plan: Option<Result<norte_proto::methods::FsRenameBatchPlanResult, String>>,
    },
    /// Resultado de `SemanticSearch` (M4-IA-2): los hits path+score
    /// (posiblemente vacíos = sin resultados) o el error aplanado a String
    /// (mismo helper que los arms vecinos). Sin `dir`: la búsqueda es
    /// global (root = None), el modal no aterriza sobre ningún dir concreto.
    SemanticHits {
        /// Hits path+score o error ya renderizable.
        result: Result<Vec<norte_proto::methods::SemanticHit>, String>,
    },
    /// Resultado de `Volumes` (2026-08-10-volumes.md task V4): los
    /// volúmenes (posiblemente vacío) o el error aplanado a String (mismo
    /// helper que los arms vecinos). `pane`/`include_pseudo` repiten lo que
    /// pidió `SessionCmd::Volumes` — el modal los necesita al abrir.
    VolumesReady {
        /// El pane que pidió esta lista — ver `SessionCmd::Volumes::pane`.
        pane: usize,
        /// El modo pedido — ver `SessionCmd::Volumes::include_pseudo`.
        include_pseudo: bool,
        /// Volúmenes o error ya renderizable.
        result: Result<Vec<norte_proto::methods::Volume>, String>,
    },
    /// La comparación ARRANCÓ (#158): hay Task, y con ella el `task_id` que
    /// marca de quién son los lotes que vengan detrás. La GUI abre el panel
    /// AQUÍ y no al pulsar la tecla, igual que la TUI: un panel abierto antes
    /// de que exista Task tendría que inventarse a qué comparación pertenece
    /// lo que le llegue.
    CompareStarted {
        /// La Task de esta comparación.
        task_id: TaskId,
        /// El pane que la lanzó — el eco de `SessionCmd::Compare::left_pane`.
        left_pane: usize,
        /// El eco de `SessionCmd::Compare::generation` (guard anti-stale).
        generation: u64,
        /// Raíz izquierda (la del pane que lanzó), para la cabecera.
        left_root: VPath,
        /// Raíz derecha.
        right_root: VPath,
    },
    /// UN lote de filas de comparación, etiquetado con su Task.
    CompareRows {
        /// La Task de la que salen: la vista DESCARTA lo que no sea suyo.
        task_id: TaskId,
        /// Las filas del lote, en el orden en que el walk las produjo.
        rows: Vec<norte_proto::methods::CompareRow>,
    },
    /// El canal de filas se cerró, con el snapshot de progreso que había en
    /// ese instante.
    ///
    /// **`state` puede NO ser terminal**, y eso no es un fallo: la bomba de
    /// filas y la de progreso son tasks distintas, así que el canal puede
    /// cerrarse antes de que el estado terminal se publique. Se manda tal
    /// cual —sin esperar al terminal, exactamente como hace la TUI— y quien
    /// decide qué significa es
    /// `norte_frontend::compare::CompareView::finish_from_task`, el mismo
    /// que llama la TUI.
    CompareDone {
        /// La Task que termina.
        task_id: TaskId,
        /// El estado leído al cerrarse el canal (puede no ser terminal).
        state: TaskState,
        /// Cuántas filas dice la Task haber emitido (`TaskProgress::entries_done`):
        /// la mitad de la cuenta que distingue «hecho» de «se perdieron lotes».
        entries_done: u64,
    },
    /// `fs.compare` fue RECHAZADA antes de existir Task alguna (dos raíces
    /// iguales, `follow_symlinks`, un daemon N-1 sin el método): NO se abre
    /// panel — uno vacío que dice «fallo» es peor que la frase en el banner,
    /// porque además hay que cerrarlo (mismo criterio que la TUI).
    CompareFailed {
        /// El pane que la pidió — el eco de `SessionCmd::Compare::left_pane`:
        /// la negativa se pinta donde se puso el «comparando…» que sustituye,
        /// aunque el foco se haya movido mientras tanto.
        left_pane: usize,
        /// El eco de `SessionCmd::Compare::generation` (guard anti-stale),
        /// igual que [`SessionEvent::CompareStarted`].
        ///
        /// Sin él, la negativa de una petición ya SUPERADA dejaba pintado
        /// «la comparación falló» describiendo algo que el lector ya
        /// reemplazó — y como cada `Compare` es su propio `tokio::spawn`, dos
        /// teclas seguidas pueden contestar en orden inverso (revisión de
        /// rama, MINOR-3).
        generation: u64,
        /// El error tipado; la GUI lo convierte en frase.
        error: Error,
    },
    /// El plan de sincronización ARRANCÓ (#161): hay Task, y con ella el
    /// `task_id` que marca de quién son los lotes que vengan detrás. La GUI
    /// abre el panel AQUÍ y no al pulsar la tecla, igual que con la
    /// comparación y por el mismo motivo.
    SyncPlanStarted {
        /// La Task de este plan.
        task_id: TaskId,
        /// El pane que lo pidió — el eco de `SessionCmd::SyncPlan::source_pane`.
        source_pane: usize,
        /// El eco de `SessionCmd::SyncPlan::generation` (guard anti-stale).
        generation: u64,
        /// El modo pedido: la cabecera tiene que decir si esto BORRA antes de
        /// que nadie apruebe nada.
        mode: norte_proto::methods::SyncMode,
        /// Raíz ORIGEN, congelada en la petición.
        source_root: VPath,
        /// Raíz DESTINO.
        dest_root: VPath,
    },
    /// UN lote de pasos del plan.
    ///
    /// **Sin `generation`, y es deliberado** (revisión rust BLOCKER-1): lo
    /// que dice de quién es un lote es el `task_id` que trae del wire, y lo
    /// compara el modelo compartido (`SyncState::on_steps`) — la misma regla
    /// que obedece la TUI, y la misma que `CompareRows` sigue en esta GUI.
    /// La generación cuenta PETICIONES, no Tasks: una petición posterior que
    /// el daemon RECHAZA (raíces solapadas, sin spool) la adelanta sin abrir
    /// panel, y un guard de generación aquí dejaría al panel vivo sin recibir
    /// ni un paso más, sin poder cerrar su plan y sin nadie que cancelara su
    /// Task.
    SyncSteps {
        /// El lote, tal cual llegó (con su `task_id`).
        batch: norte_proto::methods::SyncStepsBatch,
    },
    /// El `sync.plan_done` que CIERRA el plan: es lo que le da su `plan_hash`,
    /// y sin él no hay nada aprobable. **No es el final del canal**, que es
    /// [`SessionEvent::SyncPlanEnded`] y significa otra cosa.
    ///
    /// Sin `generation`, por lo mismo que [`SessionEvent::SyncSteps`]: el
    /// cierre trae su `task_id`, y ésa es la correlación.
    SyncPlanDone {
        /// El cierre, tal cual llegó (con su `task_id` y su `plan_hash`).
        done: norte_proto::methods::SyncPlanDone,
    },
    /// El canal de eventos del plan se cerró, con el estado que la Task tenía
    /// en ese instante.
    ///
    /// Existe porque «la Task acabó» y «el plan está completo» son dos hechos
    /// distintos, y confundirlos es el defecto que las fases A y B shipearon:
    /// un plan cancelado o fallido cierra su canal SIN `sync.plan_done`, y
    /// entonces no hay plan — solo un desenlace que pintar. Como en
    /// `CompareDone`, `state` puede NO ser terminal (la bomba de eventos y la
    /// de progreso son tasks distintas) y se manda igual: quien lo interpreta
    /// es `norte_frontend::sync::SyncRunState::from_task_state`, el mismo
    /// mapeo que usa la TUI.
    SyncPlanEnded {
        /// La Task que termina — la del PLAN, y la correlación de este
        /// evento: la vista la contrasta con la suya (la tarea 4 le pondrá
        /// encima la de `sync.apply`). Sin `generation`, igual que sus dos
        /// hermanos y por el mismo motivo.
        task_id: TaskId,
        /// El estado leído al cerrarse el canal (puede no ser terminal).
        state: TaskState,
    },
    /// `sync.plan` fue RECHAZADO antes de existir Task alguna (raíces
    /// solapadas, un daemon sin spool o N-1, más marcas de las que caben): NO
    /// se abre panel, por lo mismo que `CompareFailed`.
    SyncPlanFailed {
        /// El pane que lo pidió — el eco de `SessionCmd::SyncPlan::source_pane`.
        source_pane: usize,
        /// El eco de `SessionCmd::SyncPlan::generation` (guard anti-stale).
        ///
        /// **La negativa lo lleva también**, y no es adorno: C1 shipeó
        /// `CompareFailed` sin él y la negativa de una petición ya superada
        /// dejaba pintado un banner describiendo algo que el lector había
        /// reemplazado.
        generation: u64,
        /// El error tipado; la GUI lo convierte en frase.
        error: Error,
    },
    /// `sync.apply` ARRANCÓ: hay Task, y es la que a partir de ahora ESCRIBE.
    ///
    /// El panel la adopta —pasa a ser lo que su `Esc` y su cierre cancelan— y
    /// el modelo avanza a `Applying`, que es lo que hace imposible aprobar dos
    /// veces el mismo plan.
    SyncApplyStarted {
        /// La Task de la aplicación.
        task_id: TaskId,
        /// El eco de `SessionCmd::SyncApply::generation` (guard anti-stale).
        ///
        /// **Aquí el guard no es cosmético**, al revés que en un lote de
        /// pasos: si la petición está vencida es porque el lector pidió otro
        /// plan, y ese plan ya abrió otro panel — dejar corriendo esta Task
        /// sería un `Mirror` borrando sin nadie que lo vea ni lo pueda parar.
        generation: u64,
    },
    /// La Task de `sync.apply` TERMINÓ, con lo que `sync.report` contestó.
    ///
    /// Los dos viajan juntos a propósito: el estado dice CÓMO acabó y el
    /// informe QUÉ hizo, y son las dos mitades de una sola frase. El informe se
    /// pide también cuando la Task se canceló.
    ///
    /// Sin `generation`, igual que [`SessionEvent::SyncPlanEnded`]: lo que
    /// correlaciona un final es su `task_id`, que la vista contrasta con el
    /// suyo (reasignado al adoptar la aplicación).
    SyncApplyEnded {
        /// La Task que termina — la de la APLICACIÓN.
        task_id: TaskId,
        /// El estado terminal. Si los emisores del progreso mueren sin
        /// desenlace se sintetiza un `Failed`, igual que en
        /// [`forward_progress`]: una sincronización a medias sobre una
        /// conexión muerta no es un éxito.
        state: TaskState,
        /// Lo que `sync.report` contestó, o por qué no se pudo pedir. Sin
        /// informe no se sabe qué se escribió, y el panel lo pinta como fallo
        /// en vez de decir «hecho».
        report: Box<Result<norte_proto::methods::SyncReportResult, Error>>,
    },
    /// `sync.apply` fue RECHAZADO antes de existir Task alguna: el plan
    /// caducó en el spool, otro lo gastó (`PlanStale`), o el daemon se cayó
    /// entre el plan y la aprobación. **El panel sigue abierto** —a diferencia
    /// de un `sync.plan` rechazado—, así que la negativa se pinta también en
    /// él: un banner mientras el pie sigue ofreciendo aprobar es la pantalla
    /// contradiciéndose.
    SyncApplyFailed {
        /// El eco de `SessionCmd::SyncApply::generation` (guard anti-stale),
        /// por lo mismo que en `SyncPlanFailed`: dos peticiones seguidas
        /// pueden contestar en orden inverso.
        generation: u64,
        /// El error tipado; la GUI lo convierte en frase.
        error: Error,
    },
}

/// Arranca el hilo de sesión: conecta al `socket` UNA vez y sirve `cmd_rx`,
/// emitiendo por `event_tx`. Si `connect` falla, emite `ConnectFailed` y el
/// hilo termina (la GUI queda en estado de error, usable).
pub fn spawn(
    socket: PathBuf,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCmd>,
    event_tx: mpsc::UnboundedSender<SessionEvent>,
) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = event_tx.send(SessionEvent::ConnectFailed(format!("runtime tokio: {e}")));
                return;
            }
        };
        rt.block_on(async move {
            let remote = match connect(&socket).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = event_tx.send(SessionEvent::ConnectFailed(format!("{e}")));
                    return;
                }
            };
            // Síncrono a propósito (ver doc de `SessionEvent::Connected`):
            // `is_journalled` no hace I/O, así que preguntarlo aquí, una vez,
            // sobre el `Backend::Remote` recién construido no cuesta un
            // viaje de red extra.
            let journalled = Backend::Remote(remote.clone()).is_journalled();
            let _ = event_tx.send(SessionEvent::Connected { journalled });
            let cancellers: Arc<Mutex<HashMap<TaskId, TaskCanceller>>> =
                Arc::new(Mutex::new(HashMap::new()));
            // #106: la vigilancia de dirs vive aquí, que es donde hay runtime.
            // Soltarla al salir del bloque la para (cancelación por drop).
            let mut dir_watch = norte_frontend::watch::DirWatch::new();
            let mut watch_alive = true;
            loop {
                let cmd = tokio::select! {
                    cmd = cmd_rx.recv() => match cmd {
                        Some(c) => c,
                        // La vista soltó su extremo: se acabó la sesión.
                        None => break,
                    },
                    ev = dir_watch.rx.recv(), if watch_alive => {
                        if ev.is_none() {
                            // Inalcanzable mientras `dir_watch` viva (retiene
                            // el emisor crudo); si pasara, desarmar la rama en
                            // vez de girar en vacío.
                            watch_alive = false;
                        } else {
                            let _ = event_tx.send(SessionEvent::DirsChanged);
                        }
                        continue;
                    }
                };
                match cmd {
                    SessionCmd::Watch(dirs) => {
                        dir_watch.rewatch(&dirs);
                        if dir_watch.take_degraded_notice() {
                            let _ = event_tx.send(SessionEvent::WatchDegraded);
                        }
                    }
                    SessionCmd::List {
                        pane,
                        generation,
                        dir,
                        attrs,
                        fetch_catalog,
                    } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            // #117: el catálogo ANTES del listado (misma
                            // conexión, una vez por scheme); un fallo NO
                            // tumba el listado — sin hints se pinta Opaque.
                            if fetch_catalog && let Ok(catalog) = backend.attr_catalog(&dir).await {
                                let _ = tx.send(SessionEvent::AttrCatalog {
                                    scheme: dir.scheme().to_owned(),
                                    catalog,
                                });
                            }
                            let outcome = backend
                                .list_with_skipped_attrs(&dir, &attrs)
                                .await
                                .map_err(|e| format!("{e}"));
                            let _ = tx.send(SessionEvent::Listed {
                                pane,
                                generation,
                                dir,
                                outcome,
                            });
                        });
                    }
                    SessionCmd::Submit(op) => {
                        submit(&remote, op, &event_tx, &cancellers).await;
                    }
                    SessionCmd::Cancel(id) => {
                        // INVARIANTE: el Mutex nunca se envenena (sin panic bajo lock).
                        if let Some(c) = cancellers.lock().unwrap().get(&id) {
                            c.cancel();
                        }
                    }
                    SessionCmd::OpenViewer { path, generation } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(
                            async move { open_viewer(&backend, path, generation, &tx).await },
                        );
                    }
                    SessionCmd::Decorate {
                        pane,
                        generation,
                        dir,
                        paths,
                    } => {
                        if paths.is_empty() {
                            continue;
                        }
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let plugins = backend.plugin_decorate(&paths).await.unwrap_or_default();
                            let decorations = norte_frontend::merge_decorations(&paths, &plugins);
                            let _ = tx.send(SessionEvent::Decorated {
                                pane,
                                generation,
                                dir,
                                decorations,
                            });
                        });
                    }
                    SessionCmd::StatBatch {
                        pane,
                        generation,
                        dir,
                        paths,
                    } => {
                        if paths.is_empty() {
                            continue;
                        }
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let entries = stat_batch(&backend, paths).await;
                            let _ = tx.send(SessionEvent::Hydrated {
                                pane,
                                generation,
                                dir,
                                entries,
                            });
                        });
                    }
                    SessionCmd::Columns {
                        pane,
                        generation,
                        dir,
                        paths,
                        requested,
                    } => {
                        if paths.is_empty() || requested.is_empty() {
                            continue;
                        }
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            columns_ready(&backend, pane, generation, dir, paths, requested, &tx)
                                .await;
                        });
                    }
                    SessionCmd::PluginsList => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let res = backend
                                .plugins_list()
                                .await
                                .map(|r| (r.plugins, r.errors))
                                .map_err(|e| format!("{e}"));
                            let _ = tx.send(SessionEvent::PluginsListed(res));
                        });
                    }
                    SessionCmd::PluginGetConfig { id } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            match backend.plugin_get_config(&id).await {
                                Ok(result) => {
                                    let rows = norte_frontend::plugin_config::sanitize_config_keys(
                                        &result.keys,
                                    );
                                    let _ = tx.send(SessionEvent::PluginConfigReady { id, rows });
                                }
                                Err(e) => {
                                    let _ =
                                        tx.send(SessionEvent::PluginConfigFailed(format!("{e}")));
                                }
                            }
                        });
                    }
                    SessionCmd::PluginHelp { id } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            // Un fallo es SILENCIOSO a propósito: una página
                            // vacía con el nombre del plugin es mejor que un
                            // error encima del overlay de ayuda, y un daemon
                            // N-1 sin el handler cae aquí también
                            // (`plugin.help` es 0.34.0). Cerrar y reabrir la
                            // ayuda es el reintento del lector.
                            if let Ok(result) = backend.plugin_help(&id).await {
                                let _ = tx.send(SessionEvent::PluginHelpReady { id, result });
                            }
                        });
                    }
                    SessionCmd::PluginConfigSummaries => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let summaries = plugin_config_summaries(&backend).await;
                            let _ = tx.send(SessionEvent::PluginConfigSummariesReady(summaries));
                        });
                    }
                    SessionCmd::PluginSetConfig { id, key, value } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            match backend.plugin_set_config(&id, &key, &value).await {
                                Ok(()) => {
                                    let _ = tx.send(SessionEvent::PluginConfigSaved { key, value });
                                }
                                Err(e) => {
                                    let _ = tx
                                        .send(SessionEvent::PluginConfigSaveFailed(format!("{e}")));
                                }
                            }
                        });
                    }
                    SessionCmd::PluginSetApproval { id, approved } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            match backend.plugins_set_approval(&id, approved).await {
                                Ok(()) => {
                                    let _ = tx.send(SessionEvent::PluginGovernanceSet {
                                        id,
                                        approved: Some(approved),
                                        enabled: None,
                                    });
                                }
                                Err(e) => {
                                    let _ = tx
                                        .send(SessionEvent::PluginGovernanceFailed(format!("{e}")));
                                }
                            }
                        });
                    }
                    SessionCmd::PluginSetEnabled { id, enabled } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            match backend.plugins_set_enabled(&id, enabled).await {
                                Ok(()) => {
                                    let _ = tx.send(SessionEvent::PluginGovernanceSet {
                                        id,
                                        approved: None,
                                        enabled: Some(enabled),
                                    });
                                }
                                Err(e) => {
                                    let _ = tx
                                        .send(SessionEvent::PluginGovernanceFailed(format!("{e}")));
                                }
                            }
                        });
                    }
                    SessionCmd::AiRenamePlan { dir, instruction } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let result = backend
                                .ai_rename_plan(&dir, &instruction)
                                .await
                                .map(|r| r.entries)
                                .map_err(|e| format!("{e}"));
                            // §17: el plan del LOTE, en el MISMO viaje. Se
                            // pide solo si hay parejas que puedan llegar a
                            // aplicarse — un plan vacío, uno desbordado o uno
                            // con un segmento inválido lo rechaza el cinturón
                            // de la GUI, así que ni se molesta al daemon.
                            let plan = match &result {
                                Ok(entries)
                                    if !entries.is_empty()
                                        && entries.len() <= norte_frontend::MAX_AI_PLAN_ENTRIES =>
                                {
                                    match norte_frontend::rename_pairs(entries) {
                                        Some(pairs) => Some(
                                            backend
                                                .rename_batch_plan(&dir, &pairs)
                                                .await
                                                .map_err(|e| format!("{e}")),
                                        ),
                                        None => None,
                                    }
                                }
                                _ => None,
                            };
                            let _ = tx.send(SessionEvent::AiRenamePlan { dir, result, plan });
                        });
                    }
                    SessionCmd::SemanticSearch { query } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let result = backend
                                .index_search_semantic(None, &query, norte_frontend::SEMANTIC_K)
                                .await
                                .map_err(|e| format!("{e}"));
                            let _ = tx.send(SessionEvent::SemanticHits { result });
                        });
                    }
                    SessionCmd::Volumes {
                        pane,
                        include_pseudo,
                    } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let result = backend
                                .volumes(include_pseudo)
                                .await
                                .map_err(|e| format!("{e}"));
                            let _ = tx.send(SessionEvent::VolumesReady {
                                pane,
                                include_pseudo,
                                result,
                            });
                        });
                    }
                    SessionCmd::Compare {
                        left_pane,
                        generation,
                        params,
                    } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        let cancellers = Arc::clone(&cancellers);
                        tokio::spawn(async move {
                            compare(
                                &backend,
                                Request {
                                    left_pane,
                                    generation,
                                },
                                *params,
                                &tx,
                                &cancellers,
                            )
                            .await;
                        });
                    }
                    SessionCmd::SyncPlan {
                        source_pane,
                        generation,
                        params,
                    } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        let cancellers = Arc::clone(&cancellers);
                        tokio::spawn(async move {
                            sync_plan(
                                &backend,
                                SyncRequest {
                                    source_pane,
                                    generation,
                                },
                                *params,
                                &tx,
                                &cancellers,
                            )
                            .await;
                        });
                    }
                    SessionCmd::SyncApply {
                        generation,
                        plan_hash,
                    } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        let cancellers = Arc::clone(&cancellers);
                        tokio::spawn(async move {
                            sync_apply(&backend, generation, &plan_hash, &tx, &cancellers).await;
                        });
                    }
                    SessionCmd::PluginRunCommand { id, command, arg } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            match backend.plugin_run_command(&id, &command, &arg).await {
                                Ok(output) => {
                                    let _ = tx.send(SessionEvent::PluginRunResult(output));
                                }
                                Err(e) => {
                                    let _ = tx.send(SessionEvent::PluginRunFailed(format!("{e}")));
                                }
                            }
                        });
                    }
                }
            }
        });
    });
}

/// Stats simultáneos dentro de una tanda de `StatBatch` (#123): acota las
/// peticiones en vuelo contra el daemon sin serializar la latencia de la
/// pantalla entera (mismo número que la sonda de la TUI).
const STAT_CONCURRENCY: usize = 8;

/// Resuelve `StatBatch` (#52/#123): un `fs.stat` por ruta, en tandas de
/// [`STAT_CONCURRENCY`], devolviendo SOLO las que respondieron. Fail-soft
/// como `Decorate`/`Columns`: un stat que falla no aborta el resto ni
/// produce un error de listado — esa fila se queda con la celda en blanco
/// y el caller, que ya la anotó como pedida, no la reintenta en bucle. Sin
/// timeout propio, igual que los otros dos brazos asíncronos de este
/// worker: la vida de la petición la acota la conexión.
async fn stat_batch(
    backend: &Backend,
    paths: Vec<VPath>,
) -> Vec<(VPath, Option<u64>, Option<i64>)> {
    let mut out = Vec::with_capacity(paths.len());
    for chunk in paths.chunks(STAT_CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for path in chunk {
            let backend = backend.clone();
            let path = path.clone();
            set.spawn(async move {
                let entry = backend.stat(&path).await.ok()?;
                Some((path, entry.size, entry.mtime_ms))
            });
        }
        while let Some(res) = set.join_next().await {
            if let Ok(Some(hidratada)) = res {
                out.push(hidratada);
            }
        }
    }
    out
}

/// Resuelve `Columns` (G3c): descubre columnas de plugins `columns`
/// aprobados+activados vía `plugin.list` (`PluginInfo::columns`, 0.28.0),
/// deduplicadas por `id` (PRIMERA que casa gana — mismo criterio que
/// `PluginRegistry::resolve_columns`), y pide `plugin.column_values` para
/// CADA una sobre `paths`. Fail-soft por columna: si una falla, las demás
/// se pintan igual (`unwrap_or_default` sobre `Vec<Option<String>>` vacío
/// = sin celdas de esa columna, nunca aborta el resto).
async fn columns_ready(
    backend: &Backend,
    pane: usize,
    generation: u64,
    dir: VPath,
    paths: Vec<VPath>,
    requested: Vec<(String, String)>,
    tx: &mpsc::UnboundedSender<SessionEvent>,
) {
    // #117-follow-up: validación de pertenencia + dedupe de colisiones en
    // el modelo COMPARTIDO (`validated_plugin_requests` — review MAJOR-1:
    // una sola definición para ambos frontends; colisión de id bare =
    // blanco antes que atribución falsa, desambiguación real = issue
    // #120). Fail-soft por columna (catálogo caído o RPC fallida = celdas
    // en blanco). `tx.is_closed()` corta entre RPCs solo en el teardown de
    // la sesión (review MINOR-1; un supersede por generación no cierra el
    // canal — lo descarta el guard de `apply_event`).
    let mut values = HashMap::new();
    if let Ok(list) = backend.plugins_list().await {
        for (plugin, column) in
            norte_frontend::columns::validated_plugin_requests(&requested, &list.plugins)
        {
            if tx.is_closed() {
                return;
            }
            let raw = backend
                .plugin_column_values(&plugin, &column, &paths)
                .await
                .unwrap_or_default();
            let sanitized = norte_frontend::columns::sanitize_column_values(&paths, &raw);
            values.insert(
                norte_frontend::columns::plugin_display_id(&plugin, &column),
                sanitized,
            );
        }
    }
    let _ = tx.send(SessionEvent::ColumnsReady {
        pane,
        generation,
        dir,
        values,
    });
}

/// Resuelve `PluginConfigSummaries` (G3c): `plugins_list` filtrado a
/// aprobados+activados, luego un `plugin.get_config` POR plugin, quedándose
/// solo con los que declaran al menos una clave — mismo criterio best-effort
/// que la TUI's `plugin_config_summaries` (main.rs): un plugin cuyo
/// `get_config` falla se DESCARTA de la sección, nunca bloquea el resto.
async fn plugin_config_summaries(
    backend: &Backend,
) -> Vec<norte_frontend::settings::PluginConfigSummary> {
    let Ok(list) = backend.plugins_list().await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for p in list.plugins.iter().filter(|p| p.approved && p.enabled) {
        let Ok(cfg) = backend.plugin_get_config(&p.id).await else {
            continue;
        };
        if cfg.keys.is_empty() {
            continue;
        }
        let name = norte_frontend::display_name(p.name.as_bytes()).0;
        out.push(norte_frontend::settings::PluginConfigSummary {
            plugin_id: p.id.clone(),
            name,
            key_count: cfg.keys.len(),
        });
    }
    out
}

/// Intenta un previewer de plugin; si ninguno aplica, lee bytes acotados. El
/// `truncated` se estima con el tope de lectura alcanzado (más bytes de los
/// leídos podrían quedar pendientes). `generation` viaja intacta a los
/// eventos (guard anti-stale de `apply_event`, como `List`).
async fn open_viewer(
    backend: &Backend,
    path: VPath,
    generation: u64,
    tx: &mpsc::UnboundedSender<SessionEvent>,
) {
    // G3a (ADR 0037): intenta el preview CON ESTILO primero — `Ok(Some(_))`
    // = un previewer aplicó; `Ok(None)` (ninguno aplica, un guest cayó, o
    // los topes del wire se violaron — todo degrada igual, ver
    // `Backend::plugin_preview_styled`) o `Err` (fallo de red) caen al
    // preview PLANO clásico, que a su vez cae a los bytes crudos.
    if let Ok(Some(p)) = backend.plugin_preview_styled(&path).await {
        let _ = tx.send(SessionEvent::ViewerOpened {
            path,
            content: ViewerContent::PluginStyled {
                plugin_name: p.plugin_name,
                lines: p.lines,
                lossy: p.lossy,
            },
            image: None,
            generation,
        });
        return;
    }
    // `Err` (preview falló) o `Ok` sin previewer aplicable: cae a la vista
    // cruda igual, sin distinguir el motivo aquí.
    if let Ok(res) = backend.plugin_preview(&path).await
        && let Some(p) = res.preview
    {
        let _ = tx.send(SessionEvent::ViewerOpened {
            path,
            content: ViewerContent::Plugin {
                plugin_name: p.plugin_name,
                output: p.output,
                lossy: p.lossy,
            },
            image: None,
            generation,
        });
        return;
    }
    let budget = norte_proto::methods::FS_READ_MAX_CHUNK;
    match backend
        .read(
            &path,
            Some(norte_proto::ByteRange {
                offset: 0,
                len: Some(budget),
            }),
        )
        .await
    {
        Ok(bytes) => {
            let truncated = bytes.len() as u64 >= budget; // leímos el tope: puede haber más.
            // #92: decode de imagen AQUÍ (hilo de sesión), no en el handler
            // de UI — una imagen cara (JPEG progresivo cerca del tope) ya no
            // congela el frame. Solo bytes que el viewer pintará como imagen.
            let image = norte_frontend::viewer::image_format(&bytes)
                .is_some()
                .then(|| decode_image(&bytes));
            let _ = tx.send(SessionEvent::ViewerOpened {
                path,
                content: ViewerContent::Raw { bytes, truncated },
                image,
                generation,
            });
        }
        Err(e) => {
            let _ = tx.send(SessionEvent::ViewerFailed {
                path,
                error: format!("{e}"),
                generation,
            });
        }
    }
}

/// Conecta al daemon (sin autoarrancarlo).
async fn connect(socket: &Path) -> Result<RemoteBackend, norte_proto::Error> {
    RemoteBackend::connect(
        socket.to_path_buf(),
        None,
        ClientInfo {
            name: "norte-gui".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    )
    .await
}

/// Lanza una op mutante y arranca su forwarder de progreso, o emite
/// `SubmitFailed` si el daemon la rechaza de entrada.
async fn submit(
    remote: &RemoteBackend,
    op: PendingOp,
    event_tx: &mpsc::UnboundedSender<SessionEvent>,
    cancellers: &Arc<Mutex<HashMap<TaskId, TaskCanceller>>>,
) {
    let backend = Backend::Remote(remote.clone());
    let res = match &op {
        PendingOp::Transfer {
            kind: TransferKind::Copy,
            from,
            to,
            opts,
        } => backend.copy(from, to, *opts).await,
        PendingOp::Transfer {
            kind: TransferKind::Move,
            from,
            to,
            opts,
        } => backend.move_(from, to, *opts).await,
        PendingOp::Delete { path, mode } => backend.delete(path, *mode).await,
        // §17: UNA task para el lote entero (el core reordena, mete los
        // temporales y deshace si un paso falla).
        PendingOp::RenameBatch {
            dir,
            pairs,
            plan_hash,
        } => backend.rename_batch(dir, pairs, plan_hash).await,
    };
    match res {
        Ok(task) => {
            let id = task.id();
            let guard = register_canceller(cancellers, &task);
            let _ = event_tx.send(SessionEvent::Submitted { task_id: id, op });
            let tx = event_tx.clone();
            tokio::spawn(async move { forward_progress(task, &tx, guard).await });
        }
        Err(error) => {
            let _ = event_tx.send(SessionEvent::SubmitFailed { op, error });
        }
    }
}

/// El registro del canceller de UNA task, retirado al soltarse (#190).
///
/// Regla dura 3: una task larga se para desde fuera, y quien la para busca en
/// este mapa. Estaba escrito como un `insert` y un `remove` a mano en cuatro
/// sitios, y el `remove` es el que se olvida: un camino de salida temprana —o
/// un `?` que alguien añada mañana— deja el mapa con un canceller de una task
/// muerta, que es el mismo mapa que decide si `Cancel` encuentra algo. Con el
/// guard, el de-registro es estructural: pasa aunque se salga por donde se
/// salga.
struct CancellerGuard {
    cancellers: Arc<Mutex<HashMap<TaskId, TaskCanceller>>>,
    id: TaskId,
}

impl Drop for CancellerGuard {
    fn drop(&mut self) {
        // INVARIANTE: el Mutex nunca se envenena (sin panic bajo lock).
        self.cancellers.lock().unwrap().remove(&self.id);
    }
}

/// Registra el canceller de `task` y devuelve el guard que lo retira.
///
/// Se llama ANTES de anunciar el arranque: el `task_id` que la vista recibe
/// tiene que ser cancelable en el instante en que lo recibe, no un poco
/// después.
fn register_canceller(
    cancellers: &Arc<Mutex<HashMap<TaskId, TaskCanceller>>>,
    task: &TaskRef,
) -> CancellerGuard {
    let id = task.id();
    // INVARIANTE: el Mutex nunca se envenena (sin panic bajo lock).
    cancellers.lock().unwrap().insert(id, task.canceller());
    CancellerGuard {
        cancellers: Arc::clone(cancellers),
        id,
    }
}

/// Reenvía cada snapshot de progreso de `task` como `SessionEvent::Task` hasta
/// el terminal; de-registra el canceller al salir. Si la conexión muere sin
/// desenlace, sintetiza un terminal `Failed{ProviderUnavailable}` reusando el
/// último snapshot (mismos id/kind).
async fn forward_progress(
    task: TaskRef,
    tx: &mpsc::UnboundedSender<SessionEvent>,
    canceller: CancellerGuard,
) {
    let mut rx = task.progress();
    loop {
        let snap = rx.borrow().clone();
        let terminal = snap.state.is_terminal();
        let _ = tx.send(SessionEvent::Task(snap.clone()));
        if terminal {
            break;
        }
        if rx.changed().await.is_err() {
            let mut dead = snap;
            dead.state = TaskState::Failed {
                error: Error::ProviderUnavailable { retryable: true },
            };
            let _ = tx.send(SessionEvent::Task(dead));
            break;
        }
    }
    // El canceller se retira aquí, al soltar el guard.
    drop(canceller);
}

/// Quién pidió una comparación: el eco que la GUI necesita de vuelta para
/// abrir el panel sobre el pane correcto y descartar los arranques vencidos.
/// Un struct y no dos `usize`/`u64` sueltos, que es como se cruzan dos
/// argumentos del mismo tipo.
struct Request {
    /// El pane que la lanzó (el lado izquierdo del panel).
    left_pane: usize,
    /// La generación de la petición (guard anti-stale).
    generation: u64,
}

/// Arranca `fs.compare` y BOMBEA sus filas (#158, spec 3 fase C1).
///
/// Un rechazo de entrada (dos raíces iguales, `follow_symlinks`, un daemon
/// N-1 sin el método) sale por [`SessionEvent::CompareFailed`] y no abre
/// panel; con Task, se registra su canceller —así `task.cancel` de la GUI
/// llega por el mismo camino que el de una copia— y se anuncia el `task_id`
/// ANTES de la primera fila, que es lo que le permite a la vista descartar
/// los lotes de una comparación anterior.
async fn compare(
    backend: &Backend,
    who: Request,
    params: norte_proto::methods::FsCompareParams,
    tx: &mpsc::UnboundedSender<SessionEvent>,
    cancellers: &Arc<Mutex<HashMap<TaskId, TaskCanceller>>>,
) {
    let (left_root, right_root) = (params.left.clone(), params.right.clone());
    let (task, rx) = match backend.compare(params).await {
        Ok(pair) => pair,
        Err(error) => {
            let _ = tx.send(SessionEvent::CompareFailed {
                left_pane: who.left_pane,
                generation: who.generation,
                error,
            });
            return;
        }
    };
    let task_id = task.id();
    let _canceller = register_canceller(cancellers, &task);
    let _ = tx.send(SessionEvent::CompareStarted {
        task_id,
        left_pane: who.left_pane,
        generation: who.generation,
        left_root,
        right_root,
    });
    pump_compare(task, rx, tx).await;
}

/// Reenvía cada lote de `rx` como [`SessionEvent::CompareRows`] y, al
/// CERRARSE el canal, UN [`SessionEvent::CompareDone`] con el snapshot de
/// progreso de ese instante.
///
/// **No espera al estado terminal**, y es deliberado: la bomba de filas y la
/// de progreso son tasks independientes, así que el canal puede cerrarse
/// antes de que el terminal se publique —y también puede no publicarse nunca
/// (un daemon caído, un provider colgado en una NFS muerta)—. Esperarlo aquí
/// dejaría el panel diciendo «comparando…» para siempre. La TUI lee el
/// snapshot igual, en `drain_compare`, y quien interpreta un estado no
/// terminal es `norte_frontend::compare::CompareView::finish_from_task` — el
/// crate COMPARTIDO, al que llegan tanto `on_done` como `drain_compare`: una
/// sola regla para los dos frontends, de verdad desde la revisión de rama
/// (MAJOR-1), que encontró tres de sus cuatro brazos transcritos a mano en
/// cada superficie.
///
/// El `task_id` que etiqueta los lotes es el de la Task, no el que trae cada
/// `CompareRowsBatch`: este bombeo es dueño de SU stream, así que sabe de
/// quién son sus filas sin creerle el campo a nadie.
async fn pump_compare(
    task: TaskRef,
    mut rx: mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
    tx: &mpsc::UnboundedSender<SessionEvent>,
) {
    let task_id = task.id();
    while let Some(batch) = rx.recv().await {
        let _ = tx.send(SessionEvent::CompareRows {
            task_id,
            rows: batch.rows,
        });
    }
    let mut progress = task.progress();
    let snapshot = progress.borrow_and_update().clone();
    let _ = tx.send(SessionEvent::CompareDone {
        task_id,
        state: snapshot.state,
        entries_done: snapshot.entries_done,
    });
}

/// Quién pidió un plan de sincronización: el eco que la GUI necesita de vuelta
/// para abrir el panel y para retirar el aviso del pane correcto. Gemelo de
/// [`Request`], y separado de él a propósito: su `usize` significa el lado
/// ORIGEN, no el lado izquierdo, y en una sincronización ese sentido es la
/// mitad de lo que se aprueba.
struct SyncRequest {
    /// El pane que lo pidió (el lado ORIGEN).
    source_pane: usize,
    /// La generación de la petición (guard anti-stale).
    generation: u64,
}

/// Arranca `sync.plan` y BOMBEA sus eventos (#161, spec 3 fase C2).
///
/// Un rechazo de entrada (raíces solapadas, sin spool, un daemon N-1, más
/// marcas de las que caben) sale por [`SessionEvent::SyncPlanFailed`] y no abre
/// panel; con Task, se registra su canceller —así el `task.cancel` de la GUI
/// llega por el mismo camino que el de una copia, regla 3— y se anuncia el
/// `task_id` ANTES del primer lote, que es lo que le permite a la vista
/// descartar los pasos de un plan anterior.
///
/// **Planificar no muta nada.** El plan queda RETENIDO en el spool del daemon y
/// caduca solo; ejecutarlo es `sync.apply`, que no pasa por aquí.
async fn sync_plan(
    backend: &Backend,
    who: SyncRequest,
    params: norte_proto::methods::SyncPlanParams,
    tx: &mpsc::UnboundedSender<SessionEvent>,
    cancellers: &Arc<Mutex<HashMap<TaskId, TaskCanceller>>>,
) {
    let (source_root, dest_root, mode) = (params.source.clone(), params.dest.clone(), params.mode);
    let (task, rx) = match backend.sync_plan(params).await {
        Ok(pair) => pair,
        Err(error) => {
            let _ = tx.send(SessionEvent::SyncPlanFailed {
                source_pane: who.source_pane,
                generation: who.generation,
                error,
            });
            return;
        }
    };
    let task_id = task.id();
    let _canceller = register_canceller(cancellers, &task);
    let _ = tx.send(SessionEvent::SyncPlanStarted {
        task_id,
        source_pane: who.source_pane,
        generation: who.generation,
        mode,
        source_root,
        dest_root,
    });
    pump_sync_plan(task, rx, tx).await;
}

/// Reenvía cada evento de `rx` y, al CERRARSE el canal, UN
/// [`SessionEvent::SyncPlanEnded`] con el snapshot de progreso de ese instante.
///
/// **El cierre del canal no es el cierre del plan.** El plan cierra con su
/// `sync.plan_done`, que viaja como un evento propio: un plan cancelado a
/// medias cierra el canal sin haberlo mandado nunca, y eso significa que no
/// hay `plan_hash` y no hay nada que aprobar. Los dos se mandan por separado
/// justo para que la GUI no pueda confundirlos (la confusión que las fases A y
/// B pagaron).
///
/// **No espera al estado terminal**, igual que [`pump_compare`] y por lo mismo:
/// la bomba de eventos y la de progreso son tasks independientes, y esperarlo
/// aquí dejaría el panel diciendo «planificando…» para siempre contra un daemon
/// caído.
///
/// Los lotes se reenvían TAL CUAL, con el `task_id` que traen del wire: quien
/// decide si son de este plan es el modelo compartido, que es el que también lo
/// decide en la TUI. Este bombeo no les añade `generation` a propósito —ver
/// [`SessionEvent::SyncSteps`]—: lo que correlaciona una Task es su id.
async fn pump_sync_plan(
    task: TaskRef,
    mut rx: mpsc::Receiver<norte_core::sync::SyncPlanEvent>,
    tx: &mpsc::UnboundedSender<SessionEvent>,
) {
    let task_id = task.id();
    while let Some(event) = rx.recv().await {
        let _ = match event {
            norte_core::sync::SyncPlanEvent::Steps(batch) => {
                tx.send(SessionEvent::SyncSteps { batch })
            }
            norte_core::sync::SyncPlanEvent::Done(done) => {
                tx.send(SessionEvent::SyncPlanDone { done })
            }
        };
    }
    let mut progress = task.progress();
    let snapshot = progress.borrow_and_update().clone();
    let _ = tx.send(SessionEvent::SyncPlanEnded {
        task_id,
        state: snapshot.state,
    });
}

/// Aplica el plan APROBADO (`sync.apply`) y cosecha su informe (#161, fase C2
/// tarea 4).
///
/// **Es la única función de este fichero que escribe en el disco de alguien.**
/// Lo que viaja es el `plan_hash` y nada más (ADR 0049): el daemon ejecuta
/// exactamente el plan que retuvo en el spool, así que no hay forma de que
/// esto pida algo distinto de lo que el panel enseñó y el humano aprobó.
///
/// Un rechazo de entrada (`PlanStale`, el plan caducado, un daemon caído) sale
/// por [`SessionEvent::SyncApplyFailed`] y no crea Task. Con Task:
///
/// 1. se registra su canceller ANTES de anunciarla, para que no exista un
///    instante en el que la GUI sepa de una Task que escribe y no la pueda
///    parar (regla 3);
/// 2. se espera su desenlace por el watch de progreso — y si los emisores
///    mueren sin publicar uno terminal se sintetiza un `Failed`, igual que
///    [`forward_progress`] y por lo mismo: una sincronización a medias sobre
///    una conexión muerta no es un éxito;
/// 3. se pide `sync.report` **pase lo que pase**, cancelación incluida: lo
///    aplicado hasta el corte se queda journalizado, y media sincronización es
///    un estado real que el lector tiene que poder ver.
///
/// La aplicación NO entra en la franja de tasks de la ventana, igual que el
/// plan: su progreso lo pinta el pie del propio panel, y el panel es también el
/// único sitio desde el que se cancela.
async fn sync_apply(
    backend: &Backend,
    generation: u64,
    plan_hash: &norte_proto::methods::PlanHash,
    tx: &mpsc::UnboundedSender<SessionEvent>,
    cancellers: &Arc<Mutex<HashMap<TaskId, TaskCanceller>>>,
) {
    let task = match backend.sync_apply(plan_hash).await {
        Ok(task) => task,
        Err(error) => {
            let _ = tx.send(SessionEvent::SyncApplyFailed { generation, error });
            return;
        }
    };
    let task_id = task.id();
    let canceller = register_canceller(cancellers, &task);
    let _ = tx.send(SessionEvent::SyncApplyStarted {
        task_id,
        generation,
    });
    let state = wait_terminal(&task).await;
    let report = backend.sync_report(task_id).await;
    // El de-registro va ANTES del `Ended`: quien lo reciba no debe encontrar
    // todavía un canceller de una task terminada.
    drop(canceller);
    let _ = tx.send(SessionEvent::SyncApplyEnded {
        task_id,
        state,
        report: Box::new(report),
    });
}

/// Espera a que `task` publique un estado TERMINAL y lo devuelve.
///
/// Si los emisores del watch mueren sin publicarlo —la conexión se cayó—
/// devuelve `Failed { ProviderUnavailable }` en vez del último no terminal:
/// no se sabe qué pasó, y el desenlace honesto de una escritura interrumpida
/// por una conexión muerta no es «hecho». Mismo criterio y mismo error
/// sintético que [`forward_progress`].
async fn wait_terminal(task: &TaskRef) -> TaskState {
    let mut rx = task.progress();
    loop {
        let snap = rx.borrow_and_update().clone();
        if snap.state.is_terminal() {
            return snap.state;
        }
        if rx.changed().await.is_err() {
            return TaskState::Failed {
                error: Error::ProviderUnavailable { retryable: true },
            };
        }
    }
}

/// Presupuesto de píxeles del preview de imagen (#92, movido del hilo de UI):
/// `into_rgba8` alloca ancho×alto×4 y puede coexistir con el buffer del
/// decoder — pico real ≈ 32 MP × 4 × 2 ≈ 256 MiB (una sola imagen a la vez).
const MAX_IMAGE_PIXELS: u64 = 32_000_000;

/// Frame BGRA crudo decodificado en el hilo de sesión (#92): la UI solo lo
/// ENVUELVE en su tipo de render (O(1)), jamás decodifica.
pub struct DecodedImage {
    /// Píxeles BGRA8 (ancho×alto×4 bytes).
    pub bgra: Vec<u8>,
    /// Ancho en píxeles.
    pub width: u32,
    /// Alto en píxeles.
    pub height: u32,
}

/// Resultado del decode de sesión (#92): espejo transportable de la
/// distinción Ready/Unreadable del preview de la UI.
pub enum ImageDecode {
    /// Decodificada y lista para envolver en el render.
    Ready(DecodedImage),
    /// El decode falló (truncada/corrupta/excede el presupuesto): la UI
    /// pinta el aviso i18n, sin reintentar.
    Unreadable,
}

/// Decodifica bytes de imagen a BGRA8 con guardia anti-bomba (#92): primero
/// SOLO las dimensiones de la cabecera (rechazo por encima de
/// [`MAX_IMAGE_PIXELS`] antes de allocar), luego decode con
/// `image::Limits` (acota las allocaciones internas del codec) → RGBA8 →
/// BGRA in-place. Cualquier fallo → [`ImageDecode::Unreadable`]; jamás panic
/// ni OOM. GIF/WebP animados: primer frame (preview estático).
pub(crate) fn decode_image(bytes: &[u8]) -> ImageDecode {
    let dims = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok());
    let Some((width, height)) = dims else {
        return ImageDecode::Unreadable;
    };
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return ImageDecode::Unreadable;
    }
    let Ok(mut reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()
    else {
        return ImageDecode::Unreadable;
    };
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_IMAGE_PIXELS.saturating_mul(4));
    reader.limits(limits);
    let Ok(decoded) = reader.decode() else {
        return ImageDecode::Unreadable;
    };
    let mut rgba = decoded.into_rgba8();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    // Dimensiones REALES del decode (la cabecera pudo mentir a la baja).
    let (width, height) = rgba.dimensions();
    ImageDecode::Ready(DecodedImage {
        bgra: rgba.into_raw(),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};

    fn snap(id: TaskId, state: TaskState) -> TaskProgress {
        TaskProgress {
            task_id: id,
            kind: TaskKind::Copy,
            state,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
        }
    }

    fn id(n: u64) -> TaskId {
        TaskId::new(n)
    }

    /// [`wait_terminal`] espera al desenlace, y con los emisores muertos
    /// sintetiza un `Failed` en vez de devolver el último NO terminal: la
    /// conexión se fue sin decir qué pasó, y una sincronización a medias no es
    /// un éxito (regla 3 / revisiones rust m3 y de seguridad MINOR-5).
    #[tokio::test]
    async fn wait_terminal_sintetiza_un_fallo_si_la_conexion_muere() {
        let tid = id(21);
        let (watch_tx, watch_rx) = tokio::sync::watch::channel(snap(tid, TaskState::Running));
        let task = TaskRef::synthetic_for_tests(tid, watch_rx);
        let fut = tokio::spawn(async move { wait_terminal(&task).await });
        drop(watch_tx);
        assert!(
            matches!(
                fut.await.expect("join"),
                TaskState::Failed {
                    error: norte_proto::Error::ProviderUnavailable { retryable: true }
                }
            ),
            "sin desenlace publicado, «hecho» sería la mentira mayor"
        );

        // Y con desenlace publicado, ése: una cancelación se dice cancelada.
        let tid = id(22);
        let (watch_tx, watch_rx) = tokio::sync::watch::channel(snap(tid, TaskState::Running));
        let task = TaskRef::synthetic_for_tests(tid, watch_rx);
        let fut = tokio::spawn(async move { wait_terminal(&task).await });
        watch_tx.send_modify(|p| p.state = TaskState::Cancelled);
        assert_eq!(fut.await.expect("join"), TaskState::Cancelled);
        drop(watch_tx);
    }

    /// #190 (regla dura 3): el ciclo de vida del canceller, que `compare`,
    /// `sync_plan` y `sync_apply` comparten y ninguno probaba —sus tests
    /// llegaban por la bomba, y la bomba no ve el registro—. Extraído a
    /// [`register_canceller`], se prueba sin `Backend`: presente en cuanto la
    /// task existe, ido en cuanto el guard se suelta, y CANCELANDO la task de
    /// verdad mientras dura.
    #[test]
    fn el_canceller_vive_exactamente_lo_que_dura_su_guard() {
        let tid = id(11);
        let (_watch_tx, watch_rx) = tokio::sync::watch::channel(snap(tid, TaskState::Running));
        let task = TaskRef::synthetic_for_tests(tid, watch_rx);
        let cancellers: Arc<Mutex<HashMap<TaskId, TaskCanceller>>> = Arc::default();

        assert!(cancellers.lock().unwrap().is_empty());
        let guard = register_canceller(&cancellers, &task);
        assert!(
            cancellers.lock().unwrap().contains_key(&tid),
            "cancelable en el instante en que la vista recibe el task_id"
        );
        // Y lo registrado para de verdad: es el canceller de ESTA task.
        let TaskCanceller::Embedded(token) = task.canceller() else {
            panic!("un TaskRef sintético cancela con un token embebido");
        };
        assert!(!token.is_cancelled());
        cancellers
            .lock()
            .unwrap()
            .get(&tid)
            .expect("registrado")
            .cancel();
        assert!(
            token.is_cancelled(),
            "el Cancel de la sesión llega a la task"
        );

        drop(guard);
        assert!(
            cancellers.lock().unwrap().is_empty(),
            "y se retira al soltarse, salga la función por donde salga"
        );
    }

    /// El de-registro es ESTRUCTURAL: un camino que se va por un `return`
    /// temprano —o un panic capturado— no puede dejar el canceller de una
    /// task muerta en el mapa que decide si `Cancel` encuentra algo.
    #[test]
    fn una_salida_temprana_tambien_desregistra() {
        let tid = id(12);
        let (_watch_tx, watch_rx) = tokio::sync::watch::channel(snap(tid, TaskState::Running));
        let task = TaskRef::synthetic_for_tests(tid, watch_rx);
        let cancellers: Arc<Mutex<HashMap<TaskId, TaskCanceller>>> = Arc::default();

        fn se_va_pronto(
            cancellers: &Arc<Mutex<HashMap<TaskId, TaskCanceller>>>,
            task: &TaskRef,
        ) -> bool {
            let _canceller = register_canceller(cancellers, task);
            true
        }

        assert!(se_va_pronto(&cancellers, &task));
        assert!(cancellers.lock().unwrap().is_empty());
    }

    /// #85: la conexión muere SIN desenlace (el watch se cierra) —
    /// `forward_progress` sintetiza un terminal `Failed{ProviderUnavailable}`
    /// reusando id/kind del último snapshot y DE-REGISTRA el canceller.
    #[tokio::test]
    async fn forward_progress_sintetiza_terminal_en_muerte_de_conexion() {
        let tid = id(7);
        let (watch_tx, watch_rx) = tokio::sync::watch::channel(snap(tid, TaskState::Running));
        let task = TaskRef::synthetic_for_tests(tid, watch_rx);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let cancellers: Arc<Mutex<HashMap<TaskId, TaskCanceller>>> = Arc::default();
        let guard = register_canceller(&cancellers, &task);
        assert!(
            cancellers.lock().unwrap().contains_key(&tid),
            "registrado ANTES de arrancar la bomba"
        );

        let fut = {
            let ev_tx = ev_tx.clone();
            tokio::spawn(async move { forward_progress(task, &ev_tx, guard).await })
        };
        // Muerte de la conexión: el emisor del watch desaparece.
        drop(watch_tx);
        fut.await.expect("join");

        // Primer evento: el snapshot vivo tal cual.
        let SessionEvent::Task(first) = ev_rx.try_recv().expect("snapshot inicial") else {
            panic!("esperaba SessionEvent::Task");
        };
        assert_eq!(first.state, TaskState::Running);
        // Segundo: el terminal SINTETIZADO (mismos id/kind, Failed honesto).
        let SessionEvent::Task(dead) = ev_rx.try_recv().expect("terminal sintetizado") else {
            panic!("esperaba SessionEvent::Task");
        };
        assert_eq!(dead.task_id, tid);
        assert_eq!(dead.kind, TaskKind::Copy);
        assert!(
            matches!(
                dead.state,
                TaskState::Failed {
                    error: norte_proto::Error::ProviderUnavailable { retryable: true }
                }
            ),
            "fue {:?}",
            dead.state
        );
        assert!(
            cancellers.lock().unwrap().is_empty(),
            "el canceller se de-registra también en el camino de muerte"
        );
    }

    /// Una fila cualquiera: lo que se prueba aquí es el bombeo, no el
    /// veredicto.
    fn fila(id: u64) -> norte_proto::methods::CompareRow {
        use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareVerdict};
        norte_proto::methods::CompareRow {
            id,
            left: None,
            right: None,
            verdict: CompareVerdict::Error,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Unknown,
            newer: None,
            reason: Some(norte_proto::methods::CompareReason::Unreadable),
            side: None,
            paired_under: None,
        }
    }

    /// Un progreso de comparación con `entries_done` puesto.
    fn snap_filas(id: TaskId, state: TaskState, entries_done: u64) -> TaskProgress {
        let mut p = snap(id, state);
        p.kind = TaskKind::Compare;
        p.entries_done = entries_done;
        p
    }

    /// #158: cada lote sale etiquetado con la Task del bombeo, y al cerrarse
    /// el canal sale UN `CompareDone` con el snapshot de ese instante.
    #[tokio::test]
    async fn pump_compare_reenvia_los_lotes_y_cierra_con_el_snapshot() {
        let tid = id(11);
        let (watch_tx, watch_rx) =
            tokio::sync::watch::channel(snap_filas(tid, TaskState::Running, 0));
        let task = TaskRef::synthetic_for_tests(tid, watch_rx);
        let (rows_tx, rows_rx) = mpsc::channel(4);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();

        rows_tx
            .send(norte_proto::methods::CompareRowsBatch {
                task_id: tid,
                rows: vec![fila(1), fila(2)],
            })
            .await
            .expect("lote");
        watch_tx
            .send(snap_filas(tid, TaskState::Completed, 2))
            .expect("terminal");
        drop(rows_tx); // el walk terminó: se cierra el canal de filas.
        pump_compare(task, rows_rx, &ev_tx).await;

        let SessionEvent::CompareRows { task_id, rows } = ev_rx.try_recv().expect("el lote") else {
            panic!("esperaba CompareRows");
        };
        assert_eq!(task_id, tid, "el lote lleva la Task del bombeo");
        assert_eq!(rows.len(), 2);
        let SessionEvent::CompareDone {
            task_id,
            state,
            entries_done,
        } = ev_rx.try_recv().expect("el final")
        else {
            panic!("esperaba CompareDone");
        };
        assert_eq!(task_id, tid);
        assert_eq!(state, TaskState::Completed);
        assert_eq!(entries_done, 2);
        assert!(ev_rx.try_recv().is_err(), "UN final, no dos");
    }

    /// **La carrera benigna, vista desde el hilo de sesión.** El canal de
    /// filas se cierra ANTES de que el estado terminal se publique, y el
    /// bombeo NO espera: manda el estado que hay —no terminal— tal cual, en
    /// vez de quedarse colgado (hay finales que no llegan nunca: un daemon
    /// caído, un provider colgado en una NFS muerta) o de inventarse un
    /// `Completed` que convertiría la carrera en una acusación de pérdida.
    /// Quien la interpreta es `compare_view::CompareView::on_done`.
    #[tokio::test]
    async fn pump_compare_no_espera_al_terminal_y_manda_lo_que_hay() {
        let tid = id(12);
        let (_watch_tx, watch_rx) =
            tokio::sync::watch::channel(snap_filas(tid, TaskState::Running, 9));
        let task = TaskRef::synthetic_for_tests(tid, watch_rx);
        let (rows_tx, rows_rx) = mpsc::channel(4);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();

        drop(rows_tx);
        pump_compare(task, rows_rx, &ev_tx).await;

        let SessionEvent::CompareDone {
            state,
            entries_done,
            ..
        } = ev_rx.try_recv().expect("el final")
        else {
            panic!("esperaba CompareDone");
        };
        assert_eq!(
            state,
            TaskState::Running,
            "el estado viaja tal cual: interpretarlo no es de esta capa"
        );
        assert_eq!(entries_done, 9);
    }

    /// #85: un terminal REAL publicado por el watch llega como evento y
    /// de-registra el canceller, sin terminal sintetizado de más.
    #[tokio::test]
    async fn forward_progress_terminal_real_desregistra_sin_extra() {
        let tid = id(9);
        let (watch_tx, watch_rx) = tokio::sync::watch::channel(snap(tid, TaskState::Running));
        let task = TaskRef::synthetic_for_tests(tid, watch_rx);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let cancellers: Arc<Mutex<HashMap<TaskId, TaskCanceller>>> = Arc::default();
        let guard = register_canceller(&cancellers, &task);
        assert!(
            cancellers.lock().unwrap().contains_key(&tid),
            "registrado ANTES de arrancar la bomba"
        );

        let fut = {
            let ev_tx = ev_tx.clone();
            tokio::spawn(async move { forward_progress(task, &ev_tx, guard).await })
        };
        watch_tx
            .send(snap(tid, TaskState::Completed))
            .expect("terminal");
        fut.await.expect("join");

        let mut states = Vec::new();
        while let Ok(SessionEvent::Task(p)) = ev_rx.try_recv() {
            states.push(p.state);
        }
        assert_eq!(
            states.last(),
            Some(&TaskState::Completed),
            "el último evento es el terminal real: {states:?}"
        );
        assert!(
            !states.iter().any(|s| matches!(s, TaskState::Failed { .. })),
            "sin terminal sintetizado de más: {states:?}"
        );
        assert!(cancellers.lock().unwrap().is_empty());
    }

    /// #161: un plan CANCELADO cierra su canal sin haber mandado nunca
    /// `sync.plan_done`, y el bombeo lo cuenta tal cual — un
    /// `SyncPlanEnded` con el desenlace, y NINGÚN `SyncPlanDone`. Es la
    /// distinción entera de esta fase: sin la notificación no hay
    /// `plan_hash`, así que no hay plan que aprobar por mucho que el canal
    /// se haya acabado.
    #[tokio::test]
    async fn pump_sync_plan_cierra_el_canal_sin_cerrar_el_plan() {
        let tid = id(21);
        let (watch_tx, watch_rx) = tokio::sync::watch::channel(snap(tid, TaskState::Running));
        let task = TaskRef::synthetic_for_tests(tid, watch_rx);
        let (ev_in_tx, ev_in_rx) = mpsc::channel(4);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();

        ev_in_tx
            .send(norte_core::sync::SyncPlanEvent::Steps(
                norte_proto::methods::SyncStepsBatch {
                    task_id: tid,
                    steps: vec![],
                },
            ))
            .await
            .expect("lote");
        watch_tx
            .send(snap(tid, TaskState::Cancelled))
            .expect("terminal");
        drop(ev_in_tx); // el planificador se paró: se cierra el canal.
        pump_sync_plan(task, ev_in_rx, &ev_tx).await;

        let SessionEvent::SyncSteps { batch } = ev_rx.try_recv().expect("el lote") else {
            panic!("esperaba SyncSteps");
        };
        assert_eq!(batch.task_id, tid, "el lote llega tal cual, con su task_id");
        let SessionEvent::SyncPlanEnded { task_id, state } = ev_rx.try_recv().expect("el final")
        else {
            panic!("esperaba SyncPlanEnded");
        };
        assert_eq!(task_id, tid);
        assert_eq!(state, TaskState::Cancelled);
        assert!(
            ev_rx.try_recv().is_err(),
            "UN final, y ningún SyncPlanDone que nadie mandó"
        );
    }
}
