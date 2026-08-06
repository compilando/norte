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
    CollisionPolicy, DeleteMode, Entry, EntryKind, ResumePolicy, SymlinkPolicy, TaskId, VPath,
    VerifyPolicy,
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
///
/// 0.16.0 (#71): `policy.undo_report` — informe de una Task de undo (qué se
/// deshizo, qué se saltó y por qué, dónde se bloqueó el LIFO): el humano que
/// deshace deja de recibir un «done» a ciegas. Aditivo sobre 0.15.x.
///
/// 0.17.0 (#58): `Error::Corrupt` — contenedor/formato roto o fuera de los
/// límites anti-bomba (antes se forzaba a `Io{retryable:false}`, ADR 0018
/// D2): UX honesta («no es un zip válido») y telemetría. Aditivo sobre
/// 0.16.x (un cliente N-1 degrada el kind a `Unknown`).
///
/// 0.18.0 (M4 live search): `fs.search` + `search.hits` + `TaskKind::Search`
/// — búsqueda viva por nombre (glob/regex) y contenido (literal/regex
/// multi-encoding) bajo un subtree, streaming como Task cancelable. Aditivo
/// sobre 0.17.x (un cliente N-1 no conoce `fs.search`/`search.hits` y ve
/// `TaskKind::Search` como `Unknown`, como el resto de kinds nuevos).
///
/// 0.19.0 (#72): notificación `rpc.cancel { id }` (client→server) — retira la
/// request en vuelo suspendida en un Ask de policy. Aditiva sobre 0.18.x (un
/// cliente/daemon N-1 la ignora; degrada al Ask zombi hasta TTL, no rompe).
///
/// 0.20.0 (#44): notificación `connection.degraded` (server→client) — una sesión
/// remota se estableció con seguridad degradada (FTP `tls="allow"` → plano).
/// Aditiva sobre 0.19.x (un cliente N-1 la ignora; solo-log, no rompe).
///
/// 0.21.0 (#55, ADR 0028): `tar+gz` en `ARCHIVE_FORMATS`, resolución
/// longest-match, helper `scheme_archive_format`. Aditivo sobre 0.20.x — un
/// peer 0.20 ve `tar+gz+…` como Unsupported, sin corrupción.
///
/// 0.22.0 (#93): campo opcional `skipped` en [`FsListResult`] — total de
/// entradas del CONTENEDOR omitidas del índice (nombres hostiles/límites,
/// providers archive). Aditivo sobre 0.21.x: ausente cuando no aplica (un
/// cliente N-1 lo ignora como campo desconocido; sin él degrada a lo de
/// antes, el contador solo vivía en logs).
///
/// 0.23.0 (#95): variante [`Error::LimitExceeded`](crate::Error::LimitExceeded)
/// `{limit}` — exceder un tope anti-bomba LOCAL deja de disfrazarse de
/// `Corrupt` (mentira para un contenedor legítimo enorme). Aditiva sobre
/// 0.22.x: un cliente N-1 la degrada a `Error::Unknown` (error genérico,
/// misma UX gruesa que antes).
///
/// 0.24.0 (#56, ADR 0018 A3): direccionamiento de archivo MULTI-CAPA —
/// `zip+tar+file:///b.tar/!/i.zip/!/f` (resolución derecha→izquierda: el
/// formato más a la izquierda es la capa más externa y corta en su ÚLTIMO
/// marcador; interior plano conserva la regla v1 del primero).
/// `archive_compose` acepta un exterior que sea a su vez un path de archivo
/// bien formado; nuevo `Error::LIMIT_NESTING` en el vocabulario de
/// `LimitExceeded` (tope de capas, lo gobierna el engine). Aditivo sobre
/// 0.23.x: un peer N-1 rechaza los paths anidados limpio (su
/// `archive_split` daba `InvalidScheme` ante un interior compuesto →
/// `InvalidPath` en el wire) — direccionamiento nuevo, jamás resignifica
/// uno viejo: los paths de UNA capa se resuelven idéntico.
///
/// 0.25.0 (M4, ADR 0034): búsqueda INDEXADA. Métodos `index.build` (una Task,
/// [`IndexBuildParams`]→[`IndexBuildResult`]) e `index.query`
/// ([`IndexQueryParams`]→[`IndexQueryResult`] con [`IndexHit`]), más
/// [`TaskKind::Index`](crate::TaskKind::Index). Aditivo sobre 0.24.x: un peer
/// N-1 no conoce los métodos (los rechaza con `MethodNotFound`) y degrada
/// `TaskKind::Index` a `Unknown` vía `serde(other)` — jamás resignifica nada
/// viejo. El índice ausente (daemon sin `norte-index`) responde `Unsupported`.
///
/// 0.26.0 (P1): [`PluginInfo`] gana `description` (`Option<String>`, cosmético,
/// ausente = `None`) y `commands` (`Vec<`[`PluginCommandInfo`]`>`, catálogo de
/// comandos invocables vía [`PLUGIN_RUN_COMMAND`]). Aditivo sobre 0.25.x: un
/// peer N-1 ignora ambos campos desconocidos al deserializar; un peer N-1 que
/// construye su propio `PluginInfo` sencillamente no los emite y este core
/// los toma por su default (`None`/`vec![]`) — la ventana pasa a
/// N=0.26.x/N-1=0.25.x.
///
/// 0.27.0 (G3, ADR 0037): tres métodos nuevos de datos ESTRUCTURADOS de
/// plugin, que el HOST pinta (nunca el plugin, que jamás recibe capacidad de
/// pintar directamente):
/// - [`PLUGIN_PREVIEW_STYLED`] — gemelo con estilo de [`PLUGIN_PREVIEW`]:
///   [`PluginPreviewStyledResult`] envuelve (`flatten`, mismo patrón
///   all-or-nothing) un [`PluginPreviewStyled`] con `lines: Vec<Vec<`[`SpanWire`]`>>`.
/// - [`PLUGIN_DECORATE`] — decoraciones tipo git-status por entrada
///   ([`PluginDecorateResult`], POSICIONAL 1:1 con `params.paths`).
/// - [`PLUGIN_COLUMN_VALUES`] — valores de una columna aportada por un plugin
///   ([`PluginColumnValuesResult`], también posicional 1:1).
///
/// Los tres son métodos NUEVOS (no flags sobre los existentes). La ventana
/// N/N-1 de [`version_compatible`] es MÁS ESTRECHA que "método desconocido":
/// un cliente 0.27 NUNCA llega a llamarlos contra un daemon 0.26, porque
/// `initialize` ya rechaza ese handshake con `VERSION_MISMATCH` (un cliente
/// del futuro no negocia, ver el doctest de [`version_compatible`]) — el
/// cliente jamás ve `MethodNotFound` en ESE escenario, ve el fallo de
/// versión antes de intentar nada. `MethodNotFound` (taxonomía de métodos
/// desconocidos, ADR 0004) SÍ aplica dentro de la MISMA ventana 0.27: un
/// daemon 0.27 que aún no tiene el handler cableado (este bump es solo de
/// wire; T3/T4 lo cablean) responde `MethodNotFound` a un cliente 0.27, que
/// cae a la superficie plana existente ([`PLUGIN_PREVIEW`], sin
/// decoraciones, sin columnas) — igual que cualquier cliente que decide NO
/// llamarlos tras inspeccionar `InitializeResult::protocol_version` y
/// preferir no arriesgarse. Topes del wire
/// (server ENFORCE, cliente re-valida fail-closed a lo plano si se violan):
/// ≤10 000 líneas, ≤64 spans/línea, texto de span ≤4 KiB, payload total
/// ≤4 MiB (mismo tope de retorno del runtime, ya usado por
/// [`PLUGIN_RUN_COMMAND`]/[`PLUGIN_PREVIEW`]), badge ≤8 chars TRAS
/// enmascarar. `role: Option<String>` en [`SpanWire`]/[`DecorationWire`] se
/// valida HOST-SIDE contra el conjunto cerrado `norte_theme::Role`: un
/// nombre desconocido degrada a `None` + warning, jamás a error duro (mismo
/// trato indulgente que ADR 0020 da a un tema mal escrito). Aditivo sobre
/// 0.26.x — la ventana pasa a N=0.27.x/N-1=0.26.x.
///
/// 0.28.0 (G3c, ADR 0037): cierra las DOS deudas acumuladas de G3.
/// - [`PluginInfo`] gana `columns` (`Vec<`[`PluginColumnInfo`]`>`, aditivo —
///   default `vec![]`, mismo criterio que `commands` en 0.26.0): la UI de
///   columnas del gestor de extensiones ahora puede DESCUBRIR qué columnas
///   contribuye cada plugin sin adivinar por `category == "columns"`.
/// - Dos métodos nuevos que exponen `[config]` de P2 POR EL WIRE (P2 lo
///   dejó host-only a propósito, diferido a este bump — ver el rustdoc de
///   `norte_core::PluginRegistry::settings_of`): [`PLUGIN_GET_CONFIG`]
///   (esquema + valor efectivo, `keys: Vec<`[`PluginConfigKeyWire`]`>`) y
///   [`PLUGIN_SET_CONFIG`] (persiste UN valor tras validarlo contra el
///   MISMO esquema — jamás una ruta de validación paralela; SOLO humano,
///   mismo criterio que [`PLUGIN_SET_APPROVAL`]). `min`/`max` son
///   `Option<i64>` (`skip_serializing_if` cuando ausentes), `values` es
///   `Vec<String>` (vacío para tipos no-enum, SIEMPRE presente — mismo
///   criterio "aditivo siempre presente" que `commands`), `description` es
///   texto del PLUGIN — NO confiable (mismo trato que `PluginCommandInfo::title`).
///   Aditivo sobre 0.27.x — la ventana pasa a N=0.28.x/N-1=0.27.x.
///
/// 0.29.0 (#101): [`PluginPreview`] y [`PluginPreviewStyled`] ganan `lossy:
/// bool` — el core marca cuándo la decodificación de texto host-side (§6.2,
/// #29) fue LOSSY (`had_errors`), para que el modo preview señale los `�` de
/// decodificación igual que ya hace el viewer crudo. Aditivo sobre 0.28.x
/// (`#[serde(default)]` = un par N-1 se lee como `false`) — la ventana pasa a
/// N=0.29.x/N-1=0.28.x.
///
/// 0.30.0 (columnas bloque 1, ADR 0039): ATRIBUTOS DE PROVIDER, tipados y bajo
/// demanda — CUATRO campos aditivos repartidos en TRES superficies (catálogo,
/// petición ×2, entrada) más el vocabulario del módulo
/// [`attrs`](crate::attrs) ([`AttrType`](crate::AttrType),
/// [`AttrHint`](crate::AttrHint), [`AttrInfo`](crate::AttrInfo),
/// [`AttrValue`](crate::AttrValue)). [`FsCapabilitiesResult`] gana
/// `attrs: AttrCatalog` (discovery: qué publica ESE provider, con tipo y
/// pista de presentación; en el wire, el array de `AttrInfo` de siempre);
/// [`FsListParams`] y [`FsStatParams`] ganan
/// `attrs: Vec<String>` (el cliente pide SOLO los ids que va a pintar, nada se
/// entrega sin pedirlo); [`Entry`] gana
/// `attrs: BTreeMap<String, AttrValue>`. Los cuatro llevan
/// `skip_serializing_if` sobre el vacío, así que un peer 0.29 emite y recibe
/// payloads idénticos BYTE A BYTE a los de antes — aditivo en el sentido
/// fuerte, no solo en el de «campo desconocido que se ignora». Las
/// superficies de PETICIÓN son `fs.list` y `fs.stat` y solo esas: ni las
/// entradas de [`SEARCH_HITS`] ni los hits de [`INDEX_QUERY`] llevan atributos
/// en 0.30.
///
/// `AttrValue` deserializa A MANO (misma ruta que
/// [`CapabilityFlags`](crate::CapabilityFlags): `#[serde(other)]` no existe
/// para una variante CON datos) y degrada a `AttrValue::Unknown` TODO valor
/// malformado — etiqueta desconocida (un peer del futuro, ADR 0004 aplicado a
/// granularidad de celda), objeto AMBIGUO con dos o más etiquetas conocidas
/// (las claves de un objeto JSON no están ordenadas, RFC 8259 §4, así que
/// «gana la primera» dependería del capricho de un relay), payload del tipo
/// JSON equivocado, `null`, no-objeto, base64 indecodificable, y texto o bytes
/// por encima del tope. Ninguno es error duro: cuesta UNA celda, jamás la
/// entrada ni la página.
///
/// Los dos campos de RECEPCIÓN filtran al decodificar y tampoco erran nunca.
/// `Entry.attrs`: id malformado DESCARTADO, mapa acotado en
/// [`ATTRS_MAX_REQUEST`](crate::ATTRS_MAX_REQUEST) quedándose con los ids más
/// pequeños en orden de bytes, clave repetida last-wins.
/// `FsCapabilitiesResult.attrs`: id malformado descartado, id REPETIDO
/// first-wins (es una lista ORDENADA que el provider ranquea, al revés que un
/// objeto JSON sin orden), label largo RECORTADO a
/// [`ATTR_LABEL_MAX`](crate::ATTR_LABEL_MAX) en frontera de char (el id es lo
/// que un cliente acciona: perder el atributo por lo cosmético sería el
/// cambio malo), catálogo acotado en
/// [`ATTRS_MAX_ADVERTISED`](crate::ATTRS_MAX_ADVERTISED) conservando los
/// PRIMEROS del wire, y examen acotado en
/// [`ATTRS_MAX_CATALOG_SCAN`](crate::ATTRS_MAX_CATALOG_SCAN) (el resto se
/// drena sin materializar). Además el catálogo es un tipo, no una llamada:
/// [`AttrCatalog`](crate::AttrCatalog) tiene el vector privado y un único
/// constructor que sanea, así que el camino EMBEBIDO (TUI/CLI por defecto, que
/// no cruza la deserialización) queda cubierto igual que el del wire.
///
/// Los dos campos de PETICIÓN, en cambio, NO filtran a propósito: llevan datos
/// que este peer ENVÍA, así que un id malformado sobrevive al decode y es el
/// `-32602` del daemon — que cablea el bloque 2 — en vez de blanquearse a «no
/// pidió nada», lo que escondería el bug del llamante y haría intestable esa
/// validación. Con una excepción que NO es validación sino cota de MEMORIA: se
/// conservan los primeros [`ATTRS_MAX_REQUEST`](crate::ATTRS_MAX_REQUEST) `+ 1`
/// elementos y el resto se drena sin materializar, porque un frame de 16 MiB de
/// ids diminutos reservaría ~15× su tamaño en cabeceras de `String` antes de
/// que ningún chequeo del daemon pueda correr. Así que un id malformado
/// sobrevive al decode SIEMPRE, pero a partir del elemento 17 el id ya no
/// llega: lo que sobrevive es el TESTIGO de que se pasó (`attrs.len() >
/// ATTRS_MAX_REQUEST`), que es lo que el daemon necesita para rechazar en vez
/// de recortar la violación hasta hacerla legal.
///
/// Este bump es SOLO de wire: ningún provider anuncia atributos todavía y el
/// daemon ignora los ids pedidos, lo cual es honesto porque la AUSENCIA ya es
/// una respuesta válida del contrato (pedir un id que el provider no ofrece
/// nunca fue error: vuelve ausente). La ventana pasa a N=0.30.x/N-1=0.29.x, y
/// la dirección que tiene que sostenerse es un cliente 0.29 contra un daemon
/// 0.30: no envía `attrs`, no recibe `attrs`, nada cambia para él. La inversa
/// no es una pregunta sobre atributos — [`version_compatible`] rechaza de
/// plano a un cliente del FUTURO con `VERSION_MISMATCH`, antes de mirar campo
/// alguno.
/// 0.31.0 (#104): método nuevo `fs.mkdir` (aditivo — [`FsMkdirParams`] →
/// [`FsTaskResult`], la misma forma de Task que copy/move/delete) y variante
/// `TaskKind::Mkdir`. Ventana N=0.31.x / N-1=0.30.x: un cliente 0.30 jamás
/// llama al método nuevo y degrada el kind nuevo a `TaskKind::Unknown` por su
/// `serde(other)` (presente desde 0.10) — nada que gatear en emisión.
/// 0.32.0 (M4-IA, ADR 0031): método nuevo `ai.rename_plan` (aditivo —
/// [`AiRenamePlanParams`] → [`AiRenamePlanResult`], respuesta directa
/// cancelable con `rpc.cancel`). Ventana N=0.32.x / N-1=0.31.x: un cliente
/// 0.31 jamás llama al método nuevo — nada que gatear en emisión.
/// 0.33.0 (M4-IA-2, ADR 0031 A3): métodos nuevos `index.embed` (Task de
/// embeddings — [`IndexEmbedParams`] → [`FsTaskResult`]) y
/// `index.search_semantic` (request directa cancelable con `rpc.cancel` —
/// [`IndexSearchSemanticParams`] → [`IndexSearchSemanticResult`]) más la
/// variante `TaskKind::Embed`. Ventana N=0.33.x / N-1=0.32.x: un cliente
/// 0.32 jamás llama a los métodos nuevos y degrada el kind nuevo a
/// `TaskKind::Unknown` por su `serde(other)` — nada que gatear en emisión.
/// 0.34.0 (H3e): la ayuda de los PLUGINS por el wire. [`PluginInfo`] gana
/// `has_help: bool` (discovery barato, `skip_serializing_if` sobre `false` —
/// un plugin sin ayuda produce el MISMO payload que en 0.33) y aparece el
/// método [`PLUGIN_HELP`] ([`PluginHelpParams`] → [`PluginHelpResult`]), que
/// entrega el `help.md` ya acotado y decodificado más las banderas
/// `truncated`/`lossy` que el receptor no puede deducir. Ventana
/// N=0.34.x / N-1=0.33.x, y la dirección que tiene que sostenerse es un
/// cliente 0.33 contra un daemon 0.34: ignora el campo desconocido, no emite
/// `has_help` (default `false` aquí) y jamás llama al método nuevo — nada que
/// gatear en emisión. La inversa NO es una pregunta sobre ayuda:
/// [`version_compatible`] rechaza de plano a un cliente del FUTURO con
/// `VERSION_MISMATCH` en `initialize`, antes de despachar método alguno (el
/// mismo razonamiento que el bump 0.30.0 deja escrito arriba).
///
/// Este bump es SOLO de wire, y a diferencia del de 0.30.0 la ausencia NO es
/// aquí una respuesta válida: mientras el daemon no cablee el handler
/// (H3e T4), `plugin.help` responde `MethodNotFound` a un peer de su MISMA
/// versión, y `plugin.list` emite `has_help: false` fijo (H3e T3). Ambas son
/// lagunas de esta ventana, no el contrato: quien llame durante ella debe
/// tratar el `MethodNotFound` como «este plugin no tiene página», nunca como
/// un fallo.
pub const PROTOCOL_VERSION: &str = "0.34.0";

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
/// `fs.mkdir` — creación de UN directorio como Task (#104, F7). NO es
/// `mkdir -p`: el padre debe existir (`NotFound` si no), y un nodo previo en
/// el destino es `Conflict` — crear es una afirmación sobre un nombre LIBRE.
/// El `ConflictKind` es el del provider del DESTINO: `Exists` para un nodo
/// byte-exacto (dir previo incluido — sin idempotencia silenciosa),
/// `CaseCollision`/`Normalization` si el filesystem destino colapsa el
/// nombre con uno existente (pitfall macOS/Windows: se evalúa contra el
/// destino, no contra el origen). Sin `on_collision`: crear no ofrece
/// políticas de choque. Journal `Created` con undo (regla 4); gateado por
/// `PolicyOp::Mkdir`.
pub const FS_MKDIR: &str = "fs.mkdir";
/// `fs.search` — búsqueda viva bajo un subtree (spec §17.1a): nombre por
/// glob O regex, contenido por literal O regex. Devuelve una Task
/// (`TaskKind::Search`); los hits llegan por la notificación
/// [`SEARCH_HITS`] SOLO a la conexión que la lanzó. Cancelable con
/// `task.cancel`. Al menos un criterio; glob y regex EXCLUYENTES por eje.
///
/// Result = el [`FsTaskResult`] EXISTENTE (`{task_id}`), como
/// `fs.copy`/`fs.move`/`fs.delete` — cero struct nuevo para el result.
///
/// Mapeo de [`TaskProgress`](crate::TaskProgress) durante la búsqueda (lo
/// implementa `norte_core::search::run_walk`):
/// - `entries_done` = entradas ESCANEADAS por el walker (incluidas las
///   saltadas por un `list`/`read` con error), NO los hits.
/// - `bytes_done` = nº de HITS acumulados: una búsqueda no mueve bytes, así
///   que el campo se reutiliza como contador de resultados (el frontend lo
///   pinta como "N hits").
/// - `entries_total`/`bytes_total` = `None` (un walk no conoce su tamaño de
///   antemano); `current` = última entrada vista.
///
/// `max_hits` alcanzado ⇒ la Task termina `Completed` (jamás `Failed`); el
/// cliente infiere "truncada" comparando el total de hits recibidos con
/// `max_hits`.
pub const FS_SEARCH: &str = "fs.search";
/// Construye/actualiza el índice de búsqueda de un subárbol (M4, ADR 0034). Es
/// una Task (progreso + cancelación, como [`FS_COPY`]); su resultado al completar
/// es [`IndexBuildResult`].
pub const INDEX_BUILD: &str = "index.build";
/// Consulta el índice de un root por texto (M4, ADR 0034). Respuesta DIRECTA (no
/// Task): [`IndexQueryResult`].
pub const INDEX_QUERY: &str = "index.query";
/// `index.embed` — Task ([`TaskKind::Embed`](crate::TaskKind), 0.33.0): genera
/// embeddings de los ficheros ya indexados de un root vía el proveedor de IA
/// configurado (`[ai] embed_provider`). Requiere `index.build` previo
/// (`NotFound` si el root no tiene filas). Gate de IA completo; SOLO conexión
/// humana (una de agente recibe `PolicyDenied`): prefijos de contenido salen
/// del proceso.
pub const INDEX_EMBED: &str = "index.embed";
/// `index.search_semantic` — request directa (0.33.0), cancelable con
/// `rpc.cancel`: un embed de la query + barrido coseno en el core. `k` se
/// recorta a [`INDEX_SEMANTIC_MAX_K`]. SOLO conexión humana, como
/// [`INDEX_EMBED`] — la query sale hacia el proveedor.
/// La query está topada server-side (4 KiB) — exceso ⇒ `INVALID_PARAMS`.
pub const INDEX_SEARCH_SEMANTIC: &str = "index.search_semantic";
/// Tope de `k` en [`INDEX_SEARCH_SEMANTIC`]. Pedir más no es error: se
/// recorta (mismo patrón que [`FS_LIST_MAX_PAGE`]).
pub const INDEX_SEMANTIC_MAX_K: u32 = 100;
/// Sugiere un plan de rename REVISABLE para `dir` (M4-IA, ADR 0031). Respuesta
/// DIRECTA (no Task) pero CANCELABLE con `rpc.cancel` (#72): la llamada al
/// proveedor de IA puede tardar segundos. NO muta nada — aplicar el plan son N
/// [`FS_MOVE`] ordinarios (journal + undo + policy).
/// [`AiRenamePlanParams`] → [`AiRenamePlanResult`].
pub const AI_RENAME_PLAN: &str = "ai.rename_plan";
/// `search.hits` — notificación server→client con un LOTE de resultados de
/// [`FS_SEARCH`]. SOLO viaja a la conexión que lanzó la búsqueda (jamás
/// broadcast, mismo criterio direccional que
/// [`POLICY_APPROVAL_REQUIRED`]).
pub const SEARCH_HITS: &str = "search.hits";
/// Tope de entries por notificación [`SEARCH_HITS`] (coalescing
/// server-side, mismo espíritu que [`FS_LIST_MAX_PAGE`]).
pub const SEARCH_HITS_MAX_BATCH: usize = 256;
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
/// `connection.degraded` — notificación server→client (#44): una sesión remota
/// se estableció con seguridad DEGRADADA (hoy: FTP con `tls="allow"` cayó a
/// texto plano porque el servidor rechazó `AUTH TLS`). Solo informa (el usuario
/// debe SABER que la sesión es en claro, ADR 0015 F "nunca silencioso"); no pide
/// decisión. Se difunde solo a conexiones humanas. Un cliente N-1 la ignora
/// (notif desconocida, ADR 0004) — degrada al comportamiento previo (solo log).
pub const CONNECTION_DEGRADED: &str = "connection.degraded";
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
/// `policy.undo_report` — informe de una Task de undo (0.16.0, #71): los
/// contadores de [`POLICY_UNDO_SESSION`] (revertidas, saltadas y por qué) y el
/// primer bloqueo del LIFO si lo hubo. Es un SNAPSHOT: definitivo cuando la
/// Task es terminal ([`TASK_PROGRESS`]/[`TASK_LIST`]); antes, parcial. El
/// server retiene los informes de las últimas Tasks de undo (anillo acotado,
/// mejor esfuerzo): un `task_id` desconocido o expulsado es `INVALID_PARAMS`.
/// SOLO conexiones User (misma barrera que el undo que lo genera).
pub const POLICY_UNDO_REPORT: &str = "policy.undo_report";
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
/// `plugin.preview_styled` — gemelo CON ESTILO de [`PLUGIN_PREVIEW`] (0.27.0,
/// G3, ADR 0037): el mismo previewer devuelve líneas de spans con `role`/`fg`
/// en vez de un string plano, para que el HOST pinte resaltado real (nunca el
/// plugin). Mismo patrón all-or-nothing que [`PLUGIN_PREVIEW`]. Un daemon 0.26
/// NUNCA lo ve: un cliente 0.27 no completa el handshake contra él
/// (`VERSION_MISMATCH` en `initialize`, ver [`version_compatible`]).
/// `MethodNotFound` es la respuesta dentro de la MISMA ventana 0.27 (un
/// daemon 0.27 sin el handler aún cableado, T3/T4); el cliente cae a
/// [`PLUGIN_PREVIEW`] igual en ambos casos.
pub const PLUGIN_PREVIEW_STYLED: &str = "plugin.preview_styled";
/// `plugin.decorate` — decoraciones tipo git-status por entrada, aportadas
/// por plugins `decorator` APROBADOS y ACTIVADOS (0.27.0, G3, ADR 0037):
/// batched sobre una página visible, POSICIONAL 1:1 con `params.paths`
/// (ver [`PluginDecorateResult`]). Un daemon 0.26 NUNCA lo ve (mismo
/// `VERSION_MISMATCH` de handshake que [`PLUGIN_PREVIEW_STYLED`]);
/// `MethodNotFound` de un daemon 0.27 sin handler aún cableado, o la
/// decisión del cliente de no llamarlo, degradan igual a listar sin
/// decoraciones.
pub const PLUGIN_DECORATE: &str = "plugin.decorate";
/// `plugin.column_values` — valores de una columna aportada por un plugin
/// `columns` APROBADO y ACTIVADO (0.27.0, G3, ADR 0037), POSICIONAL 1:1 con
/// `params.paths` (ver [`PluginColumnValuesResult`]). Misma historia de
/// fallback que [`PLUGIN_DECORATE`]: un daemon 0.26 nunca lo ve
/// (`VERSION_MISMATCH` de handshake); `MethodNotFound`/la decisión del
/// cliente degradan a no mostrar la columna.
pub const PLUGIN_COLUMN_VALUES: &str = "plugin.column_values";
/// `plugin.get_config` — esquema `[config]` + valores EFECTIVOS de un plugin
/// (0.28.0, G3c, ADR 0037): un elemento [`PluginConfigKeyWire`] por clave
/// declarada, esquema y valor ACTUAL juntos (`schema+value together`) — un
/// gestor de extensiones remoto no tenía forma de leer esto antes de este
/// bump (P2 lo dejó host-only, ver el rustdoc de
/// `norte_core::PluginRegistry::settings_of`). `id` desconocido responde
/// `keys: []` (mismo criterio
/// indulgente que `plugin.list` con un catálogo vacío — nunca un error por
/// "no tengo nada que mostrar"). ABIERTO a cualquier conexión (leer un
/// esquema/valor no consiente nada, mismo criterio que `plugin.preview*`).
pub const PLUGIN_GET_CONFIG: &str = "plugin.get_config";
/// `plugin.set_config` — persiste UN valor de `[config]` para un plugin
/// (0.28.0, G3c, ADR 0037), tras validarlo contra el ESQUEMA del manifiesto
/// (la MISMA validación que `config.toml`, nunca una ruta paralela — ver
/// `norte_plugin_host::encode_wire_value`). Un valor inválido no persiste
/// nada (`INVALID_PARAMS`). SOLO una conexión HUMANA (no-agente) puede
/// llamarlo — mismo criterio que [`PLUGIN_SET_APPROVAL`]/[`PLUGIN_SET_ENABLED`]:
/// los ajustes de un plugin son datos de USUARIO, un agente no los edita por
/// su cuenta.
pub const PLUGIN_SET_CONFIG: &str = "plugin.set_config";
/// `plugin.help` — la página de ayuda de UN plugin (H3e, 0.34.0), BAJO
/// DEMANDA: el host devuelve el `help.md` ya ACOTADO (tope de bytes de
/// `norte_help::Limits::untrusted`) y ya decodificado a UTF-8 válido, con
/// dos banderas que cuentan qué pasó al acotarlo. ABIERTO como
/// [`PLUGIN_LIST`]: leer documentación no consiente nada.
///
/// El texto es de TERCEROS y no está enmascarado: el frontend lo vuelve a
/// parsear con `norte_help::parse_untrusted`, que enmascara al construir el
/// modelo. Parsear en los dos lados es deliberado — host-side para que
/// `norte doctor` y el catálogo puedan reportar problemas sin un frontend
/// delante, cliente-side porque el wire lleva TEXTO, no un árbol.
pub const PLUGIN_HELP: &str = "plugin.help";
/// `rpc.cancel` — notificación client→server (#72): retira la request en
/// vuelo cuyo `id` JSON-RPC se indica. Best-effort y SIN respuesta: la
/// confirmación real es que la request cancelada responde con su desenlace
/// ([`Error::Cancelled`](crate::Error::Cancelled) si estaba suspendida en un
/// Ask de policy). Es puramente de la CAPA RPC (no `policy.*`): cancela una
/// request, no una aprobación (el peticionario no conoce el `approval_id`, que
/// va al humano). En M3 el único camino largo suspendible en el dispatch es el
/// Ask; una op larga ya-Task se cancela con [`TASK_CANCEL`]. Un `id`
/// desconocido, ya resuelto o no suspendido = no-op benigno. Un daemon N-1 que
/// no la conozca la descarta en silencio (notificación desconocida, ADR 0004):
/// degrada al comportamiento previo (Ask zombi hasta el TTL), no rompe.
pub const RPC_CANCEL: &str = "rpc.cancel";

