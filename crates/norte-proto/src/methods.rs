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

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    CollisionPolicy, DeleteMode, Entry, EntryKind, ResumePolicy, Segment, SymlinkPolicy, TaskId,
    VPath, VPathError, VerifyPolicy,
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
/// 0.34.0 (H3e, ADR 0040): la ayuda de los PLUGINS por el wire — el corpus
/// de esa ADR alcanzando el protocolo. [`PluginInfo`] gana
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
/// 0.35.0 (#120): [`PluginColumnValuesParams`] gana `plugin_id: Option<String>`.
/// `column_id` no identifica al plugin, y el host resolvía a-la-primera-que-
/// casa, así que dos plugins consentidos que declararan el mismo id bare
/// hacían que `plugin:a/status` pintara los valores de `b` en silencio. El
/// frontend siempre supo cuál era; lo que faltaba era sitio donde decirlo.
/// Ventana N=0.35.x / N-1=0.34.x en la dirección que importa: un cliente 0.34
/// no emite el campo, el host cae al camino de antes y el resultado es el
/// mismo que tenía — incluida su ambigüedad, que es exactamente lo que un
/// cliente viejo ya se comía. `skip_serializing_if` sobre `None` deja el
/// payload de una petición sin plugin idéntico byte a byte al de 0.34. La
/// inversa la corta [`version_compatible`] en el handshake, como siempre.
///
/// 0.36.0 (batch rename, ADR 0042): un lote de renames dentro de UN directorio
/// pasa a ser UNA transacción. Aparecen [`FS_RENAME_BATCH_PLAN`]
/// ([`FsRenameBatchPlanParams`] → [`FsRenameBatchPlanResult`], respuesta
/// DIRECTA) y [`FS_RENAME_BATCH`] ([`FsRenameBatchParams`] → el
/// [`FsTaskResult`] existente), más [`TaskKind::RenameBatch`](crate::TaskKind)
/// y las categorías [`Error::PlanStale`](crate::Error::PlanStale) y
/// [`Error::PlanNotExecutable`](crate::Error::PlanNotExecutable). Los dos
/// métodos van ligados por el `plan_hash`: el cliente manda INTENCIÓN
/// (`pairs`), jamás el orden, y la ejecución devuelve el hash del plan que el
/// humano aprobó para que el core re-planifique y compare — así un cliente,
/// que puede ser un AGENTE, no puede colar un orden que nadie revisó. Los
/// nombres viajan como [`Segment`] (percent-encoded, regla dura 1), no como
/// `String`: un lote de renames es exactamente donde un nombre no-UTF8 tiene
/// que sobrevivir byte a byte. El vocabulario CERRADO de
/// [`RenameCollisionKind`] son CUATRO veredictos: `internal`, `external`,
/// `absent_source` y `ambiguous_source` — este último para el directorio con
/// gemelos, donde el origen pedido se pliega sobre dos entradas y el core se
/// niega a elegir.
/// Aparece además [`FS_RENAME_BATCH_REPORT`]
/// ([`FsRenameBatchReportParams`] → [`FsRenameBatchReportResult`]), gemelo de
/// [`POLICY_UNDO_REPORT`] (#71): un lote cuyo rollback se atascó deja el
/// directorio medio renombrado, y eso no cabe en el `Failed` de una Task —
/// hace falta poder NOMBRAR el fichero que se quedó donde no debía. Por lo
/// mismo, [`PolicyUndoReportResult`] gana `batch_stuck` y
/// `compensations_lost`: el undo de sesión ya podía deshacer un lote, y hasta
/// ahora el humano remoto solo veía `blocked` — que dice «paré, el árbol está
/// consistente», justo lo contrario de lo que había pasado. Los dos campos son
/// ADITIVOS, pero no del mismo modo: `batch_stuck` se omite cuando no lo hay,
/// mientras que `compensations_lost` es opcional a la ENTRADA (`serde(default)`)
/// y obligatorio a la SALIDA — viaja siempre, incluso en cero, como los otros
/// tres contadores del informe. Eso rompe la identidad byte a byte del payload
/// limpio de un tipo que existe desde 0.16.0, y se acepta a sabiendas: un
/// contador que se omite en cero es un contador cuya ausencia es ambigua, y ese
/// informe es el único sitio donde se cuenta esto.
/// Los TOPES declarados de este bump también son contrato:
/// [`FS_RENAME_BATCH_MAX_PAIRS`] (rechaza, no recorta) y [`PLAN_HASH_LEN`], que
/// el newtype [`PlanHash`] impone al deserializar — un hash con otra forma es
/// error de params y jamás `plan_stale`.
/// Ventana N=0.36.x / N-1=0.35.x: un cliente 0.35 no conoce los métodos nuevos
/// y no los llama, y degrada el `TaskKind` nuevo a `TaskKind::Unknown` por su
/// `serde(other)`; las dos categorías de error nuevas caen en su
/// `Error::Unknown` (mismo patrón que `CursorExpired` en 0.8.0) — nada que
/// gatear en emisión, porque solo aparecen contestando a métodos que ese
/// cliente no invoca. Los campos nuevos de `PolicyUndoReportResult` los IGNORA
/// (serde no es `deny_unknown_fields`), que es exactamente lo que hacía antes
/// de que existieran: pierde el aviso, no el informe. Los dos avisos no pesan
/// igual, eso sí. Un undo con `batch_stuck` FALLA la Task, así que un cliente
/// 0.35 ve `Failed` y sabe que algo pasó, aunque no qué. `compensations_lost`,
/// en cambio, no cambia el desenlace: para ese cliente el undo dice `Completed`
/// y la sesión se bloqueará en el siguiente intento sin que nada lo haya
/// avisado — que es justo por lo que el campo dice de sí mismo ser «la única
/// señal de eso». La inversa la corta [`version_compatible`] en el handshake.
///
/// Quien reciba `MethodNotFound` a un `plugin.help` debe tratarlo como «este
/// plugin no tiene página», nunca como un fallo. La razón es que la ayuda es
/// COSMÉTICA: si el peer no implementa el método, no hay página que pintar y no
/// hay nada roto. No es que un daemon N-1 conteste eso — nunca recibe la
/// llamada, porque el handshake ya lo rechazó.
///
/// 0.37.0 (#131, diseño `2026-08-10-volumes-design.md`): método nuevo
/// [`HOST_VOLUMES`] ([`HostVolumesParams`] → [`HostVolumesResult`]), la
/// enumeración de los volúmenes del HOST — no de un provider, y por eso vive
/// fuera de la familia `fs.*` (diseño §A). [`Volume::mount`] es un [`VPath`],
/// jamás un `String` (regla dura 1: un punto de montaje no-UTF-8 tiene que
/// sobrevivir el wire byte a byte); [`VolumeKind`] gana `#[serde(other)]`
/// sobre `Unknown`, la misma forma que [`EntryKind::Other`] y
/// `TaskKind::Unknown`, así que un tipo de volumen añadido en N+1 degrada en
/// vez de romper a un cliente viejo. El método responde SOLO a una conexión
/// `User`: la tabla de montaje nombra los discos, servidores y medios
/// extraíbles del humano, y un agente bajo scope no lo necesita para nada
/// (diseño §C) — el gate vive en `norte-core::daemon` y responde
/// `Error::PolicyDenied` con el vocabulario cerrado de
/// `DenyReason::rule_id()`, jamás la regla concreta. Ventana N=0.37.x /
/// N-1=0.36.x: un cliente 0.36 jamás llama al método nuevo — nada que gatear
/// en emisión.
///
/// 0.38.0 (task V3.5 del plan de volúmenes, hallazgo de encoding-auditor
/// diferido por V3): [`Volume::label`] pasa de `Option<String>` a
/// `Option<Vec<u8>>` (base64 en el wire, módulo `label_wire` — el mismo
/// shape que [`crate::attrs::AttrValue::Bytes`], no una tercera invención).
/// Un `String` mentía o se negaba ante una etiqueta ext4/vfat no-UTF8
/// (regla dura 1); era LATENTE porque el Linux de hoy nunca puebla el
/// campo, pero V4 (macOS/Windows) sí lo hará, y arreglarlo después habría
/// roto el wire dos veces. CAMBIO INCOMPATIBLE de forma de campo dentro de
/// un método que solo existió en esta rama sin publicar (0.37.0 nunca se
/// etiquetó ni se soltó) — se trata igual que cualquier bump: ventana
/// desplazada, no ensanchada. Ventana N=0.38.x / N-1=0.37.x.
///
/// 0.39.0 (comparación de directorios, diseño
/// `2026-08-11-directory-comparison-design.md`, ADR 0048): método nuevo
/// [`FS_COMPARE`] ([`FsCompareParams`] → el [`FsTaskResult`] EXISTENTE) y
/// notificación nueva [`COMPARE_ROWS`] ([`CompareRowsBatch`]), más el
/// vocabulario que las filas hablan: [`CompareRow`], [`CompareVerdict`],
/// [`CompareCriterion`], [`CompareConfidence`], [`CompareReason`], [`Side`],
/// [`CompareCriteria`] y los topes [`COMPARE_ROWS_MAX_BATCH`] y
/// [`COMPARE_MAX_DIR_ENTRIES`]. Con el método viaja su clase de Task,
/// [`TaskKind::Compare`](crate::TaskKind::Compare) — en ESTE bump y no en el
/// siguiente, porque el `task_id` de un lote de filas correlaciona con una
/// Task que el cliente tiene que poder clasificar; un cliente 0.38 la degrada
/// a `TaskKind::Unknown` por su `serde(other)`. Bump ADITIVO PURO: ningún tipo existente
/// cambia de forma —ni gana campos, como sí hizo `PolicyUndoReportResult` en
/// 0.36.0—, así que el payload de cualquier método anterior sigue siendo byte
/// a byte el de 0.38.0.
///
/// Lo NUEVO del vocabulario, y el motivo del ADR, es que una comparación
/// DECLARA lo que su criterio se ha ganado: cada fila lleva el rung que la
/// decidió y la confianza que ese rung merece, y
/// [`CompareConfidence::Unknown`] —«el provider no puede decirlo»— es una
/// RESPUESTA, no un error. Por eso, y solo ahí, el fallback de
/// `#[serde(other)]` no se llama `Unknown` sino
/// [`CompareConfidence::Unrecognised`]: compartir nombre convertiría una
/// respuesta honesta en un desajuste de protocolo.
///
/// Ventana N=0.39.x / N-1=0.38.x: un cliente 0.38 no conoce el método nuevo y
/// no lo llama, así que jamás recibe una fila — nada que gatear en emisión. La
/// inversa la corta [`version_compatible`] en el handshake, como siempre.
///
/// 0.40.0 (sincronización de directorios, diseño
/// `2026-08-11-directory-sync-design.md`, ADR 0049): la spec 2 del mismo ítem
/// de roadmap que 0.39.0. Métodos nuevos [`SYNC_PLAN`] ([`SyncPlanParams`] → el
/// [`FsTaskResult`] EXISTENTE), [`SYNC_APPLY`] ([`SyncApplyParams`] → el mismo)
/// y [`SYNC_REPORT`] ([`SyncReportParams`] → [`SyncReportResult`]);
/// notificaciones nuevas [`SYNC_STEPS`] ([`SyncStepsBatch`]) y
/// [`SYNC_PLAN_DONE`] ([`SyncPlanDone`]); el vocabulario que los pasos hablan
/// ([`SyncStep`], [`SyncStepKind`], [`StepReversal`], [`SyncReason`],
/// [`SyncBlocker`], [`SyncBlockerKind`], [`SyncCounts`], [`SyncFailure`],
/// [`SyncFailureCause`], [`SyncMode`], [`OnUnknown`], [`RelPath`],
/// [`SyncCompareOptions`], [`DescendSide`]) y los
/// topes [`SYNC_STEPS_MAX_BATCH`], [`SYNC_PLAN_TTL_MS`],
/// [`SYNC_MAX_BLOCKERS_REPORTED`] y [`SYNC_MAX_INCLUDE`]. Con los métodos
/// viajan sus dos clases de Task,
/// [`TaskKind::SyncPlan`](crate::TaskKind::SyncPlan) y
/// [`TaskKind::Sync`](crate::TaskKind::Sync), y una categoría de error nueva,
/// [`Error::OverlappingRoots`](crate::Error::OverlappingRoots).
///
/// Bump ADITIVO. **Un tipo existente sí gana un campo**, y es el único:
/// [`FsCompareParams`] gana [`FsCompareParams::descend_orphans`], opcional y
/// omitido cuando está ausente. El motor de comparación es el mismo que un plan
/// usa por debajo, y el planificador necesita descender el huérfano del origen;
/// duplicar el método para no tocar sus params habría dejado dos comparaciones
/// que divergen. Es compatible en las dos direcciones que importan: el payload
/// de un cliente 0.39 no cambia ni un byte —el campo se omite cuando es
/// `None`— y su ausencia significa exactamente el comportamiento de 0.39.0.
/// Todo lo demás del bump es tipo nuevo, o variante nueva de un enum que ya
/// degradaba (ver el párrafo siguiente), así que el payload de cualquier otro
/// método sigue siendo byte a byte el de 0.39.0. Lo que este bump NO hace es
/// reestructurar un tipo publicado —un `flatten`, un campo que cambia de tipo
/// o de nombre—: añadir una clave opcional y omitida no es eso.
///
/// Lo NUEVO del vocabulario es que un plan DECLARA lo que puede deshacer:
/// [`StepReversal`] viaja por paso y ANTES de la aprobación, así que el humano
/// ve cuántos pasos son irreversibles cuando todavía puede decir que no, en vez
/// de leerlo en el informe. Y la asimetría del `#[serde(other)]` es
/// deliberada: el vocabulario que va daemon→client lo lleva, como el de ADR
/// 0048; [`SyncMode`] y [`OnUnknown`], que van client→daemon, NO — aceptar un
/// modo desconocido por defecto es aceptar borrar por defecto.
///
/// Ventana N=0.40.x / N-1=0.39.x: un cliente 0.39 no conoce los métodos nuevos
/// y no los llama, así que jamás recibe un paso ni un `plan_done` — nada que
/// gatear en emisión, salvo las dos clases de Task: esas SÍ le llegan sin
/// haber llamado a nada, porque [`TASK_PROGRESS`] se difunde a toda conexión
/// humana y `task.list` las devuelve en el resync. Su `serde(other)` las degrada
/// a `TaskKind::Unknown` — es la única superficie N/N-1 real de este bump, y
/// tiene golden en `task_progress.json`. Un cliente 0.39 tampoco manda
/// `descend_orphans` (no lo conoce) y su ausencia ES el comportamiento de
/// 0.39.0, así que el campo nuevo de [`FsCompareParams`] no añade superficie
/// alguna en esa dirección. La inversa —un cliente 0.40 mandando el campo a un
/// daemon 0.39, que serde ignoraría en silencio— la corta
/// [`version_compatible`] en el handshake: en 0.x un cliente con minor MAYOR
/// que el servidor no negocia.
pub const PROTOCOL_VERSION: &str = "0.40.0";

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

/// `host.volumes` — enumera los volúmenes del HOST (0.37.0, #131): mount
/// point, tipo de filesystem, kind y espacio libre/total. No es un método
/// `fs.*` a propósito (diseño §A de `2026-08-10-volumes-design.md`): un
/// volumen es una propiedad de la MÁQUINA, no de un path, así que no hay
/// provider al que preguntarle.
///
/// SOLO una conexión `User` recibe respuesta: la tabla de montaje nombra los
/// discos, servidores y medios extraíbles del humano, y una conexión de
/// agente (`agent_session` en `initialize`) la ve como `Error::PolicyDenied`
/// — información que un scope de rutas no necesita para nada (diseño §C).
pub const HOST_VOLUMES: &str = "host.volumes";

/// Tope de bytes devueltos por UNA llamada a [`FS_READ`] (antes de
/// base64). Pedir más no es error: se recorta y `eof` lo cuenta.
pub const FS_READ_MAX_CHUNK: u64 = 8 * 1024 * 1024;

/// Tope de entradas devueltas por UNA página de [`FS_LIST`] (0.8.0, ADR
/// 0017). Pedir un `limit` mayor no es error: se recorta a este techo (mismo
/// patrón que [`FS_READ_MAX_CHUNK`]), y el resto sigue por el `next_cursor`.
pub const FS_LIST_MAX_PAGE: u32 = 10_000;

