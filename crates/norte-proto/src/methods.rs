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
/// [`SyncCompareOptions`], [`DescendSide`], [`DestTrash`]) y los
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
/// de leerlo en el informe — junto con [`SyncPlanDone::dest_trash`], sin el
/// cual esa columna no basta (ver [`DestTrash`]). Y la asimetría del `#[serde(other)]` es
/// deliberada: el vocabulario que va daemon→client lo lleva, como el de ADR
/// 0048; [`SyncMode`] y [`OnUnknown`], que van client→daemon, NO — aceptar un
/// modo desconocido por defecto es aceptar borrar por defecto.
///
/// **Dentro de la propia rama, 0.40.0 se movió una vez**: [`SyncPlanDone`] ganó
/// [`SyncPlanDone::dest_trash`], obligatorio y sin default (tarea 12 del plan de
/// sincronización). Como el número de versión no cambió —0.40.0 no se ha
/// publicado—, [`version_compatible`] no distingue un binario de antes de otro
/// de después: un cliente nuevo contra un daemon viejo no puede decodificar el
/// cierre y se queda esperando un plan que nunca cierra. Dos binarios 0.40.0 de
/// commits distintos de esta rama NO son intercambiables; es el mismo criterio
/// que 0.38.0 dejó escrito para [`Volume::label`], anotado aquí para que nadie
/// lo diagnostique como un cuelgue.
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
///
/// 0.41.0 (#178): UNA categoría de error nueva,
/// [`Error::JournalUnavailable`](crate::Error::JournalUnavailable) — el journal
/// de esta sesión no se puede abrir, así que la mutación se rehúsa y no se toca
/// nada. Ni método, ni notificación, ni campo: el bump más pequeño que existe.
///
/// **Hoy no la emite nadie por el wire, y aun así se paga el bump.** El daemon
/// con un journal ilegible no llega a arrancar, así que el único emisor posible
/// es el transporte EMBEBIDO, que habla in-process. El bump cuesta interop —un
/// frontend 0.41 deja de negociar con un daemon 0.40— y no compra un solo byte
/// de conversación nueva. Se paga porque la taxonomía se PUBLICA
/// (`docs/schema/proto.schema.json` va con cada release, #13): un tercero que
/// escriba un cliente contra ese fichero tiene que poder ver la categoría que
/// su `match` va a recibir el día que un daemon pueda perder su journal en
/// caliente. Una categoría que existe en el tipo y no en el esquema es la clase
/// de divergencia que ADR 0038 congela el esquema para no tener.
///
/// **Sin campos, y con sitio reservado para el detalle.** La ruta del fichero y
/// el texto crudo de `SQLite` son locales del proceso que la emite —y el
/// segundo lo moldea en parte quien pueda escribir `journal.db`—, así que no
/// cruzan la frontera: viajan por el canal in-process
/// `norte_core::embedded::NoJournal` (que este crate no puede enlazar: es su
/// consumidor, no su dependencia), saneado. Si algún día un
/// daemon necesita nombrar el fichero, el sitio ya existe y NO pide bump: el
/// `message` del `RpcError` lleva el `Display` (ver
/// `impl From<Error> for RpcError`), que es donde ADR 0004 pone el detalle
/// legible. Añadirle un campo a la variante más adelante también sería aditivo
/// en el wire —serde ignora las claves de más al decodificar una variante
/// unitaria con tag interno— y rompería solo la API de Rust.
///
/// **Sin ADR, y el porqué.** La decisión de producto —un journal ilegible
/// REHÚSA, y no hay `--no-journal` que lo salte— la toma #178 y vive en el
/// rustdoc del módulo `norte_core::embedded`, que es donde alguien la va a
/// buscar. Lo que llega al wire es una categoría más en un enum que ya degrada;
/// no cambia la forma de ningún mensaje, ni la negociación, ni el modelo de
/// confianza entre extremos. Los bumps de esa talla (0.29.0, 0.31.0, 0.35.0)
/// tampoco llevaron ADR.
///
/// Ventana N=0.41.x / N-1=0.40.x: un cliente 0.40 que recibiera esta categoría
/// la degrada a `Error::Unknown` por su `#[serde(other)]`, que es el mecanismo
/// que este enum lleva desde M0 y tiene su propio test. En la práctica no la
/// recibe: solo el embebido la emite, y el embebido no tiene wire.
///
/// 0.45.0 (#153, #145, #164, ADR 0054): un provider deja de responder solo
/// sobre sí mismo. Dos flags de capability nuevos —
/// [`CapabilityFlags::FULL_FOLD`](crate::CapabilityFlags::FULL_FOLD) (esta
/// ubicación pliega EXPANDIENDO: un ext4/f2fs en `+F`) y
/// [`CapabilityFlags::CONFINED_WRITES`](crate::CapabilityFlags::CONFINED_WRITES)
/// (una escritura bajo esta ubicación puede confinarse bajo la raíz que nombre
/// el caller)—, un subtipo de conflicto ([`ConflictKind::EscapesRoot`](crate::ConflictKind::EscapesRoot))
/// y una transformación de emparejamiento ([`PairTransform::FullFold`]).
///
/// Los tres primeros degradan solos en un cliente 0.44 (nombre desconocido que
/// se ignora, ADR 0004; subtipo que cae en `Unknown`, ADR 0005). El cuarto es
/// el que decidió que esto fuera una variante NUEVA y no un valor existente:
/// dos nombres que solo emparejan expandiendo **no nombran un mismo texto**, y
/// meterlos en `CaseFold` habría hecho que [`PairTransform::names_one_text`]
/// contestara `true` sobre una pareja que puede ser dos ficheros — con la
/// puerta de ADR 0053 en `norte-sync` sobrescribiendo detrás.
///
/// `fs.capabilities` no cambia de forma y sí de SIGNIFICADO: siempre tomó un
/// path y ahora responde por él.
///
/// 0.42.0 (#170, #152, #195): TRES campos, en tres tipos que ya existían, y
/// **un solo bump**. Los tres son la misma clase de hueco —un mensaje que se
/// lee sin el contexto que lo produjo y al que le falta el dato que ese
/// contexto tenía en la mano—, así que llevan un juego de goldens, una
/// regeneración de esquema y una revisión de `protocol-guardian`. Bumpear tres
/// veces por tres campos habría costado tres ventanas N/N-1 por el mismo
/// trabajo.
///
/// * [`SyncReportResult::dest_trash`] (#170), obligatorio, con el mismo valor
///   que ya viajaba en [`SyncPlanDone::dest_trash`]. Un cliente que reconectó,
///   que no fue quien planificó o que soltó el `sync.plan_done` podía leer qué
///   se copió, se sobrescribió y se borró, y no podía saber si algo de eso
///   vuelve.
/// * [`SyncFailure::kind`] (#195), obligatorio, la misma clase que el paso
///   llevaba en el plan. Sin ella, de qué raíz cuelga el `rel` de un fallo se
///   DEDUCÍA de la presencia de `dest_rel`, y esa deducción solo es correcta
///   mientras el core no emita jamás un `DeleteTree` con `dest_rel` — un
///   invariante que el wire no enunciaba y que sostenía un test de
///   `norte-sync`.
/// * [`CompareRow::paired_under`] (#152), OPCIONAL y omitido cuando está
///   ausente. Marca la pareja cuyos dos nombres no son los mismos bytes y dice
///   bajo qué transformación emparejó ([`PairTransform`]), que es lo único que
///   separa un par NFC/NFD legítimo de dos ficheros distintos juntados por una
///   descomposición singleton de NFC.
///
/// Bump ADITIVO, con la misma asimetría que 0.40.0: **dos tipos existentes
/// ganan un campo OBLIGATORIO** y uno gana uno opcional. Los dos obligatorios
/// van en la dirección daemon→client y ninguno afecta a lo que un cliente
/// MANDA, así que el payload de toda petición sigue siendo byte a byte el de
/// 0.41.0; el opcional deja además intacto el payload de una comparación
/// corriente, porque la clave se omite cuando no hay transformación que
/// nombrar. Ningún tipo se reestructura, ningún campo cambia de nombre o de
/// tipo, y no hay método ni notificación nuevos.
///
/// **Lo que este bump NO hace es cambiar la semántica del emparejamiento.** La
/// clave de `norte-compare` sigue plegando y normalizando exactamente igual, y
/// las mismas parejas siguen emparejando; lo que cambia es que ahora la fila lo
/// DICE. Cambiar a quién empareja con quién sí pediría un ADR —y decidir qué
/// hace un plan de sincronización con un
/// [`PairTransform::NormalizationSingleton`] es esa otra decisión, que este
/// bump deja tomada a medias a propósito: primero el dato, después la política.
///
/// Ventana N=0.42.x / N-1=0.41.x: un cliente 0.41 ignora `paired_under` y
/// `kind` y `dest_trash` de más —serde descarta las claves que no conoce—, así
/// que sigue leyendo informes y filas sin romperse; lo que no puede es
/// aprovecharlos, que es justo la deuda que este bump cierra para el siguiente.
/// La inversa —un daemon 0.41 mandando un informe SIN `dest_trash` a un cliente
/// 0.42, que fallaría al deserializar— no ocurre: [`version_compatible`] no
/// negocia un cliente con minor MAYOR que el servidor.
///
/// **0.46.0** (roadmap ítem 10): [`DAEMON_GOING_AWAY`] y
/// [`DaemonShutdownParams::mode`]. Aditivo: `mode` no se serializa cuando vale
/// [`ShutdownMode::Stop`], así que una parada corriente sale byte por byte como
/// en 0.45, y una notificación desconocida se ignora (ADR 0004). La ventana se
/// DESPLAZA igualmente, y éste es el ejemplo más claro de por qué: un cliente
/// 0.45 ignora la notificación —correctamente— y por tanto no se entera de que
/// venía un relevo, con lo que se queda reconectando contra un socket muerto.
/// No se rompe; simplemente no obtiene lo que 0.46 existe para dar.
/// **0.47.0** (roadmap ítem 11): `rar` entra en [`ARCHIVE_FORMATS`](crate::ARCHIVE_FORMATS)
/// (`crates/norte-proto/src/vpath.rs`). La whitelist decide qué schemes
/// compuestos puede FORMAR un cliente, así que ampliarla es cambio de wire
/// aunque no mueva un solo byte de un mensaje existente.
///
/// Ventana N=0.47.x / N-1=0.46.x, y la asimetría es la de siempre: un cliente
/// 0.46 no OFRECE `rar+file://…` porque su propia whitelist no lo trae, así
/// que sencillamente no ve la funcionalidad.
///
/// Lo que ese cliente sí hace —y la primera redacción de este párrafo decía
/// lo contrario (#247)— es PARSEAR uno que le llegue: [`crate::VPath::parse`]
/// no consulta [`ARCHIVE_FORMATS`](crate::ARCHIVE_FORMATS); solo lo hace
/// `archive_compose`. Un 0.46 que reciba `rar+file:///a.rar/!/x` por un
/// marcador, por el historial o por el cuerpo de una sesión se queda con un
/// scheme que no conoce y un segmento `!` literal, y falla aguas abajo al
/// pedirlo. No se corrompe nada y el bump sigue siendo MINOR; lo que no vale
/// es el razonamiento de que no llega a formarse.
///
/// Al revés, un cliente 0.47 contra
/// un daemon 0.46 sí forma el path —y el daemon viejo responde `Unsupported`
/// por su arm `_` de dispatch, que es la respuesta honesta— porque
/// [`version_compatible`] no negocia un cliente con minor MAYOR que el
/// servidor: la conexión ni se establece. Aditivo, por tanto, MINOR.
/// **0.48.0** (L2, la sesión de UI): [`SESSION_GET`] y [`SESSION_PUT`], con
/// [`Session`], [`SessionGetResult`], [`SessionPutParams`] y
/// [`SessionPutResult`]. Aditivo: ningún mensaje existente cambia de forma, y
/// el cuerpo de la sesión es OPACO para este crate y para el core.
/// **0.49.0** (#139, #140): [`FS_DIR_SIZE`] con [`FsDirSizeParams`] y
/// [`TaskKind::DirSize`](crate::TaskKind::DirSize), y [`CONNECTION_CLOSE`] con
/// [`ConnectionCloseParams`]/[`ConnectionCloseResult`]. Dos métodos en un solo
/// bump porque la rama no llegó a publicarse por separado. Aditivo: `dir_size`
/// entrega su resultado por el PROGRESO de una Task en vez de por un tipo
/// nuevo, y `connection.close` cierra por RUTA en vez de por una clave de
/// sesión que el frontend no tiene por qué conocer.
/// **0.50.0** (#132, escribir archivos): [`ARCHIVE_PACK`], [`ARCHIVE_TEST`],
/// [`ARCHIVE_TEST_REPORT`], [`FILE_SPLIT`] y [`FILE_COMBINE`] —CINCO métodos—,
/// con [`ArchivePackParams`], [`ArchiveTestParams`], [`ArchiveTestResult`],
/// [`ArchiveTestFailure`], [`ArchiveTestReportParams`], [`FileSplitParams`],
/// [`FileCombineParams`], [`ArchiveFormat`] y cuatro
/// [`TaskKind`](crate::TaskKind) nuevos.
///
/// Aditivo, y no toca el provider de archivos: ninguno de los cuatro escribe
/// DENTRO de un contenedor —eso seguiría siendo `READ_ONLY` (ADR 0018)—, los
/// cuatro fabrican ficheros nuevos. Desempaquetar no está aquí porque no hace
/// falta: es un `fs.copy` desde el interior del contenedor, que ya funciona.
///
/// Ventana N=0.50.x / N-1=0.49.x: un cliente 0.49 no conoce los métodos y no
/// los llama; un cliente 0.50 contra un daemon 0.49 recibe `METHOD_NOT_FOUND`,
/// que el `Backend` traduce a [`Error::Unsupported`](crate::Error) — «tu
/// daemon es más viejo», no un fallo genérico.
/// **0.51.0** (#247): ni un tipo nuevo ni un campo nuevo — lo que cambia es
/// lo que [`SESSION_PUT`] ACEPTA, y por eso es un bump.
///
/// Un `put` cuyo `version` este core no sabe leer se rehúsa ahora con
/// [`Error::Unsupported`](crate::Error), y `0` cuenta como desconocido. Antes
/// se aceptaba: el core volcaba a disco un documento que su propio guard de
/// carga rechaza, así que desde el arranque siguiente la sesión quedaba «del
/// futuro» para siempre —sin dueña, sin persistencia— hasta que alguien
/// borrase el fichero a mano. Bastaba un `ntc` más nuevo contra un `norte`
/// más viejo: los dos `SCHEMA_VERSION` viven en crates distintos y solo los
/// ata un test.
///
/// Y el esquema del cuerpo lo declara [`SessionPutParams::version`] y NADA
/// más: los frontends de norte metían además una copia sin documentar dentro
/// del `body`, y era la única que su lector miraba. Un cliente ajeno que
/// hiciera lo que dice este contrato —`version: 2` en el sobre, cuerpo v2—
/// llegaba a un lector que la veía ausente, la tomaba por 0, se comía los
/// campos que no entendía y los reescribía perdidos. El lector toma ahora la
/// MAYOR de las dos, así que un cuerpo antiguo con su copia dentro se sigue
/// leyendo igual.
///
/// Ventana N=0.51.x / N-1=0.50.x: un cliente 0.50 manda `version: 1` como
/// siempre y no nota nada; un cliente 0.51 contra un daemon 0.50 tampoco —el
/// daemon viejo acepta lo que aceptaba—. Lo que se pierde contra el viejo es
/// la protección, no la funcionalidad.
/// **0.52.0** (#163): [`SyncBlockerKind::IllegalDestName`], un nombre que el
/// DESTINO no puede tener.
///
/// Nada comprobaba que un nombre legal bajo la raíz de origen lo fuera bajo la
/// de destino, así que `CON`, `f:ads` o un punto final —los tres legales en
/// ext4— se descubrían al EJECUTAR. El peor es `f:ads`: en NTFS funciona y
/// escribe un flujo alternativo, con lo que la copia dice que fue bien y el
/// fichero no está. Ahora lo decide el provider del destino, que es quien
/// conoce sus reglas, y sale como bloqueo del PLAN — donde un humano puede
/// hacer algo al respecto.
///
/// Aditivo: [`SyncBlockerKind`] es `#[non_exhaustive]` con `#[serde(other)]
/// Unknown`, así que un cliente 0.51 pinta el bloqueo como «clase
/// desconocida» y **no aprueba el plan**, que es exactamente lo que tiene que
/// pasar — un bloqueo que no se entiende sigue bloqueando.
///
/// Ventana N=0.52.x / N-1=0.51.x: un daemon 0.51 no emite la clase y un
/// cliente 0.51 la degrada. Lo que se pierde contra el viejo es la
/// comprobación, no la corrección.
///
/// `0.53.0` (#251, #265, #282, ADR 0071): tres campos OPCIONALES, agrupados en
/// un bump porque cada uno solo habría costado su propia ventana.
/// [`crate::TaskProgress::unreadable`] dice cuántos subárboles no se pudieron
/// leer —`fs.dir_size` los contaba en un local y los tiraba a un log, así que
/// contestaba con un total confiado y corto—; [`PluginLoadError::dir_bytes`]
/// lleva los bytes del basename, que hasta ahora cruzaban ya convertidos por
/// un `to_string_lossy` sin marcar; y
/// [`PluginSetApprovalParams::expected_digest`] hace que lo que se concede sea
/// lo que el humano leyó.
///
/// Ventana N=0.53.x / N-1=0.52.x. Los tres se omiten cuando no hay nada que
/// decir, así que el JSON corriente no cambia, y lo que un peer 0.52 pierde es
/// una COMPROBACIÓN y no la corrección. Con un matiz que ADR 0071 registra: la
/// degradación de `expected_digest` es segura porque un daemon 0.52 descubre
/// el catálogo UNA vez al arrancar y por tanto no tiene ventana que explotar
/// — es una propiedad de aquella implementación, no del protocolo, y ningún
/// cliente puede comprobarla.
pub const PROTOCOL_VERSION: &str = "0.53.0";

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
/// `fs.capabilities` — capabilities de LA UBICACIÓN que nombra un path
/// (0.5.0; por ubicación desde 0.45.0, ADR 0054): el frontend decide p. ej. si
/// F8 ofrece papelera (ADR 0009).
///
/// Siempre tomó un path y hasta 0.44 contestaba lo mismo para todos, que es
/// falso en cuanto una máquina monta dos filesystems distintos. Desde 0.45.0
/// la respuesta la da el directorio: `CASE_SENSITIVE` y
/// [`CapabilityFlags::FULL_FOLD`](crate::CapabilityFlags::FULL_FOLD) pueden
/// diferir entre dos rutas de un mismo provider, mientras que lo que es del
/// backend —`READ_ONLY`, `TRASH`, `SYMLINKS`— sale igual por las dos puertas.
///
/// Preguntar por un FICHERO responde por el directorio que lo contiene: lo que
/// se decide con esta respuesta es si dos nombres pueden convivir ahí. Y una
/// ruta que no existe **no es un error**: se contesta lo que el provider
/// declara, igual que antes de 0.45.0.
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