/// Params de [`FS_LIST`] (paginación por cursor desde 0.8.0, ADR 0017).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// Ids of the provider attributes to deliver with each entry (0.30.0,
    /// ADR 0039). Empty (the default, and the only possibility for a 0.29
    /// client) = none: nothing is delivered unrequested. The contract is at
    /// most [`ATTRS_MAX_REQUEST`](crate::attrs::ATTRS_MAX_REQUEST) ids, each
    /// well-formed ([`is_valid_attr_id`](crate::attrs::is_valid_attr_id)), and
    /// violating either is `-32602` — a rule BLOCK 2 wires up: a 0.30.0 daemon
    /// ships no producer and ignores the requested ids entirely, so today the
    /// answer to any request is the same empty set of attributes. An id the
    /// provider does not offer is NOT an error either way: it comes back
    /// absent, so a client with a stale catalog degrades instead of failing.
    ///
    /// # Bounded at decode, but NOT validated — on purpose
    ///
    /// This is a REQUEST: data this peer is SENDING, not data it received. So
    /// whatever the wire carries lands in the vector VERBATIM — malformed ids
    /// and over-cap length included. Silently dropping a bad id here would
    /// turn "the client asked for `../etc/passwd`" into "the client asked for
    /// nothing", hiding a caller's bug and making the daemon-side validation
    /// untestable. The asymmetry with the two RECEIVE-side fields —
    /// [`Entry::attrs`](crate::Entry::attrs) and
    /// [`FsCapabilitiesResult::attrs`], which both filter at decode — is
    /// deliberate: a bad cell or a bad advertised descriptor costs itself,
    /// while a bad request is the daemon's `-32602` to raise.
    ///
    /// The one thing decoding does impose is a MEMORY bound, which is not
    /// validation: only the first `ATTRS_MAX_REQUEST + 1` elements are kept
    /// and the rest is drained unmaterialised, because a 16 MiB frame of
    /// `["a","a",…]` would otherwise allocate ~15× its own size in `String`
    /// headers before any daemon check could run. The `+ 1` is what keeps
    /// over-cap OBSERVABLE (`attrs.len() > ATTRS_MAX_REQUEST`) instead of
    /// trimming a violation into legality.
    #[serde(
        default,
        deserialize_with = "crate::attrs::deserialize_attr_request",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(extend(
            "maxItems" = crate::attrs::ATTRS_MAX_REQUEST,
            "items" = serde_json::json!({
                "type": "string",
                "maxLength": crate::attrs::ATTR_ID_MAX,
                "pattern": r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$",
            })
        ))
    )]
    pub attrs: Vec<String>,
}