/// Tope de bytes de [`PluginHelpResult::markdown`] (H3e, 0.34.0), aplicado
/// tanto al fichero fuente como al TEXTO decodificado.
///
/// Está aquí, y no solo en el host, porque es NORMATIVO: el contrato invita a
/// un receptor a dimensionar contra él, y un peer que no es Rust no puede
/// resolver `norte_help::Limits::untrusted()`. El host DEBE recortar a este
/// número — `norte-core` tiene un test que ancla los dos valores, así que no
/// pueden separarse en silencio— y cambiarlo cambia el contrato de wire, con
/// bump.
pub const PLUGIN_HELP_MAX_BYTES: usize = 64 * 1024;

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
/// `fs.rename_batch_plan` — el plan REVISABLE de un lote de renames dentro de
/// UN directorio (0.36.0). Respuesta DIRECTA: ni Task, ni journal, ni mutación.
/// El cliente manda intención (`pairs`); el core decide orden, temporales y
/// colisiones, así que un cliente — que puede ser un agente — nunca puede colar
/// un orden que el humano no vio (regla dura 7: la lógica vive en el core).
/// [`FsRenameBatchPlanParams`] → [`FsRenameBatchPlanResult`].
///
/// «No muta» NO quiere decir «inocuo». Es una LECTURA de directorio disfrazada:
/// los veredictos `External` y `AbsentSource` dicen qué nombres existen y
/// cuáles no, así que este método es un ORÁCULO de existencia y va sujeto al
/// MISMO gate de lectura que [`FS_LIST`]/[`FS_STAT`] (#80) — un agente sin
/// scope sobre `dir` recibe `PolicyDenied`, no un plan. Quien implemente el
/// dispatch no puede leer «ni Task, ni journal, ni mutación» y concluir lo
/// contrario.
pub const FS_RENAME_BATCH_PLAN: &str = "fs.rename_batch_plan";
/// `fs.rename_batch` — ejecuta un lote de renames como UNA Task
/// ([`TaskKind::RenameBatch`](crate::TaskKind)) y UNA unidad deshacible del
/// journal (0.36.0). Lleva el `plan_hash` del plan que se revisó: el
/// core re-planifica y rehúsa con [`Error::PlanStale`](crate::Error::PlanStale)
/// si el directorio se movió entre la vista previa y la ejecución, y con
/// [`Error::PlanNotExecutable`](crate::Error::PlanNotExecutable) si el plan
/// tiene colisiones. Result = el [`FsTaskResult`] existente (`{task_id}`), como
/// `fs.copy`/`fs.move`/`fs.delete`.
///
/// El hash es un token de FRESCURA, no una prueba de aprobación: es una función
/// pública y determinista de `(dir, pasos, veredictos)`, sin secreto ni estado
/// server-side, así que un cliente puede calcularlo sin haber llamado jamás a
/// [`FS_RENAME_BATCH_PLAN`]. Lo que garantiza es que se ejecuta el plan que el
/// re-plan produce AHORA, y —por la atadura al directorio— que un hash
/// aprobado para un directorio no vale contra otro. Quien decide si esta
/// llamada puede ocurrir es la policy.
pub const FS_RENAME_BATCH: &str = "fs.rename_batch";
/// `fs.rename_batch_report` — el informe de una Task de [`FS_RENAME_BATCH`]
/// (0.36.0): qué se aplicó, qué se deshizo, y —lo único que no cabe en un
/// error— QUÉ SE QUEDÓ A MEDIAS y con qué nombres.
/// [`FsRenameBatchReportParams`] → [`FsRenameBatchReportResult`].
///
/// Existe por la misma razón que [`POLICY_UNDO_REPORT`] (#71): un `Failed` en
/// `task.progress` cuenta la CAUSA, y un lote cuyo rollback se atascó deja al
/// humano con un directorio medio renombrado que hay que poder NOMBRAR. Sin
/// este método el ejecutor cumple su promesa («nunca un error pelado») solo
/// para el llamante embebido, que recibe el informe en la mano.
///
/// Snapshot: definitivo cuando la Task es terminal. El server retiene los
/// informes en un anillo acotado, así que un id demasiado viejo —o que jamás
/// fue un lote— es [`Error::NotFound`](crate::Error::NotFound). Lo ve quien
/// podría ver la Task: su dueño, o cualquier conexión humana; para todo lo
/// demás la respuesta es LA MISMA que la de un id desconocido (no se filtra
/// existencia, mismo criterio que `task.cancel`). Que un id desalojado del
/// anillo no se distinga de uno que nunca existió es deliberado: separarlos
/// obligaría a separar también el tercer caso, que es justo el que no puede
/// separarse (ADR 0042 §8).
pub const FS_RENAME_BATCH_REPORT: &str = "fs.rename_batch_report";

/// Tope de parejas de UNA petición a [`FS_RENAME_BATCH_PLAN`] o
/// [`FS_RENAME_BATCH`] (0.36.0). A diferencia de [`FS_LIST_MAX_PAGE`] NO se
/// recorta: recortar un lote de renames ejecutaría un plan distinto del pedido,
/// así que el daemon RECHAZA la petición entera con
/// [`Error::InvalidPath`](crate::Error::InvalidPath) — la misma categoría que
/// contesta la API embebida ante el mismo exceso, para que un cliente no tenga
/// que aprender dos respuestas según por dónde entre.
///
/// 4096 porque un lote humano — una temporada de serie, un carrete de fotos —
/// vive dos órdenes de magnitud por debajo, y porque el planificador es
/// superlineal sobre el listado del directorio y `fs.rename_batch_plan` es una
/// respuesta DIRECTA: el trabajo ocurre en el camino de la petición, no en una
/// Task cancelable, y `fs.*` es alcanzable por un agente. El techo es la
/// diferencia entre un lote grande y una petición que ocupa el hilo del
/// dispatch.
///
/// `usize` y no `u32` como [`FS_LIST_MAX_PAGE`]: aquellos acotan un CAMPO
/// entero del wire (`limit`, `k`), este solo se mide contra `pairs.len()` —
/// mismo criterio que [`SEARCH_HITS_MAX_BATCH`] y [`PLUGIN_HELP_MAX_BYTES`],
/// que también acotan tamaños de colección. Un `u32` aquí solo compraría un
/// `as usize` en el daemon, en el engine y en el planificador.
pub const FS_RENAME_BATCH_MAX_PAIRS: usize = 4096;

/// Longitud EXACTA de `plan_hash` (0.36.0): sha256 en hex minúscula, 64
/// caracteres. Un hash con otra forma es un error de PARAMS (`-32602`), jamás
/// [`Error::PlanStale`](crate::Error::PlanStale) — decirle «el directorio
/// cambió» a quien mandó basura es mentirle sobre el mundo.
pub const PLAN_HASH_LEN: usize = 64;
/// `search.hits` — notificación server→client con un LOTE de resultados de
/// [`FS_SEARCH`]. SOLO viaja a la conexión que lanzó la búsqueda (jamás
/// broadcast, mismo criterio direccional que
/// [`POLICY_APPROVAL_REQUIRED`]).
pub const SEARCH_HITS: &str = "search.hits";
/// Tope de entries por notificación [`SEARCH_HITS`] (coalescing
/// server-side, mismo espíritu que [`FS_LIST_MAX_PAGE`]).
pub const SEARCH_HITS_MAX_BATCH: usize = 256;

/// `fs.compare` — compara DOS árboles de directorios y emite una fila por
/// pareja (0.39.0, ADR 0048). Devuelve una Task; las filas llegan por
/// [`COMPARE_ROWS`] SOLO a la conexión que la lanzó, igual que
/// [`FS_SEARCH`]/[`SEARCH_HITS`]. Cancelable con `task.cancel`.
///
/// Result = el [`FsTaskResult`] EXISTENTE (`{task_id}`), como
/// `fs.copy`/`fs.move`/`fs.search` — cero struct nuevo para el result.
///
/// NO muta nada: no hay journal, no hay undo, no se escribe un byte (regla
/// dura 4 no aplica, y decirlo aquí evita que alguien pida una entrada que no
/// significaría nada). Lo que sí hace es LEER dos árboles enteros, y con el
/// rung de hash leer su CONTENIDO — más de lo que revela un listado —, así que
/// va sujeto al gate de lectura sobre AMBAS raíces y el hash exige además
/// scope de contenido — que hoy significa un scope vivo sobre la raíz que
/// conceda `copy` o `move`, las dos ops que no se ejecutan sin leer bytes. La
/// denegación es la categoría gruesa de siempre (`out-of-scope`), la misma que
/// para una raíz fuera de scope: si el remedio es pedir `copy`, lo dice esta
/// línea, no el error. Dos raíces que resuelven al mismo provider y path son
/// `-32602`: comparar algo contra sí mismo durante una hora no es una
/// petición, es un error de quien llama.
///
/// Los ERRORES son filas, no el final de la Task
/// ([`CompareVerdict::Error`]): un subdirectorio ilegible, un directorio por
/// encima de [`COMPARE_MAX_DIR_ENTRIES`] o una lectura que falla a mitad de
/// hash cuestan SU fila y el walk sigue. Una comparación de tres horas no
/// puede morirse en un `EACCES` de la hoja 40 000.
pub const FS_COMPARE: &str = "fs.compare";
/// `compare.rows` — notificación server→client con un LOTE de filas de
/// [`FS_COMPARE`] ([`CompareRowsBatch`]). SOLO viaja a la conexión que lanzó
/// la comparación (jamás broadcast, mismo criterio direccional que
/// [`SEARCH_HITS`]).
///
/// # Cómo se sabe si llegaron TODAS
/// Una notificación se puede perder: el daemon expulsa a un suscriptor que no
/// vacía su cola, y a diferencia de `fs.search` aquí no hay un `max_hits`
/// contra el que contar (hallazgo MINOR de protocol-guardian, revisión de C1).
/// La señal es
/// [`TaskProgress::entries_done`](crate::TaskProgress::entries_done), que en
/// una Task [`TaskKind::Compare`](crate::TaskKind::Compare) cuenta FILAS
/// emitidas: el último snapshot de `task.progress` lleva siempre estado
/// terminal y totales finales, así que un cliente compara lo que recibió con
/// ese número y sabe si le falta algo. Un plan de sincronización (spec 2) que
/// vaya a ESCRIBIR a partir de estas filas tiene que hacer esa comprobación.
///
/// CUÁNDO hacerla: el snapshot terminal puede ADELANTAR al último lote de
/// filas —la bomba de filas y la de progreso son tasks independientes que
/// escriben al mismo sink—, así que comparar en el instante en que llega el
/// terminal denuncia pérdidas que no hubo. Se compara cuando el flujo de
/// filas se ha agotado.
pub const COMPARE_ROWS: &str = "compare.rows";
/// Tope de filas por notificación [`COMPARE_ROWS`] (coalescing server-side,
/// el mismo número y el mismo motivo que [`SEARCH_HITS_MAX_BATCH`]: un millón
/// de filas no puede convertirse en un millón de frames).
pub const COMPARE_ROWS_MAX_BATCH: usize = 256;
/// Tope de entradas de UN directorio que [`FS_COMPARE`] empareja en memoria.
///
/// El emparejamiento es directorio contra directorio —`fs.list` no garantiza
/// orden, así que no hay dos streams ordenados que fusionar—, y eso es O(n) en
/// RAM sobre el directorio más ancho. Por encima de este techo la comparación
/// emite una fila [`CompareVerdict::Error`] con
/// [`CompareReason::DirTooLarge`] para ESE directorio y sigue: un directorio
/// desmesurado cuesta el directorio, jamás un OOM que se lleve las otras tres
/// horas de trabajo.
pub const COMPARE_MAX_DIR_ENTRIES: usize = 200_000;

/// `sync.plan` — planifica una sincronización de UN SOLO SENTIDO
/// (`source` → `dest`) como Task cancelable (0.40.0, ADR 0049). Result = el
/// [`FsTaskResult`] EXISTENTE (`{task_id}`), como [`FS_COMPARE`]; los pasos
/// llegan por [`SYNC_STEPS`] SOLO a la conexión que lanzó el plan, y el plan se
/// CIERRA con [`SYNC_PLAN_DONE`].
///
/// Planificar NO muta: por debajo es la comparación de [`FS_COMPARE`] con una
/// decisión por fila, así que va sujeto al MISMO gate de lectura sobre AMBAS
/// raíces, y el rung de hash exige además scope de contenido. Quien escribe es
/// [`SYNC_APPLY`], y escribe el plan RETENIDO — no lo que el cliente vuelva a
/// mandar.
///
/// Dos raíces que se SOLAPAN —iguales, o una dentro de la otra— son
/// [`Error::OverlappingRoots`](crate::Error::OverlappingRoots) y no se crea Task
/// alguna. [`FS_COMPARE`] sí permite ese par: comparar `/a` contra `/a/sub`
/// cuesta un walk y no escribe un byte. Planificar escrituras dentro del propio
/// origen no tiene esa licencia.
pub const SYNC_PLAN: &str = "sync.plan";
/// `sync.steps` — notificación server→client con un LOTE de pasos de
/// [`SYNC_PLAN`] ([`SyncStepsBatch`]). SOLO viaja a la conexión que lanzó el
/// plan (jamás broadcast, mismo criterio direccional que [`COMPARE_ROWS`]).
///
/// Cómo se sabe si llegaron TODOS: igual que en [`COMPARE_ROWS`], contando
/// contra [`TaskProgress::entries_done`](crate::TaskProgress::entries_done) —
/// que en una Task [`TaskKind::SyncPlan`](crate::TaskKind::SyncPlan) cuenta
/// PASOS emitidos— cuando el flujo se ha agotado. Aquí importa más que allí: un
/// cliente que apruebe un plan del que se perdió un lote está aprobando algo que
/// no ha visto entero. La aprobación se hace contra
/// [`SyncPlanDone::counts`], que es el total que el core sí conoce.
pub const SYNC_STEPS: &str = "sync.steps";
/// `sync.plan_done` — notificación que CIERRA un plan ([`SyncPlanDone`]): su
/// [`PlanHash`], los contadores, los bloqueos y el veredicto `executable`.
/// Llega una vez por Task de [`SYNC_PLAN`] que termine bien, y a la misma
/// conexión que los lotes.
pub const SYNC_PLAN_DONE: &str = "sync.plan_done";
/// `sync.apply` — ejecuta un plan RETENIDO ([`SyncApplyParams`] → el
/// [`FsTaskResult`] existente), como una Task
/// [`TaskKind::Sync`](crate::TaskKind::Sync) y UNA unidad deshacible del
/// journal.
///
/// **Lleva NADA MÁS que el hash**, y eso es lo que convierte «ejecuta lo que se
/// aprobó» en un invariante en vez de una promesa: no hay un segundo parámetro
/// por el que pueda colarse otra intención. El plan vive server-side, atado a la
/// CONEXIÓN que lo produjo — nadie aplica un plan que no planificó—, así que un
/// hash que no nombre un plan vivo (otra conexión, TTL vencido, daemon
/// reiniciado) es [`Error::PlanStale`](crate::Error::PlanStale). Un hash
/// MALFORMADO no: eso muere en la deserialización de [`PlanHash`] como error de
/// params, porque «esto no es un hash» y «el mundo se movió» son hechos
/// distintos.
///
/// Un plan con `executable == false` se rehúsa con
/// [`Error::PlanNotExecutable`](crate::Error::PlanNotExecutable) AUNQUE el hash
/// case.
pub const SYNC_APPLY: &str = "sync.apply";
/// `sync.report` — el informe de una Task de [`SYNC_APPLY`]
/// ([`SyncReportParams`] → [`SyncReportResult`]): qué se hizo, qué falló y con
/// qué `batch_id` deshacerlo. Gemelo de [`FS_RENAME_BATCH_REPORT`], con sus
/// mismas reglas de retención y de visibilidad.
pub const SYNC_REPORT: &str = "sync.report";

/// Tope de pasos por notificación [`SYNC_STEPS`] (coalescing server-side): el
/// mismo número y el mismo motivo que [`COMPARE_ROWS_MAX_BATCH`] — medio millón
/// de pasos no puede convertirse en medio millón de frames.
pub const SYNC_STEPS_MAX_BATCH: usize = 256;
/// Cuánto vive un plan aprobable tras cerrarse (0.40.0): diez minutos.
///
/// Es el hueco entre el plan y su ejecución, y por tanto lo que el ejecutor
/// tiene que revalidar: antes de cada paso DESTRUCTIVO se comprueba que el
/// destino sigue como el plan lo anotó. Más TTL es más ventana para que el
/// árbol cambie debajo; menos es un humano que se levanta a por café y pierde
/// el plan. Vencido, el hash es
/// [`Error::PlanStale`](crate::Error::PlanStale).
pub const SYNC_PLAN_TTL_MS: u64 = 600_000;
/// Tope de bloqueos LISTADOS en [`SyncPlanDone::blockers`]. A diferencia de
/// [`SYNC_MAX_INCLUDE`] este SÍ recorta —es una respuesta, no una petición— y
/// por eso [`SyncPlanDone::blockers_total`] viaja aparte y sin tope: un humano
/// necesita saber que hay 40 000 aunque solo se le enseñen 256.
pub const SYNC_MAX_BLOCKERS_REPORTED: usize = 256;
/// Tope de fallos LISTADOS en [`SyncReportResult::failures`]. Hoy es el mismo
/// número que [`SYNC_MAX_BLOCKERS_REPORTED`] y tiene nombre propio a propósito:
/// un tercero que dimensione la lista de fallos no debería tener que leer una
/// constante que se llama «bloqueos», y compartir el nombre les ata el futuro a
/// los dos. Como aquel, RECORTA (es una respuesta), y por eso
/// [`SyncReportResult::failed`] cuenta sin tope.
pub const SYNC_MAX_FAILURES_REPORTED: usize = SYNC_MAX_BLOCKERS_REPORTED;
/// Tope de rutas de [`SyncPlanParams::include`] (0.40.0). Como
/// [`FS_RENAME_BATCH_MAX_PAIRS`] y por el mismo motivo, NO se recorta: pasarse
/// es error de params (`-32602`). Una lista acortada en silencio planifica una
/// sincronización que el usuario no pidió, y el usuario la aprobaría creyendo
/// que la vio entera.
pub const SYNC_MAX_INCLUDE: usize = 4096;

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
///
/// Un id DESCONOCIDO es `INVALID_PARAMS` (mismo trato que
/// [`PLUGIN_SET_APPROVAL`] da a un plugin fantasma), NO una página vacía. Ojo
/// con la analogía: la apertura es la de [`PLUGIN_LIST`], pero la indulgencia
/// con los ids NO es la de [`PLUGIN_GET_CONFIG`], que ante un id desconocido
/// responde `keys: []` y jamás falla. Aquí sí falla, y a propósito: la página
/// vacía ya SIGNIFICA algo distinto ("este plugin existe y no documentó nada"),
/// así que devolverla también para un plugin inexistente borraría la diferencia
/// que un frontend necesita para decidir si su catálogo está rancio.
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