/// ¿Es `version` al menos `major.minor`?
///
/// Para decidir si el OTRO extremo conoce una capacidad concreta, que es una
/// pregunta distinta de [`version_compatible`]: aquélla dice si se pueden
/// hablar, ésta dice si merece la pena pedir algo que llegó en una versión
/// dada. Un `false` no es un error — es la señal de degradar Y DECIRLO, que es
/// lo que separa «esto no se hizo» de un silencio.
///
/// Una versión que no parsea contesta `false`: sin saber qué habla el otro, no
/// se le supone nada.
///
/// ```
/// use norte_proto::methods::version_at_least;
/// assert!(version_at_least("0.46.0", 0, 46));
/// assert!(version_at_least("0.47.1", 0, 46));
/// assert!(!version_at_least("0.45.9", 0, 46));
/// assert!(!version_at_least("no-semver", 0, 46));
/// ```
#[must_use]
pub fn version_at_least(version: &str, major: u64, minor: u64) -> bool {
    let mut it = version.split('.');
    let (Some(j), Some(n)) = (it.next(), it.next()) else {
        return false;
    };
    let (Ok(j), Ok(n)) = (j.parse::<u64>(), n.parse::<u64>()) else {
        return false;
    };
    (j, n) >= (major, minor)
}

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
///
/// Un cliente que no vacía su cola pierde los frames que no quepan, pero NO
/// la suscripción (#155): el dueño de un feed dirigido vivo conserva su sitio
/// para recibir el snapshot terminal de la Task. Con `max_hits` puesto, ese
/// snapshot es además contra lo que se mide una búsqueda truncada; con
/// `max_hits: None` es la única señal que hay, igual que en
/// [`COMPARE_ROWS`].
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

/// `fs.dir_size` — cuánto ocupa de verdad un directorio (0.49.0, #139).
///
/// Un listado dice el tamaño de un fichero y NO el de una carpeta: saberlo
/// exige recorrerla entera, y un listado que lo hiciera por cada fila
/// convertiría bajar un nivel en una tormenta de peticiones. Por eso se pide,
/// no se supone.
///
/// Devuelve una Task, y el TOTAL viaja en el progreso que ya existe:
/// `bytes_done` suma los tamaños y `entries_done` cuenta las entradas, así que
/// el último snapshot ES el resultado. Cero tipos nuevos para el result
/// —[`FsTaskResult`], como `fs.copy`— y cero notificaciones nuevas: un cliente
/// que ya pinta la barra de una copia sabe pintar ésta.
///
/// `bytes_total`/`entries_total` van SIEMPRE a `None`: se sabrá cuánto era
/// cuando termine, y fingir un total mientras se cuenta sería una barra que
/// avanza hacia un número inventado.
///
/// NO muta: sin journal, sin undo, ni un byte escrito (la regla 4 no aplica).
/// Lee la FORMA del árbol, no su contenido, así que va sujeto al mismo gate de
/// lectura que un listado sobre cada raíz que se le pase.
///
/// Un subdirectorio ilegible cuesta lo suyo y el recorrido sigue: contar una
/// carpeta de tres horas no puede morirse en un `EACCES` de la hoja 40 000, y
/// el número que sale es el de lo que se pudo leer. **Hoy el progreso no
/// tiene forma de decir que el número es un SUELO** —cuántas ramas se
/// saltaron no viaja—, y eso es deuda anotada, no un olvido.
///
/// Dos raíces que se solapan se RECHAZAN con
/// [`Error::OverlappingRoots`](crate::Error), como en `fs.compare` y
/// `sync.plan` (#247): `["file:///a", "file:///a/b"]` contaba `b` dos veces y
/// devolvía un número mayor que el sitio que ocupa, que es lo contrario de lo
/// que este método existe para contestar.
///
/// Suma tamaño APARENTE y no bloques, y no deduplica enlaces duros: dos
/// nombres del mismo inodo cuentan dos veces. Para «¿cabe esto en el
/// destino?» —que es la pregunta— pasarse es el lado seguro.
pub const FS_DIR_SIZE: &str = "fs.dir_size";
/// `archive.pack` — fabrica un archivo NUEVO a partir de un conjunto de rutas
/// (0.50.0, #132).
///
/// Devuelve una Task ([`FsTaskResult`], [`TaskKind::Pack`](crate::TaskKind))
/// cancelable, y **no escribe dentro de ningún contenedor**: el provider de
/// archivos sigue siendo `READ_ONLY` (ADR 0018). Lo que hace es leer las
/// entradas por su provider y escribir UN fichero por el provider del destino,
/// que puede ser otro cualquiera.
///
/// MUTA, así que va al journal (regla 4) como UNA creación: deshacerlo es
/// borrar el archivo, y eso es un undo completo.
///
/// El [`ArchiveFormat`] viaja EXPLÍCITO. El frontend lo deduce del nombre que
/// el usuario escribe y se lo enseña antes de mandarlo; deducirlo aquí sería
/// decidir por él sin decírselo, y dos clientes con dos heurísticas darían dos
/// archivos distintos de la misma petición.
pub const ARCHIVE_PACK: &str = "archive.pack";
/// `archive.test` — comprueba lo que el formato promete de cada entrada
/// (0.50.0, #132).
///
/// Task cancelable ([`TaskKind::TestArchive`](crate::TaskKind)) que devuelve
/// [`ArchiveTestResult`] al terminar. NO muta: sin journal, sin undo, ni un
/// byte escrito.
///
/// Lo que se comprueba depende del formato y **el resultado lo dice**
/// ([`ArchiveTestResult::checked`]): un zip tiene un CRC-32 por entrada y un
/// `tar.gz` un CRC en su cola, pero un tar plano no tiene ninguna suma de
/// comprobación de contenido, así que decir «pasa» sobre un tar sin más sería
/// afirmar más de lo que el formato puede sostener.
pub const ARCHIVE_TEST: &str = "archive.test";
/// `file.split` — parte un fichero en trozos numerados (0.50.0, #132).
///
/// Task cancelable ([`TaskKind::Split`](crate::TaskKind)), journalizada con
/// una creación por trozo. La convención de nombres es la de Total Commander
/// —`nombre.001`, `nombre.002`…—, que es la que tienen los usuarios de las
/// teclas que piden esto.
pub const FILE_SPLIT: &str = "file.split";
/// `file.combine` — vuelve a juntar los trozos de un [`FILE_SPLIT`] (0.50.0,
/// #132).
///
/// Task cancelable ([`TaskKind::Combine`](crate::TaskKind)), journalizada como
/// una creación. Se le da el PRIMER trozo y encuentra el resto por la
/// convención. Un hueco en la numeración, o un trozo intermedio de tamaño
/// distinto del primero, es [`Error::Conflict`](crate::Error) y no un fichero
/// corto: un fichero mal unido es un fichero corrupto con buena pinta.
pub const FILE_COMBINE: &str = "file.combine";
/// `archive.test_report` — el informe de un [`ARCHIVE_TEST`] ya lanzado
/// (0.50.0, #132).
///
/// Existe por lo mismo que [`FS_RENAME_BATCH_REPORT`]: una Task no devuelve un
/// valor, y lo que este método tiene que contar —qué entrada está corrupta y
/// por qué— no cabe en el `Failed` de una Task. Se pide con el `task_id`, y
/// solo lo ve quien podría ver esa Task: un id ajeno contesta lo mismo que uno
/// que no existe.
pub const ARCHIVE_TEST_REPORT: &str = "archive.test_report";
/// Trozos como mucho de un [`FILE_SPLIT`], que es lo que da la convención
/// `.001`.
///
/// Se comprueba ANTES de escribir nada: descubrirlo en el trozo 1000 deja un
/// conjunto que nadie puede volver a juntar.
pub const FILE_SPLIT_MAX_PARTS: u64 = 999;
/// Trozo mínimo que acepta [`FILE_SPLIT`], para que partir un fichero no
/// produzca un millón de ficheros de un byte.
pub const FILE_SPLIT_MIN_BYTES: u64 = 4096;
/// Fallos que [`ARCHIVE_TEST`] llega a listar antes de recortar.
///
/// Un archivo en el que TODO está corrupto no puede costarle al cliente un
/// informe de un giga; el que sobra se cuenta en
/// [`ArchiveTestResult::truncated`], que es la misma disciplina que
/// [`COMPARE_ROWS_MAX_BATCH`].
pub const ARCHIVE_TEST_MAX_FAILURES: usize = 256;
/// `compare.rows` — notificación server→client con un LOTE de filas de
/// [`FS_COMPARE`] ([`CompareRowsBatch`]). SOLO viaja a la conexión que lanzó
/// la comparación (jamás broadcast, mismo criterio direccional que
/// [`SEARCH_HITS`]).
///
/// # Cómo se sabe si llegaron TODAS
/// Una notificación se puede perder: un cliente que no vacía su cola pierde
/// los frames que no caben, y a diferencia de `fs.search` aquí no hay un
/// `max_hits` contra el que contar (hallazgo MINOR de protocol-guardian,
/// revisión de C1). Lo que NO pierde es la suscripción: el dueño de un feed
/// dirigido vivo se queda en el mapa aunque su cola se llene, precisamente
/// para que le llegue el snapshot terminal con el que hace esta comprobación
/// (#155 — antes se le expulsaba, y la comprobación se perdía justo en el
/// caso para el que existe). La señal es
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
///
/// **Un lote puede llegar ANTES de la respuesta de [`SYNC_PLAN`]**, y con ella
/// el `task_id` con el que se correlaciona: la Task arranca dentro del dispatch
/// y sus notificaciones salen por la misma cola que la respuesta. Un cliente que
/// tire los lotes cuyo `task_id` todavía no conoce pierde pasos en silencio y
/// después recibe un [`SYNC_PLAN_DONE`] que parece completo, así que hay que
/// BUFFEARLOS por `task_id` hasta tener la respuesta. Es la misma forma que
/// [`COMPARE_ROWS`], y aquí importa más: allí se pinta un diff incompleto, aquí
/// se aprueba una escritura.
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

/// `connection.close` — cierra la sesión remota de una ruta (0.49.0, #140).
///
/// Lo que se cierra es la SESIÓN cacheada bajo `scheme://authority`, con los
/// providers de archivo compuestos que colgaran de ella: sin esto, «desconectar»
/// no desconectaba nada — el panel se iba a otro sitio y el socket seguía
/// abierto hasta que la sesión venciera sola.
///
/// Idempotente: cerrar lo que ya no estaba contesta `closed: false` y no es un
/// error. Un scheme de PROCESO —`file://`, y los providers registrados por un
/// plugin— no se cierra: no hay sesión que soltar, y contestar que sí sería
/// mentir sobre algo que sigue exactamente igual.
///
/// La siguiente operación sobre esa autoridad vuelve a conectar por el camino
/// de siempre. Cerrar no prohíbe nada: suelta.
pub const CONNECTION_CLOSE: &str = "connection.close";
/// `connection.degraded` — notificación server→client (#44): una sesión remota
/// se estableció con seguridad DEGRADADA (hoy: FTP con `tls="allow"` cayó a
/// texto plano porque el servidor rechazó `AUTH TLS`). Solo informa (el usuario
/// debe SABER que la sesión es en claro, ADR 0015 F "nunca silencioso"); no pide
/// decisión. Se difunde solo a conexiones humanas. Un cliente N-1 la ignora
/// (notif desconocida, ADR 0004) — degrada al comportamiento previo (solo log).
pub const CONNECTION_DEGRADED: &str = "connection.degraded";
/// `daemon.going_away` — el daemon avisa de que se va ANTES de dejar de
/// aceptar (0.46.0).
///
/// Existe porque un RELEVO y una PARADA son el mismo evento visto desde el
/// cliente —una conexión cerrada— y la respuesta correcta es la contraria en
/// cada caso: volver, o rendirse. Sin esto, un cliente que reconectara siempre
/// resucitaría un daemon que el usuario acaba de parar, y uno que no
/// reconectara nunca dejaría la sesión muerta tras una actualización.
///
/// Va a TODAS las conexiones, también a las de agente: la sesión de un agente
/// muere con el daemon igual que la de un humano, y necesita saberlo.
pub const DAEMON_GOING_AWAY: &str = "daemon.going_away";
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

    /// Desde el digest CRUDO de un sha256: la forma hex minúscula, sin poder
    /// equivocarse.
    ///
    /// Es el camino de quien PRODUCE un hash, y existe para que no haya un
    /// codificador hex por crate: cada copia es una ocasión de escribir
    /// mayúsculas —el detalle que hace que dos escrituras del mismo hash
    /// comparen distinto— y obliga además a un `expect` sobre
    /// [`PlanHash::parse`] que aquí no hace falta, porque 32 bytes no pueden
    /// dar otra cosa que 64 caracteres de `0`-`9` y `a`-`f`.
    /// [`PlanHash::parse`] sigue siendo el camino de quien lo RECIBE.
    ///
    /// ```
    /// use norte_proto::methods::PlanHash;
    /// let h = PlanHash::from_digest(&[0xab; 32]);
    /// assert_eq!(h.as_str(), "ab".repeat(32));
    /// assert_eq!(h, PlanHash::parse(&"ab".repeat(32)).expect("hex"));
    /// ```
    #[must_use]
    pub fn from_digest(digest: &[u8; 32]) -> Self {
        Self(crate::hashing::hex_lower(digest))
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

/// Params de [`DAEMON_GOING_AWAY`].
///
/// ```
/// use norte_proto::methods::DaemonGoingAway;
/// let n = DaemonGoingAway { reconnect: true };
/// let j = serde_json::to_value(&n).expect("json");
/// assert_eq!(j["reconnect"], serde_json::json!(true));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonGoingAway {
    /// `true` = viene un relevo; vuelve a conectar, y arráncalo si no está.
    /// `false` = este daemon se para y se queda parado.
    ///
    /// El `false` no es decoración: mandarlo en una parada corriente es lo que
    /// deja a un frontend decir «el daemon se paró» en vez de «se cayó la
    /// conexión», que para quien lo lee no son lo mismo.
    pub reconnect: bool,
}