/// Result de [`FS_LIST`].
///
/// Cláusula de compatibilidad (ADR 0004/0017): sin `cursor` NI `limit`, el
/// core DEVUELVE el listado COMPLETO con `next_cursor: null` — un cliente
/// 0.7 (N-1) recibe exactamente lo de antes, jamás un truncado en silencio.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsListResult {
    /// Entradas de ESTA página (orden: el del provider, sin garantía).
    pub entries: Vec<Entry>,
    /// Cursor de la página siguiente, o `None` si el listado terminó. Un
    /// cliente 0.7 lo ignora (campo desconocido para él).
    #[serde(default)]
    pub next_cursor: Option<String>,
    /// Entradas del CONTENEDOR omitidas de TODO su índice (#93, desde
    /// 0.22.0): nombres hostiles/límites anti-bomba de un provider archive
    /// (ADR 0018 C2). Es un total POR CONTENEDOR, no por página ni por
    /// directorio (las omitidas no tienen ruta representable donde
    /// atribuirse): cada página del listado repite el mismo valor. Ausente
    /// (`None`) = no aplica o desconocido; los clientes solo deben
    /// señalizarlo cuando es `Some(n)` con `n > 0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u64>,
}

/// Params de [`FS_STAT`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsStatParams {
    /// Nodo a consultar.
    pub path: VPath,
    /// Ids of the provider attributes to deliver with the entry (0.30.0,
    /// ADR 0039). Empty (the default, and the only possibility for a 0.29
    /// client) = none: nothing is delivered unrequested. The contract is at
    /// most [`ATTRS_MAX_REQUEST`](crate::attrs::ATTRS_MAX_REQUEST) ids, each
    /// well-formed ([`is_valid_attr_id`](crate::attrs::is_valid_attr_id)), and
    /// violating either is `-32602` — a rule BLOCK 2 wires up: a 0.30.0 daemon
    /// ignores the requested ids entirely. An id the provider does not offer
    /// is NOT an error either way: it comes back absent.
    ///
    /// # Bounded at decode, but NOT validated — on purpose
    ///
    /// Same as [`FsListParams::attrs`], for the same reasons: a malformed id
    /// survives decoding and is the daemon's `-32602` to raise, instead of
    /// being laundered into "asked for nothing"; only the RECEIVE-side fields
    /// — [`Entry::attrs`](crate::Entry::attrs) and
    /// [`FsCapabilitiesResult::attrs`] — filter. Decoding keeps the first
    /// `ATTRS_MAX_REQUEST + 1` elements and drains the rest, which is a memory
    /// bound (a 16 MiB frame of tiny ids would otherwise allocate ~15× its own
    /// size) that leaves over-cap observable.
    #[serde(
        default,
        deserialize_with = "crate::attrs::deserialize_attr_request",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(extend(
            "maxItems" = crate::attrs::ATTRS_MAX_REQUEST,
            "items" = serde_json::json!({
                "type": "string",
                "maxLength": crate::attrs::ATTR_ID_MAX,
                "pattern": r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$",
            })
        ))
    )]
    pub attrs: Vec<String>,
}