/// Por qué una cadena no es un [`PlanHash`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PlanHashError {
    /// Longitud distinta de [`PLAN_HASH_LEN`].
    #[error("plan hash must be exactly {PLAN_HASH_LEN} characters")]
    BadLength,
    /// Algún carácter no es hex MINÚSCULA (`0`-`9`, `a`-`f`).
    #[error("plan hash must be lowercase hex (0-9, a-f)")]
    NotLowercaseHex,
}

/// El hash de un plan de renames: sha256 en hex MINÚSCULA, exactamente
/// [`PLAN_HASH_LEN`] caracteres.
///
/// Es un TIPO y no un `String` por el mismo motivo que [`Segment`] lo es: la
/// regla «una forma equivocada es error de PARAMS, no
/// [`Error::PlanStale`](crate::Error::PlanStale)» escrita solo en prosa se
/// re-implementa en el dispatch del daemon, otra vez en el puente MCP y otra
/// vez allí donde un frontend devuelva el hash que recibió — y lo que una de
/// esas copias se deja es justo la minúscula, que es el detalle que hace que
/// dos escrituras del MISMO hash comparen distinto. Validado en la
/// DESERIALIZACIÓN, se cumple una vez para todas las capas: un valor inválido
/// no llega a existir.
///
/// ```
/// use norte_proto::methods::PlanHash;
/// let h = PlanHash::parse(&"ab".repeat(32)).expect("64 hex en minúscula");
/// assert_eq!(h.as_str().len(), 64);
/// assert_eq!(serde_json::to_string(&h).expect("json"), format!("\"{h}\""));
/// // Mayúsculas, longitud y basura: rechazadas en construcción...
/// assert!(PlanHash::parse(&"AB".repeat(32)).is_err());
/// assert!(PlanHash::parse("00").is_err());
/// // ...y por el wire, que es donde importa.
/// assert!(serde_json::from_str::<PlanHash>(r#""00""#).is_err());
/// assert!(serde_json::from_str::<PlanHash>(&format!(r#""{}""#, "ab".repeat(32))).is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PlanHash(String);

impl PlanHash {
    /// Valida y construye desde la forma hex. Es el ÚNICO camino: el core la
    /// usa sobre el hex que produce su hasher, y el wire la usa al
    /// deserializar.
    ///
    /// # Errors
    /// [`PlanHashError::BadLength`] si no mide [`PLAN_HASH_LEN`];
    /// [`PlanHashError::NotLowercaseHex`] si algún carácter no es `0`-`9` o
    /// `a`-`f`.
    pub fn parse(hex: &str) -> Result<Self, PlanHashError> {
        if hex.len() != PLAN_HASH_LEN {
            return Err(PlanHashError::BadLength);
        }
        if !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(PlanHashError::NotLowercaseHex);
        }
        Ok(Self(hex.to_owned()))
    }

    /// La forma hex, tal cual viaja.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PlanHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PlanHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        Self::parse(&hex).map_err(serde::de::Error::custom)
    }
}

// El serde es a mano (valida al deserializar), así que el schema también: es un
// string con patrón, y el patrón sale de `PLAN_HASH_LEN` para que no pueda
// separarse del validador.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for PlanHash {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PlanHash".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": PLAN_HASH_LEN,
            "maxLength": PLAN_HASH_LEN,
            "pattern": format!("^[0-9a-f]{{{PLAN_HASH_LEN}}}$"),
            "description": "sha256 of a rename plan's conclusions, as exactly 64 \
                            LOWERCASE hex digits. Uppercase is rejected: two \
                            spellings of one hash must not compare differently. A \
                            string of any other shape is a params error, never \
                            `plan_stale`.",
        })
    }
}

/// Un rename PEDIDO dentro de un directorio: nombres base, no rutas.
///
/// A diferencia de [`AiRenameEntry`] (donde el proveedor de IA garantiza UTF-8
/// y el engine rechaza lo demás fail-loud), aquí los nombres son [`Segment`]:
/// un lote de renames es exactamente donde un nombre no-UTF8 tiene que
/// sobrevivir byte a byte (regla dura 1).
///
/// ```
/// use norte_proto::{Segment, methods::RenamePair};
/// // Un nombre que NO es UTF-8 a la izquierda: el wire lo escapa y los bytes
/// // vuelven intactos, que es justo lo que un `String` no podría prometer.
/// let p = RenamePair {
///     from: Segment::new(b"caf\xff.txt".to_vec()).expect("segment"),
///     to: Segment::new(b"cafe.txt".to_vec()).expect("segment"),
/// };
/// let json = serde_json::to_string(&p).expect("json");
/// assert_eq!(json, r#"{"from":"caf%FF.txt","to":"cafe.txt"}"#);
/// let back: RenamePair = serde_json::from_str(&json).expect("json");
/// assert_eq!(back.from.as_bytes(), b"caf\xff.txt");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenamePair {
    /// Nombre existente en el directorio.
    pub from: Segment,
    /// Nombre propuesto.
    pub to: Segment,
}

/// Un paso del plan ORDENADO. `temp` marca un paso que es MAQUINARIA del
/// planificador (romper un ciclo), no algo que el usuario haya pedido.
///
/// Una permutación `a→b, b→a` son TRES pasos, y el temporal aparece en dos:
/// `a → .norte-rename-XXXXXXXX-0` (`temp`), `b → a`, y
/// `.norte-rename-XXXXXXXX-0 → b` (`temp`). Sin el tercero el fichero que
/// empezó como `a` se queda aparcado bajo el nombre de máquina.
///
/// Un paso NO dice de qué pareja viene, y es deliberado: los pasos son
/// MAQUINARIA opaca. Un frontend pinta las parejas del usuario — que ya tiene —
/// más los veredictos, que sí llevan `pair_index`; el ORDEN es asunto del core
/// (ver [`FS_RENAME_BATCH_PLAN`]), y un temporal parte una pareja en dos pasos
/// justo porque lo es. Si algún día un frontend demuestra que necesita el
/// mapeo, añadir el campo es un cambio ADITIVO — la forma correcta para una
/// necesidad que todavía no se ha enseñado.
///
/// ```
/// use norte_proto::{Segment, methods::RenameStep};
/// let s = RenameStep {
///     from: Segment::new(b"caf\xff.txt".to_vec()).expect("segment"),
///     to: Segment::new(b"cafe.txt".to_vec()).expect("segment"),
///     temp: false,
/// };
/// assert_eq!(serde_json::to_string(&s).expect("json"),
///            r#"{"from":"caf%FF.txt","to":"cafe.txt","temp":false}"#);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameStep {
    /// Nombre antes de este paso.
    pub from: Segment,
    /// Nombre después de este paso.
    pub to: Segment,
    /// `true` si ALGUNO de los dos lados de este paso es un nombre temporal
    /// propiedad del planificador — propiedad del PASO, no de `to`: el paso que
    /// saca al fichero del temporal lo lleva en `from` y es maquinaria igual.
    /// Un frontend jamás lo presenta como propuesta del usuario.
    pub temp: bool,
}

/// Por qué un plan no se puede ejecutar. Vocabulario CERRADO: el core jamás
/// inventa una clase.
///
/// Tolerancia N/N-1 (ADR 0004/0005, mismo patrón que
/// [`ConflictKind`](crate::ConflictKind)): una clase desconocida deserializa a
/// [`RenameCollisionKind::Unknown`] en vez de reventar el parse del plan
/// entero. Importa aunque el vocabulario nazca cerrado: un cliente 0.36 negocia
/// con un daemon 0.37, y un veredicto nuevo allí no puede dejarlo sin plan que
/// pintar — degrada a «rechazado, motivo que no entiendo».
///
/// ```
/// use norte_proto::methods::RenameCollisionKind;
/// assert_eq!(
///     serde_json::to_string(&RenameCollisionKind::AbsentSource).expect("json"),
///     r#""absent_source""#
/// );
/// let futuro: RenameCollisionKind =
///     serde_json::from_str(r#""clase_del_futuro""#).expect("json");
/// assert_eq!(futuro, RenameCollisionKind::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RenameCollisionKind {
    /// Una pareja ANTERIOR del mismo lote hace imposible esta: le tomó el
    /// destino, o le tomó el ORIGEN. Ambas cosas se miden con la equivalencia
    /// de colisiones del directorio (NFC, y plegado de caja donde la caja se
    /// pliega), no byte a byte, así que dos parejas que nombran ficheros
    /// distintos que ese directorio no distingue también caen aquí.
    Internal,
    /// El destino ya existe y NINGUNA pareja del lote lo va a quitar de en
    /// medio. Que exista una pareja cuyo origen sea ese nombre no basta: si esa
    /// pareja es nula, o su origen es un fichero GEMELO del que estorba (`café`
    /// en NFC frente a `café` en NFD), el fichero sigue ahí.
    External,
    /// El origen de la pareja no está en el directorio (el plan se construyó
    /// contra un listado rancio).
    AbsentSource,
    /// El origen no coincide EXACTAMENTE con ningún nombre del directorio y se
    /// pliega sobre DOS O MÁS a la vez — dos ficheros que las reglas de ese
    /// directorio no distinguen, típicamente `café` en NFC y en NFD conviviendo
    /// en un ext4, o `Foo` y `foo` en un listado que contradice sus propias
    /// capacidades.
    ///
    /// El origen NO falta: sobra. El core no adivina cuál de los dos se pedía,
    /// porque acertar la mitad de las veces es peor que no hacer nada. Lo que
    /// resuelve ESTE veredicto es escribir el nombre BYTE A BYTE como lo
    /// devuelve `fs.list`: con la ortografía exacta hay coincidencia exacta y
    /// el origen queda identificado.
    ///
    /// Ojo, no es una llave maestra para el directorio gemelo. Renombrar los
    /// DOS gemelos en un mismo lote sigue sin poder ser, porque las colisiones
    /// se miden plegadas y el segundo se lleva un [`Self::Internal`]. Hay que
    /// mandarlos en lotes distintos.
    AmbiguousSource,
    /// Clase de un protocolo más nuevo (fallback de deserialización).
    /// El core JAMÁS la emite.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// Un nombre rechazado más su veredicto, DIRECCIONABLE a la pareja que lo
/// provocó.
///
/// ```
/// use norte_proto::Segment;
/// use norte_proto::methods::{RenameCollision, RenameCollisionKind};
/// let c = RenameCollision {
///     pair_index: 1,
///     name: Segment::new(b"ep01.mkv".to_vec()).expect("segment"),
///     kind: RenameCollisionKind::Internal,
/// };
/// assert_eq!(serde_json::to_string(&c).expect("json"),
///            r#"{"pair_index":1,"name":"ep01.mkv","kind":"internal"}"#);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameCollision {
    /// Índice, dentro de `pairs` de la petición, de la pareja que este
    /// veredicto RECHAZA. Para `Internal` es la pareja POSTERIOR en el orden de
    /// `pairs` (la primera se señala sola si también resulta rechazada). No hay
    /// pareja «ganadora» que un frontend pueda pintar como aceptada: un plan con
    /// veredictos no ejecuta NADA.
    ///
    /// Está definido para toda clase, incluidas las futuras, y ese es su
    /// motivo: `name` cambia de significado con `kind` (ver ahí abajo), así que
    /// bajo [`RenameCollisionKind::Unknown`] un cliente no sabría qué está
    /// mirando. Con el índice siempre puede señalar la fila culpable, aunque no
    /// entienda el veredicto — que es justo lo que el fallback promete.
    pub pair_index: u32,
    /// El nombre ofensor. QUÉ nombre depende de `kind`:
    ///
    /// - `Internal` — el DESTINO que esta pareja no va a conseguir;
    /// - `External` — el fichero que ESTORBA, escrito como lo escribe el
    ///   directorio, que no tiene por qué ser como lo escribió la petición: un
    ///   gemelo NFD tapa un destino NFC y lo que el humano necesita ver es el
    ///   gemelo, no un eco de lo que ya tecleó;
    /// - `AbsentSource` / `AmbiguousSource` — el ORIGEN, tal cual lo mandó
    ///   quien pidió, porque ese es el texto que tiene que corregir.
    ///
    /// El que no depende de nada es `pair_index`.
    pub name: Segment,
    /// El veredicto.
    pub kind: RenameCollisionKind,
}

/// Params de [`FS_RENAME_BATCH_PLAN`] (0.36.0).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchPlanParams {
    /// El directorio en el que viven TODAS las parejas.
    pub dir: VPath,
    /// Los renames pedidos. Más de [`FS_RENAME_BATCH_MAX_PAIRS`] es error de
    /// params (`-32602`), no un recorte.
    #[cfg_attr(
        feature = "schema",
        schemars(extend("maxItems" = FS_RENAME_BATCH_MAX_PAIRS))
    )]
    pub pairs: Vec<RenamePair>,
}

/// Result de [`FS_RENAME_BATCH_PLAN`] (0.36.0).
///
/// **Un plan que no se puede ejecutar no lleva pasos** — el core no ordena a
/// medias un plan que no va a ejecutar (decisión 4 del diseño). El invariante
/// NORMATIVO, con su dirección exacta, está enunciado UNA vez, en `executable`.
///
/// ```
/// use norte_proto::methods::{FsRenameBatchPlanResult, PlanHash};
/// let r = FsRenameBatchPlanResult {
///     steps: vec![],
///     collisions: vec![],
///     executable: true,
///     plan_hash: PlanHash::parse(&"0".repeat(64)).expect("hex"),
/// };
/// let json = serde_json::to_value(&r).expect("json");
/// // `collisions` vacío es una lista vacía, jamás una clave ausente.
/// assert_eq!(json["collisions"], serde_json::json!([]));
/// // El hash viaja como el string desnudo, sin envoltorio del newtype.
/// assert_eq!(json["plan_hash"], serde_json::json!("0".repeat(64)));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchPlanResult {
    /// Pasos en ORDEN de ejecución, temporales incluidos. Acotado por
    /// [`FS_RENAME_BATCH_MAX_PAIRS`]: como mucho una pareja no nula más un
    /// temporal por ciclo, y un ciclo consume al menos dos parejas — el techo
    /// duro es `pairs * 3 / 2`. Sin constante propia: lo acota el tope de
    /// parejas.
    ///
    /// VACÍO siempre que `executable` sea `false` (ver el invariante en ese
    /// campo).
    pub steps: Vec<RenameStep>,
    /// Todo lo que detiene el plan; vacío cuando `executable`. Como mucho UNA
    /// entrada por pareja, así que su techo es exactamente
    /// [`FS_RENAME_BATCH_MAX_PAIRS`] — no el de `steps`, que es mayor porque
    /// los temporales añaden pasos sin añadir parejas.
    ///
    /// Es la EXPLICACIÓN, no el veredicto: quien decide si se puede ejecutar es
    /// `executable`.
    pub collisions: Vec<RenameCollision>,
    /// `true` cuando el plan se puede ejecutar tal cual. Campo NORMATIVO: el
    /// frontend deshabilita el confirmar con `!executable`, y no deduce nada de
    /// `collisions`.
    ///
    /// Es derivable de `collisions.is_empty()` HOY, y aun así manda este campo:
    /// un veredicto futuro podría parar un plan sin nombre ofensor que listar,
    /// y un cliente que dedujera de la lista lo ejecutaría.
    ///
    /// INVARIANTE (el core lo mantiene, un cliente puede asumirlo):
    /// `executable == false` ⟹ `steps` vacío, y `collisions` no vacío ⟹
    /// `executable == false`. Nótese la DIRECCIÓN: lo que vacía `steps` es
    /// `!executable`, no la presencia de veredictos — así el invariante sigue
    /// en pie para ese veredicto futuro sin nombre que listar, y un cliente que
    /// ignorase este campo y ejecutase `steps` a ciegas no tendría, en ningún
    /// caso, nada que ejecutar.
    pub executable: bool,
    /// sha256 en hex minúscula sobre las CONCLUSIONES del plan (directorio,
    /// parejas ordenadas, pasos resultantes, veredictos, banderas de caja y
    /// normalización). Devuélvelo en [`FsRenameBatchParams`]: el core
    /// re-planifica y compara. Es una cadena hex a propósito — legible en un
    /// log, sin ambigüedad de base64, sin bytes en el wire.
    ///
    /// Su tipo YA impone la forma ([`PLAN_HASH_LEN`] caracteres hex minúscula):
    /// una cadena de otra forma es un error de PARAMS que muere en la
    /// deserialización, no [`Error::PlanStale`](crate::Error::PlanStale) — «no
    /// casa» y «el directorio cambió» son cosas distintas, y contestar lo
    /// segundo a quien mandó basura le miente sobre el estado del mundo.
    pub plan_hash: PlanHash,
}