/// Qué clase de apagado es (0.46.0).
///
/// ORTOGONAL a [`DaemonShutdownParams::graceful`], que decide qué pasa con las
/// tasks vivas. Éste decide quién viene después.
///
/// **Sin `#[serde(other)]`, a diferencia de casi todo este wire.** El resto de
/// los enums degradan ante un valor desconocido porque malinterpretarlos cuesta
/// una feature; malinterpretar éste apaga un daemon de una forma que el que
/// llamó no pidió. Misma asimetría que las políticas de mutación (ADR 0005).
///
/// ```
/// use norte_proto::methods::ShutdownMode;
/// assert_eq!(ShutdownMode::default(), ShutdownMode::Stop);
/// assert!(serde_json::from_str::<ShutdownMode>(r#""inventado""#).is_err());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownMode {
    /// Se para y se queda parado. El comportamiento de siempre.
    #[default]
    Stop,
    /// Viene un relevo: los clientes deben volver, y arrancarlo si no está.
    Handover,
}

impl ShutdownMode {
    /// ¿Es la parada de siempre? Lo usa el `skip_serializing_if` de
    /// [`DaemonShutdownParams::mode`].
    #[must_use]
    pub const fn is_stop(&self) -> bool {
        matches!(self, Self::Stop)
    }
}

/// Params de [`DAEMON_SHUTDOWN`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonShutdownParams {
    /// `true` (default): terminar las tasks vivas antes de salir.
    /// `false`: cancelarlas primero (estado limpio garantizado igual).
    #[serde(default = "default_graceful")]
    pub graceful: bool,
    /// Parada o relevo (0.46.0). Default [`ShutdownMode::Stop`], que es lo que
    /// hacía este método antes de que el campo existiera.
    ///
    /// No se serializa cuando es `Stop`: el mensaje que este cliente manda para
    /// una parada corriente sigue siendo BYTE POR BYTE el de 0.45, así que la
    /// compatibilidad hacia atrás no depende de que el otro lado ignore campos
    /// que no conoce — depende de que no haya campo.
    #[serde(default, skip_serializing_if = "ShutdownMode::is_stop")]
    pub mode: ShutdownMode,
}

impl Default for DaemonShutdownParams {
    fn default() -> Self {
        Self {
            graceful: true,
            mode: ShutdownMode::Stop,
        }
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
    /// La UBICACIÓN a consultar. Un fichero se responde por el directorio que
    /// lo contiene (ADR 0054).
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
    /// Capabilities de la ubicación consultada (ADR 0054): la declaración del
    /// provider, refinada con lo que se pueda averiguar de ESE directorio.
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

/// Bajo qué transformación emparejaron dos nombres que NO son los mismos bytes
/// (0.42.0, #152).
///
/// La clave de emparejamiento pliega caja y normaliza a NFC, y **ninguna de las
/// dos es inyectiva**: `README` y `readme` emparejan porque un lado no puede
/// sostener las dos grafías, y `café` NFC y `café` NFD porque son el MISMO
/// texto escrito de dos maneras. Las dos cosas son el comportamiento que se
/// quiere. Lo que no se veía es que la fila resultante —un [`CompareVerdict::Same`]
/// o un [`CompareVerdict::Different`] perfectamente normales— no decía que sus
/// dos mitades no son los mismos bytes, y
/// [`CompareRow::reason_is_consistent`] prohíbe un [`CompareReason`] fuera de
/// [`CompareVerdict::Ambiguous`]/[`CompareVerdict::Error`], así que no había
/// dónde decirlo.
///
/// [`PairTransform::NormalizationSingleton`] es el caso por el que este campo
/// existe. NFC tiene descomposiciones SINGLETON —U+212A KELVIN SIGN normaliza
/// a `K`, U+2126 OHM SIGN a U+03A9—, así que dos ficheros que coexisten en
/// ext4, sin plegado de caja de por medio, y que un lector lee como caracteres
/// DISTINTOS, emparejan y se comparan como si fueran uno. Sin marca en el wire,
/// una sincronización lee esa fila como «actualiza el de la derecha con el de
/// la izquierda» y escribe encima de un fichero que no tiene nada que ver.
///
/// **Separar el singleton del resto es todo el valor de este vocabulario.** Un
/// consumidor que solo supiera «estos dos nombres difieren en bytes» tendría
/// que elegir entre fiarse de todo emparejamiento por normalización —que es el
/// fallo— o rechazarlos todos, y eso rompe el caso macOS↔Linux para el que la
/// clave se diseñó.
///
/// Daemon→client: `#[serde(other)]`, como todo el vocabulario de ADR 0048.
///
/// ```
/// use norte_proto::methods::PairTransform;
/// assert_eq!(
///     serde_json::to_string(&PairTransform::NormalizationSingleton).expect("json"),
///     r#""normalization_singleton""#
/// );
/// // Una transformación de un daemon N+1 degrada; NO tira el lote de filas.
/// let futuro: PairTransform = serde_json::from_str(r#""transliteration""#).expect("degrada");
/// assert_eq!(futuro, PairTransform::Unknown);
/// assert!(!PairTransform::Unknown.names_one_text());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum PairTransform {
    /// **Sin plegar caja NO emparejan**: hizo falta el pliegue, porque uno de
    /// los dos lados no distingue caja (ver `norte_compare::Sides`).
    ///
    /// No dice «difieren SOLO en la caja»: una pareja que además esté en
    /// grafías Unicode distintas —`CAFÉ` precompuesta contra `café`
    /// descompuesta— contesta esto, porque lo que la junta es el pliegue. Lo
    /// que sí promete es [`PairTransform::names_one_text`].
    ///
    /// No es un aviso: en el lado que no distingue caja los dos nombres NO
    /// pueden coexistir, así que emparejarlos es exactamente lo correcto. Viaja
    /// para que un pintor pueda explicar por qué la fila enseña dos grafías.
    CaseFold,
    /// Canónicamente equivalentes con grafías distintas: el par NFC/NFD
    /// clásico, macOS repartiendo NFD y Linux NFC.
    ///
    /// Tampoco es un aviso — es el caso para el que la clave existe—, pero un
    /// destino que deletrea el nombre de otra manera sí importa al escribir:
    /// es lo que [`SyncStep::dest_rel`] lleva.
    Normalization,
    /// Los junta el pliegue COMPLETO de un ext4/f2fs `+F` (0.45.0, #145): la
    /// expansión que `straße.txt` y `strasse.txt` comparten en ESA ubicación y
    /// en ninguna otra.
    ///
    /// Es distinta de [`PairTransform::CaseFold`] y no un matiz suyo: dos
    /// nombres que solo emparejan expandiendo **no nombran un mismo texto**.
    /// Son dos textos que un volumen concreto no puede sostener a la vez, que
    /// es exactamente la situación de [`PairTransform::NormalizationSingleton`]
    /// — el otro lado de la comparación puede tener los dos ficheros, y
    /// distintos. Por eso [`PairTransform::names_one_text`] contesta `false`
    /// aquí, y un plan de sincronización no sobrescribe sobre esta pareja.
    ///
    /// Un cliente 0.44 la lee como [`PairTransform::Unknown`]
    /// (`#[serde(other)]`), que también contesta `false`: degrada al lado
    /// prudente sin saber por qué.
    FullFold,
    /// Emparejaron por una descomposición SINGLETON de NFC, y esa es la que
    /// **puede estar juntando dos ficheros distintos**: U+212A KELVIN SIGN
    /// contra `K`, U+2126 OHM SIGN contra U+03A9. Unicode los declara
    /// canónicamente equivalentes; ext4 los guarda como dos ficheros y un
    /// lector los ve como dos caracteres.
    ///
    /// Gana sobre las otras dos cuando concurren: una pareja que además pliega
    /// caja sigue siendo la peligrosa, y el consumidor que solo mira esta
    /// variante tiene que verla.
    ///
    /// **Es BEST-EFFORT y se equivoca hacia el lado seguro.** Quien la produce
    /// mira si alguno de los dos nombres CONTIENE un carácter con
    /// descomposición singleton, no si ese carácter es exactamente el que los
    /// separa: una pareja NFC/NFD que además lleve un OHM SIGN idéntico en los
    /// dos lados se marca aquí. El conjunto de caracteres es diminuto y ninguno
    /// aparece en un nombre corriente, así que el falso positivo cuesta un
    /// aviso de más y el falso negativo costaría un fichero.
    NormalizationSingleton,
    /// Transformación que este decodificador no conoce (`#[serde(other)]`): un
    /// daemon N+1 la emitió. **No se puede leer como «inocua»** — ver
    /// [`PairTransform::names_one_text`].
    #[serde(other)]
    Unknown,
}

impl PairTransform {
    /// ¿Las dos grafías nombran, con seguridad, UN MISMO texto?
    ///
    /// `true` solo para [`PairTransform::CaseFold`] y
    /// [`PairTransform::Normalization`], que son las dos transformaciones cuyo
    /// emparejamiento es el comportamiento buscado.
    /// [`PairTransform::NormalizationSingleton`] es `false` porque puede juntar
    /// dos ficheros distintos, y [`PairTransform::Unknown`] también: una
    /// transformación que este binario no sabe nombrar tampoco sabe si es
    /// inocua, y el default de «no sé» tiene que ser el prudente.
    ///
    /// ```
    /// use norte_proto::methods::PairTransform;
    /// assert!(PairTransform::CaseFold.names_one_text());
    /// assert!(PairTransform::Normalization.names_one_text());
    /// assert!(!PairTransform::FullFold.names_one_text());
    /// assert!(!PairTransform::NormalizationSingleton.names_one_text());
    /// assert!(!PairTransform::Unknown.names_one_text());
    /// ```
    #[must_use]
    pub fn names_one_text(self) -> bool {
        matches!(self, Self::CaseFold | Self::Normalization)
    }
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
///     paired_under: None,
/// };
/// assert!(row.sides_are_consistent() && row.reason_is_consistent());
/// // Lo ausente NO viaja: ni `null` ni clave (comprobado sobre las CLAVES
/// // del objeto, no por substring: un path puede contener "right").
/// let json = serde_json::to_string(&row).expect("json");
/// let obj: serde_json::Value = serde_json::from_str(&json).expect("objeto");
/// for ausente in ["right", "newer", "reason", "side", "paired_under"] {
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
    /// Los dos lados emparejaron y **sus nombres NO son los mismos bytes**:
    /// bajo qué transformación emparejaron (0.42.0, #152).
    ///
    /// `None` en el caso corriente —una pareja de nombres byte a byte iguales,
    /// o una fila que no tiene dos lados—, y por eso la clave se OMITE: el
    /// payload de una comparación normal sigue siendo byte a byte el de 0.41.0.
    ///
    /// Es independiente de [`CompareRow::reason`] a propósito. Un
    /// emparejamiento por normalización no hace la fila ambigua —el veredicto
    /// es un `Same` o un `Different` legítimo, decidido por el rung que tocara—
    /// y meterlo en `reason` habría exigido relajar
    /// [`CompareRow::reason_is_consistent`], con lo que un cliente 0.41 vería
    /// filas buenas fallar su propia comprobación de coherencia.
    ///
    /// **Lo que un cliente NO debe hacer es recalcularlo.** Los dos [`Entry`]
    /// viajan enteros, así que comparar los bytes de los dos nombres es
    /// posible; saber si el plegado de caja estaba en vigor no lo es, porque
    /// eso sale de las [`Capabilities`](crate::Capabilities) de los DOS
    /// providers y es una propiedad de la pareja, no de un lado. Quien empareja
    /// es quien puede contestar, y este campo es su respuesta.
    ///
    /// # Habla del NOMBRE de esta fila, no de su ruta entera
    /// El emparejamiento es por segmento, y este campo también: dice cómo
    /// emparejaron los dos ÚLTIMOS segmentos, no si algún directorio de encima
    /// emparejó por una transformación. Un directorio `K/` (U+212A) contra un
    /// `K/` ASCII sale con su propia fila marcada, se DESCIENDE —los dos son
    /// directorios y emparejaron—, y cada hijo de dentro empareja por nombres
    /// idénticos y llega con `None`.
    ///
    /// **Un consumidor que decida sobre un SUBÁRBOL tiene que propagar la marca
    /// del ancestro él mismo.** Las filas llegan en pre-orden —el directorio
    /// antes que su contenido—, así que se puede; lo que no se puede es leer
    /// fila a fila y creer que un `None` significa «esta ruta es segura».
    /// Protegido fichero a fichero, un `Mirror` seguiría espejando un subárbol
    /// entero bajo un directorio que solo empareja por un singleton
    /// (`protocol-guardian`, W4b, MAJOR-2).
    ///
    /// # Invariantes (el core las mantiene; un cliente puede asumirlas)
    /// `Some` ⟹ la fila tiene los DOS lados: una transformación es una
    /// propiedad de una pareja, y una fila con un solo lado no la tiene. En
    /// particular una [`CompareVerdict::Ambiguous`] —que es de UN lado por
    /// definición— jamás lo lleva. Una [`CompareVerdict::Error`] SÍ puede, si
    /// trae las dos entradas: el directorio emparejó y lo que falló fue
    /// listarlo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired_under: Option<PairTransform>,
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
    /// #                  confidence: CompareConfidence::Certain, newer: None, reason: None, side: None,
    /// #                  paired_under: None }
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
    /// #                  confidence: CompareConfidence::Unknown, newer: None, reason, side: None,
    /// #                  paired_under: None }
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

/// Params de [`CONNECTION_CLOSE`] (0.49.0, #140).
///
/// ```
/// use norte_proto::methods::ConnectionCloseParams;
/// let p: ConnectionCloseParams =
///     serde_json::from_str(r#"{"path":"sftp://host/casa"}"#).expect("params");
/// assert_eq!(p.path.scheme(), "sftp");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionCloseParams {
    /// Una ruta CUALQUIERA de la conexión. Se cierra por
    /// `scheme://authority`, que es como el core la tiene cacheada: el
    /// frontend manda el sitio donde está el panel y no tiene que saber cómo
    /// se llavea una sesión por dentro.
    pub path: VPath,
}

/// Result de [`CONNECTION_CLOSE`].
///
/// ```
/// use norte_proto::methods::ConnectionCloseResult;
/// let r = ConnectionCloseResult { closed: true };
/// assert!(serde_json::to_value(&r).expect("json")["closed"].as_bool().expect("bool"));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionCloseResult {
    /// `true` si había una sesión y se soltó; `false` si no había ninguna.
    ///
    /// No es un error: quien desconecta quiere quedarse sin conexión, y ya lo
    /// está. Lo dice para que un frontend pueda distinguir «la he cerrado» de
    /// «no había nada», que es la diferencia entre un mensaje útil y uno que
    /// miente.
    pub closed: bool,
}