/// Result de [`FS_STAT`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsStatResult {
    /// Metadatos del nodo.
    pub entry: Entry,
}

/// Params de [`FS_COPY`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsDeleteParams {
    /// Nodo a borrar (recursivo si es dir).
    pub path: VPath,
    /// Papelera o permanente. `#[serde(default)]` = Trash: el default del
    /// wire es el SEGURO (ADR 0009).
    #[serde(default)]
    pub mode: DeleteMode,
}

/// Params de [`FS_MKDIR`] (#104).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsMkdirParams {
    /// Directorio a crear, COMPLETO (el último segmento es el nombre nuevo).
    /// El padre debe existir; no hay `-p`.
    pub path: VPath,
}

/// Result de [`FS_COPY`], [`FS_MOVE`], [`FS_DELETE`] y [`FS_MKDIR`]: la Task
/// creada. El progreso llega por [`TASK_PROGRESS`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsTaskResult {
    /// Id de la Task encolada.
    pub task_id: TaskId,
}

/// Params de [`FS_SEARCH`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsSearchParams {
    /// Raíz del walk (subtree entero).
    pub root: VPath,
    /// Glob sobre el NOMBRE (último segmento), p.ej. `*.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_glob: Option<String>,
    /// Regex sobre el nombre. Excluyente con `name_glob`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_regex: Option<String>,
    /// Texto literal a buscar en el CONTENIDO (multi-encoding: la aguja se
    /// transcodifica, el pajar jamás se decodifica entero).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Regex sobre el contenido (solo ficheros que el detector dé como
    /// texto). Excluyente con `content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_regex: Option<String>,
    /// Sensible a mayúsculas (default false; el matching de nombre es
    /// sobre el lossy en NFC — misma disciplina que el quick search).
    #[serde(default)]
    pub case_sensitive: bool,
    /// Tope de hits: alcanzado, la Task completa (`Completed`, no `Failed`).
    /// No hay flag `truncated` en el wire — el cliente infiere la
    /// truncación comparando el total de hits recibidos contra `max_hits`
    /// (== implica truncada).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hits: Option<u32>,
}

/// Un lote de resultados de [`SEARCH_HITS`]. `matches` alineado 1:1 con
/// `entries` cuando la búsqueda es de contenido (None si es solo nombre).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHits {
    /// Task dueña (correlación con `fs.search` → `task_id`).
    pub task_id: TaskId,
    /// Entradas que casan.
    pub entries: Vec<Entry>,
    /// Contexto del match de contenido, alineado con `entries`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches: Option<Vec<MatchInfo>>,
}