/// Params de [`FS_RENAME_BATCH`] (0.36.0).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchParams {
    /// El directorio en el que viven TODAS las parejas.
    pub dir: VPath,
    /// Los renames pedidos — la MISMA intención que produjo `plan_hash`. Tope
    /// [`FS_RENAME_BATCH_MAX_PAIRS`], igual que en el plan.
    #[cfg_attr(
        feature = "schema",
        schemars(extend("maxItems" = FS_RENAME_BATCH_MAX_PAIRS))
    )]
    pub pairs: Vec<RenamePair>,
    /// El hash del plan que el humano aprobó. Que la FORMA sea válida lo
    /// garantiza [`PlanHash`] al deserializar; que el CONTENIDO case es lo que
    /// comprueba el core, y no casar es
    /// [`Error::PlanStale`](crate::Error::PlanStale).
    pub plan_hash: PlanHash,
}

/// Un paso de un lote que se quedó APLICADO y que el core no pudo devolver a
/// su sitio (0.36.0). Dicho de otro modo: el directorio está medio renombrado
/// y esto dice dónde mirar.
///
/// Los dos lados van como [`VPath`] absoluto y no como [`Segment`]: quien lee
/// esto está buscando un fichero, y darle el nombre suelto le obligaría a
/// recomponer el directorio de la petición para poder ir a por él.
///
/// ```
/// use norte_proto::{VPath, methods::RenameStuckStep};
/// let s = RenameStuckStep {
///     from: VPath::parse("file:///fotos/a").expect("path"),
///     to: VPath::parse("file:///fotos/b").expect("path"),
///     pair_index: 0,
///     error: norte_proto::Error::Io { retryable: false },
///     journalled: true,
///     still_applied: 1,
/// };
/// let json = serde_json::to_value(&s).expect("json");
/// assert_eq!(json["to"], serde_json::json!("file:///fotos/b"));
/// assert_eq!(json["journalled"], serde_json::json!(true));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameStuckStep {
    /// El nombre que el fichero tenía ANTES del lote: donde no se pudo
    /// devolver.
    pub from: VPath,
    /// El nombre que el fichero lleva AHORA. Es el que hay que buscar.
    pub to: VPath,
    /// Índice, dentro de `pairs` de la petición, de la pareja de la que
    /// desciende el paso. Un temporal parte una pareja en dos pasos, así que
    /// dos atascos distintos pueden citar la misma pareja.
    pub pair_index: u32,
    /// Por qué se rehusó la reversa, o por qué el destino del paso es
    /// desconocido (taxonomía de errores del protocolo).
    pub error: crate::Error,
    /// ¿Hay entrada de journal detrás de este paso?
    ///
    /// Decide QUIÉN tiene que limpiar. `true`: la entrada describe el rename,
    /// así que un `policy.undo_session` posterior puede rematarlo cuando el
    /// obstáculo desaparezca. `false`: el rename surtió efecto pero su entrada
    /// nunca aterrizó (regla dura 4 — el journal no sabe que ocurrió), así que
    /// ningún undo lo encontrará y solo un humano puede deshacerlo.
    pub journalled: bool,
    /// Cuántos pasos de ese lote siguen aplicados, este incluido.
    pub still_applied: u64,
}

/// Params de [`FS_RENAME_BATCH_REPORT`] (0.36.0).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchReportParams {
    /// Task del lote cuyo informe se pide (la de [`FsTaskResult::task_id`]
    /// que devolvió [`FS_RENAME_BATCH`]).
    pub task_id: TaskId,
}

/// Result de [`FS_RENAME_BATCH_REPORT`] (0.36.0): qué hizo el lote.
///
/// Una corrida limpia deja `applied` == el número de pasos del plan y todo lo
/// demás en cero/ausente. **Cualquier otra forma es el ejecutor diciendo la
/// verdad sobre un directorio que no pudo dejar como lo encontró** — léelo
/// aunque la Task haya fallado, y sobre todo cuando diga `cancelled`.
///
/// ```
/// use norte_proto::methods::FsRenameBatchReportResult;
/// let r = FsRenameBatchReportResult {
///     applied: 3,
///     rolled_back: 0,
///     failed_pair: None,
///     stuck: None,
///     uncertain: None,
///     compensations_lost: 0,
/// };
/// let json = serde_json::to_value(&r).expect("json");
/// // Lo ausente se OMITE: un lote limpio no manda tres `null`.
/// assert_eq!(json.get("stuck"), None);
/// assert_eq!(json["applied"], serde_json::json!(3));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchReportResult {
    /// Pasos aplicados Y journalizados.
    pub applied: u64,
    /// Pasos que el rollback consiguió devolver.
    pub rolled_back: u64,
    /// La pareja pedida cuyo paso falló, si falló alguno.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_pair: Option<u32>,
    /// El rollback se rehusó AQUÍ y paró: el directorio está medio
    /// renombrado.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck: Option<RenameStuckStep>,
    /// Un paso cuyo provider reportó fallo y que después no se pudo SONDEAR,
    /// así que si surtió efecto o no es desconocido. No se asumió ninguna de
    /// las dos cosas: `to` es dónde mirar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertain: Option<RenameStuckStep>,
    /// Reversas que se APLICARON pero cuya entrada compensatoria no se pudo
    /// escribir. El árbol volvió; el journal sigue diciendo que el rename
    /// directo está en vigor, así que un `policy.undo_session` posterior
    /// llegará a esa entrada y se BLOQUEARÁ ahí. Un valor distinto de cero es
    /// el único aviso de eso.
    #[serde(default)]
    pub compensations_lost: u64,
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

/// What kind of volume a [`Volume`] is, over the wire (0.37.0, #131). Mirrors
/// `norte_core::volumes::VolumeKind` field for field, but this crate cannot
/// depend on `norte-core` (the dependency runs the other way), so the two
/// stay in sync by convention plus the daemon-side mapping test, not by a
/// shared type.
///
/// `#[serde(other)]` on `Unknown` is the same forward-compat shape
/// [`EntryKind::Other`] and `TaskKind::Unknown` already use: a kind this
/// client has never heard of degrades to `Unknown` on decode instead of
/// failing the whole `host.volumes` response.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeKind {
    /// A local, non-removable disk.
    Fixed,
    /// A local disk the OS considers removable (USB, SD, …).
    Removable,
    /// A network filesystem (NFS, CIFS/SMB, sshfs, …).
    Network,
    /// A synthetic/virtual filesystem (`proc`, `tmpfs`, …), hidden from the
    /// picker unless it asks for everything.
    Pseudo,
    /// The platform could not tell, or this decoder does not recognize a
    /// kind a newer peer sent (`#[serde(other)]`).
    #[serde(other)]
    Unknown,
}

/// (De)serializes [`Volume::label`] as base64 text, or an absent key —
/// mirroring [`crate::attrs::AttrValue::Bytes`]'s wire shape (§ its
/// rustdoc) rather than inventing a third byte-carrying representation next
/// to it and [`VPath`]. `VPath` does not fit here: it is a scheme +
/// authority + segment structure for a PATH, and a label is a flat blob with
/// none of that shape to preserve.
///
/// Unlike `AttrValue::Bytes`, malformed base64 does not need the "whole cell
/// degrades to `Unknown`" ceremony ADR 0039 built for third-party plugin
/// data: a `Volume` is produced by the host service, not a WASM guest, and a
/// broken `label` is cheaply dropped to `None` — the rest of the `Volume`
/// (`mount` above all) is still perfectly usable, so losing the WHOLE entry
/// over one bad field would be a worse failure than the one it avoids.
///
/// A real ext4/vfat/NTFS label is at most a few dozen bytes, but nothing
/// upstream enforces that on the wire — so a decoded payload OVER
/// [`crate::attrs::ATTR_BYTES_MAX`] bytes degrades exactly like an
/// undecodable one (encoding-auditor MINOR, V3.5 review): the same cap
/// `AttrValue::Bytes` already publishes, reused rather than a second
/// bespoke number for what is the same class of payload.
mod label_wire {
    use serde::{Deserialize as _, Deserializer, Serializer};

    use crate::attrs::{ATTR_BYTES_MAX, decode_bytes_b64_lenient, encode_bytes_b64};

    // `ref_option` (pedantic) wants `Option<&T>` here, but serde's `with =`
    // codegen calls this with `&self.label` — the field's actual type,
    // `&Option<Vec<u8>>` — not something this function gets to choose.
    #[allow(clippy::ref_option)]
    pub(super) fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(bytes) => s.serialize_str(&encode_bytes_b64(bytes)),
            None => s.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        let raw = Option::<String>::deserialize(d)?;
        // Undecodable OR oversized base64 costs this ONE field, not the
        // `Volume` it is on — see the module's rustdoc.
        Ok(raw.and_then(|s| {
            decode_bytes_b64_lenient(&s).filter(|bytes| bytes.len() <= ATTR_BYTES_MAX)
        }))
    }
}

/// One volume the host has mounted, over the wire (0.37.0, #131; `label`'s
/// current byte shape is 0.38.0, see its own doc).
///
/// # Non-UTF-8 mount points and labels
/// `mount` is a [`VPath`], never a `String`: rule 1 requires a mount point's
/// bytes to survive the wire exactly, and a `String` cannot hold bytes that
/// are not valid UTF-8 (`/proc/mounts` places no such restriction on what a
/// filesystem may be mounted at). [`Volume::label`] carries the same hazard
/// for a different reason — see its own rustdoc for the per-platform
/// breakdown, Windows above all.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    /// Mount point. A [`VPath`] — see the type's rustdoc for why this is
    /// never a `String`.
    pub mount: VPath,
    /// What the OS or the filesystem calls it, when it says — raw bytes,
    /// base64 on the wire (`label_wire`, this module), never a `String`. No platform
    /// this crate targets promises the label is UTF-8, and Windows'
    /// `GetVolumeInformationW` returns UTF-16 rather than bytes at all — see
    /// `norte_core::volumes::Volume::label`'s rustdoc for the full
    /// per-platform breakdown (that crate cannot be linked from here, the
    /// dependency runs the other way, hence the plain-text pointer). In
    /// short: Linux is `None` today (`/proc/mounts` carries no label);
    /// macOS and Windows are V4 and unverified, and Windows crosses as
    /// WTF-8 — the same encoding CONVENTION `norte_vfs_local`'s (private)
    /// `native_path`/`wtf8` modules already apply to Windows path segments,
    /// not literally reusable code (this crate cannot depend on
    /// `norte-vfs-local` either) — so an unpaired surrogate a FAT/NTFS
    /// label can legally contain survives instead of being replaced with
    /// `U+FFFD` before rule 1 gets a say.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "label_wire")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<String>", extend("contentEncoding" = "base64"))
    )]
    pub label: Option<Vec<u8>>,
    /// `ext4`, `apfs`, `ntfs`, `nfs4`… as the platform spells it.
    pub fs_type: String,
    /// What kind of volume this is, so far as the platform can tell.
    pub kind: VolumeKind,
    /// `None` when the filesystem did not answer its space query in time —
    /// never a zero standing in for "unknown" (design §A: a `0` here would
    /// read as "full").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    /// `None` when the filesystem did not answer its space query in time —
    /// see [`Volume::total_bytes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_bytes: Option<u64>,
    /// Whether the mount is read-only.
    pub read_only: bool,
}

/// Params de [`HOST_VOLUMES`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostVolumesParams {
    /// The picker's "show everything" toggle: with this `false` (the
    /// default), a [`Volume`] classified [`VolumeKind::Pseudo`] is left out.
    #[serde(default)]
    pub include_pseudo: bool,
}

/// Result de [`HOST_VOLUMES`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostVolumesResult {
    /// The host's volumes, in no particular guaranteed order.
    pub volumes: Vec<Volume>,
}

/// What a comparison concluded about ONE pair (0.39.0, ADR 0048).
///
/// The verdict says WHAT, [`CompareCriterion`] says WHICH RUNG decided it and
/// [`CompareConfidence`] says WHAT THAT IS WORTH. The three travel together on
/// every [`CompareRow`] precisely because `Same` alone is not an answer: a
/// `Same` earned by comparing bytes and a `Same` earned by two dates that are
/// two seconds apart are different facts, and a client that cannot tell them
/// apart cannot tell the user either.
///
/// `#[serde(other)]` on `Unknown` is the forward-compat shape
/// [`EntryKind::Other`] and [`VolumeKind::Unknown`]
/// already use: a verdict an N+1 daemon adds degrades to `Unknown` on decode
/// instead of failing the whole batch of rows.
///
/// ```
/// use norte_proto::methods::CompareVerdict;
/// assert_eq!(
///     serde_json::to_string(&CompareVerdict::OnlyLeft).expect("json"),
///     r#""only_left""#
/// );
/// // Un veredicto de un daemon más nuevo degrada; NO tira el lote entero.
/// let futuro: CompareVerdict = serde_json::from_str(r#""conflicted""#).expect("degrada");
/// assert_eq!(futuro, CompareVerdict::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum CompareVerdict {
    /// Los dos lados se tienen por iguales bajo el criterio que decidió.
    Same,
    /// Los dos lados difieren bajo el criterio que decidió.
    Different,
    /// Solo existe a la izquierda. `right` es `None` (ver
    /// [`CompareRow::sides_are_consistent`]).
    OnlyLeft,
    /// Solo existe a la derecha. `left` es `None`.
    OnlyRight,
    /// Mismo nombre, [`EntryKind`] distinto: un fichero
    /// contra un directorio no es una diferencia de contenido, es otra cosa.
    TypeMismatch,
    /// Dos entradas de UN MISMO lado colapsan a la misma clave de
    /// emparejamiento (`README`/`readme` contra APFS, NFC/NFD en ext4): no se
    /// emparejan, se REPORTAN — con [`CompareRow::reason`] poblado. Es justo
    /// la colisión que una sincronización posterior tiene que ver ANTES de
    /// escribir nada.
    ///
    /// # Forma de la fila (normativa)
    /// La colisión es de UN lado, así que la fila también lo es: se emite **una
    /// fila por entrada implicada**, con esa entrada en el campo de SU lado, el
    /// otro lado en `None`, y [`CompareRow::side`] nombrando dónde ocurrió. Dos
    /// nombres que colapsan son DOS filas, jamás una que los junte y jamás una
    /// deduplicada — perder un nombre aquí es perder exactamente el fichero
    /// sobre el que un plan de sincronización iba a escribir.
    ///
    /// Nada en el wire lo impide (`sides_are_consistent` exime a este
    /// veredicto: no hay regla de lados que un cliente pueda comprobar, y
    /// afirmar una haría desconfiar de filas legítimas de un daemon N+1), así
    /// que es contrato de quien produce las filas y está congelado en los
    /// goldens `ambiguous_*` de `compare_row.json`. Si algún día hiciera falta
    /// enseñar las dos entradas colisionadas en UNA fila, el wire necesita un
    /// campo nuevo: los dos que hay son LADOS, no miembros de una colisión
    /// (hallazgo MAJOR de protocol-guardian, revisión de C1).
    Ambiguous,
    /// El walk no pudo responder por esta entrada (listado ilegible,
    /// directorio por encima de [`COMPARE_MAX_DIR_ENTRIES`], lectura fallida a
    /// mitad de hash). Lleva [`CompareRow::reason`] y, cuando aplica a un lado,
    /// [`CompareRow::side`]. NO termina la Task.
    Error,
    /// Veredicto que este decodificador no conoce (`#[serde(other)]`): un
    /// daemon N+1 lo emitió, este cliente lo pinta como «no sé qué dice».
    #[serde(other)]
    Unknown,
}