/// Params de [`FS_DIR_SIZE`] (0.49.0, #139).
///
/// ```
/// use norte_proto::methods::FsDirSizeParams;
/// let p: FsDirSizeParams =
///     serde_json::from_str(r#"{"paths":["file:///a"]}"#).expect("params");
/// assert_eq!(p.paths.len(), 1);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FsDirSizeParams {
    /// Lo que hay que medir. VARIAS raíces a propósito: lo que el humano tiene
    /// marcado es una selección, y sumarla de una vez da UN número —el que
    /// contesta «¿cabe esto en el destino?»— en vez de N tareas que él tenga
    /// que sumar a mano.
    ///
    /// Un fichero suelto vale: cuenta su propio tamaño y no recorre nada.
    /// Vacío es `-32602`: medir la nada no es una petición.
    pub paths: Vec<VPath>,
}

/// Formato de archivo que se sabe ESCRIBIR (0.50.0, #132).
///
/// Menos que los que se saben leer, a propósito: `rar` se delega a un programa
/// externo y solo para lectura (ADR 0056), y 7z no se lee siquiera. Un enum y
/// no un string libre: el conjunto es cerrado y el servidor no tiene que
/// validar vocabulario.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    /// zip con `deflate`, o `store` a nivel 0.
    #[default]
    Zip,
    /// tar plano, sin comprimir.
    Tar,
    /// tar comprimido con gzip.
    TarGz,
}

/// Params de [`ARCHIVE_PACK`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::{ArchiveFormat, ArchivePackParams};
/// let p: ArchivePackParams = serde_json::from_str(
///     r#"{"sources":["file:///a/x"],"dest":"file:///a.zip","base":"file:///a","format":"zip"}"#,
/// )
/// .expect("params");
/// assert_eq!(p.format, ArchiveFormat::Zip);
/// assert_eq!(p.level, None, "el nivel sí lo elige el core");
/// // Y sin `format` NO parsea: es la decisión del cliente, no un default.
/// assert!(
///     serde_json::from_str::<ArchivePackParams>(
///         r#"{"sources":["file:///a/x"],"dest":"file:///a.zip","base":"file:///a"}"#,
///     )
///     .is_err()
/// );
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivePackParams {
    /// Lo que se empaqueta. Un directorio entra con su árbol.
    pub sources: Vec<VPath>,
    /// El archivo que se crea. Tiene que NO existir: sobrescribir aquí sería
    /// una pérdida silenciosa, y el frontend ya sabe preguntar.
    pub dest: VPath,
    /// Formato, explícito y OBLIGATORIO. Ver [`ARCHIVE_PACK`] para por qué no
    /// se deduce del nombre en el servidor — y por qué tampoco tiene default:
    /// con uno, `{"dest":"backup.tar.gz"}` sin `format` producía un ZIP
    /// llamado `backup.tar.gz`, en silencio y contradiciendo el nombre. Eso es
    /// peor que la inferencia que este método rechaza. Exigirlo en un tipo
    /// NUEVO no cuesta compatibilidad; exigirlo después sí sería romperla.
    pub format: ArchiveFormat,
    /// Nivel de compresión 0..=9, o `None` para el del core. 0 es «guardar sin
    /// comprimir» en los formatos que lo permiten.
    ///
    /// Un valor por encima de 9 se RECORTA a 9 en vez de rechazarse: el nivel
    /// es una preferencia, no una petición, y tirar un empaquetado de media
    /// hora por un 42 sería peor que comprimirlo bien.
    #[serde(default)]
    pub level: Option<u8>,
    /// El directorio contra el que se calculan los nombres GUARDADOS.
    ///
    /// Sin esto, «empaqueta estas tres marcas» no tiene un nombre definido
    /// para cada entrada, y dos clientes que eligieran distinto darían dos
    /// archivos distintos de la misma petición. Toda `source` tiene que caer
    /// bajo esta base.
    pub base: VPath,
}

/// Params de [`ARCHIVE_TEST_REPORT`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::ArchiveTestReportParams;
/// let p: ArchiveTestReportParams =
///     serde_json::from_str(r#"{"task_id":7}"#).expect("params");
/// assert_eq!(p.task_id.get(), 7);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveTestReportParams {
    /// La Task cuyo informe se pide.
    pub task_id: crate::TaskId,
}

/// Params de [`ARCHIVE_TEST`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::ArchiveTestParams;
/// let p: ArchiveTestParams =
///     serde_json::from_str(r#"{"path":"file:///a.zip"}"#).expect("params");
/// assert_eq!(p.path.to_wire(), "file:///a.zip");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveTestParams {
    /// El contenedor, como fichero (no como raíz interior): lo que se prueba
    /// es el archivo entero, no una entrada suya.
    pub path: VPath,
}

/// Una entrada que no pasó [`ARCHIVE_TEST`].
///
/// ```
/// use norte_proto::methods::ArchiveTestFailure;
/// let f: ArchiveTestFailure =
///     serde_json::from_str(r#"{"name":"a.txt","reason":"crc"}"#).expect("fallo");
/// assert_eq!(f.reason, "crc");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveTestFailure {
    /// La entrada que falló, ENTERA y en su forma wire — que es la única que
    /// conserva los bytes (regla 1).
    ///
    /// Empezó siendo solo el `name` con pérdidas de abajo, y las dos mitades
    /// de eso estaban mal: `a/x.txt` y `b/x.txt` reportaban lo mismo, y un
    /// nombre que no es UTF-8 volvía como `U+FFFD` sin nada que dijera cuál de
    /// los dos era. Este informe es el ÚNICO sitio donde se nombra la entrada
    /// corrupta, así que tiene que poder señalarla — es el mismo criterio que
    /// [`RenameStuckStep`], que lleva `VPath` por lo mismo.
    #[serde(default = "wire_vacio")]
    pub path: String,
    /// El nombre para ENSEÑAR, con la conversión con pérdidas que lleva
    /// cualquier otro nombre que se pinte. Acompaña a [`Self::path`]; no lo
    /// sustituye.
    pub name: String,
    /// Categoría del fallo, vocabulario ABIERTO comparable por igualdad:
    /// `crc`, `truncated`, `unsupported`, `io`. Puede CRECER de forma aditiva
    /// —un formato futuro falla de formas que este conjunto no tiene—, así que
    /// un cliente que reciba una que no conozca la enseña tal cual y jamás
    /// rechaza el informe por ella. Mismo contrato que
    /// [`ConnectionDegraded::reason`].
    pub reason: String,
}

/// El `path` por defecto de un [`ArchiveTestFailure`] deserializado sin él (un
/// informe de un daemon 0.50 antes de que el campo existiera).
fn wire_vacio() -> String {
    String::new()
}

/// Resultado de [`ARCHIVE_TEST`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::ArchiveTestResult;
/// let r: ArchiveTestResult = serde_json::from_str(r#"{"entries":3}"#).expect("result");
/// assert!(r.failed.is_empty() && !r.truncated);
/// assert!(r.checked.is_empty(), "sin decir qué se comprobó, no se afirma nada");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveTestResult {
    /// Entradas recorridas.
    pub entries: u64,
    /// Las que fallaron, hasta [`ARCHIVE_TEST_MAX_FAILURES`].
    pub failed: Vec<ArchiveTestFailure>,
    /// `true` si hubo más fallos de los que caben en `failed`.
    pub truncated: bool,
    /// QUÉ se ha comprobado de verdad: `crc` cuando el formato lleva suma por
    /// entrada, `gzip_crc` para la cola de un `tar.gz`, `sizes` cuando lo
    /// único verificable es que cada tamaño declarado es alcanzable.
    ///
    /// `snake_case`, como cada otro token que acuña este protocolo
    /// (`tar_gz`, `dir_size`, `rename_batch`): un guion aquí era una
    /// invitación a que un cliente escribiera `gzip_crc`, no encontrara nada y
    /// no encendiera nunca el caso de `tar.gz`. Los tres viajan en el golden.
    ///
    /// Va en el resultado y no en la documentación porque «pasa» significa
    /// cosas distintas en cada formato, y un cliente que pinte «íntegro» sobre
    /// un tar plano estaría afirmando lo que el formato no puede sostener.
    pub checked: Vec<String>,
}

/// Params de [`FILE_SPLIT`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::FileSplitParams;
/// let p: FileSplitParams = serde_json::from_str(
///     r#"{"path":"file:///g.iso","part_bytes":1048576,"dest_dir":"file:///trozos"}"#,
/// )
/// .expect("params");
/// assert_eq!(p.part_bytes, 1_048_576);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSplitParams {
    /// El fichero que se parte. No se toca: los trozos son ficheros nuevos.
    pub path: VPath,
    /// Bytes por trozo, al menos [`FILE_SPLIT_MIN_BYTES`]. El último puede ser
    /// más pequeño; si la división es exacta NO hay un trozo vacío al final.
    pub part_bytes: u64,
    /// Dónde se dejan los trozos.
    pub dest_dir: VPath,
}