/// Contexto de UN match de contenido.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchInfo {
    /// Línea (1-based) del primer match, si se computó.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// La línea del match decodificada lossy y saneada EN ORIGEN
    /// (controles/bidi/invisibles enmascarados a `U+FFFD`, luego recorte a un
    /// tope fijo de chars). El consumidor puede pintarla directa —jamás llegan
    /// ANSI ni overrides bidi crudos por wire— aunque el TUI sigue pasándola
    /// por `detail_for_bar` como cinturón.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// Params de [`INDEX_BUILD`] (M4): raíz a indexar.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexBuildParams {
    /// Raíz del subárbol a (re)indexar.
    pub root: VPath,
}

/// Resultado de [`INDEX_BUILD`] al completar la Task (M4).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexBuildResult {
    /// Entradas indexadas (insertadas o actualizadas).
    pub indexed: u64,
    /// Filas barridas (paths que ya no existían).
    pub removed: u64,
}

/// Params de [`INDEX_QUERY`] (M4).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexQueryParams {
    /// Raíz cuyo índice se consulta.
    pub root: VPath,
    /// Texto libre del usuario (se sanea a una query FTS5 en el core).
    pub text: String,
    /// Tope de resultados.
    pub limit: u32,
}

/// Un hit de [`INDEX_QUERY`] (M4). `path` en bytes crudos vía [`VPath`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexHit {
    /// Path completo del resultado.
    pub path: VPath,
    /// Tipo de entrada.
    pub kind: EntryKind,
    /// Tamaño (`None` para dirs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// mtime en ms desde epoch (`None` si desconocido).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ms: Option<i64>,
}

/// Resultado de [`INDEX_QUERY`] (M4): hits ordenados por relevancia.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexQueryResult {
    /// Hits (bm25, más relevante primero).
    pub hits: Vec<IndexHit>,
}

/// Params de [`INDEX_EMBED`] (0.33.0, M4-IA-2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEmbedParams {
    /// Root YA indexado con `index.build` (misma clave exacta).
    pub root: VPath,
}

/// Params de [`INDEX_SEARCH_SEMANTIC`] (0.33.0, M4-IA-2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexSearchSemanticParams {
    /// Root a consultar; ausente ⇒ todos los roots del índice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<VPath>,
    /// Consulta en lenguaje natural (sale hacia el proveedor de IA).
    pub query: String,
    /// Máximo de hits; el server recorta a [`INDEX_SEMANTIC_MAX_K`].
    pub k: u32,
}

/// Un hit semántico: path + similitud coseno. Sin `Eq` (a diferencia de sus
/// hermanos de `index.*`): `score` es `f64`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticHit {
    /// Path del fichero (wire encoding).
    pub path: VPath,
    /// Similitud coseno en `[-1, 1]` (mayor = más afín).
    /// Siempre finito: el server jamás emite NaN/Infinity (cinturón en el
    /// engine — un no-finito serializaría como null y envenenaría la
    /// respuesta).
    pub score: f64,
}

/// Result de [`INDEX_SEARCH_SEMANTIC`] (0.33.0), mejor primero.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexSearchSemanticResult {
    /// Hits ordenados por score descendente.
    pub hits: Vec<SemanticHit>,
}

/// Params de [`AI_RENAME_PLAN`] (M4-IA, ADR 0031).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenamePlanParams {
    /// Directorio cuyos basenames se envían al proveedor (tras el gate de IA).
    pub dir: VPath,
    /// Instrucción del usuario.
    pub instruction: String,
}

/// Una pareja del plan de [`AI_RENAME_PLAN`]. Nombres BASE, UTF-8 garantizado:
/// el engine rechaza nombres hostiles fail-loud ANTES de llamar al proveedor y
/// valida `to` como `Segment` (sin `/`, `..`, NUL, `!` ni `\`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenameEntry {
    /// Nombre existente en `dir`.
    pub from: String,
    /// Nombre destino propuesto.
    pub to: String,
}

/// Resultado de [`AI_RENAME_PLAN`]: el plan REVISABLE (spec §9). Vacío = el
/// modelo no propuso cambios. El plan es el producto: aplicarlo son N
/// [`FS_MOVE`] gobernados; este método jamás muta.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenamePlanResult {
    /// Parejas from→to (solo las que cambian de nombre).
    pub entries: Vec<AiRenameEntry>,
}

/// Identidad de un cliente (va en [`InitializeParams`]).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Nombre del frontend (`norte-tui`, `norte-cli`, un tercero…).
    pub name: String,
    /// Versión del frontend (informativa, jamás se compara).
    pub version: String,
}

/// Identidad del servidor (va en [`InitializeResult`]).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Nombre del servidor (`norte-core`).
    pub name: String,
    /// Versión del binario del daemon (informativa).
    pub version: String,
}

/// Params de [`INITIALIZE`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonShutdownResult {}

/// Params de [`TASK_LIST`]: objeto vacío, reservado para extensión
/// (filtros por estado/kind llegarán aquí como campos opcionales).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListParams {}

/// Result de [`TASK_LIST`].
///
/// ```
/// use norte_proto::methods::TaskListResult;
/// let r: TaskListResult = serde_json::from_str(r#"{"tasks":[]}"#).unwrap();
/// assert!(r.tasks.is_empty());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadResult {
    /// Bytes del tramo, en base64 estándar (los bytes de un archivo no
    /// son texto: JSON no puede llevarlos crudos).
    pub content_b64: String,
    /// `true` si el tramo termina EN el fin del archivo.
    pub eof: bool,
}

/// Params de [`FS_CAPABILITIES`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCapabilitiesParams {
    /// Un path del provider a consultar.
    pub path: VPath,
}

/// Result de [`FS_CAPABILITIES`].
///
/// ```
/// use norte_proto::methods::FsCapabilitiesResult;
/// // Un catálogo con un id mal formado NO llega al vector: se descarta al
/// // decodificar, sin error (ver [`FsCapabilitiesResult::attrs`]).
/// let wire = r#"{
///     "capabilities": {"flags": "", "max_path": null},
///     "attrs": [{"id": "MODE", "label": "Mode", "type": "uint", "hint": "mode"}]
/// }"#;
/// let caps: FsCapabilitiesResult = serde_json::from_str(wire).unwrap();
/// assert!(caps.attrs.is_empty());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCapabilitiesResult {
    /// Capabilities declaradas por el provider.
    pub capabilities: crate::Capabilities,
    /// Provider attributes this provider offers (0.30.0, ADR 0039): the
    /// discovery half of [`Entry::attrs`](crate::Entry::attrs). Empty (the
    /// default, and the only possibility for a 0.29 peer) is omitted from the
    /// wire entirely, so a response from a provider that publishes none is
    /// byte-identical to 0.29's.
    ///
    /// [`AttrInfo::label`](crate::attrs::AttrInfo::label) is THIRD-PARTY text
    /// — a frontend masks and clamps it exactly as it does a plugin's column
    /// header. The ORDER is the provider's and it is meaningful: a column
    /// picker paints the catalog in it, so it is never sorted here.
    ///
    /// # Rules applied ON DECODE
    ///
    /// This is RECEIVED data, so the same shape of filter that guards
    /// [`Entry::attrs`](crate::Entry::attrs) guards it: decoding runs
    /// [`sanitize_catalog`](crate::attrs::sanitize_catalog), whose rules are
    /// documented there in full and NONE of which is ever an error —
    ///
    /// 1. an [`AttrInfo`](crate::attrs::AttrInfo) whose `id` is not
    ///    well-formed ([`is_valid_attr_id`](crate::attrs::is_valid_attr_id))
    ///    is DROPPED. An advertised id becomes a requested id, a configuration
    ///    id and a map lookup downstream, so `MODE` or `../etc/passwd` must
    ///    not survive the wire — and a caller must not have to remember to
    ///    check;
    /// 2. a REPEATED id is dropped, FIRST WINS — the opposite of
    ///    [`Entry::attrs`](crate::Entry::attrs)'s last-wins, because this is
    ///    an ordered list the provider ranked (where "first" means something)
    ///    and that is an unordered JSON object (where it does not). Keeping
    ///    both would let two consumers render the same bytes differently, one
    ///    folding into a map and one using `find()`;
    /// 3. an over-long `label` is CLAMPED to
    ///    [`ATTR_LABEL_MAX`](crate::attrs::ATTR_LABEL_MAX) bytes on a char
    ///    boundary; the descriptor survives, since the id is what a client
    ///    acts on;
    /// 4. the vector is bounded at
    ///    [`ATTRS_MAX_ADVERTISED`](crate::attrs::ATTRS_MAX_ADVERTISED)
    ///    descriptors, keeping the FIRST in wire order — rules 1 and 2 run
    ///    before a slot is taken, so rejects and duplicates never starve a
    ///    legitimate later attribute. A fat catalog is a buggy provider, not a
    ///    broken peer, so it truncates rather than failing the call: the same
    ///    spirit as an unknown requested id coming back absent.
    ///
    /// Decoding additionally stops EXAMINING elements past
    /// [`ATTRS_MAX_CATALOG_SCAN`](crate::attrs::ATTRS_MAX_CATALOG_SCAN) and
    /// drains the rest unmaterialised, so a catalog padded with rejects costs
    /// bounded work rather than unbounded work for an empty result.
    ///
    /// Those rules are NOT the deserialiser's alone: the field is an
    /// [`AttrCatalog`](crate::attrs::AttrCatalog), whose only constructor runs
    /// `sanitize_catalog` and whose contents are private. That matters because
    /// an EMBEDDED backend — the default TUI/CLI configuration, no daemon in
    /// between — never crosses the deserialisation boundary, so in block 2 a
    /// catalog from a WASM provider plugin (untrusted by the threat model)
    /// would otherwise reach a frontend unfiltered whenever someone forgot the
    /// call. The wire shape is unchanged: a plain array of `AttrInfo`.
    ///
    /// A descriptor that is not a well-formed `AttrInfo` at all (a missing
    /// `label`, `attrs` that is not a list) IS a hard error: that is serde's
    /// decision, and a peer that sends one is broken rather than newer — an
    /// unknown `type` or `hint` from a NEWER peer already degrades to its
    /// `Unknown` variant instead.
    ///
    /// # Unlike `Entry::attrs`, this cannot carry a producer's bug
    ///
    /// [`Entry::attrs`](crate::Entry::attrs) is a public map with no
    /// constructor, so an entry built in-process with an invalid id emits it
    /// and decodes back different — deliberately, so a producer's bug stays
    /// visible at the boundary that validates it. A catalog has no such hole:
    /// it cannot be built dirty in the first place, so serialisation has
    /// nothing to launder. The asymmetry is on purpose — an advertised id
    /// becomes a REQUESTED id, a configuration id and a map lookup downstream,
    /// which is a longer blast radius than one entry's cell.
    #[serde(default, skip_serializing_if = "crate::attrs::AttrCatalog::is_empty")]
    #[cfg_attr(
        feature = "schema",
        schemars(
            with = "Vec<crate::attrs::AttrInfo>",
            extend("maxItems" = crate::attrs::ATTRS_MAX_ADVERTISED)
        )
    )]
    pub attrs: crate::attrs::AttrCatalog,
}