/// Qué RUNG de la cascada decidió una fila (0.39.0, ADR 0048).
///
/// La cascada va de barato a caro y para en el primer rung que decide, así que
/// este campo es también «hasta dónde hubo que llegar». Sin él, `Different` no
/// dice si se comparó un tamaño o 40 GB de bytes.
///
/// ```
/// use norte_proto::methods::CompareCriterion;
/// assert_eq!(
///     serde_json::to_string(&CompareCriterion::LinkTarget).expect("json"),
///     r#""link_target""#
/// );
/// let futuro: CompareCriterion = serde_json::from_str(r#""etag""#).expect("degrada");
/// assert_eq!(futuro, CompareCriterion::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum CompareCriterion {
    /// Un lado no existe: no hay nada más que comparar.
    ///
    /// Es TAMBIÉN el criterio de las filas que no decidió rung alguno — un
    /// listado ilegible, un directorio por encima de
    /// [`COMPARE_MAX_DIR_ENTRIES`], una colisión de emparejamiento —, por
    /// convención y no porque sea cierto: el campo no es opcional (una clave
    /// nullable en cada fila de un camino que lleva millones no se paga por un
    /// valor sobre el que nadie ramifica) y `Unknown` es el fallback de decode,
    /// que el core no emite jamás. En esas filas lo que informa es
    /// `verdict: error`/`ambiguous` más [`CompareRow::reason`]; el criterio no
    /// se lee. Ver ADR 0048, consecuencias negativas.
    Presence,
    /// Los [`EntryKind`] difieren.
    Kind,
    /// Destinos de symlink comparados COMO BYTES (jamás se siguen: sin
    /// seguimiento no hace falta detectar ciclos, y un enlace cuyo destino
    /// cambió es una diferencia real).
    LinkTarget,
    /// Tamaños en bytes.
    Size,
    /// Fechas de modificación, bajo la tolerancia de la petición
    /// ([`FsCompareParams::mtime_tolerance_ms`]).
    Mtime,
    /// sha256 en streaming de los dos lados. Solo corre si el llamante lo pidió
    /// y solo alcanza a las parejas que los rungs baratos dieron por iguales.
    Hash,
    /// Criterio que este decodificador no conoce (`#[serde(other)]`).
    #[serde(other)]
    Unknown,
}

/// Cuánto vale el veredicto de una fila (0.39.0, ADR 0048).
///
/// El vocabulario central del ítem: una comparación DECLARA lo que su criterio
/// se ha ganado. Un tamaño distinto PRUEBA bytes distintos (`Certain`); una
/// fecha distinta solo lo SUGIERE (`Probable`); un provider que no puede
/// contestar deja `Unknown`, que es una respuesta honesta y no un fallo.
///
/// # Por qué el fallback aquí se llama `Unrecognised`
/// En [`CompareVerdict`] y [`CompareCriterion`] el fallback de
/// `#[serde(other)]` se llama `Unknown`, la convención de la casa. Aquí
/// `Unknown` ya es un VALOR con significado — «el provider no puede decirlo» —
/// y «un peer más nuevo dijo algo que no conozco» es un hecho DISTINTO.
/// Compartir nombre convertiría una respuesta honesta en un desajuste de
/// protocolo, y al revés.
///
/// ```
/// use norte_proto::methods::CompareConfidence;
/// // `unknown` es un valor REAL del vocabulario...
/// let honesto: CompareConfidence = serde_json::from_str(r#""unknown""#).expect("valor real");
/// assert_eq!(honesto, CompareConfidence::Unknown);
/// // ...y el fallback forward-compat es OTRO.
/// let futuro: CompareConfidence = serde_json::from_str(r#""quantum""#).expect("degrada");
/// assert_eq!(futuro, CompareConfidence::Unrecognised);
/// assert_ne!(honesto, futuro);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum CompareConfidence {
    /// El criterio PRUEBA su veredicto (presencia, kind, tamaño distinto,
    /// destino de symlink, hash).
    Certain,
    /// El criterio SUGIERE su veredicto sin probarlo (mtime: dos ficheros con
    /// la misma fecha pueden tener bytes distintos, y al revés).
    Probable,
    /// El provider no puede decirlo: no hay tamaño, no hay fecha fiable (un
    /// archivo comprimido, un object store cuyo `ETag` solo a veces es un
    /// hash).
    /// Es una RESPUESTA, no un error.
    Unknown,
    /// Confianza que este decodificador no conoce (`#[serde(other)]`). NO es
    /// [`CompareConfidence::Unknown`] — ver el apartado de arriba.
    #[serde(other)]
    Unrecognised,
}

/// Por qué una fila es [`CompareVerdict::Ambiguous`] o
/// [`CompareVerdict::Error`] (0.39.0, ADR 0048). Vocabulario CERRADO: el core
/// jamás inventa una clase.
///
/// ```
/// use norte_proto::methods::CompareReason;
/// assert_eq!(
///     serde_json::to_string(&CompareReason::DirTooLarge).expect("json"),
///     r#""dir_too_large""#
/// );
/// let futuro: CompareReason = serde_json::from_str(r#""solar_flare""#).expect("degrada");
/// assert_eq!(futuro, CompareReason::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum CompareReason {
    /// Dos nombres de un mismo lado que solo difieren en mayúsculas colapsan
    /// porque el OTRO lado no distingue caja.
    CaseFold,
    /// Dos nombres de un mismo lado que colapsan al normalizar a NFC (el
    /// clásico NFD de macOS junto a su gemelo NFC).
    Normalization,
    /// No se pudo LEER lo que hacía falta para comparar (permisos, E/S): un
    /// directorio que no se dejó listar, o una entrada que no se dejó `stat`ear
    /// cuando el rung de tamaño o el de fecha necesitaba su dato. Ver
    /// [`CompareRow::side`], que nombra el lado que falló.
    ///
    /// No es un `Unknown` de confianza: `Unknown` es «el provider no puede
    /// contestar esta pregunta» y viaja con un veredicto `Same`; esto es «no se
    /// pudo preguntar», y viaja con [`CompareVerdict::Error`].
    Unreadable,
    /// El directorio supera [`COMPARE_MAX_DIR_ENTRIES`] entradas.
    DirTooLarge,
    /// Una lectura falló a mitad del rung de hash. Ver [`CompareRow::side`].
    ReadFailed,
    /// Motivo que este decodificador no conoce (`#[serde(other)]`).
    #[serde(other)]
    Unknown,
}

/// Un lado de la comparación (0.39.0, ADR 0048): el panel izquierdo es el que
/// lanzó la comparación.
///
/// ```
/// use norte_proto::methods::Side;
/// assert_eq!(serde_json::to_string(&Side::Right).expect("json"), r#""right""#);
/// let futuro: Side = serde_json::from_str(r#""middle""#).expect("degrada");
/// assert_eq!(futuro, Side::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// El lado izquierdo: el panel desde el que se lanzó la comparación.
    Left,
    /// El lado derecho.
    Right,
    /// Lado que este decodificador no conoce (`#[serde(other)]`).
    #[serde(other)]
    Unknown,
}

/// Qué rungs de la cascada corren (0.39.0, ADR 0048).
///
/// El default es el que decide si comparar dos árboles LEE CONTENIDO: `size` y
/// `mtime` puestos, `hash` NO. Un usuario que no pidió hashear un terabyte por
/// SFTP no debe acabar haciéndolo, así que el rung caro es siempre explícito.
///
/// Un objeto PARCIAL en el wire completa desde ese mismo default en vez de
/// fallar: un peer que solo quiere encender el hash manda `{"hash": true}`.
///
/// ```
/// use norte_proto::methods::CompareCriteria;
/// let d = CompareCriteria::default();
/// assert!(d.size && d.mtime && !d.hash, "el rung caro es opt-in");
/// let parcial: CompareCriteria = serde_json::from_str(r#"{"hash":true}"#).expect("parcial");
/// assert_eq!(parcial, CompareCriteria { size: true, mtime: true, hash: true });
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct CompareCriteria {
    /// Comparar tamaños. Default `true`.
    pub size: bool,
    /// Comparar fechas de modificación bajo tolerancia. Default `true`.
    pub mtime: bool,
    /// Comparar sha256 del contenido de las parejas que los rungs baratos
    /// dieron por iguales. Default `false`: LEE los dos ficheros enteros, y en
    /// el daemon exige además scope de CONTENIDO.
    pub hash: bool,
}

impl Default for CompareCriteria {
    fn default() -> Self {
        Self {
            size: true,
            mtime: true,
            hash: false,
        }
    }
}

/// Tolerancia de mtime por defecto: 2000 ms, la regla FAT — la granularidad
/// real más ancha que un filesystem de los que este árbol toca puede tener.
fn default_mtime_tolerance_ms() -> u32 {
    2000
}

/// UNA fila de una comparación (0.39.0, ADR 0048): una pareja, su veredicto, y
/// cuánto vale ese veredicto.
///
/// La fila se emite FINAL: ninguna se corrige después, así que el wire no
/// necesita actualizaciones de fila ni el panel reconciliación.
///
/// Lleva los dos [`Entry`] ENTEROS y no dos paths porque el panel pinta
/// tamaño, fecha y los BYTES del nombre de cada lado, y el walk acaba de
/// listar ambos directorios: refetchear por fila convertiría un listado en N
/// `fs.stat` sobre dos providers, con el árbol ya cambiando debajo.
///
/// ```
/// use norte_proto::methods::{
///     CompareConfidence, CompareCriterion, CompareRow, CompareVerdict,
/// };
/// use norte_proto::{Entry, EntryKind, VPath};
/// let izquierda = Entry {
///     path: VPath::parse("file:///a/informe%FF%FE.dat").expect("path"),
///     kind: EntryKind::File,
///     size: Some(7),
///     mtime_ms: None,
///     attrs: Default::default(),
/// };
/// let row = CompareRow {
///     id: 1,
///     left: Some(izquierda),
///     right: None,
///     verdict: CompareVerdict::OnlyLeft,
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     newer: None,
///     reason: None,
///     side: None,
/// };
/// assert!(row.sides_are_consistent() && row.reason_is_consistent());
/// // Lo ausente NO viaja: ni `null` ni clave (comprobado sobre las CLAVES
/// // del objeto, no por substring: un path puede contener "right").
/// let json = serde_json::to_string(&row).expect("json");
/// let obj: serde_json::Value = serde_json::from_str(&json).expect("objeto");
/// for ausente in ["right", "newer", "reason", "side"] {
///     assert!(obj.get(ausente).is_none(), "{ausente} no debe viajar: {json}");
/// }
/// // Y el nombre no-UTF8 vuelve byte a byte (regla dura 1).
/// let back: CompareRow = serde_json::from_str(&json).expect("json");
/// assert_eq!(back, row);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareRow {
    /// Identificador monótono dentro de UNA comparación. La selección del
    /// panel se ancla a él: un filtro esconde filas, jamás las renumera.
    pub id: u64,
    /// La entrada del lado izquierdo, si la hay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<Entry>,
    /// La entrada del lado derecho, si la hay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<Entry>,
    /// Qué concluyó la comparación.
    pub verdict: CompareVerdict,
    /// Qué rung lo decidió.
    pub criterion: CompareCriterion,
    /// Cuánto vale ese veredicto.
    pub confidence: CompareConfidence,
    /// Qué lado es más NUEVO, cuando la fecha decidió la fila. Nada de esta
    /// spec lo lee: la spec 2 (el plan de sincronización) lo necesita para
    /// proponer una dirección, y producirlo aquí no cuesta nada.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newer: Option<Side>,
    /// El porqué, para los DOS veredictos que tienen porqué
    /// ([`CompareVerdict::Ambiguous`] y [`CompareVerdict::Error`]). `None`
    /// para cualquier otro — ver [`CompareRow::reason_is_consistent`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<CompareReason>,
    /// El lado al que aplica `reason`, cuando aplica a uno solo: una lectura
    /// que falló únicamente a la izquierda.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
}

impl CompareRow {
    /// ¿Concuerdan veredicto y lados presentes?
    ///
    /// La invariante que el wire no sabe expresar: [`CompareVerdict::OnlyLeft`]
    /// implica `right: None`, y `Same`/`Different`/`TypeMismatch` implican los
    /// dos lados. NO es un rechazo de `Deserialize` a propósito: una fila
    /// malformada tiene que degradar como una celda de atributo mala, no matar
    /// el lote entero. El daemon lo afirma en sus tests; un cliente lo usa para
    /// decidir si se fía de la fila.
    ///
    /// [`CompareVerdict::Ambiguous`], [`CompareVerdict::Error`] y
    /// [`CompareVerdict::Unknown`] no tienen regla que romper: el primero
    /// nombra una colisión de UN lado, el segundo puede no tener entrada que
    /// enseñar, y del tercero —un veredicto de un daemon N+1— este cliente no
    /// sabe nada. Inventarles una regla haría que un cliente N-1 desconfiara de
    /// filas legítimas.
    ///
    /// ```
    /// # use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareRow, CompareVerdict};
    /// # use norte_proto::{Entry, EntryKind, VPath};
    /// # fn entry() -> Entry {
    /// #     Entry { path: VPath::parse("file:///a").expect("path"), kind: EntryKind::File,
    /// #             size: None, mtime_ms: None, attrs: Default::default() }
    /// # }
    /// # fn row(verdict: CompareVerdict, left: Option<Entry>, right: Option<Entry>) -> CompareRow {
    /// #     CompareRow { id: 1, left, right, verdict, criterion: CompareCriterion::Presence,
    /// #                  confidence: CompareConfidence::Certain, newer: None, reason: None, side: None }
    /// # }
    /// assert!(row(CompareVerdict::OnlyLeft, Some(entry()), None).sides_are_consistent());
    /// assert!(!row(CompareVerdict::OnlyLeft, Some(entry()), Some(entry())).sides_are_consistent());
    /// assert!(!row(CompareVerdict::Same, Some(entry()), None).sides_are_consistent());
    /// ```
    #[must_use]
    pub fn sides_are_consistent(&self) -> bool {
        let (l, r) = (self.left.is_some(), self.right.is_some());
        match self.verdict {
            CompareVerdict::OnlyLeft => l && !r,
            CompareVerdict::OnlyRight => r && !l,
            CompareVerdict::Same | CompareVerdict::Different | CompareVerdict::TypeMismatch => {
                l && r
            }
            CompareVerdict::Ambiguous | CompareVerdict::Error | CompareVerdict::Unknown => true,
        }
    }

    /// ¿Concuerdan veredicto y motivo?
    ///
    /// `reason` es `Some` para EXACTAMENTE dos veredictos —
    /// [`CompareVerdict::Ambiguous`] y [`CompareVerdict::Error`] — y `None`
    /// para el resto: en cualquier otro sitio sería ruido que un cliente
    /// tendría que adivinar. [`CompareVerdict::Unknown`] queda EXENTO por el
    /// mismo motivo que en [`CompareRow::sides_are_consistent`]: un veredicto
    /// que este cliente no conoce puede legítimamente traer motivo.
    ///
    /// ```
    /// # use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareReason, CompareRow, CompareVerdict};
    /// # fn row(verdict: CompareVerdict, reason: Option<CompareReason>) -> CompareRow {
    /// #     CompareRow { id: 1, left: None, right: None, verdict, criterion: CompareCriterion::Presence,
    /// #                  confidence: CompareConfidence::Unknown, newer: None, reason, side: None }
    /// # }
    /// assert!(row(CompareVerdict::Ambiguous, Some(CompareReason::CaseFold)).reason_is_consistent());
    /// assert!(!row(CompareVerdict::Ambiguous, None).reason_is_consistent());
    /// assert!(!row(CompareVerdict::Same, Some(CompareReason::CaseFold)).reason_is_consistent());
    /// ```
    #[must_use]
    pub fn reason_is_consistent(&self) -> bool {
        match self.verdict {
            CompareVerdict::Ambiguous | CompareVerdict::Error => self.reason.is_some(),
            CompareVerdict::Unknown => true,
            _ => self.reason.is_none(),
        }
    }
}