/// Params de [`FILE_COMBINE`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::FileCombineParams;
/// let p: FileCombineParams =
///     serde_json::from_str(r#"{"first":"file:///g.iso.001","dest":"file:///g.iso"}"#)
///         .expect("params");
/// assert_eq!(p.first.to_wire(), "file:///g.iso.001");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileCombineParams {
    /// El PRIMER trozo (`.001`). El resto se encuentra por la convención, y un
    /// hueco es un error en vez de una unión a través de él.
    pub first: VPath,
    /// El fichero que se crea. Tiene que no existir.
    pub dest: VPath,
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

    /// La ruta de `path` RELATIVA a `root`, o `None` si no cuelga de ella.
    ///
    /// Vive aquí porque aquí viven los tres tipos que toca —[`VPath`],
    /// [`Segment`] y este— y porque la respuesta tiene que ser UNA: el
    /// transductor que produce los pasos y el frontend que arma la petición
    /// DERIVAN sus `rel` por aquí, que es lo que hace que la comparación de
    /// cadenas wire que el filtro `include` del core hace después signifique
    /// algo. Dos implementaciones que difirieran mandarían un `include` que no
    /// selecciona lo que el lector marcó.
    ///
    /// # Cómo compara, y por qué así
    /// Scheme, authority y luego los segmentos UNO A UNO por sus bytes crudos:
    /// sin `to_str`, sin lossy, sin normalizar y sin plegar mayúsculas (regla
    /// dura 1). Que sea por SEGMENTOS y no por prefijo de cadena es lo que
    /// impide que `…/ab` cuelgue de `…/a`.
    ///
    /// La authority también se compara BYTE A BYTE, y es deliberado aunque un
    /// hostname DNS no distinga mayúsculas: para `mem://` y para un id de
    /// conexión de object storage la authority es un testigo opaco, y plegarla
    /// juntaría dos conexiones distintas. Falla CERRADO — un `None`, nunca una
    /// escritura de más.
    ///
    /// # Lo que el llamante tiene que decidir
    /// El resultado no se puede escapar de la raíz (un [`Segment`] no puede ser
    /// `..`), pero SÍ puede ser la raíz misma (`path == root`), que en un plan
    /// de sincronización es el blanco más destructivo que existe y en un
    /// [`SyncBlocker`] es el valor CORRECTO —un destino de solo lectura cuelga
    /// de la raíz—. Quién sabe cuál de las dos cosas es lo sabe el llamante, así
    /// que se devuelve y se decide allí.
    ///
    /// Y lo mismo con el `None`: **falla cerrado siempre que el llamante lo
    /// convierta en una NEGATIVA**. Los dos de hoy lo hacen (el plan muere, la
    /// selección se rechaza). Un llamante que lo leyera como «sáltate esta
    /// fila» convertiría una comparación estricta en un filtro silencioso, que
    /// es la única forma que tiene esto de ser peligroso.
    ///
    /// ```
    /// use norte_proto::VPath;
    /// use norte_proto::methods::RelPath;
    /// let root = VPath::parse("file:///origen").expect("root");
    /// let path = VPath::parse("file:///origen/sub/a.txt").expect("path");
    /// assert_eq!(
    ///     RelPath::under(&root, &path).expect("cuelga").to_wire(),
    ///     "sub/a.txt"
    /// );
    /// // Por SEGMENTOS, no por prefijo de cadena.
    /// let otro = VPath::parse("file:///origenes/a.txt").expect("path");
    /// assert!(RelPath::under(&root, &otro).is_none());
    /// // La raíz misma sale como la RAÍZ, y decidir qué hacer con eso es del
    /// // llamante.
    /// assert!(RelPath::under(&root, &root).expect("es la raíz").is_root());
    /// ```
    #[must_use]
    pub fn under(root: &VPath, path: &VPath) -> Option<Self> {
        if path.scheme() != root.scheme() || path.authority() != root.authority() {
            return None;
        }
        let mut rest = path.segments();
        for root_segment in root.segments() {
            if rest.next() != Some(root_segment) {
                return None;
            }
        }
        // Inalcanzable: cada uno de estos bytes salió de un `Segment` que un
        // `VPath` ya validó. Se mapea a `None` en vez de `expect` (regla 6).
        let rest = rest.map(Segment::new).collect::<Result<Vec<_>, _>>().ok()?;
        Some(Self::new(rest))
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
/// # Esta columna SOLA no dice si un paso vuelve, y quien pinte un diálogo
/// tiene que leerla junto a [`SyncPlanDone::dest_trash`]
///
/// Dice cómo volvería el paso *donde el destino pueda devolverlo*, que no es la
/// misma pregunta. Un [`StepReversal::Delete`] contra un destino sin papelera
/// se emite igual y el undo lo SALTA (ver esa variante), así que un plan de
/// copias pintado desde esta columna promete un undo que no va a ocurrir. La
/// respuesta completa es el par `(reversal, dest_trash)`, y está escrita en
/// [`DestTrash`].
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
    /// el paso ES reversible allí donde el destino tenga papelera.
    ///
    /// **Con [`DestTrash::Absent`] el undo NO lo ejecuta**, y esta variante
    /// viaja igual. No es un descuido: borrar «lo que hoy haya en esa ruta»
    /// sin papelera de la que volver puede destruir trabajo que el humano hizo
    /// DESPUÉS de sincronizar (#65), así que el undo lo salta y lo cuenta en
    /// `skipped_created_no_trash`. Marcar el paso `Irreversible` mentiría en la
    /// otra dirección —sobre un destino con papelera vuelve entero—, de modo
    /// que la verdad no cabe en esta columna: hay que leerla junto a
    /// [`SyncPlanDone::dest_trash`].
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
    /// **El destino no tiene una papelera de la que VOLVER**, así que este paso
    /// no se puede deshacer. Acompaña SIEMPRE a [`StepReversal::Irreversible`].
    ///
    /// Cubre los dos casos, y quien lo pinte no debe prometer que son el mismo:
    ///
    /// - el destino no tiene papelera, y lo que este paso entierra no está en
    ///   ningún sitio;
    /// - el destino SÍ tiene papelera pero no dice dónde deja las cosas
    ///   (`Provider::trash_restorable` en `false`: macOS y Windows). Lo
    ///   enterrado se saca a mano desde la papelera del sistema, pero el undo
    ///   de norte no puede acertar cuál era — y entonces NINGÚN paso del plan
    ///   es reversible, ni siquiera una copia, porque deshacer una creación
    ///   también pasa por la papelera (#65).
    ///
    /// El token del wire no distingue los dos a propósito: son la misma
    /// consecuencia para quien aprueba, y separarlos sería una variante nueva
    /// (bump de protocolo) para una frase.
    ///
    /// La distinción sí viaja, pero del PLAN y no del paso:
    /// [`SyncPlanDone::dest_trash`] separa «no hay papelera»
    /// ([`DestTrash::Absent`]) de «la hay y no dice dónde deja las cosas»
    /// ([`DestTrash::Opaque`]), que es donde tiene sentido —es una propiedad
    /// del destino, igual para todos los pasos— y donde un diálogo la puede
    /// leer una vez.
    NoTrashOnTarget,
    /// **Los dos lados emparejaron por una transformación que puede juntar
    /// ficheros DISTINTOS**, así que el plan no actúa sobre esa pareja
    /// (0.43.0, #207).
    ///
    /// El caso que lo motiva es
    /// [`PairTransform::NormalizationSingleton`]: `K.txt` con U+212A KELVIN
    /// SIGN contra `K.txt` con la `K` ASCII. Unicode los declara canónicamente
    /// equivalentes, ext4 los guarda como dos ficheros, y un `Overwrite` sobre
    /// esa pareja escribe los bytes de uno encima del otro — que es la pérdida
    /// de datos que #152 describió.
    ///
    /// El criterio es [`PairTransform::names_one_text`] y no la variante
    /// concreta: se salta TODA transformación de la que este binario no pueda
    /// afirmar que nombra un solo texto, incluida una que nombre un daemon más
    /// nuevo. Las corrientes —[`PairTransform::CaseFold`] y
    /// [`PairTransform::Normalization`]— siguen actuando: son las parejas para
    /// las que la clave de emparejamiento existe, y negarlas rompería el caso
    /// macOS↔Linux que sirve.
    ///
    /// Es un `Skip` y NO un bloqueo a propósito: el plan sigue siendo
    /// aprobable y el resto del árbol se sincroniza. Un bloqueo dejaría sin
    /// sincronizar el árbol entero por una pareja rara, y la fila peligrosa se
    /// ve igual en el plan antes de aprobar nada.
    ///
    /// Un cliente N-1 lo decodifica como [`SyncReason::Unknown`] y pinta «un
    /// motivo que esta versión no sabe nombrar»: no actúa de menos ni de más,
    /// porque el paso ya es un `Skip` en el wire.
    NonInjectivePairing,
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
    /// Un [`CompareVerdict::TypeMismatch`] en el que uno de los dos lados es un
    /// DIRECTORIO: cambiar un árbol por un fichero (o al revés) es un cambio
    /// estructural destructivo, y esta spec no lo promete.
    ///
    /// El resto de los desajustes de clase sigue siendo un
    /// [`SyncStepKind::Overwrite`]: sustituir un symlink por un fichero —o un
    /// device, un socket o un `EntryKind::Other` cualquiera— es sustituir bytes,
    /// y es exactamente lo que el paso significa. Un directorio no: un
    /// `Overwrite` dice normativamente «a la papelera y copiar bytes», el paso no
    /// lleva [`EntryKind`] con el que distinguirlo, y el subárbol implicado ni
    /// siquiera está en el plan —el walk no desciende un par que no es de dos
    /// directorios—. Así que lo decide un humano.
    ///
    /// [`SyncBlocker::side`] nombra el lado que tiene el DIRECTORIO (convenio de
    /// plan: [`Side::Left`] el origen, [`Side::Right`] el destino) y va SIEMPRE
    /// presente en esta clase, porque es lo que distingue «borro un árbol del
    /// destino para poner un fichero» de «no copio un árbol del origen encima de
    /// un fichero» — ver [`SyncBlocker::shape_is_consistent`]. Solo uno de los
    /// dos lados puede serlo: si los dos fueran directorios no habría desajuste.
    ///
    /// # Por qué el lado del ORIGEN también bloquea
    /// Es la excepción a la regla que esta familia sigue dos veces —una colisión
    /// del origen es un [`SyncReason::AmbiguousSource`] y una del destino un
    /// [`SyncBlockerKind::AmbiguousDest`]; un directorio demasiado grande del
    /// origen es un `Skip` y uno del destino un
    /// [`SyncBlockerKind::DirTooLarge`]—, y la excepción es deliberada.
    ///
    /// Saltarse la entrada, que es lo que haría un `Skip`, no pierde nada
    /// INMEDIATO: el árbol del origen sigue ahí y el fichero del destino
    /// también. Lo que pierde es la promesa del modo. Quien pidió
    /// [`SyncMode::Mirror`] pidió que el destino quedara como el origen, y con un
    /// fichero donde debería haber un árbol no queda: el plan diría que sí y el
    /// resultado diría que no, y esa divergencia es ESTRUCTURAL —un subárbol
    /// entero que nunca llegará— y no una entrada suelta que el informe pueda
    /// listar. Una colisión de nombres es distinta: ahí no se sabe QUÉ copiar, y
    /// no copiar es la única respuesta segura.
    ///
    /// El precio está medido y aceptado: un solo desajuste de estos en un árbol
    /// de cien mil ficheros deja el plan entero sin aprobar, y el remedio es
    /// arreglar ese nombre o acotar el plan con `include`. Si algún día se
    /// prefiere el `Skip`, hace falta un [`SyncReason`] nuevo — vocabulario
    /// CERRADO daemon→client, o sea un bump y un argumento de compatibilidad.
    TypeMismatchDir,
    /// Un nombre que el DESTINO no puede tener (0.52.0, #163).
    ///
    /// Lo decide el provider del destino (`Provider::name_is_legal`), que es
    /// quien conoce sus reglas: `CON`, `f:ads`, un punto o un espacio finales
    /// —todos legales en ext4— no lo son en NTFS, y `f:ads` es el peor de los
    /// cuatro porque ahí **funciona**: escribe un flujo alternativo, y la
    /// copia dice que fue bien mientras el fichero no está.
    ///
    /// Bloquea en vez de saltar por lo mismo que [`Self::TypeMismatchDir`]:
    /// quien pidió un espejo pidió que el destino quedara como el origen, y un
    /// nombre que no puede existir allí es una divergencia estructural que
    /// ningún informe posterior arregla. El remedio es renombrar en el origen
    /// o acotar el plan con `include`.
    IllegalDestName,
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
///     dest_rel: None,
///     size: Some(1234),
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     reversal: Some(StepReversal::Delete),
///     reason: None,
/// };
/// assert!(s.shape_is_consistent());
/// // Lo ausente NO viaja: ni `null` ni clave.
/// let json = serde_json::to_value(&s).expect("json");
/// assert!(json.get("reason").is_none() && json.get("dest_rel").is_none());
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
    /// La ruta, RELATIVA a las raíces del plan, en BYTES (regla dura 1). Ni un
    /// `String` ni un [`VPath`]: las dos raíces pueden ser de providers
    /// distintos, así que no tiene scheme que llevar — ver [`RelPath`].
    ///
    /// # Respecto a CUÁL de las dos raíces (normativo)
    /// La del lado del que el paso habla, que casi siempre es el ORIGEN:
    /// `dest_root + rel` nombra la misma entrada, salvo cuando
    /// [`SyncStep::dest_rel`] dice otra cosa —y ese campo existe justamente
    /// porque «casi siempre» no es «siempre»—.
    ///
    /// Las excepciones son los pasos que solo hablan del DESTINO, donde `rel`
    /// es relativa a `dest_root` y no hay ruta de origen que nombrar: un
    /// [`SyncStepKind::DeleteTree`], y el [`SyncStepKind::Skip`] de un listado
    /// del destino que no se dejó leer. El paso no lleva un campo que lo
    /// distinga —añadir un lado por dos formas que no escriben no lo valía— así
    /// que un panel que ancle todo `rel` al panel del origen pintará esas dos
    /// en el sitio equivocado.
    pub rel: RelPath,
    /// Cómo se llama la entrada EN EL DESTINO, relativa a la raíz de destino,
    /// cuando sus bytes NO son los de `rel`.
    ///
    /// # La regla, normativa
    /// La ruta del DESTINO sobre la que este paso cae, presente si y SOLO si
    /// sus bytes difieren de los de `rel` (regla dura 1: se comparan bytes,
    /// jamás cadenas, y jamás después de plegar). `None` —el caso común, y por
    /// eso la clave viaja AUSENTE— significa «en el destino se llama
    /// exactamente `rel`».
    ///
    /// Quien ejecuta el paso lee `source_root + rel` y escribe
    /// `dest_root + dest_rel.unwrap_or(rel)`.
    ///
    /// Presente NO afirma que ahí exista algo, y quien lo lea no debe deducirlo:
    /// hoy el core solo lo puebla desde una entrada del destino que la fila
    /// traía, pero la regla es sobre la RUTA, no sobre lo que hay en ella.
    /// Tampoco afirma qué clase de cosa hay: una ruta byte-idéntica puede ser
    /// hoy un symlink que apunta fuera del árbol, y eso solo lo puede resolver
    /// el ejecutor cuando abre.
    ///
    /// # Por qué hace falta
    /// La comparación empareja por una clave PLEGADA —NFC siempre, mayúsculas
    /// cuando alguno de los dos lados no distingue caja—, así que una pareja
    /// legítima puede tener dos nombres de bytes distintos: un `café` NFC del
    /// origen contra el `café` NFD del destino, un `README` contra el `readme`
    /// de un APFS. Sin este campo un [`SyncStepKind::Overwrite`] se escribiría
    /// bajo el nombre del ORIGEN, que sobre ext4 crea un SEGUNDO fichero al
    /// lado del que se quería sobrescribir; y su
    /// [`StepReversal::RestoreTrash`] prometería sacar de la papelera algo que
    /// nadie enterró. Ver <https://github.com/compilando/norte/issues/152>.
    ///
    /// # Por qué el destino NO se renombra
    /// Deletrear el destino como lo deletrea el origen convertiría cada
    /// sincronización entre macOS y Linux en un baile de renombrados —NFD y NFC
    /// son EL MISMO nombre para quien lo lee— y ese churn es justo lo que este
    /// árbol existe para no producir. Se escribe donde está.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_rel: Option<RelPath>,
    /// Bytes que MUEVE este paso, cuando se saben; un provider que no da tamaño
    /// deja `None`.
    ///
    /// Un [`SyncStepKind::DeleteTree`], un [`SyncStepKind::CreateDir`] y un
    /// [`SyncStepKind::Skip`] no mueven ninguno y la dejan AUSENTE.
    ///
    /// La regla es NORMATIVA aunque casi nada la haga cumplir:
    /// [`SyncCounts::add`] IGNORA el tamaño de un paso que no mueve bytes —así
    /// que un tamaño de más no corrompe el número que el humano aprueba— y
    /// [`SyncStep::shape_is_consistent`] tampoco lo mira. Lo que sí lo nota es
    /// el `plan_hash`, que alimenta el campo pase lo que pase: dos planes que
    /// solo difieran en un tamaño puesto donde no toca son dos planes
    /// distintos, y hacen falta dos aprobaciones.
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
    /// ¿Concuerdan entre sí `kind`, `reversal`, `reason` y `dest_rel`?
    ///
    /// TRES de las invariantes que el wire no sabe expresar, enunciadas UNA
    /// vez, aquí: `reversal` es `None` si y solo si `kind` es
    /// [`SyncStepKind::Skip`], `reason` es `Some` para EXACTAMENTE un `Skip`
    /// y un paso cuya reversa es [`StepReversal::Irreversible`], y
    /// [`SyncStep::dest_rel`] —que existe para nombrar la OTRA ortografía— no
    /// puede ser la misma `rel`: ahí la clave sobra, y un consumidor que la vea
    /// repetida está leyendo un paso que su productor no calculó como manda.
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
    /// legítimos. La regla de `dest_rel` alcanza igualmente a un paso cuya
    /// REVERSA no se conozca —no depende de ella—, pero no a uno cuya CLASE no
    /// se conozca: ahí la exención es total, y se puede permitir serlo porque un
    /// `dest_rel` repetido es redundante y no peligroso (las dos ramas de
    /// `dest_rel.unwrap_or(rel)` dan la misma ruta).
    ///
    /// ```
    /// # use norte_proto::methods::{
    /// #     CompareConfidence, CompareCriterion, RelPath, StepReversal, SyncReason, SyncStep,
    /// #     SyncStepKind,
    /// # };
    /// # fn step(kind: SyncStepKind, reversal: Option<StepReversal>, reason: Option<SyncReason>) -> SyncStep {
    /// #     SyncStep { id: 1, kind, rel: RelPath::parse_wire("a").expect("rel"), dest_rel: None,
    /// #                size: None, criterion: CompareCriterion::Presence,
    /// #                confidence: CompareConfidence::Certain, reversal, reason }
    /// # }
    /// assert!(step(SyncStepKind::Copy, Some(StepReversal::Delete), None).shape_is_consistent());
    /// assert!(!step(SyncStepKind::Copy, None, None).shape_is_consistent());
    /// assert!(
    ///     step(SyncStepKind::Skip, None, Some(SyncReason::Unreadable)).shape_is_consistent()
    /// );
    /// assert!(!step(SyncStepKind::Skip, None, None).shape_is_consistent());
    /// // `dest_rel` nombra la OTRA ortografía, así que repetir `rel` es ruido.
    /// let mut s = step(SyncStepKind::Overwrite, Some(StepReversal::Delete), None);
    /// s.dest_rel = Some(s.rel.clone());
    /// assert!(!s.shape_is_consistent());
    /// s.dest_rel = Some(RelPath::parse_wire("A").expect("rel"));
    /// assert!(s.shape_is_consistent());
    /// ```
    #[must_use]
    pub fn shape_is_consistent(&self) -> bool {
        if self.kind == SyncStepKind::Unknown {
            return true;
        }
        // Se compara por BYTES —lo hace el `Eq` de [`Segment`]—, que es la
        // misma comparación con la que quien produce el paso decidió poblarla:
        // plegar aquí daría por buena justamente la pareja que el campo existe
        // para distinguir.
        if self.dest_rel.as_ref() == Some(&self.rel) {
            return false;
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
/// Se llena paso a paso con [`SyncCounts::add`], que es donde están escritas
/// —una vez— las reglas de qué cuenta dónde.
///
/// ```
/// use norte_proto::methods::SyncCounts;
/// let c = SyncCounts { copy: 2, bytes: 30, ..SyncCounts::default() };
/// assert_eq!(serde_json::to_value(&c).expect("json")["delete_tree"], 0);
/// // El total de bytes viene SIEMPRE acompañado de cuántos pasos no lo saben.
/// assert_eq!(serde_json::to_value(&c).expect("json")["unmeasured_steps"], 0);
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
    /// Pasos de una clase que este decodificador NO conoce
    /// ([`SyncStepKind::Unknown`]): un daemon una versión por delante emitió
    /// algo que este cliente no sabe clasificar.
    ///
    /// El core lo deja SIEMPRE en cero — jamás emite un paso que no sepa
    /// nombrar—, así que solo se llena en un cliente N-1 que sume por su cuenta
    /// los lotes de [`SYNC_STEPS`]. Existe por lo mismo que `unmeasured_steps`:
    /// sin él, esos pasos no aparecerían en NINGÚN contador y la suma de las cinco
    /// clases diría que el plan es más pequeño de lo que es, que es aprobar a
    /// ciegas un trozo del plan.
    pub unknown_kind: u64,
    /// Pasos cuya reversa es [`StepReversal::Irreversible`]. Se cuenta APARTE
    /// porque es el único número que un humano no debe tener que derivar.
    ///
    /// Es TRANSVERSAL a las clases —un `Overwrite`, un `DeleteTree` y un paso
    /// de clase desconocida que se declare irreversible suman aquí—, así que no
    /// se suma con los contadores de clase: se lee al lado de ellos.
    pub irreversible: u64,
    /// Bytes que mueve el plan, de los pasos que MUEVEN bytes y traen tamaño.
    /// Un borrado y un `Skip` no mueven ninguno.
    ///
    /// Se lee SIEMPRE junto a `unmeasured_steps`: por sí solo es una cota
    /// inferior, no un total — [`SyncCounts::exact_bytes`] es el total o nada.
    /// La suma satura en `u64::MAX`, así que un valor exactamente igual a
    /// `u64::MAX` puede ser un tope y no una medida.
    ///
    /// NO es lo mismo que [`SyncReportResult::bytes`], que son los bytes
    /// EFECTIVAMENTE movidos al ejecutar: sobre `file://` los dos números
    /// difieren de serie, así que este no sirve de denominador de una barra de
    /// progreso.
    pub bytes: u64,
    /// Cuántos pasos mueven bytes SIN que se sepa cuántos
    /// ([`SyncStep::size`] ausente).
    ///
    /// No es un caso raro: un huérfano no se hidrata
    /// (<https://github.com/compilando/norte/issues/157>) y `norte-vfs-local`
    /// lista con `size: None`, así que sobre `file://` es el caso NORMAL. Sin
    /// este campo, `bytes` valdría cero y el diálogo de aprobación diría con
    /// toda confianza que un plan de 40 GB no mueve nada.
    ///
    /// Contarlos aparte es la regla que ADR 0048 ya fijó para esta familia: «el
    /// provider no lo puede decir» es una respuesta, no un error, y no se
    /// esconde dentro de un número que parece cierto. El diálogo lee «1,2 GB +
    /// 340 ficheros de tamaño desconocido». Hidratar los tamaños es una
    /// optimización posterior (issue #156) que solo puede ENCOGER este número,
    /// nunca cambiar la forma.
    ///
    /// Son PASOS, no bytes —el nombre lo dice, y por eso lo dice: al lado de
    /// `bytes`, un `bytes_unknown` se leía como «7 bytes que no sabemos» en vez
    /// de «7 pasos que no pudimos medir». Solo [`SyncStepKind::Copy`] y
    /// [`SyncStepKind::Overwrite`] lo incrementan, así que
    /// `unmeasured_steps <= copy + overwrite` siempre, y un consumidor puede
    /// comprobarlo antes de fiarse de unos contadores que no calculó él.
    pub unmeasured_steps: u64,
}

impl SyncCounts {
    /// Suma UN paso. Las reglas de qué cuenta dónde, escritas una vez.
    ///
    /// - Cada clase suma en su contador, [`SyncStepKind::Unknown`] incluida
    ///   (`unknown_kind`): meterla en el contador de otra mentiría sobre lo que
    ///   el plan hace, pero no contarla en ninguno mentiría sobre CUÁNTO plan
    ///   hay. El `match` es EXHAUSTIVO a propósito —dentro del crate que define
    ///   el enum, `#[non_exhaustive]` no aplica— para que una clase nueva rompa
    ///   la compilación aquí en vez de dejar de contarse en silencio.
    /// - `irreversible` suma para CUALQUIER clase cuya reversa sea
    ///   [`StepReversal::Irreversible`], la desconocida incluida: es el número
    ///   que un humano no debe tener que derivar, y no saber qué clase de paso
    ///   es no lo hace menos irreversible.
    /// - Solo [`SyncStepKind::Copy`] y [`SyncStepKind::Overwrite`] mueven
    ///   bytes. Un `CreateDir` no escribe contenido, y un `DeleteTree` y un
    ///   `Skip` no escriben nada — un tamaño en cualquiera de ellos se IGNORA
    ///   en vez de sumarse, porque un byte contado que nunca se mueve es el
    ///   diálogo de aprobación mintiendo.
    /// - De los que sí mueven, el que trae tamaño suma en `bytes` y el que no
    ///   suma UNO en `unmeasured_steps`. Jamás un cero fingido.
    ///
    /// Las sumas son saturantes: un contador desbordado es un número raro, pero
    /// un pánico en el camino de un plan de medio millón de pasos es una Task
    /// muerta.
    ///
    /// ```
    /// use norte_proto::methods::{
    ///     CompareConfidence, CompareCriterion, RelPath, StepReversal, SyncCounts, SyncStep,
    ///     SyncStepKind,
    /// };
    /// let paso = |kind, size| SyncStep {
    ///     id: 1,
    ///     kind,
    ///     rel: RelPath::parse_wire("a").expect("rel"),
    ///     dest_rel: None,
    ///     size,
    ///     criterion: CompareCriterion::Presence,
    ///     confidence: CompareConfidence::Certain,
    ///     reversal: Some(StepReversal::Delete),
    ///     reason: None,
    /// };
    /// let mut c = SyncCounts::default();
    /// c.add(&paso(SyncStepKind::Copy, Some(10)));
    /// c.add(&paso(SyncStepKind::Copy, None));
    /// assert_eq!((c.copy, c.bytes, c.unmeasured_steps), (2, 10, 1));
    /// // Y con un paso sin medir, el total EXACTO no existe.
    /// assert_eq!(c.exact_bytes(), None);
    /// ```
    pub fn add(&mut self, step: &SyncStep) {
        // EXHAUSTIVO, sin comodín: `#[non_exhaustive]` no aplica dentro del
        // crate que define el enum, así que una clase nueva rompe aquí la
        // compilación en vez de dejar de contarse sin que nadie se entere. Los
        // bytes se deciden en el MISMO `match` por lo mismo: quien añada una
        // clase que escribe contenido tiene que decir a la vez si suma bytes.
        match step.kind {
            SyncStepKind::CreateDir => self.create_dir = self.create_dir.saturating_add(1),
            SyncStepKind::Copy => {
                self.copy = self.copy.saturating_add(1);
                self.add_bytes(step.size);
            }
            SyncStepKind::Overwrite => {
                self.overwrite = self.overwrite.saturating_add(1);
                self.add_bytes(step.size);
            }
            SyncStepKind::DeleteTree => self.delete_tree = self.delete_tree.saturating_add(1),
            SyncStepKind::Skip => self.skip = self.skip.saturating_add(1),
            SyncStepKind::Unknown => self.unknown_kind = self.unknown_kind.saturating_add(1),
        }
        if step.reversal == Some(StepReversal::Irreversible) {
            self.irreversible = self.irreversible.saturating_add(1);
        }
    }

    /// Los bytes de un paso que SÍ mueve bytes: sumados si se saben, contados
    /// aparte si no.
    fn add_bytes(&mut self, size: Option<u64>) {
        match size {
            Some(bytes) => self.bytes = self.bytes.saturating_add(bytes),
            None => self.unmeasured_steps = self.unmeasured_steps.saturating_add(1),
        }
    }

    /// El total EXACTO de bytes, o `None` si algún paso no se pudo medir.
    ///
    /// Es el `Option<u64>` que `bytes` deliberadamente NO es. Los dos campos
    /// viajan por el wire porque una cota inferior más el tamaño de la
    /// ignorancia («1,2 GB + 340 ficheros sin medir») es una frase que se puede
    /// enseñar, y un `None` no lo es — sobre `file://` sería además el caso
    /// normal, así que el diálogo no tendría nunca nada que decir. Quien de
    /// verdad necesite el total o nada, lo pide aquí y no vuelve a derivarlo.
    ///
    /// ```
    /// use norte_proto::methods::SyncCounts;
    /// let exacto = SyncCounts { copy: 1, bytes: 10, ..SyncCounts::default() };
    /// assert_eq!(exacto.exact_bytes(), Some(10));
    /// let a_medias = SyncCounts { unmeasured_steps: 1, ..exacto };
    /// assert_eq!(a_medias.exact_bytes(), None, "un paso sin medir no es cero");
    /// ```
    #[must_use]
    pub fn exact_bytes(&self) -> Option<u64> {
        (self.unmeasured_steps == 0).then_some(self.bytes)
    }
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
    /// El lado en el que ocurrió, cuando ocurrió en uno solo.
    ///
    /// # El convenio, normativo
    /// En un plan el ORIGEN es [`Side::Left`] y el DESTINO [`Side::Right`],
    /// **siempre**, y no tiene nada que ver con qué panel lanzó la comparación:
    /// una petición de sincronización nombra `source` y `dest`
    /// ([`SyncPlanParams`]) y no lleva ningún [`Side`], así que dentro de esta
    /// familia no hay un segundo sistema de coordenadas con el que confundirlo.
    /// Un frontend que sincronizó de derecha a izquierda pinta un `right` en su
    /// panel IZQUIERDO.
    ///
    /// Ausente cuando el bloqueo no es de un lado: un solape lo es de las dos
    /// raíces a la vez.
    ///
    /// **Presente SIEMPRE para [`SyncBlockerKind::TypeMismatchDir`]**, que es el
    /// único cuyo lado no se puede deducir de la clase — ver
    /// [`SyncBlocker::shape_is_consistent`].
    ///
    /// (`Error::OverlappingRoots` NO usa este convenio y no hay que buscarlo
    /// ahí: lleva un [`RootOverlap`](crate::RootOverlap) precisamente porque
    /// «son el mismo árbol» es un tercer caso que dos lados no saben decir.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
}

impl SyncBlocker {
    /// ¿Concuerdan `kind` y `side`?
    ///
    /// La invariante que el wire no sabe expresar, enunciada UNA vez, aquí:
    /// [`SyncBlockerKind::TypeMismatchDir`] lleva `side` SIEMPRE. Es el único
    /// bloqueo cuyo lado no se deduce de su clase —`AmbiguousDest`,
    /// `DirTooLarge` y `DestReadOnly` son del destino por definición, y un
    /// solape no es de ninguno— y a la vez el único en el que el lado ES la
    /// frase: «no copio un árbol del origen encima de un fichero» y «no borro un
    /// árbol del destino para poner un fichero» son dos cosas distintas, y sin
    /// `side` no hay ninguna que pintar.
    ///
    /// NO es un rechazo de `Deserialize`, por el mismo motivo que
    /// [`SyncStep::shape_is_consistent`]: un bloqueo malformado tiene que
    /// degradar, no matar la lista entera. Y
    /// [`SyncBlockerKind::Unknown`] queda EXENTO: un bloqueo de un daemon una
    /// versión por delante no es algo que este cliente pueda juzgar.
    ///
    /// ```
    /// use norte_proto::methods::{RelPath, Side, SyncBlocker, SyncBlockerKind};
    /// let mut b = SyncBlocker {
    ///     rel: RelPath::parse_wire("build").expect("rel"),
    ///     kind: SyncBlockerKind::TypeMismatchDir,
    ///     side: Some(Side::Right),
    /// };
    /// assert!(b.shape_is_consistent());
    /// b.side = None;
    /// assert!(!b.shape_is_consistent(), "sin lado no hay frase que pintar");
    /// // Un solape no es de un lado, y eso es correcto.
    /// b.kind = SyncBlockerKind::OverlapDetected;
    /// assert!(b.shape_is_consistent());
    /// ```
    #[must_use]
    pub fn shape_is_consistent(&self) -> bool {
        self.kind != SyncBlockerKind::TypeMismatchDir || self.side.is_some()
    }
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
    ///
    /// # Qué significa exactamente «se restringe»
    /// Cinco reglas, normativas, porque ninguna se deduce de la frase de arriba:
    ///
    /// 1. **Restringe los PASOS del plan, no las filas de la comparación.** El
    ///    árbol se recorre entero de todos modos: la ortografía que el destino
    ///    le da a una carpeta viaja en la fila de la carpeta, que suele ser
    ///    `Same` y no produce paso alguno, así que un plan que solo mirase lo
    ///    seleccionado compondría rutas de destino con la ortografía del ORIGEN.
    /// 2. **Nombrar una carpeta arrastra su subárbol**, por prefijo de
    ///    SEGMENTOS. `café` no arrastra a `cafétière`.
    /// 3. **Y al revés lo justo:** un [`SyncStepKind::CreateDir`] cuya `rel` sea
    ///    ancestro de algo seleccionado se queda, aunque no se nombrara — sin él
    ///    la copia elegida iría a un directorio que no existe. Ninguna otra
    ///    clase se arrastra hacia arriba: un [`SyncStepKind::DeleteTree`] en un
    ///    ancestro borraría justo lo que se pidió sincronizar.
    /// 4. **La comparación es por BYTES**, sobre esta misma forma wire. No
    ///    normaliza y no pliega la caja, ni siquiera sobre un sistema de
    ///    ficheros que sí lo haga: una ruta en NFC no casa con la misma en NFD, y
    ///    `README` no casa con `readme`. Un cliente debe mandar los bytes que
    ///    vio, no una versión reescrita de ellos. Cuidado además con los pasos
    ///    cuya `rel` se mide contra el DESTINO ([`SyncStep::rel`]): un
    ///    [`SyncStepKind::DeleteTree`] bajo una carpeta que los dos lados
    ///    escriben distinto no lo cubre una selección tomada del lado del origen.
    /// 5. **La lista VACÍA es una selección de nada**, no «todo»: un plan sin
    ///    pasos, `executable`. `include: [""]` (la raíz) sí es todo. Quien no
    ///    quiera filtrar OMITE el campo.
    ///
    /// Los BLOQUEOS no se filtran: ver [`SyncPlanDone::executable`].
    #[cfg_attr(feature = "schema", schemars(extend("maxItems" = SYNC_MAX_INCLUDE)))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<RelPath>>,
}

/// Qué papelera tiene el DESTINO de un plan, y por tanto qué puede devolver el
/// undo (0.40.0, ADR 0049). Daemon→client: `#[serde(other)]`.
///
/// # Por qué viaja, si cada paso ya lleva su [`StepReversal`]
///
/// Porque `reversal` NO alcanza para decidir la frase que un humano necesita
/// leer antes de aprobar. Un [`SyncStepKind::Copy`] contra un destino SIN
/// papelera sale con [`StepReversal::Delete`] —el paso es reversible allí donde
/// hay papelera, y marcarlo irreversible mentiría en la otra dirección—, pero el
/// undo de ese `created` pasa también por la papelera (#65) y, no habiéndola, lo
/// SALTA: la copia se queda. Un plan de solo copias contra un destino sin
/// papelera y otro contra un destino con papelera restaurable son, paso a paso,
/// byte a byte, el MISMO plan; y uno se deshace entero y el otro no se deshace
/// nada. Sin este campo no hay forma de distinguirlos, y un diálogo que lea
/// `reversal` a secas promete lo que el undo no va a dar.
///
/// # Las tres respuestas, y la diferencia entre las dos malas
///
/// - [`DestTrash::Restorable`] — hay papelera y NOMBRA lo que entierra
///   (`Provider::trash_restorable`): el journal se queda con su `reversal_ref` y
///   el undo PUEDE devolver el lote, copias incluidas. `file://` en Linux/BSD, y
///   `sftp://`/objeto con la papelera lógica.
/// - [`DestTrash::Opaque`] — hay papelera pero no dice dónde deja las cosas
///   (`file://` en macOS y Windows). Todos los pasos que ACTÚAN salen
///   [`StepReversal::Irreversible`] con [`SyncReason::NoTrashOnTarget`] (un
///   [`SyncStepKind::Skip`] no actúa y sigue sin reversa); lo enterrado sigue
///   existiendo y se puede rescatar A MANO desde la papelera del sistema, pero
///   el undo de norte no puede acertar cuál era.
/// - [`DestTrash::Absent`] — no hay papelera. Lo que se sobrescribe o se borra no
///   está en ningún sitio, y lo que se copia tampoco vuelve (el undo lo salta).
///
/// # Es una promesa sobre el PLAN, no una garantía por entrada
///
/// [`DestTrash::Restorable`] dice que el destino sabe nombrar lo que entierra,
/// no que cada entrada vaya a volver. `Provider::trash_restorable` es una
/// promesa de la IMPLEMENTACIÓN y su propio contrato admite que un caso
/// concreto conteste `None`; y una ruta que cambió entre el `sync.apply` y el
/// undo se BLOQUEA en vez de tocarse. Las dos cosas terminan igual: la entrada
/// no vuelve y el informe del undo la NOMBRA. Un diálogo puede decir «esto se
/// puede deshacer»; no puede decir «esto va a volver entero pase lo que pase».
///
/// El resultado NETO de las dos últimas es el mismo —el undo no devuelve nada—,
/// y aun así son dos avisos distintos: en una el fichero existe y en la otra no.
/// Por eso son dos valores y no un booleano.
///
/// ```
/// use norte_proto::methods::DestTrash;
/// assert_eq!(serde_json::to_string(&DestTrash::Opaque).expect("json"), r#""opaque""#);
/// // Solo una de las tres devuelve algo.
/// assert!(DestTrash::Restorable.restores());
/// assert!(!DestTrash::Opaque.restores() && !DestTrash::Absent.restores());
/// // Y un valor de un daemon del futuro NO se toma por ninguna de las tres.
/// let futuro: DestTrash = serde_json::from_str(r#""quantum""#).expect("degrada");
/// assert_eq!(futuro, DestTrash::Unknown);
/// assert!(!futuro.restores(), "lo que no se conoce no se promete");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum DestTrash {
    /// Papelera que nombra lo que entierra: el undo PUEDE devolver el lote
    /// entero, copias incluidas. Sin garantía por entrada — ver la nota del
    /// tipo.
    Restorable,
    /// Papelera que NO nombra lo que entierra: nada del plan se deshace, y lo
    /// enterrado se rescata a mano desde la papelera del sistema.
    Opaque,
    /// Sin papelera: nada del plan se deshace, y lo destruido no está en ningún
    /// sitio.
    Absent,
    /// Respuesta que este decodificador no conoce (`#[serde(other)]`). El core
    /// jamás la emite. **No promete nada**: [`DestTrash::restores`] es `false`.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

impl DestTrash {
    /// La respuesta a partir de los dos hechos que el core mide del provider del
    /// destino: si declara `CapabilityFlags::TRASH` y qué promete
    /// `Provider::trash_restorable`.
    ///
    /// Escrita UNA vez, aquí, porque es la traducción de la MISMA pareja de
    /// booleanos con la que `norte_sync` decide el [`StepReversal`] de cada
    /// paso: si las dos derivaciones se separan, el plan y su resumen dirían
    /// cosas distintas sobre el mismo destino.
    ///
    /// ```
    /// use norte_proto::methods::DestTrash;
    /// assert_eq!(DestTrash::of(true, true), DestTrash::Restorable);
    /// assert_eq!(DestTrash::of(true, false), DestTrash::Opaque);
    /// // Sin papelera, lo que la papelera prometería no decide nada.
    /// assert_eq!(DestTrash::of(false, true), DestTrash::Absent);
    /// assert_eq!(DestTrash::of(false, false), DestTrash::Absent);
    /// ```
    #[must_use]
    pub fn of(has_trash: bool, restorable: bool) -> Self {
        match (has_trash, restorable) {
            (true, true) => Self::Restorable,
            (true, false) => Self::Opaque,
            (false, _) => Self::Absent,
        }
    }

    /// ¿Devuelve algo el undo de un plan aplicado sobre este destino?
    ///
    /// `true` para [`DestTrash::Restorable`] y para nada más — incluida la
    /// variante desconocida, que no es una promesa sino una laguna.
    ///
    /// ```
    /// use norte_proto::methods::DestTrash;
    /// assert!(DestTrash::Restorable.restores());
    /// assert!(!DestTrash::Absent.restores());
    /// ```
    #[must_use]
    pub fn restores(self) -> bool {
        matches!(self, Self::Restorable)
    }
}

/// Payload de [`SYNC_PLAN_DONE`] (0.40.0, ADR 0049): lo que hay que saber para
/// aprobar un plan, y el hash con el que se aprueba.
///
/// ```
/// use norte_proto::TaskId;
/// use norte_proto::methods::{DestTrash, PlanHash, SyncCounts, SyncPlanDone};
/// let d = SyncPlanDone {
///     task_id: TaskId::new(7),
///     plan_hash: PlanHash::parse(&"0".repeat(64)).expect("hex"),
///     counts: SyncCounts::default(),
///     blockers: vec![],
///     blockers_total: 0,
///     executable: true,
///     dest_trash: DestTrash::Restorable,
/// };
/// let json = serde_json::to_value(&d).expect("json");
/// // `blockers` vacío es una lista vacía, jamás una clave ausente.
/// assert_eq!(json["blockers"], serde_json::json!([]));
/// assert_eq!(json["plan_hash"], serde_json::json!("0".repeat(64)));
/// assert_eq!(json["dest_trash"], serde_json::json!("restorable"));
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
    ///
    /// `counts.bytes` es una COTA INFERIOR, no un total: los pasos cuyo tamaño
    /// el provider no dio se cuentan en `counts.unmeasured_steps` en vez de sumar
    /// cero (ver [`SyncCounts`], y [`SyncCounts::exact_bytes`] para el total o
    /// nada). Un diálogo que enseñe `bytes` a secas miente sobre casi cualquier
    /// plan de `file://`.
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
    ///
    /// **[`SyncPlanParams::include`] NO recorta los bloqueos**, así que esto
    /// habla siempre de la comparación ENTERA. Es deliberado: hay bloqueos cuyo
    /// alcance es el árbol —[`SyncBlockerKind::DestReadOnly`] cuelga de la raíz,
    /// que ninguna selección nombra— y recortarlos por la selección convertiría
    /// un destino de solo lectura en un plan ejecutable. La consecuencia que un
    /// frontend tiene que saber pintar: una selección de tres ficheros puede
    /// volver con `executable: false` por algo que está a cuarenta mil filas de
    /// distancia y que el usuario no tiene delante.
    pub executable: bool,
    /// Qué papelera tiene el DESTINO, o sea qué puede devolver el undo de este
    /// plan si se aplica ([`DestTrash`]).
    ///
    /// **Sin este campo no se puede pintar el diálogo de aprobación sin
    /// mentir**, y el motivo está entero en el rustdoc de [`DestTrash`]: el
    /// [`StepReversal`] de cada paso no distingue un plan de copias que se
    /// deshace entero de uno idéntico que no se deshace nada. Habla del plan
    /// COMPLETO —es una propiedad del provider del destino, no de un paso—, así
    /// que [`SyncPlanParams::include`] no lo afecta.
    ///
    /// Sin `serde(default)` a propósito, por lo mismo que los contadores nuevos
    /// de [`SyncCounts`]: un default sería una respuesta inventada sobre si algo
    /// se puede deshacer, y no hay ninguna versión publicada que lo omita (0.40.0
    /// es el bump que estrena la familia entera). [`DestTrash`] tampoco deriva
    /// `Default`, así que ponerle un `serde(default)` más adelante no compila en
    /// silencio: hay que elegir a mano qué se inventa, que es justo la decisión
    /// que no debe pasar desapercibida.
    ///
    /// No entra en el `plan_hash` y no hace falta que entre: sale de
    /// `dest_has_trash` y `dest_trash_restorable`, que el hasher ya siembra, así
    /// que dos planes con papeleras distintas ya tienen digests distintos. O
    /// sea que este campo no puede contradecir al plan que autoriza ejecutar.
    ///
    /// **DESPUÉS de aplicar, quien manda es el informe.**
    /// [`SyncReportResult::dest_trash`] repite este valor (0.42.0, #170) para
    /// que el informe se baste solo, pero es el `batch_id` de ese informe el
    /// que dice si hay algo que deshacer: ausente significa que no llegó a
    /// abrirse lote alguno, por mucho que el plan prometiera. Lo que el undo
    /// acabó salvando lo cuenta `PolicyUndoReportResult`, con
    /// `skipped_created_no_trash` como la cara *a posteriori* de
    /// [`DestTrash::Absent`].
    pub dest_trash: DestTrash,
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
    ///
    /// Es una respuesta sobre el PERMISO, y por eso está separada de
    /// [`SyncFailureCause::IllegalName`]: «no puedes» y «así no se puede llamar»
    /// llevan a acciones distintas —pedir acceso, o arreglar el nombre— y un
    /// informe que las mezclara no serviría para ninguna de las dos.
    Denied,
    /// El nombre no es legal bajo la raíz de DESTINO.
    ///
    /// # Por qué es un fallo de ejecución y no un bloqueo del plan
    /// Nada comprueba, al planificar, que un nombre legal bajo el origen lo sea
    /// bajo el destino, y comprobarlo exigiría modelar las reglas de nombres de
    /// cada filesystem —cuáles, y con qué límites, no está en
    /// [`Capabilities`](crate::Capabilities)—. Los casos son reales: 86 `é` en
    /// NFC ocupan 172 bytes y 258 en NFD, que revienta `NAME_MAX`; `CON`, un
    /// punto final y un espacio final no son nombres en Windows; y `f:ads`
    /// escribe un flujo de datos alternativo y «funciona».
    ///
    /// Así que sale por aquí, con nombre propio. Un [`SyncFailureCause::Io`]
    /// genérico habría dicho «algo se rompió» de la única familia de fallos que
    /// el usuario puede arreglar él solo, y de la única que se repetirá idéntica
    /// en cada intento hasta que la arregle.
    ///
    /// # Es BEST-EFFORT, y conviene no leerlo como una garantía
    /// Depende de que el provider sepa distinguir «ese nombre no vale» de «algo
    /// falló», y no todos pueden: `file://` sí (`InvalidFilename`, `EILSEQ`),
    /// pero SFTP v3 contesta un `Failure` genérico a casi todo y el
    /// almacenamiento de objetos no distingue una clave demasiado larga de
    /// cualquier otro rechazo — en esos dos, un nombre ilegal llega como
    /// [`SyncFailureCause::Io`]. La ausencia de esta causa NO prueba que los
    /// nombres estuvieran bien; su presencia sí prueba que uno no lo estaba.
    IllegalName,
    /// La lectura o la escritura se rompieron.
    Io,
    /// Causa que este decodificador no conoce (`#[serde(other)]`). El core
    /// jamás la emite.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// UN paso que no ocurrió (0.40.0, ADR 0049).
///
/// ```
/// use norte_proto::methods::{RelPath, SyncFailure, SyncFailureCause, SyncStepKind};
/// let f = SyncFailure {
///     rel: RelPath::parse_wire("viejo").expect("rel"),
///     dest_rel: None,
///     cause: SyncFailureCause::Denied,
///     kind: SyncStepKind::DeleteTree,
/// };
/// let json = serde_json::to_value(&f).expect("json");
/// assert_eq!(json["kind"], serde_json::json!("delete_tree"));
/// // Y con la clase delante, la fila dice de qué raíz cuelga su `rel` sin
/// // que nadie tenga que deducirlo (0.42.0, #195).
/// assert!(!json.as_object().expect("objeto").contains_key("dest_rel"));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFailure {
    /// Dónde, RELATIVO a las dos raíces y en BYTES, igual que
    /// [`SyncStep::rel`].
    pub rel: RelPath,
    /// La ruta del DESTINO sobre la que el paso caía, cuando no se deletrea como
    /// `rel` — el mismo campo y la misma regla que [`SyncStep::dest_rel`],
    /// repetidos aquí porque el informe se lee sin el plan delante.
    ///
    /// Sin él, el caso de [`SyncFailureCause::IllegalName`] se cuenta al revés:
    /// un `café/x.txt` NFC del origen cuya carpeta el destino deletrea en NFD
    /// falla por longitud del nombre —NFD ocupa más— y el informe enseñaría la
    /// grafía NFC, que es la corta y la legal. «Este nombre no vale» señalando un
    /// nombre que sí vale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_rel: Option<RelPath>,
    /// Por qué.
    pub cause: SyncFailureCause,
    /// QUÉ paso era (0.42.0, #195): la misma clase que llevaba en el plan,
    /// [`SyncStep::kind`].
    ///
    /// El informe se lee SIN el plan delante, y hasta 0.41.0 esa era la
    /// diferencia entre un paso y un fallo: [`SyncStep`] declara su clase y
    /// [`SyncFailure`] la tiraba, aunque el core la tiene en la mano cuando
    /// construye la fila. Lo que se perdía es **de qué raíz cuelga `rel`**. Un
    /// [`SyncStepKind::DeleteTree`] habla siempre del DESTINO; todo lo demás
    /// que escribe, del origen. Sin la clase, la única prueba que quedaba en el
    /// wire era `dest_rel`, y de su ausencia no se deduce nada: un `DeleteTree`
    /// denegado por permisos contra un destino de solo lectura —la fila hostil
    /// más corriente de un [`SyncMode::Mirror`]— no lleva `dest_rel` y su `rel`
    /// cuelga del destino. Un panel que pinte esa ruta bajo la columna del
    /// origen, o que la decodifique con el override de encoding del árbol que
    /// no se ha tocado, está nombrando un subárbol del destino con el codepage
    /// del otro lado, en la pantalla donde se explica qué se ha borrado.
    ///
    /// **Obligatorio y sin `serde(default)`**, por lo mismo que
    /// [`SyncPlanDone::dest_trash`]: un default sería una clase inventada sobre
    /// un paso que falló, y [`SyncStepKind::Unknown`] —el `#[serde(other)]` del
    /// enum— significa «un daemon N+1 nombró una clase que este binario no
    /// conoce», que es una respuesta distinta de «el emisor no la dijo». La
    /// ventana N/N-1 no lo necesita: un daemon 0.41 hablando con un cliente
    /// 0.42 no negocia (ver [`version_compatible`]), y un cliente 0.41 leyendo
    /// un informe 0.42 ignora la clave de más.
    ///
    /// **Con una salvedad sobre [`SyncStepKind::Unknown`]**, que es el único
    /// sitio del wire donde el core PODRÍA emitirlo: un paso cuya clase este
    /// binario no sabe nombrar falla con
    /// [`SyncFailureCause::Io`]-por-`Unsupported` y su clase se copia tal cual
    /// a esta fila. Hoy no ocurre —el spool rechaza al LEER un paso de clase
    /// desconocida, así que un plan con uno no llega a ejecutarse—, y si
    /// ocurriera significaría «este binario leyó un plan que no entiende», no
    /// «el emisor no dijo la clase». Un cliente lo trata igual que un
    /// [`SyncStepKind::Unknown`] en un paso: sin ancla que afirmar.
    pub kind: SyncStepKind,
}

/// Result de [`SYNC_REPORT`] (0.40.0, ADR 0049): qué hizo la aplicación del
/// plan.
///
/// Un fallo es una FILA del informe, no el final de la Task: un paso que muere
/// en el fichero 40 000 de 500 000 se anota y la Task sigue, igual que la
/// comparación convirtió sus errores en filas.
///
/// ```
/// use norte_proto::methods::{DestTrash, SyncReportResult};
/// let r = SyncReportResult {
///     done: 3, failed: 0, skipped: 1, bytes: 4096,
///     failures: vec![], batch_id: Some(12), dest_trash: DestTrash::Restorable,
/// };
/// let json = serde_json::to_value(&r).expect("json");
/// assert_eq!(json["failures"], serde_json::json!([]));
/// assert_eq!(json["batch_id"], serde_json::json!(12));
/// // El informe se basta solo: «¿se puede devolver esto?» se contesta con él
/// // en la mano, sin haber conservado el `sync.plan_done` (0.42.0, #170).
/// assert_eq!(json["dest_trash"], serde_json::json!("restorable"));
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
    ///
    /// Este sí es exacto: son bytes escritos, contados al escribirlos. No tiene
    /// por qué cuadrar con [`SyncCounts::bytes`] del plan, que es una cota
    /// inferior porque el listado no siempre da tamaños — sobre `file://` no los
    /// da casi nunca.
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
    /// Qué papelera tenía el DESTINO cuando esto se aplicó, o sea qué puede
    /// devolver el undo de este lote (0.42.0, #170).
    ///
    /// El MISMO valor que viajó en [`SyncPlanDone::dest_trash`], sacado del
    /// mismo par de opciones del plan, y por el mismo motivo: sin él,
    /// «¿se puede deshacer esto?» no tiene respuesta. Un plan de solo copias
    /// contra un destino sin papelera y otro idéntico contra uno con papelera
    /// restaurable son byte a byte el mismo informe, y uno se deshace entero y
    /// el otro no se deshace nada (ADR 0049, #65).
    ///
    /// **Está aquí porque el informe se lee sin el `plan_done` delante.** Quien
    /// aplicó lo recibió segundos antes —tuvo que recibirlo para tener el
    /// `plan_hash`—, pero un cliente que reconectó, que no fue quien planificó,
    /// o que simplemente soltó la notificación, podía leer qué se copió, qué se
    /// sobrescribió y qué se borró, y no podía saber si algo de eso vuelve.
    /// [`PolicyUndoReportResult`] contesta lo mismo *a posteriori*, con
    /// `skipped_created_no_trash`, que es exactamente el momento equivocado:
    /// esto informa una decisión ANTES de tomarla.
    ///
    /// Obligatorio y sin `serde(default)`, igual que su gemelo de
    /// [`SyncPlanDone`] y por la misma razón — un default sería una respuesta
    /// inventada sobre si algo se puede deshacer.
    ///
    /// **No sustituye a `batch_id`.** Un [`DestTrash::Restorable`] con
    /// `batch_id` ausente sigue significando que no llegó a abrirse lote alguno
    /// y no hay nada que deshacer; los dos campos contestan preguntas
    /// distintas y hay que leerlos juntos.
    pub dest_trash: DestTrash,
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
    /// **Unidades que la POLICY denegó y el undo saltó** (0.43.0, #171).
    ///
    /// No es [`Self::blocked`], y leerlas como lo mismo sería leer el informe
    /// al revés: `blocked` dice «paré aquí, el árbol quedó consistente», y
    /// esto dice «esta unidad no se tocó y el undo siguió con las demás». Cada
    /// fila lleva el `seq` de la primera entrada de su unidad y el motivo, con
    /// la misma forma que un bloqueo porque la pregunta del lector es la misma:
    /// qué no volvió y por qué.
    ///
    /// El undo pregunta a la policy unidad a unidad y DENTRO de la Task
    /// (antes lo hacía todo por adelantado, en el hilo de quien llamaba), así
    /// que un scope que vence a mitad lo ve la unidad que le toca. Es la misma
    /// regla que el ejecutor hacia delante: `Deny` es una fila de informe, no
    /// un modal por paso.
    ///
    /// Recortada a [`UNDO_MAX_DENIED_REPORTED`];
    /// [`Self::denied_total`] las cuenta todas.
    #[serde(default)]
    pub denied: Vec<UndoBlocked>,
    /// Cuántas unidades denegó la policy, recortadas o no (0.43.0, #171).
    #[serde(default)]
    pub denied_total: u64,
}