/// Params de [`TASK_CANCEL`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCancelParams {
    /// Task a cancelar. Cancelar una Task terminal o inexistente no es error:
    /// la respuesta llega igual y el estado real viaja por [`TASK_PROGRESS`].
    pub task_id: TaskId,
}

/// Result de [`TASK_CANCEL`]: objeto vacío, reservado para extensión.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCancelResult {}

/// Params de [`CONNECTION_TRUST_HOST_KEY`] (flujo TOFU, ADR 0015 D). Lleva el
/// fingerprint que el usuario VERIFICÓ; el core lo compara con la clave que
/// vuelve a presentar el servidor al reintentar, y solo registra si coincide.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionTrustHostKeyResult {
    /// `true` si la clave quedó registrada (idempotente: `true` también si ya
    /// estaba). `false` reservado para un futuro rechazo por política.
    pub trusted: bool,
}

/// Notificación [`CONNECTION_DEGRADED`] (server→client): una sesión remota se
/// estableció con seguridad degradada. Las rutas/host van REDACTADOS (rule 10):
/// `host` jamás lleva userinfo.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionDegraded {
    /// Scheme de la sesión (p. ej. `"ftp"`).
    pub scheme: String,
    /// Host de la sesión, SIN userinfo (rule 10).
    pub host: String,
    /// Causa, vocabulario CERRADO comparable por igualdad (como
    /// `PolicyDenied.rule`). Valores actuales: `"tls-auth-rejected"` (el servidor
    /// rechazó `AUTH TLS` bajo `tls="allow"`; la sesión viaja en claro) y
    /// `"ftp-plaintext"` (FTP-por-plugin, ADR 0033: FTPS es deuda, la sesión es
    /// SIEMPRE en claro). El conjunto puede CRECER de forma aditiva: un consumidor
    /// que reciba un `reason` DESCONOCIDO debe degradar con gracia (mensaje
    /// genérico de "sesión degradada" apoyándose en `detail`), jamás rechazar la
    /// notif.
    pub reason: String,
    /// Detalle humano opcional (presentación, jamás contrato).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Params de [`POLICY_REQUEST_SCOPE`] (M3-3b): un agente pide un scope.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestScopeResult {
    /// Id de la petición, para que un humano la conceda con `policy.grant_scope`.
    pub request_id: u64,
}

/// Params de [`POLICY_GRANT_SCOPE`] (un humano concede una petición pendiente).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantScopeParams {
    /// Id devuelto por `policy.request_scope`.
    pub request_id: u64,
}

/// Result de [`POLICY_GRANT_SCOPE`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantScopeResult {}

/// Notificación [`POLICY_APPROVAL_REQUIRED`] (server→client): una op `ask`
/// espera decisión. Las rutas van REDACTADAS si llevan userinfo (regla 10).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecideParams {
    /// Id de la aprobación pendiente.
    pub approval_id: u64,
    /// `true` = aprobar, `false` = denegar.
    pub approve: bool,
}

/// Result de [`POLICY_DECIDE`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecideResult {}

/// Una aprobación pendiente (elemento de [`PolicyPendingResult`]).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyPendingResult {
    /// Aprobaciones pendientes.
    pub pending: Vec<PendingApproval>,
}

/// Params de [`RPC_CANCEL`] (#72): el id de la request en vuelo a cancelar.
///
/// El `id` es el mismo tipo que [`crate::wire::RequestId`] — número (emisor
/// canónico) o string (tolerancia JSON-RPC). No se valida contra un mapa aquí
/// (es una notificación best-effort): el daemon lo coteja con sus requests en
/// vuelo y un id sin correspondencia es un no-op.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcCancelParams {
    /// Id JSON-RPC de la request a cancelar.
    pub id: crate::wire::RequestId,
}

/// Params de [`POLICY_UNDO_SESSION`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUndoSessionParams {
    /// Sesión de agente cuyas mutaciones se deshacen (mismo formato que
    /// `agent_session` del initialize: `[A-Za-z0-9._-]`, 1..=64).
    pub session: String,
}

/// Result de [`POLICY_UNDO_SESSION`]: el undo corre como Task (progreso por
/// `task.progress`, cancelable con `task.cancel`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUndoSessionResult {
    /// Task del undo.
    pub task_id: TaskId,
}

/// Params de [`POLICY_UNDO_REPORT`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUndoReportParams {
    /// Task de undo cuyo informe se pide (la de
    /// [`PolicyUndoSessionResult::task_id`]).
    pub task_id: TaskId,
}

/// Result de [`POLICY_UNDO_REPORT`]: el informe del undo. Todo cero y sin
/// `blocked` = no había nada que deshacer SOLO si la Task terminó
/// `Completed`: una Task `Failed`/`Cancelled` deja contadores PARCIALES con
/// `blocked` ausente (el motivo vive en su `task.progress` terminal) — el
/// estado de la Task se consulta aparte, este result no lo lleva.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUndoReportResult {
    /// Entradas revertidas con éxito.
    pub undone: u64,
    /// Entradas `Irreversible` saltadas (no hay nada que pisar).
    pub skipped_irreversible: u64,
    /// Reversas de `Created` saltadas porque el provider no tiene papelera
    /// (#65): el nodo SIGUE en el destino — deshacerlo habría sido un borrado
    /// permanente y el undo jamás destruye de forma irrecuperable.
    pub skipped_created_no_trash: u64,
    /// Primer paso donde el LIFO paró (estricto), si lo hubo. El undo va de
    /// la entrada MÁS NUEVA hacia atrás: lo posterior al bloqueo en el
    /// journal ya se deshizo; lo ANTERIOR a él en el journal (seq menor)
    /// quedó SIN deshacer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<UndoBlocked>,
}

/// Un bloqueo del undo: dónde y por qué (elemento de
/// [`PolicyUndoReportResult::blocked`]).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoBlocked {
    /// `seq` de la entrada del journal cuya reversa bloqueó. Referencia
    /// OPACA para el cliente: solo cobra sentido contra el journal/audit del
    /// server (M3-5) — sirve para citarla, no para interpretarla.
    pub seq: i64,
    /// Motivo (taxonomía de errores del protocolo; drift/conflicto típicos).
    pub error: crate::Error,
}

/// Un comando de plugin invocable (elemento de [`PluginInfo::commands`],
/// P1): espejo minimal, solo-lectura, de la contribución `command` del
/// manifiesto (`CommandContrib` en `norte-plugin-host`) — lo que un
/// frontend necesita para listar el comando (paleta, gestor de
/// extensiones), no para ejecutarlo. `title` es texto suministrado por el
/// plugin: NO CONFIABLE, un frontend debe enmascararlo antes de
/// renderizarlo (mismo trato que `PluginInfo::name`).
///
/// ```
/// use norte_proto::methods::PluginCommandInfo;
/// let c: PluginCommandInfo =
///     serde_json::from_str(r#"{"id":"greet","title":"Greet"}"#).unwrap();
/// assert_eq!(c.id, "greet");
/// assert_eq!(c.title, "Greet");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCommandInfo {
    /// Id del comando dentro del plugin (estable; se pasa junto al id del
    /// plugin a [`PLUGIN_RUN_COMMAND`]).
    pub id: String,
    /// Título legible para mostrar. Texto del plugin — NO confiable.
    pub title: String,
}

/// Una columna que un plugin `columns` contribuye (elemento de
/// [`PluginInfo::columns`], 0.28.0, G3c, ADR 0037): discovery — con QUÉ
/// `column_id` llamar a [`PLUGIN_COLUMN_VALUES`] y QUÉ cabecera pintar, sin
/// que el frontend tenga que adivinar por `category == "columns"`. `header`
/// es texto del plugin — NO confiable, un frontend debe enmascararlo antes
/// de renderizarlo (mismo trato que [`PluginCommandInfo::title`]).
///
/// ```
/// use norte_proto::methods::PluginColumnInfo;
/// let c: PluginColumnInfo =
///     serde_json::from_str(r#"{"id":"git-status","header":"Git"}"#).unwrap();
/// assert_eq!(c.id, "git-status");
/// assert_eq!(c.header, "Git");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginColumnInfo {
    /// Id de la columna (el mismo `column_id` que espera
    /// [`PLUGIN_COLUMN_VALUES`]).
    pub id: String,
    /// Cabecera legible para mostrar. Texto del plugin — NO confiable.
    pub header: String,
}