/// Params de [`FS_COMPARE`] (0.39.0, ADR 0048).
///
/// ```
/// use norte_proto::methods::{CompareCriteria, FsCompareParams};
/// // Lo MÍNIMO que hay que mandar: dos raíces. Todo lo demás tiene default.
/// let p: FsCompareParams =
///     serde_json::from_str(r#"{"left":"file:///a","right":"file:///b"}"#).expect("params");
/// assert_eq!(p.criteria, CompareCriteria::default());
/// assert_eq!(p.mtime_tolerance_ms, 2000);
/// assert!(p.max_depth.is_none() && !p.follow_symlinks && p.descend_orphans.is_none());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCompareParams {
    /// Raíz izquierda: el panel que lanzó la comparación.
    pub left: VPath,
    /// Raíz derecha. Si resuelve al mismo provider y path que `left`, la
    /// petición es `-32602` y no se crea Task alguna.
    pub right: VPath,
    /// Qué rungs corren. Ausente = [`CompareCriteria::default`].
    #[serde(default)]
    pub criteria: CompareCriteria,
    /// Profundidad máxima del descenso, contando la raíz como 0. `None` = sin
    /// límite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Tolerancia del rung de mtime en milisegundos. Default 2000 (la regla
    /// FAT). Es parámetro de la PETICIÓN y no una capability del provider:
    /// tocar el wire de `Capabilities` por un único consumidor no lo valía.
    ///
    /// `u32` y no `i64` (hallazgo MAJOR de protocol-guardian, revisión de C1)
    /// aunque [`Entry::mtime_ms`](crate::Entry::mtime_ms) sí sea `i64`: una
    /// tolerancia NEGATIVA hace que `|Δ| > tolerancia` sea cierto para toda
    /// pareja, así que un signo de más en la petición de un cliente
    /// convertiría dos árboles idénticos en un árbol entero de `Different`
    /// —una respuesta callada y equivocada en el camino caliente— en vez de un
    /// error. Con `u32` lo rechaza el DESERIALIZADOR, que es más fuerte que
    /// cualquier chequeo que el handler pueda olvidar, y el techo (49 días)
    /// sobra para la granularidad de cualquier filesystem.
    #[serde(default = "default_mtime_tolerance_ms")]
    pub mtime_tolerance_ms: u32,
    /// Seguir symlinks. Default `false`, y hoy es lo único que el core
    /// implementa: los destinos se comparan COMO BYTES
    /// ([`CompareCriterion::LinkTarget`]), lo que hace innecesaria la detección
    /// de ciclos.
    #[serde(default)]
    pub follow_symlinks: bool,
    /// Descender en los directorios que existen SOLO en este lado (0.40.0).
    /// Ausente —el default, y todo lo que 0.39.0 sabía hacer— emite UNA fila
    /// por el huérfano y no lo recorre.
    ///
    /// Un lado, nunca los dos: el tipo lo impone, y el motivo está en
    /// [`SyncCompareOptions::descend_orphans`], que es el mismo campo visto
    /// desde un plan. Aquí, en cambio, **sí es del llamante**: «enséñame todo
    /// lo que solo está a la izquierda, no solo la punta» es una petición
    /// legítima de comparación.
    ///
    /// Es un [`DescendSide`] y no un [`Side`]: un lado mal escrito muere en el
    /// DESERIALIZADOR de cualquier peer (`-32602`) en vez de degradar a
    /// [`Side::Unknown`] —que no es ningún lado— y servir en silencio un
    /// conjunto de filas distinto del pedido. El motivo largo está en
    /// [`DescendSide`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descend_orphans: Option<DescendSide>,
}

/// Un LOTE de filas de [`COMPARE_ROWS`] (0.39.0, ADR 0048). Acotado por
/// [`COMPARE_ROWS_MAX_BATCH`] y coalescido server-side, el mismo contrato que
/// [`SearchHits`].
///
/// ```
/// use norte_proto::TaskId;
/// use norte_proto::methods::CompareRowsBatch;
/// let b = CompareRowsBatch { task_id: TaskId::new(7), rows: vec![] };
/// assert_eq!(
///     serde_json::to_string(&b).expect("json"),
///     r#"{"task_id":7,"rows":[]}"#
/// );
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareRowsBatch {
    /// Task dueña (correlación con `fs.compare` → `task_id`).
    pub task_id: TaskId,
    /// Las filas de este lote, en el orden en que el walk las produjo. Nunca
    /// más de [`COMPARE_ROWS_MAX_BATCH`].
    pub rows: Vec<CompareRow>,
}

/// Una ruta RELATIVA a las dos raíces de un plan (0.40.0, ADR 0049): cero o
/// más [`Segment`], en BYTES (regla dura 1). La secuencia vacía es la RAÍZ.
///
/// Wire: los segmentos percent-encodeados (ADR 0001, el mismo códec de
/// [`VPath`]) unidos por `/`, así que la raíz es la cadena vacía. Un `rel` que
/// nombra tres niveles cuesta una cadena, no un objeto — y por él pasan medio
/// millón de pasos.
///
/// # Por qué no un [`VPath`]
/// Un [`VPath`] es «siempre absoluto respecto a la raíz del provider» y lleva
/// SIEMPRE scheme y authority. Meter una ruta relativa dentro obliga a
/// inventarlos, y lo inventado es visible y dañino:
///
/// - Un plan de `file:///…` a `sftp://nas/…` mandaría `file:///sub` como ruta
///   relativa contra un destino sftp. Un tercero que lea el schema mandará
///   `sftp://nas/sub`, igual de «correcto», y las dos no comparan iguales.
/// - El `plan_hash` cubre las conclusiones del plan, y `rel` es una de ellas:
///   un scheme que el daemon tiene orden de IGNORAR no puede estar dentro del
///   token con el que se aprueba. No se puede ignorar un campo y hashearlo.
/// - «Un `rel` jamás se escapa de la raíz» pasa a ser propiedad del TIPO:
///   [`Segment::new`] ya rechaza `/`, `.`, `..` y el NUL, y lo valida
///   POST-decode, así que un `%2E%2E` no cuela un `..`. Lo comprueba el
///   deserializador de cualquier peer, no un chequeo que se pueda olvidar.
///
/// ```
/// use norte_proto::methods::RelPath;
/// let r = RelPath::parse_wire("sub/informe%FF%FE.dat").expect("rel");
/// assert_eq!(r.segments().len(), 2);
/// assert_eq!(r.segments()[1].as_bytes(), b"informe\xff\xfe.dat");
/// assert_eq!(r.to_wire(), "sub/informe%FF%FE.dat");
/// // La cadena vacía es la RAÍZ, y viaja como tal.
/// assert!(RelPath::default().is_root());
/// assert_eq!(serde_json::to_string(&RelPath::default()).expect("json"), r#""""#);
/// // Lo que se escaparía de la raíz no llega a existir.
/// assert!(RelPath::parse_wire("../etc").is_err());
/// assert!(RelPath::parse_wire("%2E%2E/etc").is_err());
/// assert!(RelPath::parse_wire("/a").is_err());
/// assert!(RelPath::parse_wire("a//b").is_err());
/// // Ni por el wire, que es donde importa.
/// assert!(serde_json::from_str::<RelPath>(r#""a/../b""#).is_err());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(into = "String")]
pub struct RelPath(Vec<Segment>);

impl RelPath {
    /// Construye desde segmentos ya validados.
    #[must_use]
    pub fn new(segments: Vec<Segment>) -> Self {
        Self(segments)
    }

    /// Parsea la forma wire: segmentos percent-encodeados unidos por `/`. La
    /// cadena VACÍA es la raíz.
    ///
    /// # Errors
    /// Lo que devuelva [`Segment::parse_wire`] para cualquiera de los
    /// segmentos: [`VPathError::EmptySegment`] (un `/` de más, al principio, al
    /// final o doblado), [`VPathError::DotSegment`] (`.`/`..`, comprobado
    /// después de decodificar), [`VPathError::NulByte`] o
    /// [`VPathError::BadEscape`].
    pub fn parse_wire(wire: &str) -> Result<Self, VPathError> {
        if wire.is_empty() {
            return Ok(Self::default());
        }
        wire.split('/')
            .map(Segment::parse_wire)
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    /// La forma wire canónica.
    #[must_use]
    pub fn to_wire(&self) -> String {
        let mut out = String::with_capacity(self.0.len() * 12);
        for (i, seg) in self.0.iter().enumerate() {
            if i > 0 {
                out.push('/');
            }
            out.push_str(&seg.to_wire());
        }
        out
    }

    /// Los segmentos, en orden.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.0
    }

    /// `true` si nombra la propia raíz (sin segmentos).
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_wire())
    }
}

impl From<RelPath> for String {
    fn from(value: RelPath) -> Self {
        value.to_wire()
    }
}

impl<'de> Deserialize<'de> for RelPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = String::deserialize(deserializer)?;
        Self::parse_wire(&wire).map_err(serde::de::Error::custom)
    }
}

// El serde es a mano (valida al deserializar), así que el schema también.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for RelPath {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "RelPath".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A path RELATIVE to a sync plan's two roots: \
                            percent-encoded segments joined by `/`, with the \
                            empty string meaning the root itself. It carries no \
                            scheme and no authority — the same `rel` names an \
                            entry under both roots, which may be different \
                            providers. `.`, `..`, an empty segment and a NUL are \
                            rejected when decoding (after percent-decoding, so \
                            `%2E%2E` smuggles nothing), which is what makes \
                            \"a rel never escapes its root\" a property of the \
                            wire rather than a check a server can forget.",
        })
    }
}

/// En qué sentido sincroniza un plan (0.40.0, ADR 0049).
///
/// # Por qué este enum NO tiene `#[serde(other)]`
/// Viaja CLIENT→DAEMON. Un token que este daemon no conoce muere en el
/// deserializador y la petición es `-32602`, al revés que todo el vocabulario
/// de [`CompareRow`] —que viaja daemon→client y degrada—. Aceptar un modo
/// desconocido «por defecto» es aceptar BORRAR por defecto, y el default
/// tendría que ser uno de los dos: no hay valor neutro. Lo mismo vale para
/// [`OnUnknown`].
///
/// Que [`OnUnknown`] sí tenga default y este no, no es incoherencia: son dos
/// hechos distintos del wire. La CLAVE AUSENTE dice «no tengo opinión» y merece
/// un default documentado; un TOKEN DESCONOCIDO dice «tengo una opinión que
/// este daemon no sabe honrar» y muere igual en los dos enums. Y donde el
/// default de `on_unknown` es asumible es aquí: `mode` decide si el plan LLEGA
/// A TENER pasos de borrado, mientras que `on_unknown` solo reparte filas entre
/// dos clases de paso que el modo ya autorizó — y el humano ve el resultado, con
/// su `confidence` y su reversa, antes de aprobar. Un `on_unknown` equivocado se
/// VE antes de actuar; un `mode` equivocado produciría otro plan.
///
/// `#[non_exhaustive]` sí, por el motivo de #126: un modo futuro no puede
/// romper el `match` de nadie fuera de este crate.
///
/// ```
/// use norte_proto::methods::SyncMode;
/// assert_eq!(serde_json::to_string(&SyncMode::Mirror).expect("json"), r#""mirror""#);
/// // Un modo del futuro NO degrada: se rechaza.
/// assert!(serde_json::from_str::<SyncMode>(r#""obliterate""#).is_err());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    /// Copiar al destino lo que falta y lo que difiere. JAMÁS borra: lo que el
    /// destino tiene de más se queda donde está.
    Update,
    /// [`SyncMode::Update`] más borrar del destino lo que el origen no tiene.
    /// Es el único modo que emite [`SyncStepKind::DeleteTree`].
    Mirror,
}

/// El lado cuyos huérfanos se ENUMERAN (0.40.0, ADR 0049): el valor de
/// [`FsCompareParams::descend_orphans`] y de
/// [`SyncCompareOptions::descend_orphans`].
///
/// # Por qué no es un [`Side`]
///
/// Porque este campo viaja CLIENT→DAEMON y [`Side`] no: [`Side`] nombra el lado
/// de una fila o de un blocker que el daemon EMITE, así que lleva
/// `#[serde(other)]` y un `"lft"` se convierte en [`Side::Unknown`] en vez de
/// morir. Como parámetro de una petición eso sería un valor pisado en silencio
/// de la peor clase: `Unknown` no es ningún lado, así que la comparación no
/// descendería por NINGUNO y el llamante recibiría —sin un solo error— un
/// conjunto de filas distinto del que pidió, por una errata de tres letras.
///
/// Es la misma regla que [`SyncMode`] y [`OnUnknown`] ya siguen, y la razón por
/// la que el chequeo no vive en el handler: un `if` en `handle_fs_compare` no
/// alcanza al brazo EMBEBIDO (`CoreBackend::Embedded` llama al engine sin pasar
/// por el daemon), y lo que el tipo prohíbe no hay handler que lo pueda olvidar.
///
/// `#[non_exhaustive]` por el motivo de #126, como los otros dos.
///
/// ```
/// use norte_proto::methods::{DescendSide, Side};
/// assert_eq!(serde_json::to_string(&DescendSide::Left).expect("json"), r#""left""#);
/// // Una errata NO degrada: muere en el deserializador.
/// assert!(serde_json::from_str::<DescendSide>(r#""lft""#).is_err());
/// // Y "unknown", que `Side` sí acepta, tampoco es un lado que se pueda pedir.
/// assert!(serde_json::from_str::<DescendSide>(r#""unknown""#).is_err());
/// assert_eq!(Side::from(DescendSide::Right), Side::Right);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum DescendSide {
    /// El lado izquierdo (`left` de [`FsCompareParams`], `source` de
    /// [`SyncPlanParams`] cuando el origen es la izquierda).
    Left,
    /// El lado derecho.
    Right,
}

impl From<DescendSide> for Side {
    /// El lado pedido, ya como el [`Side`] con el que habla el motor. En esta
    /// dirección la conversión es total; la contraria no existe a propósito,
    /// porque [`Side::Unknown`] no tiene destino aquí.
    fn from(side: DescendSide) -> Self {
        match side {
            DescendSide::Left => Self::Left,
            DescendSide::Right => Self::Right,
        }
    }
}

/// Qué hacer con una fila cuya confianza es
/// [`CompareConfidence::Unknown`] (0.40.0, ADR 0049): el provider no pudo
/// decir si los dos lados son iguales.
///
/// El default es [`OnUnknown::Copy`]: ante «no se puede saber», copiar cuesta
/// ancho de banda y saltar cuesta datos rancios en silencio. Sea cual sea la
/// elección, el paso conserva `confidence: unknown`, así que el informe puede
/// explicar por qué escribió.
///
/// Client→daemon: SIN `#[serde(other)]`, por el motivo escrito en
/// [`SyncMode`].
///
/// ```
/// use norte_proto::methods::OnUnknown;
/// assert_eq!(OnUnknown::default(), OnUnknown::Copy);
/// assert!(serde_json::from_str::<OnUnknown>(r#""maybe""#).is_err());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum OnUnknown {
    /// Copiar igualmente (default).
    #[default]
    Copy,
    /// No tocar, y decirlo: [`SyncStepKind::Skip`] con
    /// [`SyncReason::UnknownConfidence`].
    Skip,
}

/// Qué hace UN paso de un plan (0.40.0, ADR 0049).
///
/// Daemon→client: `#[serde(other)]`, como los cuatro enums de ADR 0048. Un
/// daemon N+1 que añada una clase de paso no puede matarle a un cliente N-1 un
/// lote de [`SYNC_STEPS_MAX_BATCH`] pasos.
///
/// ```
/// use norte_proto::methods::SyncStepKind;
/// assert_eq!(
///     serde_json::to_string(&SyncStepKind::DeleteTree).expect("json"),
///     r#""delete_tree""#
/// );
/// let futuro: SyncStepKind = serde_json::from_str(r#""teleport""#).expect("degrada");
/// assert_eq!(futuro, SyncStepKind::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SyncStepKind {
    /// Crear en el destino un directorio que solo existe en el origen.
    CreateDir,
    /// Copiar al destino una entrada que no está.
    Copy,
    /// Escribir encima de una entrada del destino que difiere. Con papelera en
    /// el destino es «a la papelera y copiar»; sin ella es
    /// [`StepReversal::Irreversible`].
    Overwrite,
    /// Borrar del destino un árbol que el origen no tiene. SOLO bajo
    /// [`SyncMode::Mirror`], y es UN paso para el árbol entero: un movimiento a
    /// la papelera, una entrada de journal, una cosa que restaurar.
    DeleteTree,
    /// No tocar nada, y decir por qué ([`SyncStep::reason`] poblado). Se emite
    /// solo para lo NOTABLE —una colisión en el origen, una confianza que el
    /// usuario pidió saltar, una entrada ilegible—: dos árboles idénticos
    /// producen CERO pasos, no un millón de `Skip`.
    Skip,
    /// Clase que este decodificador no conoce (`#[serde(other)]`). El core
    /// jamás la emite.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// Cómo vuelve atrás un paso ya ejecutado (0.40.0, ADR 0049).
///
/// Es función de la clase del paso y de si el DESTINO tiene papelera, y viaja
/// en el plan —antes de aprobar— justamente para que el humano vea cuántos
/// pasos no se pueden deshacer ANTES de decir que sí, y no en el informe
/// después.
///
/// Daemon→client: `#[serde(other)]`.
///
/// ```
/// use norte_proto::methods::StepReversal;
/// assert_eq!(
///     serde_json::to_string(&StepReversal::RestoreTrash).expect("json"),
///     r#""restore_trash""#
/// );
/// let futuro: StepReversal = serde_json::from_str(r#""time_travel""#).expect("degrada");
/// assert_eq!(futuro, StepReversal::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum StepReversal {
    /// Deshacer = borrar lo que este paso creó. No se destruyó nada, así que
    /// vale con o sin papelera en el destino.
    Delete,
    /// Deshacer = sacar de la papelera lo que este paso enterró (y, en un
    /// `Overwrite`, borrar antes lo que escribió: el journal recorre `seq`
    /// descendente, así que el orden sale solo).
    RestoreTrash,
    /// No se puede deshacer, y el plan lo dice ANTES (regla dura 4: o hay undo
    /// o hay una clasificación explícita con su motivo, que va en
    /// [`SyncStep::reason`]).
    Irreversible,
    /// Reversa que este decodificador no conoce (`#[serde(other)]`). El core
    /// jamás la emite.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// Por qué un paso es un [`SyncStepKind::Skip`] o es