/// Tope de filas de [`PolicyUndoReportResult::denied`] que el informe LISTA
/// (0.43.0, #171); `denied_total` las cuenta todas.
///
/// Mismo criterio que los topes de `sync`: una lista sin tope viaja por el
/// wire y se queda en memoria del cliente, y bajo una policy que deniegue por
/// defecto habría una fila por unidad de la sesión.
pub const UNDO_MAX_DENIED_REPORTED: usize = 256;

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

/// `true` si `id` es un identificador de plugin reverse-DNS válido: uno o más
/// segmentos `[A-Za-z0-9-]+` separados por puntos, con al menos un punto,
/// ningún segmento vacío (ni punto inicial ni final), y longitud total
/// `1..=128`.
///
/// Vive JUNTO a [`PluginInfo`], que es donde el id entra al proceso, y no en
/// el crate que parsea manifiestos, porque la pregunta tiene dos entradas y
/// una sola respuesta: el host la hace al leer un `plugin.toml`, y todo el
/// que RECIBE un `PluginInfo` por el wire la hace otra vez, porque el proceso
/// que lo manda no es de fiar por defecto. `norte-plugin-host` la re-exporta
/// para que su parseo siga llamándose igual; dos implementaciones del mismo
/// alfabeto serían dos, y una acabaría siendo más laxa.
///
/// El alfabeto es tan estrecho a propósito: un id NO es prosa. Es clave de
/// búsqueda contra el catálogo, argumento de [`PLUGIN_HELP`] por el wire, y
/// lo que el filtro de la barra lateral de la ayuda pliega en cada tecla. Con
/// este alfabeto no puede pintar peligros de terminal ni suplantar a otro
/// plugin, y por eso un id que no lo cumple se DESCARTA en vez de
/// enmascararse: enmascarar no es inyectivo, así que mapearía dos plugins
/// distintos a la misma fila.
///
/// El tope de 128 también acota el trabajo: un megabyte de `id` cuesta una
/// comparación rechazada, no una copia enmascarada por plugin.
///
/// ```
/// use norte_proto::methods::is_valid_plugin_id;
///
/// assert!(is_valid_plugin_id("acme.ftp"));
/// assert!(!is_valid_plugin_id("acme"), "hace falta al menos un punto");
/// assert!(!is_valid_plugin_id("acme.\u{202E}ftp"), "alfabeto cerrado");
/// ```
#[must_use]
pub fn is_valid_plugin_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    let mut segments = 0_usize;
    for segment in id.split('.') {
        if segment.is_empty()
            || !segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return false;
        }
        segments += 1;
    }
    // Al menos un punto ⇒ al menos dos segmentos.
    segments >= 2
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
    /// El ancla de aprobación del manifiesto Y del binario, tal como el daemon
    /// la calcula AHORA (0.53.0, #282). Hex sha256, o `None` en un peer viejo.
    ///
    /// Es lo que un humano está mirando cuando decide, así que es lo que
    /// [`PluginSetApprovalParams::expected_digest`] devuelve al confirmar. Sin
    /// él, un cliente solo puede comparar la LISTA de capabilities, que es lo
    /// que se pinta y no lo que se concede: `category` y `contributions`
    /// —cuándo y cómo se dispara el plugin— entran en el ancla y no en la
    /// lista.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<String>,
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
    ///
    /// Es el basename, nunca la ruta absoluta: ésta revelaría el home del
    /// usuario a un agente que llame a `plugin.list`.
    ///
    /// Se conserva por compatibilidad y sigue siendo lo que un cliente pinta
    /// cuando [`Self::dir_bytes`] no viene. Lo que NO puede es decir si se
    /// alteró: el `to_string_lossy` que lo produce pone `U+FFFD`, y `U+FFFD`
    /// no es un peligro de terminal —es Specials, ni control ni
    /// `Default_Ignorable`—, así que ninguna heurística del receptor lo
    /// recupera. Por eso existe el campo de al lado (#265).
    pub dir: String,
    /// Los BYTES del basename, tal como el OS los dio (0.53.0, #265).
    ///
    /// El nombre de un directorio de plugin es bytes: en Linux `caf\xff` es
    /// un nombre legal, y `PluginLoadError.dir` llegaba ya convertido con un
    /// `to_string_lossy` SIN marcar, así que la fila del error se declaraba
    /// fiel. Con los bytes, el receptor hace su propia conversión y sabe
    /// marcarla — que es la regla de siempre: lo que se enmascara se dice.
    ///
    /// Aditivo: un daemon 0.52 no lo emite y el receptor cae a [`Self::dir`],
    /// que es exactamente lo que hacía antes. Lo que se pierde contra el
    /// viejo es la marca, no el nombre.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "label_wire")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<String>", extend("contentEncoding" = "base64"))
    )]
    pub dir_bytes: Option<Vec<u8>>,
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
    /// El ancla que el humano LEYÓ, si el cliente la tiene (0.53.0, #282).
    ///
    /// El daemon ancla el digest que ÉL tiene en el momento de escribir, no el
    /// que se enseñó, así que entre el `plugin.list` que vio el humano y el
    /// `set_approval` que confirma cabe un `plugin.toml` distinto. Hoy esa
    /// ventana está cerrada por ACCIDENTE en el daemon —descubre el catálogo
    /// una vez al arrancar— y NO lo está en el `Backend` embebido, que
    /// redescubre en cada llamada.
    ///
    /// Con este campo el daemon rehúsa si no casa, y lo que se concede es
    /// exactamente lo que se leyó. Solo aplica al APROBAR: revocar no concede
    /// nada, y rehusar una revocación por un digest rancio dejaría vivo un
    /// permiso que alguien está intentando quitar.
    ///
    /// Aditivo: `None` es «el cliente no lo manda», y entonces el daemon se
    /// comporta como 0.52 — la comprobación es lo que se pierde contra un
    /// cliente viejo, no la corrección.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_digest: Option<String>,
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