/// Un plugin descubierto (elemento de [`PluginListResult::plugins`], M4-P3).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// Descripción cosmética declarada en el manifiesto (P1); ausente =
    /// `None`. NO forma parte del digest de aprobación (editarla no
    /// reinvalida capabilities ya aprobadas) y es texto del plugin — NO
    /// confiable, un frontend debe enmascararla antes de renderizarla.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Comandos que el plugin expone (P1); vacío si no contribuye ninguno.
    /// Un peer N-1 que construye su propio `PluginInfo` no emite este
    /// campo — se toma por su default (`vec![]`) al deserializar aquí.
    #[serde(default)]
    pub commands: Vec<PluginCommandInfo>,
    /// Columnas que el plugin contribuye (0.28.0, G3c); vacío si no
    /// contribuye ninguna. Un peer N-1 que construye su propio `PluginInfo`
    /// no emite este campo — se toma por su default (`vec![]`) al
    /// deserializar aquí (mismo criterio aditivo que `commands` en 0.26.0).
    #[serde(default)]
    pub columns: Vec<PluginColumnInfo>,
    /// `true` si el plugin trae un `help.md` junto a su `plugin.toml`
    /// (H3e, 0.34.0). Es DISCOVERY barato: decide si el nodo del plugin
    /// aparece en la barra de temas de la ayuda, y evita que 64 KiB por
    /// plugin viajen en cada `plugin.list` — el contenido se pide aparte
    /// con [`PLUGIN_HELP`], bajo demanda.
    ///
    /// El host DEBE calcularlo con UN `is_file` al descubrir: no lee el
    /// fichero, no lo parsea, y por tanto un `help.md` presente pero ilegible
    /// o vacío sale `true` aquí y se degrada al pedirlo (markdown vacío), que
    /// es la dirección correcta — la ayuda es cosmética y jamás tumba un
    /// plugin. Eso es el CONTRATO; el daemon de 0.34.0 todavía emite `false`
    /// fijo, ver el bump en [`PROTOCOL_VERSION`].
    ///
    /// `skip_serializing_if` sobre `false`: un plugin sin ayuda produce un
    /// payload IDÉNTICO byte a byte al de 0.33 (mismo criterio aditivo
    /// fuerte que los `attrs` de 0.30). Un peer N-1 que construye su propio
    /// `PluginInfo` no lo emite y aquí se toma por `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_help: bool,
}

/// Un directorio de plugin que NO se pudo cargar (elemento de
/// [`PluginListResult::errors`], M4-P3): se reporta para diagnóstico, sin
/// tumbar el resto del catálogo.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginLoadError {
    /// Directorio del plugin que falló (display; puede llevar bytes lossy).
    pub dir: String,
    /// Motivo legible del fallo (manifiesto inválido, versión no soportada…).
    pub reason: String,
}

/// Params de [`PLUGIN_LIST`]: objeto vacío, reservado para extensión
/// (filtros por categoría/estado llegarán aquí como campos opcionales).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginListParams {}

/// Result de [`PLUGIN_LIST`]: el catálogo descubierto y los fallos de carga.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginListResult {
    /// Plugins descubiertos y cargados (con su estado aprobado/activo).
    pub plugins: Vec<PluginInfo>,
    /// Directorios que fallaron al cargar (mejor esfuerzo; ver
    /// [`PluginLoadError`]).
    pub errors: Vec<PluginLoadError>,
}

/// Params de [`PLUGIN_SET_APPROVAL`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetApprovalParams {
    /// Id del plugin a (des)aprobar.
    pub id: String,
    /// `true` = aprobar las capabilities, `false` = revocar.
    pub approved: bool,
}

/// Result de [`PLUGIN_SET_APPROVAL`]: objeto vacío, reservado para extensión.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetApprovalResult {}

/// Params de [`PLUGIN_SET_ENABLED`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetEnabledParams {
    /// Id del plugin a activar/desactivar.
    pub id: String,
    /// `true` = activar, `false` = desactivar.
    pub enabled: bool,
}

/// Result de [`PLUGIN_SET_ENABLED`]: objeto vacío, reservado para extensión.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetEnabledResult {}

/// Params de [`PLUGIN_RUN_COMMAND`] (M4-P4).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRunCommandResult {
    /// Salida (string) que devuelve el comando del plugin.
    pub output: String,
}

/// Params de [`PLUGIN_PREVIEW`] (M4-P5).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewParams {
    /// Ruta del archivo a previsualizar (el core lee sus bytes).
    pub path: VPath,
}

/// La preview producida por un plugin previewer: los tres campos van JUNTOS
/// (all-or-nothing). Ver [`PluginPreviewResult`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreview {
    /// Id del plugin previewer que produjo la salida.
    pub plugin_id: String,
    /// Nombre legible del plugin previewer (para el indicador «via …»).
    pub plugin_name: String,
    /// Salida (texto) de la preview.
    pub output: String,
    /// La decodificación host-side del fichero fue LOSSY (0.29.0, #101): el
    /// core detectó texto en un encoding no-UTF8 y algún byte no era válido,
    /// así que los `�` de la salida vienen de la decodificación, no del
    /// fichero. El frontend lo señala junto al indicador «via …» (el viewer
    /// crudo ya marca su propio `had_errors`; esto le da al modo preview la
    /// misma honestidad). Aditivo sobre 0.28.x: `#[serde(default)]` = un
    /// cliente/daemon N-1 que no lo emite se lee como `false` (sin aviso,
    /// dirección segura). SIEMPRE presente al serializar (mismo criterio
    /// «aditivo siempre presente» que `PluginInfo::commands`).
    #[serde(default)]
    pub lossy: bool,
}

/// Result de [`PLUGIN_PREVIEW`] (M4-P5): la preview del primer previewer que
/// aplica, o NADA. El `flatten` sobre un `Option` hace que el wire sea
/// `{plugin_id,plugin_name,output}` (aplicó) o `{}` (ninguno); el TIPO Rust hace
/// INCONSTRUIBLE un estado parcial (los tres campos van juntos en
/// [`PluginPreview`]), y un objeto parcial del wire colapsa a `None` (sin
/// preview, seguro) — jamás un `plugin_id` sin `output` (protocol-guardian
/// M4-P5). `None` = ningún previewer maneja el mimetype; el frontend cae a la
/// vista cruda.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewResult {
    /// La preview, o `None` si ningún previewer aplicó.
    #[serde(flatten)]
    pub preview: Option<PluginPreview>,
}

/// Un span de texto con estilo opcional (elemento de una línea de
/// [`PluginPreviewStyled::lines`], 0.27.0, G3, ADR 0037): el HOST pinta, el
/// plugin solo describe. `text` es texto del plugin — NO CONFIABLE, un
/// frontend debe enmascararlo antes de renderizarlo (mismo trato que
/// `PluginInfo::name`/`title`). `role` referencia un nombre de
/// `norte_theme::Role` — el HOST lo valida contra el conjunto CERRADO al
/// producirlo (un nombre desconocido nunca sale al wire como texto libre,
/// colapsa a `None` antes de serializar); un cliente remoto que reciba de un
/// daemon en el que no confía plenamente un `role` que no reconoce debe
/// tratarlo igual, como `None`. `fg` es un fallback de color RGB crudo para
/// spans sin rol (p. ej. la paleta fija de un highlighter); cuando AMBOS
/// están presentes, `role` gana — el tema del usuario tiene precedencia
/// sobre un color fijo del plugin.
///
/// ```
/// use norte_proto::methods::SpanWire;
/// let s: SpanWire = serde_json::from_str(r#"{"text":"fn"}"#).unwrap();
/// assert_eq!(s.text, "fn");
/// assert_eq!(s.role, None);
/// assert_eq!(s.fg, None);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanWire {
    /// Texto del span. Texto del plugin — NO confiable.
    pub text: String,
    /// Nombre de rol de `norte_theme::Role` (validado host-side; un nombre
    /// desconocido nunca llega hasta aquí como `Some`, ver el rustdoc del
    /// tipo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Color RGB crudo de respaldo cuando no hay `role` (un byte por canal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<[u8; 3]>,
}

/// La preview CON ESTILO producida por un plugin previewer (0.27.0, G3, ADR
/// 0037): gemelo de [`PluginPreview`] con `lines` de [`SpanWire`] en vez de
/// un `output: String` plano. Topes del wire (ADR 0037): ≤10 000 líneas,
/// ≤64 spans por línea, texto de span ≤4 KiB, payload total ≤4 MiB (mismo
/// tope de retorno del runtime que ya usan [`PLUGIN_PREVIEW`]/
/// [`PLUGIN_RUN_COMMAND`]) — el server los aplica antes de enviar; un
/// cliente los re-valida y cae a [`PLUGIN_PREVIEW`] si se violan.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewStyled {
    /// Id del plugin previewer que produjo la salida.
    pub plugin_id: String,
    /// Nombre legible del plugin previewer (para el indicador «via …»).
    pub plugin_name: String,
    /// Líneas de la preview; cada línea es una lista de spans en orden.
    pub lines: Vec<Vec<SpanWire>>,
    /// La decodificación host-side del fichero fue LOSSY (0.29.0, #101):
    /// idéntico a [`PluginPreview::lossy`] — el previewer con estilo recibe el
    /// MISMO texto ya decodificado por el core, así que hereda el mismo aviso.
    #[serde(default)]
    pub lossy: bool,
}

/// Params de [`PLUGIN_PREVIEW_STYLED`]: idéntico a [`PluginPreviewParams`]
/// (mismo archivo, misma resolución de previewer — solo cambia la forma del
/// result).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewStyledParams {
    /// Ruta del archivo a previsualizar (el core lee sus bytes).
    pub path: VPath,
}

/// Result de [`PLUGIN_PREVIEW_STYLED`]: la preview con estilo del primer
/// previewer que aplica, o NADA. Mismo patrón `flatten`-sobre-`Option`
/// all-or-nothing que [`PluginPreviewResult`] (ver su rustdoc): el wire es
/// `{plugin_id,plugin_name,lines}` (aplicó) o `{}` (ninguno), jamás un
/// estado parcial.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewStyledResult {
    /// La preview con estilo, o `None` si ningún previewer aplicó.
    #[serde(flatten)]
    pub preview: Option<PluginPreviewStyled>,
}