/// [`StepReversal::Irreversible`] (0.40.0, ADR 0049). Vocabulario CERRADO: el
/// core jamás inventa un motivo.
///
/// Daemon→client: `#[serde(other)]`.
///
/// ```
/// use norte_proto::methods::SyncReason;
/// assert_eq!(
///     serde_json::to_string(&SyncReason::NoTrashOnTarget).expect("json"),
///     r#""no_trash_on_target""#
/// );
/// let futuro: SyncReason = serde_json::from_str(r#""solar_flare""#).expect("degrada");
/// assert_eq!(futuro, SyncReason::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SyncReason {
    /// Dos nombres del ORIGEN colapsan a una misma clave de emparejamiento
    /// ([`CompareVerdict::Ambiguous`] de ese lado): no se sabe cuál de los dos
    /// copiar, así que no se copia ninguno y el resto del plan sigue en pie. En
    /// el DESTINO la misma colisión no es un motivo sino un bloqueo
    /// ([`SyncBlockerKind::AmbiguousDest`]): solo uno de los dos lados puede
    /// perder datos.
    AmbiguousSource,
    /// [`OnUnknown::Skip`] y el criterio se ganó
    /// [`CompareConfidence::Unknown`].
    UnknownConfidence,
    /// No se pudo LEER lo que hacía falta, en el lado que importaba
    /// ([`CompareVerdict::Error`]).
    Unreadable,
    /// El destino no tiene papelera, así que lo que este paso entierra no se
    /// puede recuperar. Acompaña SIEMPRE a [`StepReversal::Irreversible`].
    NoTrashOnTarget,
    /// Motivo que este decodificador no conoce (`#[serde(other)]`). El core
    /// jamás lo emite.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// Por qué un plan NO se puede ejecutar (0.40.0, ADR 0049). Vocabulario
/// CERRADO.
///
/// Un bloqueo no es un paso fallido: es una razón para que el plan ENTERO no se
/// pueda aprobar ([`SyncPlanDone::executable`] a `false`). Daemon→client:
/// `#[serde(other)]`.
///
/// ```
/// use norte_proto::methods::SyncBlockerKind;
/// assert_eq!(
///     serde_json::to_string(&SyncBlockerKind::OverlapDetected).expect("json"),
///     r#""overlap_detected""#
/// );
/// let futuro: SyncBlockerKind = serde_json::from_str(r#""cosmic_ray""#).expect("degrada");
/// assert_eq!(futuro, SyncBlockerKind::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SyncBlockerKind {
    /// Dos nombres del DESTINO colapsan a una misma clave de emparejamiento:
    /// escribir ahí es escribir sobre uno de dos ficheros sin saber cuál.
    AmbiguousDest,
    /// El walk llegó a la OTRA raíz: las dos nombran el mismo árbol. La
    /// comprobación estructural previa
    /// ([`Error::OverlappingRoots`](crate::Error::OverlappingRoots)) se puede
    /// derrotar con un symlink, una raíz SFTP bajo dos authorities o un archivo
    /// abierto por dos caminos; esta no.
    OverlapDetected,
    /// El provider del destino no admite escritura (ver `Capabilities`). Se
    /// levanta ANTES de planificar un solo paso: no se planifican escrituras
    /// contra un árbol que las rehúsa.
    DestReadOnly,
    /// Un directorio del DESTINO por encima de [`COMPARE_MAX_DIR_ENTRIES`]. En
    /// una comparación eso cuesta una fila y el walk sigue; en un plan que va a
    /// escribir ahí, no: no se sabe qué hay en ese directorio.
    DirTooLarge,
    /// Clase que este decodificador no conoce (`#[serde(other)]`). El core
    /// jamás la emite.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// UN paso de un plan de sincronización (0.40.0, ADR 0049): qué se va a hacer,
/// sobre qué, por qué, y cómo se vuelve atrás.
///
/// `criterion` y `confidence` viajan POR PASO y no por plan: es la obligación
/// que ADR 0048 dejó pendiente, saldada. Un informe puede decir «lo sobrescribí
/// porque no se pudo leer la fecha» en vez de «lo sobrescribí», y quien revise
/// una sincronización que salió mal ve qué rung autorizó cada escritura.
///
/// ```
/// use norte_proto::methods::{
///     CompareConfidence, CompareCriterion, RelPath, StepReversal, SyncStep, SyncStepKind,
/// };
/// let s = SyncStep {
///     id: 1,
///     kind: SyncStepKind::Copy,
///     rel: RelPath::parse_wire("sub/informe%FF%FE.dat").expect("rel"),
///     size: Some(1234),
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     reversal: Some(StepReversal::Delete),
///     reason: None,
/// };
/// assert!(s.shape_is_consistent());
/// // Lo ausente NO viaja: ni `null` ni clave.
/// let json = serde_json::to_value(&s).expect("json");
/// assert!(json.get("reason").is_none());
/// // Y el nombre no-UTF8 vuelve byte a byte (regla dura 1).
/// let back: SyncStep = serde_json::from_value(json).expect("json");
/// assert_eq!(back, s);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStep {
    /// Identificador monótono dentro de UN plan. El cursor del panel se ancla a
    /// él: un filtro esconde pasos, jamás los renumera. NO entra en el
    /// `plan_hash` — es presentación, no conclusión.
    pub id: u64,
    /// Qué hace el paso.
    pub kind: SyncStepKind,
    /// La ruta, RELATIVA a las dos raíces del plan, en BYTES (regla dura 1).
    /// Ni un `String` ni un [`VPath`]: el mismo `rel` nombra la entrada en el
    /// origen y en el destino, que pueden ser providers distintos, así que no
    /// tiene scheme que llevar — ver [`RelPath`].
    pub rel: RelPath,
    /// Bytes que MUEVE este paso, cuando se saben; un provider que no da tamaño
    /// deja `None`.
    ///
    /// Un [`SyncStepKind::DeleteTree`] y un [`SyncStepKind::Skip`] no mueven
    /// ninguno y la dejan AUSENTE. No es cosmético:
    /// [`SyncCounts::bytes`] es la suma de este campo, así que un tamaño en un
    /// paso que no escribe es un byte contado que nunca se movió, y el diálogo
    /// de aprobación enseña ese número.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Qué rung de la cascada decidió la fila de la que sale este paso.
    pub criterion: CompareCriterion,
    /// Cuánto vale esa decisión.
    pub confidence: CompareConfidence,
    /// Cómo se deshace. `None` si y SOLO si `kind` es [`SyncStepKind::Skip`]
    /// — ver [`SyncStep::shape_is_consistent`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reversal: Option<StepReversal>,
    /// El porqué, para las dos formas que tienen porqué: un `Skip` y un paso
    /// [`StepReversal::Irreversible`]. `None` en cualquier otra.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<SyncReason>,
}

impl SyncStep {
    /// ¿Concuerdan `kind`, `reversal` y `reason`?
    ///
    /// DOS de las invariantes que el wire no sabe expresar, enunciadas UNA vez,
    /// aquí: `reversal` es `None` si y solo si `kind` es
    /// [`SyncStepKind::Skip`], y `reason` es `Some` para EXACTAMENTE un `Skip`
    /// y un paso cuya reversa es [`StepReversal::Irreversible`].
    ///
    /// Las otras dos del diseño no caben en un paso suelto y no se comprueban
    /// aquí: «`DeleteTree` solo bajo `Mirror`» necesita el modo, que no viaja en
    /// el paso, y «`blockers` no vacío ⟹ `!executable`» es de
    /// [`SyncPlanDone`]. `size` tampoco se mira: su regla —un `Skip` y un
    /// `DeleteTree` la dejan ausente— es de los CONTADORES, y un paso que la
    /// incumpliera sigue siendo un paso ejecutable.
    ///
    /// NO es un rechazo de `Deserialize`, a propósito y por el mismo motivo que
    /// [`CompareRow::reason_is_consistent`]: un paso malformado tiene que
    /// degradar como una celda de atributo mala, no matar un lote de
    /// [`SYNC_STEPS_MAX_BATCH`]. El daemon lo afirma en sus tests; un cliente lo
    /// usa para decidir si se fía del paso.
    ///
    /// [`SyncStepKind::Unknown`] y [`StepReversal::Unknown`] quedan EXENTOS: un
    /// paso de un daemon una versión por delante no es algo que este cliente
    /// pueda juzgar, y afirmar lo contrario le haría desconfiar de pasos
    /// legítimos.
    ///
    /// ```
    /// # use norte_proto::methods::{
    /// #     CompareConfidence, CompareCriterion, RelPath, StepReversal, SyncReason, SyncStep,
    /// #     SyncStepKind,
    /// # };
    /// # fn step(kind: SyncStepKind, reversal: Option<StepReversal>, reason: Option<SyncReason>) -> SyncStep {
    /// #     SyncStep { id: 1, kind, rel: RelPath::parse_wire("a").expect("rel"), size: None,
    /// #                criterion: CompareCriterion::Presence,
    /// #                confidence: CompareConfidence::Certain, reversal, reason }
    /// # }
    /// assert!(step(SyncStepKind::Copy, Some(StepReversal::Delete), None).shape_is_consistent());
    /// assert!(!step(SyncStepKind::Copy, None, None).shape_is_consistent());
    /// assert!(
    ///     step(SyncStepKind::Skip, None, Some(SyncReason::Unreadable)).shape_is_consistent()
    /// );
    /// assert!(!step(SyncStepKind::Skip, None, None).shape_is_consistent());
    /// ```
    #[must_use]
    pub fn shape_is_consistent(&self) -> bool {
        if self.kind == SyncStepKind::Unknown {
            return true;
        }
        let reversal_ok = if self.kind == SyncStepKind::Skip {
            self.reversal.is_none()
        } else {
            self.reversal.is_some()
        };
        if self.reversal == Some(StepReversal::Unknown) {
            return reversal_ok;
        }
        let owes_reason =
            self.kind == SyncStepKind::Skip || self.reversal == Some(StepReversal::Irreversible);
        reversal_ok && self.reason.is_some() == owes_reason
    }
}

/// Un LOTE de pasos de [`SYNC_STEPS`] (0.40.0, ADR 0049). Acotado por
/// [`SYNC_STEPS_MAX_BATCH`] y coalescido server-side, el mismo contrato que
/// [`CompareRowsBatch`].
///
/// ```
/// use norte_proto::TaskId;
/// use norte_proto::methods::SyncStepsBatch;
/// let b = SyncStepsBatch { task_id: TaskId::new(7), steps: vec![] };
/// assert_eq!(
///     serde_json::to_string(&b).expect("json"),
///     r#"{"task_id":7,"steps":[]}"#
/// );
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStepsBatch {
    /// Task dueña (correlación con `sync.plan` → `task_id`).
    pub task_id: TaskId,
    /// Los pasos de este lote, en ORDEN de ejecución (el walk es pre-orden, así
    /// que un `CreateDir` precede a toda copia dentro de él sin necesidad de
    /// ordenar). Nunca más de [`SYNC_STEPS_MAX_BATCH`].
    pub steps: Vec<SyncStep>,
}

/// A cuánto suma un plan (0.40.0, ADR 0049). El diálogo de aprobación abre por
/// `irreversible`.
///
/// ```
/// use norte_proto::methods::SyncCounts;
/// let c = SyncCounts { copy: 2, bytes: 30, ..SyncCounts::default() };
/// assert_eq!(serde_json::to_value(&c).expect("json")["delete_tree"], 0);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCounts {
    /// Directorios a crear.
    pub create_dir: u64,
    /// Entradas a copiar.
    pub copy: u64,
    /// Entradas a sobrescribir.
    pub overwrite: u64,
    /// Árboles a borrar del destino (solo bajo [`SyncMode::Mirror`]).
    pub delete_tree: u64,
    /// Pasos que no tocan nada y dicen por qué.
    pub skip: u64,
    /// Pasos cuya reversa es [`StepReversal::Irreversible`]. Se cuenta APARTE
    /// porque es el único número que un humano no debe tener que derivar.
    pub irreversible: u64,
    /// Bytes que mueve el plan. Un borrado y un `Skip` no mueven ninguno.
    pub bytes: u64,
}

/// Por qué un plan no se puede ejecutar, con su sitio (0.40.0, ADR 0049).
///
/// ```
/// use norte_proto::methods::{RelPath, Side, SyncBlocker, SyncBlockerKind};
/// let b = SyncBlocker {
///     rel: RelPath::parse_wire("LEEME").expect("rel"),
///     kind: SyncBlockerKind::AmbiguousDest,
///     side: Some(Side::Right),
/// };
/// assert_eq!(serde_json::to_value(&b).expect("json")["kind"], "ambiguous_dest");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncBlocker {
    /// Dónde, RELATIVO a las dos raíces y en BYTES, igual que
    /// [`SyncStep::rel`]. Un bloqueo que no es de un sitio concreto (un destino
    /// de solo lectura) lo lleva vacío: la raíz ([`RelPath::is_root`]).
    pub rel: RelPath,
    /// Qué clase de bloqueo.
    pub kind: SyncBlockerKind,
    /// El lado en el que ocurrió, cuando ocurrió en uno solo. En un plan el
    /// origen es [`Side::Left`] y el destino [`Side::Right`] — ver
    /// [`Error::OverlappingRoots`](crate::Error::OverlappingRoots), que usa el
    /// mismo convenio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
}

/// Las opciones de comparación que un plan de sincronización EMBEBE (0.40.0,
/// ADR 0049): los mismos rungs, tolerancia y profundidad que
/// [`FsCompareParams`], sin las dos raíces —que en un plan se llaman `source` y
/// `dest`—.
///
/// Es un tipo APARTE y no un `flatten` de [`FsCompareParams`] a propósito:
/// aquel es la petición de un método y ya está publicado con su forma.
/// Reestructurarla con `#[serde(flatten)]` cambia la SEMÁNTICA de su
/// deserialización —mapa bufferizado, otro camino para los errores de tipo—
/// aunque el objeto JSON se vea igual, y eso es lo que este bump no hace.
/// Añadirle un campo OPCIONAL, como hace `descend_orphans` en 0.40.0, no es lo
/// mismo: la petición de un cliente 0.39 sigue siendo byte a byte la de 0.39.
/// Ver [`PROTOCOL_VERSION`]. Los dos tipos deben moverse JUNTOS cuando la
/// cascada gane un rung — lo fija el test
/// `las_dos_caras_de_las_opciones_de_comparacion_no_divergen`.
///
/// Se llama `Sync…` y no `CompareOptions` a secas porque `norte-compare` ya
/// tiene un `CompareOptions` que NO es de wire (es la configuración del motor),
/// y el handler de `sync.plan` va a tener los dos delante en el mismo fichero.
///
/// **Dos de sus campos NO son del llamante en [`SYNC_PLAN`]**: ver
/// `descend_orphans` y `follow_symlinks` aquí abajo. Están presentes —en vez de
/// omitidos— justamente para poder RECHAZARLOS: serde ignora los campos que no
/// conoce, así que un campo ausente convertiría «pídelo y te digo que no» en
/// «pídelo y no pasa nada», que es el valor pisado en silencio que el diseño
/// rehúsa.
///
/// ```
/// use norte_proto::methods::{CompareCriteria, SyncCompareOptions};
/// // Todo tiene default: `{}` es una comparación barata de árbol entero.
/// let o: SyncCompareOptions = serde_json::from_str("{}").expect("options");
/// assert_eq!(o.criteria, CompareCriteria::default());
/// assert_eq!(o.mtime_tolerance_ms, 2000);
/// assert!(o.max_depth.is_none() && !o.follow_symlinks && o.descend_orphans.is_none());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncCompareOptions {
    /// Qué rungs corren. Ausente = [`CompareCriteria::default`]. Con `hash`
    /// encendido, [`SYNC_PLAN`] exige además scope de CONTENIDO sobre las dos
    /// raíces.
    pub criteria: CompareCriteria,
    /// Profundidad máxima del descenso, contando la raíz como 0. `None` = sin
    /// límite.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Tolerancia del rung de mtime en milisegundos. Default 2000 (la regla
    /// FAT). `u32` por el mismo motivo que en
    /// [`FsCompareParams::mtime_tolerance_ms`].
    pub mtime_tolerance_ms: u32,
    /// Seguir symlinks. En [`SYNC_PLAN`] **no es del llamante**: mandarlo en
    /// `true` es `-32602`, no un valor que el core sobrescriba en silencio. Los
    /// destinos se comparan COMO BYTES, y planificar copias a través de un
    /// enlace seguido es otra cosa que nadie ha diseñado.
    pub follow_symlinks: bool,
    /// Descender en los directorios que existen SOLO en este lado. `None` — el
    /// default— emite una fila por el huérfano y no lo recorre.
    ///
    /// En [`SYNC_PLAN`] **tampoco es del llamante**: el planificador lo fija al
    /// lado del ORIGEN, porque quien aprueba un plan necesita cuántos ficheros
    /// y cuántos bytes, y el ejecutor un paso por fichero para journalizar y
    /// para aislar un fallo. En el DESTINO un huérfano es un
    /// [`SyncStepKind::DeleteTree`] entero, y descenderlo compraría cuarenta mil
    /// listados que no cambian un solo paso. Pedirlo es `-32602`.
    ///
    /// [`DescendSide`] y no [`Side`] por lo que ese tipo explica: es un campo
    /// de petición, y un lado mal escrito tiene que morir en el
    /// deserializador.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub descend_orphans: Option<DescendSide>,
}