/// `session.get` — la sesión de UI del daemon (L2, 0.48.0): la disposición y
/// el estado por hueco que el cliente dejó, para que un relevo del daemon
/// (ADR 0055) no cueste la pantalla.
///
/// El resultado dice además si ESTA conexión es la DUEÑA. La primera conexión
/// humana que pregunta se la queda; las siguientes reciben una COPIA y corren
/// sueltas —misma pantalla, mismas rutas, y a partir de ahí divergen sin
/// escribir—. Abrir un segundo terminal da lo que el lector esperaba y nunca
/// hay dos escritores sobre un estado.
///
/// SOLO conexiones humanas: una sesión de agente no tiene pantalla que
/// guardar. Un agente recibe `INVALID_REQUEST`.
///
/// **No lleva params, y en 0.48 el servidor no mira los que le manden.** Un
/// params futuro (p. ej. «lee sin reclamar») no sería aditivo por eso: un
/// daemon 0.48 lo IGNORARÍA y reclamaría igual, así que quien lo añada tiene
/// que hacerlo con su bump y su método o su campo comprobable.
///
/// ```
/// assert_eq!(norte_proto::methods::SESSION_GET, "session.get");
/// ```
pub const SESSION_GET: &str = "session.get";

/// `session.put` — reemplaza la sesión ENTERA (L2, 0.48.0).
///
/// Viaja el blob completo y el cliente coalesce: el cursor se mueve en cada
/// flecha, y una familia de métodos por campo serían quince métodos, quince
/// goldens y un motor de fusión que nadie pidió.
///
/// [`SessionPutParams::revision`] es toda la historia de concurrencia: un
/// `put` con una revisión rancia se rechaza con [`crate::Error::Conflict`] y
/// el cliente re-lee. No está para editores simultáneos —no los hay— sino
/// para el cliente que reconecta tras un relevo con estado de antes.
///
/// ```
/// assert_eq!(norte_proto::methods::SESSION_PUT, "session.put");
/// ```
pub const SESSION_PUT: &str = "session.put";