/// Una decoración tipo git-status de UNA entrada (elemento de
/// [`PluginDecorations::decorations`], 0.27.0, G3, ADR 0037). `badge` es
/// texto del plugin — NO confiable, ≤8 chars TRAS enmascarar (tope del
/// wire, ADR 0037); un frontend debe enmascarar (y truncar de nuevo) antes
/// de confiar en el tope ya aplicado por el server — defensa en
/// profundidad. `role` sigue la misma validación host-side que
/// [`SpanWire::role`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecorationWire {
    /// Badge corto (p. ej. `"M"`, `"++"`). Ausente = sin badge para esta
    /// entrada de este plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    /// Nombre de rol de `norte_theme::Role` para pintar el badge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// Params de [`PLUGIN_DECORATE`]: las entradas VISIBLES de la página actual
/// (batched — el frontend no pide decoraciones entrada por entrada).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDecorateParams {
    /// Rutas a decorar, en el orden en que se listan.
    pub paths: Vec<VPath>,
}

/// Las decoraciones de UN plugin `decorator` (elemento de
/// [`PluginDecorateResult::plugins`]): `decorations` es POSICIONAL 1:1 con
/// `PluginDecorateParams::paths` — el elemento `i` decora la ruta `i`, jamás
/// una clave por path (barato en el wire, y un nombre hostil no puede
/// colisionar con otro como clave). Un plugin muerto o que falló
/// simplemente no aparece en `plugins` (sin decoraciones de ESE plugin; el
/// resto de la página se pinta igual — mismo contrato de fallback que
/// [`PLUGIN_PREVIEW`]).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDecorations {
    /// Id del plugin `decorator` que produjo estas decoraciones.
    pub plugin_id: String,
    /// Decoraciones, UNA por elemento de `paths` en el mismo orden (una
    /// entrada sin decoración de este plugin lleva
    /// `DecorationWire{badge:None,role:None}`, nunca se omite — el índice es
    /// el único enlace con la ruta).
    pub decorations: Vec<DecorationWire>,
}

/// Result de [`PLUGIN_DECORATE`]: las decoraciones de cada plugin
/// `decorator` aprobado y activado que respondió.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDecorateResult {
    /// Un elemento por plugin `decorator` que decoró esta página.
    pub plugins: Vec<PluginDecorations>,
}

/// Params de [`PLUGIN_COLUMN_VALUES`]: el id de columna declarado por el
/// plugin `columns` en su manifiesto, más las rutas visibles a valorar.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginColumnValuesParams {
    /// Id de la columna (declarado por el plugin; identifica QUÉ columna
    /// entre varias que un mismo plugin `columns` podría exponer).
    pub column_id: String,
    /// Rutas a valorar, en el orden en que se listan.
    pub paths: Vec<VPath>,
}

/// Result de [`PLUGIN_COLUMN_VALUES`]: `values` es POSICIONAL 1:1 con
/// `PluginColumnValuesParams::paths` — el valor `i` es la celda de la ruta
/// `i`. Cada celda es `Option<String>` (no `String`) por la MISMA razón que
/// [`DecorationWire::badge`]: una columna que no aplica a esa entrada (p.
/// ej. "duración" sobre un archivo que no es media) necesita distinguirse
/// de un valor real que resulta ser la cadena vacía — `None` = sin celda
/// para esta entrada de esta columna, jamás se omite del vector posicional
/// (protocol-guardian, ADR 0037). Texto del plugin — NO confiable, un
/// frontend debe enmascararlo antes de renderizarlo.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginColumnValuesResult {
    /// Valores de celda, uno por elemento de `paths` en el mismo orden;
    /// `None` = la columna no aplica a esa entrada.
    pub values: Vec<Option<String>>,
}

/// Params de [`PLUGIN_GET_CONFIG`] (0.28.0, G3c).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginGetConfigParams {
    /// Id del plugin cuyo esquema `[config]` se consulta.
    pub id: String,
}

/// Params de [`PLUGIN_HELP`].
///
/// ```
/// use norte_proto::methods::PluginHelpParams;
/// let p: PluginHelpParams = serde_json::from_str(r#"{"id":"acme.ftp"}"#).unwrap();
/// assert_eq!(p.id, "acme.ftp");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHelpParams {
    /// Id del plugin cuyo `help.md` se pide. Es una CLAVE DE BÚSQUEDA
    /// contra el catálogo: el host la resuelve contra los plugins que
    /// descubrió y jamás la compone en una ruta de fichero.
    pub id: String,
}

/// Result de [`PLUGIN_HELP`]: el `help.md` acotado y qué se perdió al
/// acotarlo.
///
/// ```
/// use norte_proto::methods::PluginHelpResult;
/// let r: PluginHelpResult =
///     serde_json::from_str(r#"{"markdown":"body","truncated":true,"lossy":false}"#)
///         .unwrap();
/// assert!(r.truncated && !r.lossy);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHelpResult {
    /// El `help.md` del plugin, ya acotado y ya UTF-8 VÁLIDO (el host
    /// decodifica y sustituye lo irrecuperable). Sin `help.md` legible:
    /// cadena vacía, nunca un error — la ayuda es cosmética.
    pub markdown: String,
    /// El fichero superaba el tope y se cortó. Viaja porque el receptor NO
    /// puede deducirlo: el texto le llega ya corto, así que su propio parseo
    /// saldría limpio y la insignia — la mitigación entera frente a un
    /// `help.md` hostil — se apagaría en silencio.
    #[serde(default)]
    pub truncated: bool,
    /// Algún byte no decodificó bajo ninguna lectura y salió `U+FFFD`.
    /// Viaja por la misma razón que `truncated`.
    #[serde(default)]
    pub lossy: bool,
}

/// Una clave `[config.<key>]` del esquema de un plugin, esquema + valor
/// EFECTIVO juntos (elemento de [`PluginGetConfigResult::keys`], 0.28.0,
/// G3c, ADR 0037): mismo criterio "schema+value together" que evita una
/// segunda ida y vuelta al wire para pintar la UI de ajustes. `kind` es
/// texto CERRADO (`"string"|"bool"|"int"|"enum"` — los cuatro únicos que
/// `norte_plugin_host::ConfigKeySpec` declara); un frontend que ve un valor
/// desconocido (peer más nuevo) debe tratarlo como no-editable, jamás
/// reventar. `default`/`value` viajan como `String` SIEMPRE (la MISMA
/// codificación canónica que `norte_plugin_host::resolve_settings`: `bool`
/// → `"true"`/`"false"`, `int` → decimal), coherente con
/// [`PluginSetConfigParams::value`], que también es `String`.
///
/// ```
/// use norte_proto::methods::PluginConfigKeyWire;
/// let k: PluginConfigKeyWire = serde_json::from_str(
///     r#"{"key":"greeting","kind":"string","default":"hola","value":"hola"}"#,
/// )
/// .unwrap();
/// assert_eq!(k.key, "greeting");
/// assert_eq!(k.kind, "string");
/// assert!(k.min.is_none());
/// assert!(k.values.is_empty());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginConfigKeyWire {
    /// Nombre de la clave (charset `[a-z0-9-]{1,32}` del manifiesto, seguro
    /// de mostrar tal cual — mismo criterio que
    /// `norte_plugin_host::is_valid_config_key`).
    pub key: String,
    /// Tipo declarado: `"string"`, `"bool"`, `"int"` o `"enum"`.
    pub kind: String,
    /// Valor por defecto del esquema, codificado como string canónico.
    pub default: String,
    /// Cota inferior inclusive (solo `kind == "int"`). Ausente = sin cota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    /// Cota superior inclusive (solo `kind == "int"`). Ausente = sin cota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    /// Valores permitidos (solo `kind == "enum"`); vacío para el resto de
    /// tipos — SIEMPRE presente (mismo criterio aditivo que
    /// `PluginInfo::commands`), nunca omitido.
    #[serde(default)]
    pub values: Vec<String>,
    /// Descripción cosmética del manifiesto. Texto del plugin — NO
    /// confiable, un frontend debe enmascararla antes de renderizarla
    /// (mismo trato que `PluginInfo::description`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Valor EFECTIVO actual (defaults de esquema + `config.toml` ya
    /// superpuesto), codificado como string canónico — la MISMA
    /// codificación que `default`.
    pub value: String,
}

/// Result de [`PLUGIN_GET_CONFIG`]: el esquema completo + valores
/// efectivos, EN ORDEN DE CLAVE del manifiesto (mismo criterio que
/// `PluginInfo::commands`: orden de manifiesto, no reordenado). `id`
/// desconocido responde `keys: []` — nunca un error (mismo criterio
/// indulgente que `PLUGIN_LIST` con un catálogo vacío).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginGetConfigResult {
    /// Una entrada por clave `[config.<key>]` declarada.
    pub keys: Vec<PluginConfigKeyWire>,
}

/// Params de [`PLUGIN_SET_CONFIG`] (0.28.0, G3c): `value` es SIEMPRE
/// `String` (la codificación canónica descrita en
/// [`PluginConfigKeyWire::value`]) — el daemon la valida contra el ESQUEMA
/// de `key` antes de persistir; nunca se persiste sin validar (spec S2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetConfigParams {
    /// Id del plugin cuyo ajuste se cambia.
    pub id: String,
    /// Clave `[config.<key>]` a fijar.
    pub key: String,
    /// Valor nuevo, codificado como string canónico (ver
    /// [`PluginConfigKeyWire::value`]).
    pub value: String,
}

/// Result de [`PLUGIN_SET_CONFIG`]: objeto vacío, reservado para extensión.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetConfigResult {}