impl Default for SyncCompareOptions {
    fn default() -> Self {
        Self {
            criteria: CompareCriteria::default(),
            max_depth: None,
            mtime_tolerance_ms: default_mtime_tolerance_ms(),
            follow_symlinks: false,
            descend_orphans: None,
        }
    }
}

/// Params de [`SYNC_PLAN`] (0.40.0, ADR 0049).
///
/// Se llaman `source` y `dest`, jamás `left` y `right`: comparar es simétrico y
/// sincronizar no, así que la dirección se traduce UNA vez, en el frontend que
/// sabe en qué panel estaba el usuario. Aguas abajo ya es un hecho.
///
/// ```
/// use norte_proto::methods::{OnUnknown, SyncMode, SyncPlanParams};
/// // Lo MÍNIMO: dos raíces y el modo. `mode` no tiene default —no hay valor
/// // neutro entre copiar y borrar—; todo lo demás sí.
/// let p: SyncPlanParams = serde_json::from_str(
///     r#"{"source":"file:///a","dest":"file:///b","mode":"update"}"#,
/// )
/// .expect("params");
/// assert_eq!(p.mode, SyncMode::Update);
/// assert_eq!(p.on_unknown, OnUnknown::Copy);
/// assert!(p.include.is_none());
/// assert!(serde_json::from_str::<SyncPlanParams>(r#"{"source":"file:///a","dest":"file:///b"}"#)
///     .is_err());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlanParams {
    /// De dónde salen los bytes.
    pub source: VPath,
    /// A dónde van. Si se solapa con `source` —igual, o una dentro de la
    /// otra— la petición es
    /// [`Error::OverlappingRoots`](crate::Error::OverlappingRoots) y no se crea
    /// Task alguna.
    pub dest: VPath,
    /// `Update` o `Mirror`. SIN default a propósito.
    pub mode: SyncMode,
    /// Las opciones de la comparación que hay debajo. Ausente = todo por
    /// defecto. Dos de sus campos no son del llamante (ver
    /// [`SyncCompareOptions`]).
    #[serde(default)]
    pub compare: SyncCompareOptions,
    /// Qué hacer con lo que el provider no puede decidir. Ausente =
    /// [`OnUnknown::Copy`].
    #[serde(default)]
    pub on_unknown: OnUnknown,
    /// Rutas RELATIVAS a las que se restringe el plan; ausente = el árbol
    /// entero. Es como la selección de primera clase del panel de diferencias
    /// siembra un plan.
    ///
    /// Más de [`SYNC_MAX_INCLUDE`] es error de params (`-32602`), no un
    /// recorte: la misma regla que [`FS_RENAME_BATCH_MAX_PAIRS`] y por el mismo
    /// motivo — una lista acortada en silencio sincroniza algo que nadie pidió.
    #[cfg_attr(feature = "schema", schemars(extend("maxItems" = SYNC_MAX_INCLUDE)))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<RelPath>>,
}

/// Payload de [`SYNC_PLAN_DONE`] (0.40.0, ADR 0049): lo que hay que saber para
/// aprobar un plan, y el hash con el que se aprueba.
///
/// ```
/// use norte_proto::TaskId;
/// use norte_proto::methods::{PlanHash, SyncCounts, SyncPlanDone};
/// let d = SyncPlanDone {
///     task_id: TaskId::new(7),
///     plan_hash: PlanHash::parse(&"0".repeat(64)).expect("hex"),
///     counts: SyncCounts::default(),
///     blockers: vec![],
///     blockers_total: 0,
///     executable: true,
/// };
/// let json = serde_json::to_value(&d).expect("json");
/// // `blockers` vacío es una lista vacía, jamás una clave ausente.
/// assert_eq!(json["blockers"], serde_json::json!([]));
/// assert_eq!(json["plan_hash"], serde_json::json!("0".repeat(64)));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlanDone {
    /// Task dueña (la misma que devolvió [`SYNC_PLAN`]). Va aquí porque una
    /// conexión puede tener dos planes en vuelo, y hasta que llega esta
    /// notificación el cliente no conoce el `plan_hash` con el que
    /// distinguirlos.
    pub task_id: TaskId,
    /// El hash del plan, que es lo ÚNICO que [`SYNC_APPLY`] lleva. Reusa
    /// [`PlanHash`] sin cambios: [`PLAN_HASH_LEN`] caracteres hex minúscula, y
    /// una cadena de otra forma muere en la deserialización.
    pub plan_hash: PlanHash,
    /// A cuánto suma el plan, por clase de paso.
    pub counts: SyncCounts,
    /// Los bloqueos, recortados a [`SYNC_MAX_BLOCKERS_REPORTED`]. Es la
    /// EXPLICACIÓN, no el veredicto: quien decide es `executable`.
    pub blockers: Vec<SyncBlocker>,
    /// Cuántos bloqueos hubo REALMENTE. Sin tope: 256 en la lista y 40 000 aquí
    /// es una respuesta honesta.
    pub blockers_total: u64,
    /// `true` cuando el plan se puede ejecutar tal cual. Campo NORMATIVO, con la
    /// misma dirección que [`FsRenameBatchPlanResult::executable`]: el frontend
    /// deshabilita el confirmar con `!executable` y NO deduce nada de
    /// `blockers`, porque un bloqueo futuro podría no tener nombre que listar.
    ///
    /// INVARIANTE (el core lo mantiene, un cliente puede asumirlo): `blockers`
    /// no vacío ⟹ `executable == false`; y `executable == false` ⟹
    /// [`SYNC_APPLY`] rehúsa con
    /// [`Error::PlanNotExecutable`](crate::Error::PlanNotExecutable) aunque el
    /// hash case.
    pub executable: bool,
}

/// Params de [`SYNC_APPLY`] (0.40.0, ADR 0049): el hash, y NADA más.
///
/// ```
/// use norte_proto::methods::{PlanHash, SyncApplyParams};
/// let p = SyncApplyParams { plan_hash: PlanHash::parse(&"a".repeat(64)).expect("hex") };
/// let json = serde_json::to_value(&p).expect("json");
/// assert_eq!(json.as_object().expect("objeto").len(), 1, "no hay segundo parámetro");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncApplyParams {
    /// El plan aprobado. Nombra un plan RETENIDO server-side y atado a esta
    /// conexión; si no nombra ninguno vivo es
    /// [`Error::PlanStale`](crate::Error::PlanStale).
    pub plan_hash: PlanHash,
}

/// Params de [`SYNC_REPORT`] (0.40.0, ADR 0049).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReportParams {
    /// Task de la aplicación cuyo informe se pide (la de
    /// [`FsTaskResult::task_id`] que devolvió [`SYNC_APPLY`]).
    pub task_id: TaskId,
}

/// Por qué un paso no llegó a ocurrir (0.40.0, ADR 0049). Daemon→client:
/// `#[serde(other)]`.
///
/// ```
/// use norte_proto::methods::SyncFailureCause;
/// assert_eq!(
///     serde_json::to_string(&SyncFailureCause::Conflict).expect("json"),
///     r#""conflict""#
/// );
/// let futuro: SyncFailureCause = serde_json::from_str(r#""gremlins""#).expect("degrada");
/// assert_eq!(futuro, SyncFailureCause::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SyncFailureCause {
    /// El destino dejó de parecerse a lo que el plan anotó. Lo cazó el `stat`
    /// de revalidación y NO se escribió nada — es el único seguro entre el TTL
    /// del plan y un fichero perdido.
    Conflict,
    /// El provider rehusó la escritura.
    Denied,
    /// La lectura o la escritura se rompieron.
    Io,
    /// Causa que este decodificador no conoce (`#[serde(other)]`). El core
    /// jamás la emite.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// UN paso que no ocurrió (0.40.0, ADR 0049).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFailure {
    /// Dónde, RELATIVO a las dos raíces y en BYTES, igual que
    /// [`SyncStep::rel`].
    pub rel: RelPath,
    /// Por qué.
    pub cause: SyncFailureCause,
}

/// Result de [`SYNC_REPORT`] (0.40.0, ADR 0049): qué hizo la aplicación del
/// plan.
///
/// Un fallo es una FILA del informe, no el final de la Task: un paso que muere
/// en el fichero 40 000 de 500 000 se anota y la Task sigue, igual que la
/// comparación convirtió sus errores en filas.
///
/// ```
/// use norte_proto::methods::SyncReportResult;
/// let r = SyncReportResult {
///     done: 3, failed: 0, skipped: 1, bytes: 4096,
///     failures: vec![], batch_id: Some(12),
/// };
/// let json = serde_json::to_value(&r).expect("json");
/// assert_eq!(json["failures"], serde_json::json!([]));
/// assert_eq!(json["batch_id"], serde_json::json!(12));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReportResult {
    /// Pasos ejecutados Y journalizados.
    pub done: u64,
    /// Pasos que fallaron. NO tiene tope: `failures` lista los primeros, este
    /// número los cuenta todos.
    pub failed: u64,
    /// Pasos que el plan ya traía como [`SyncStepKind::Skip`], más los que la
    /// cancelación dejó sin intentar.
    pub skipped: u64,
    /// Bytes efectivamente movidos.
    pub bytes: u64,
    /// Los fallos, recortados a [`SYNC_MAX_FAILURES_REPORTED`]; `failed` no se
    /// recorta.
    pub failures: Vec<SyncFailure>,
    /// La unidad del journal bajo la que quedó todo lo aplicado. Referencia
    /// OPACA para el cliente: sirve para CITARLA —en un log, en un aviso, en un
    /// informe de soporte—, no para interpretarla, y ningún método la acepta
    /// como parámetro (el undo se pide por sesión con
    /// [`POLICY_UNDO_SESSION`], y agrupar por lote es asunto del core).
    ///
    /// `None` SOLO cuando la aplicación murió antes de poder abrir una — no
    /// cuando no hizo nada.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<i64>,
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
    ///
    /// Puede ser un PREFIJO de las rutas que la decisión cubre: ver
    /// `paths_total`.
    pub paths: Vec<String>,
    /// Cuántas rutas cubre de verdad la decisión (0.36.0). `0` = DESCONOCIDO
    /// (un server N-1 no lo mandaba), y entonces vale `paths.len()`.
    ///
    /// NORMATIVO para un frontend: si es mayor que `paths.len()`, la lista está
    /// RECORTADA y hay que decírselo al humano. Un lote de renames
    /// ([`FS_RENAME_BATCH`]) gatea dos rutas por paso y puede traer miles; el
    /// server recorta lo que difunde —la notificación va a cada conexión humana
    /// y se retiene durante el TTL—, pero la DECISIÓN se toma sobre todas. Un
    /// humano que aprueba 32 rutas de aspecto inocente sin saber que había ocho
    /// mil no está consintiendo lo que cree.
    #[serde(default)]
    pub paths_total: u64,
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
    ///
    /// Puede ser un PREFIJO: ver `paths_total`.
    pub paths: Vec<String>,
    /// Cuántas rutas cubre la decisión (0.36.0). Mismo contrato que
    /// [`PolicyApprovalRequired::paths_total`], incluido `0` = desconocido.
    #[serde(default)]
    pub paths_total: u64,
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
    /// **Deshacer un LOTE de renames (0.36.0) se quedó a medias**: el ejecutor
    /// no pudo devolver un paso de undo que ya había aplicado, así que el
    /// directorio NO volvió a como estaba.
    ///
    /// Es una categoría propia y no un `blocked`: `blocked` dice «paré aquí y
    /// el árbol está consistente», y esto dice justo lo contrario. Cuando está
    /// presente la Task termina `Failed` — un undo que dijera `Completed`
    /// prometería un árbol restaurado que no lo está.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_stuck: Option<RenameStuckStep>,
    /// Reversas del undo de un lote que se APLICARON pero cuya compensación no
    /// se pudo escribir (0.36.0). Cada una deja una entrada que sigue
    /// pareciendo pendiente aunque su efecto ya volvió: un undo posterior la
    /// encontrará y se bloqueará ahí. Es la única señal de eso.
    #[serde(default)]
    pub compensations_lost: u64,
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
    /// `true` si el plugin trae un `help.md` SERVIBLE junto a su `plugin.toml`
    /// (H3e, 0.34.0). Es DISCOVERY barato: decide si el nodo del plugin
    /// aparece en la barra de temas de la ayuda, y evita que
    /// [`PLUGIN_HELP_MAX_BYTES`] por plugin viajen en cada `plugin.list` — el
    /// contenido se pide aparte con [`PLUGIN_HELP`], bajo demanda.
    ///
    /// El host DEBE calcularlo con la MISMA guarda que aplica al servir
    /// [`PLUGIN_HELP`], no con una comprobación de existencia más laxa. Si no,
    /// el par (`has_help: true`, `markdown: ""`) le dice a quien llama «esa ruta
    /// existe y es un fichero regular» sobre un fichero que el host se niega a
    /// servir — un oráculo de rutas montado con dos métodos abiertos, ninguno
    /// gateado por policy.
    ///
    /// Qué puede concluir un RECEPTOR, que es lo que importa aquí:
    ///
    /// - `true` NO promete una página con contenido. El host no lee el fichero
    ///   ni lo parsea, así que un `help.md` vacío o ilegible sale `true` y se
    ///   degrada al pedirlo (markdown vacío) — la dirección correcta, porque la
    ///   ayuda es cosmética y jamás tumba un plugin.
    /// - `false` NO significa «no hay fichero». Significa «no hay página que yo
    ///   vaya a servir»: puede no existir, o existir y no pasar la guarda (un
    ///   symlink que sale del directorio del plugin). Las dos son
    ///   indistinguibles a propósito, y distinguirlas es justo lo que reabriría
    ///   el oráculo. Quien quiera el diagnóstico lo obtiene de `norte doctor`,
    ///   en local, no del wire.
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
    /// QUÉ plugin sirve la columna (0.35.0, #120). Ausente = el host resuelve
    /// por `column_id` a secas, que es lo que hacía antes y sigue haciendo
    /// para un cliente 0.34.
    ///
    /// El campo existe porque `column_id` NO identifica al plugin y el host
    /// resolvía a-la-primera-que-casa: dos plugins consentidos que declaren el
    /// mismo id bare —`status` es el ejemplo obvio— hacían que una columna
    /// configurada como `plugin:a/status` pintara los valores de `b` sin que
    /// nada lo dijera. El frontend SIEMPRE sabe cuál configuró el usuario (el
    /// id de configuración lleva el plugin dentro), así que lo que faltaba era
    /// sitio en el wire para decirlo.
    ///
    /// Un host que lo reciba DEBE servir ese plugin o ninguno: si el plugin
    /// nombrado no está aprobado, activado, o no declara `column_id`, la
    /// respuesta son celdas ausentes — jamás las de otro plugin. Caer al
    /// primero que case sería reintroducir el fallo con un campo más.
    ///
    /// `skip_serializing_if`: una petición sin este campo es byte a byte la de
    /// 0.34, así que la ventana N-1 no ve forma nueva.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
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
    /// cadena vacía, nunca un error — la ayuda es cosmética. Por eso el
    /// campo AUSENTE también se acepta y se lee como esa misma página vacía
    /// (`#[serde(default)]`): un peer que expresa «no hay página» omitiéndolo
    /// no puede recibir un fallo de deserialización por decir justo lo que el
    /// contrato ya permite decir. En emisión NO se omite nunca (sin
    /// `skip_serializing_if`), así que ausente y vacío solo se distinguen de
    /// ENTRADA, y ahí significan lo mismo.
    ///
    /// El tope es [`PLUGIN_HELP_MAX_BYTES`] y acota ESTE TEXTO, no solo los
    /// bytes del fichero: un byte puede decodificar a tres (windows-1252 `0x80`
    /// → `U+20AC`), así que una fuente que cabía justa daría el triple del tope
    /// si solo se acotara la fuente. El host corta las dos veces y `truncated`
    /// cubre ambos cortes, de modo que un receptor puede dimensionar por esa
    /// constante y los dos lados ven la MISMA página.
    ///
    /// NO está enmascarado: lleva verbatim los peligros de terminal que el
    /// plugin escribiera (ESC, controles C0, anulaciones bidi). Se parsea con
    /// `norte_help::parse_untrusted`, que enmascara al construir el modelo;
    /// nunca se pinta ni se loguea en crudo.
    #[serde(default)]
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