/// Tope del `body` de una sesión, en bytes serializados: 1 MiB.
///
/// Lo comprueba el core, que es lo ÚNICO que puede comprobar honestamente de
/// un documento que no lee. Por encima, [`crate::Error::LimitExceeded`] y la
/// sesión almacenada se queda como estaba: jamás se trunca un documento cuyo
/// esquema no se conoce.
///
/// ```
/// assert_eq!(norte_proto::methods::SESSION_BODY_MAX, 1024 * 1024);
/// ```
pub const SESSION_BODY_MAX: usize = 1024 * 1024;

/// La sesión de UI tal y como cruza el wire (L2).
///
/// `body` es OPACO para el core: `Node`, `SortSpec` y `ColumnId` viven en
/// `norte-frontend`, que depende de este crate y no al revés, y espejarlos
/// aquí duplicaría cuatro tipos a través de una arista de dependencias y
/// convertiría cada campo nuevo de UI en un cambio de wire con su bump y su
/// golden. Añadir un campo al cuerpo es subir la `version` DENTRO del cuerpo,
/// en el crate que le da significado.
///
/// ```
/// use norte_proto::methods::Session;
/// let s = Session {
///     version: 1,
///     revision: 3,
///     body: serde_json::json!({ "slots": {} }),
/// };
/// let j = serde_json::to_value(&s).expect("json");
/// assert_eq!(j["revision"], serde_json::json!(3));
/// // Y una sesión que nadie ha escrito todavía es la revisión cero:
/// assert_eq!(Session::default().revision, 0);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
// Campos con default, como el resto del wire: a un par que omita uno le
// falta un campo, no le sobra un error.
#[serde(default)]
pub struct Session {
    /// Esquema de `body`, propiedad de los frontends. 1 en esta versión; 0 en
    /// una sesión que nadie ha escrito todavía.
    pub version: u32,
    /// La sube el core en cada `put` aceptado. 0 = sesión nunca escrita.
    pub revision: u64,
    /// El documento. El core lo guarda, lo versiona y lo devuelve; no lo lee.
    pub body: serde_json::Value,
}

/// Result de [`SESSION_GET`].
///
/// ```
/// use norte_proto::methods::{Session, SessionGetResult};
/// let r = SessionGetResult {
///     session: Session::default(),
///     owner: false,
/// };
/// let j = serde_json::to_value(&r).expect("json");
/// assert_eq!(j["owner"], serde_json::json!(false));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
// Campos con default, como el resto del wire: a un par que omita uno le
// falta un campo, no le sobra un error.
#[serde(default)]
pub struct SessionGetResult {
    /// La sesión almacenada, o una vacía con `revision: 0`.
    pub session: Session,
    /// `true` si lo que esta conexión escriba se va a GUARDAR: es la dueña, y
    /// el core que la atiende tiene dónde y derecho a volcarlo.
    ///
    /// Las dos cosas son la misma pregunta para quien lee esto —«¿mis
    /// escrituras sobreviven?»— y separarlas solo servía para contestar que sí
    /// a un cliente que iba a perderlo todo al salir.
    pub owner: bool,
}

/// Params de [`SESSION_PUT`].
///
/// ```
/// use norte_proto::methods::SessionPutParams;
/// let p = SessionPutParams {
///     version: 1,
///     revision: 3,
///     body: serde_json::json!({}),
/// };
/// let j = serde_json::to_value(&p).expect("json");
/// assert_eq!(j["revision"], serde_json::json!(3));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
// Campos con default, como el resto del wire: a un par que omita uno le
// falta un campo, no le sobra un error.
#[serde(default)]
pub struct SessionPutParams {
    /// Esquema de `body` que escribe este cliente.
    pub version: u32,
    /// La revisión que el cliente cree vigente. Rancia = [`crate::Error::Conflict`].
    pub revision: u64,
    /// El documento entero.
    pub body: serde_json::Value,
}

/// Result de [`SESSION_PUT`]: la revisión NUEVA.
///
/// ```
/// use norte_proto::methods::SessionPutResult;
/// let r = SessionPutResult { revision: 4 };
/// let j = serde_json::to_value(&r).expect("json");
/// assert_eq!(j["revision"], serde_json::json!(4));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
// Campos con default, como el resto del wire: a un par que omita uno le
// falta un campo, no le sobra un error.
#[serde(default)]
pub struct SessionPutResult {
    /// Revisión resultante; el cliente la guarda para su siguiente `put`.
    pub revision: u64,
}
