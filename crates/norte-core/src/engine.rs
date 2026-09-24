//! [`Engine`]: la API embebida del core (M0). El daemon JSON-RPC (M1)
//! envolverá esta misma API; los frontends no contienen lógica de negocio.

use std::sync::{Arc, RwLock};

use norte_proto::{
    CapabilityFlags, CollisionPolicy, DeleteMode, Entry, Error, ResumePolicy, Segment,
    SymlinkPolicy, TaskId, TaskKind, VPath, VerifyPolicy,
    methods::{PlanHash, RelPath},
};
use norte_vfs::{EntryStream, Provider};

use crate::observer::{MutationObserver, NoopObserver};
use crate::ops;
use crate::ops::OnExists;
use crate::scheduler::{Priority, Scheduler, TaskHandle};
use crate::sessions::SessionPool;

/// Opciones de una copia/movimiento (ADR 0005): qué hacer ante colisiones
/// y con los symlinks. `Default` = el comportamiento estricto de M0
/// (`Fail` + `Preserve`).
///
/// ```
/// use norte_core::TransferOptions;
/// use norte_proto::{CollisionPolicy, SymlinkPolicy};
/// let opts = TransferOptions::default();
/// assert_eq!(opts.on_collision, CollisionPolicy::Fail);
/// assert_eq!(opts.symlinks, SymlinkPolicy::Preserve);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferOptions {
    /// Qué hacer si el destino ya existe.
    pub on_collision: CollisionPolicy,
    /// Qué hacer con los symlinks del origen.
    pub symlinks: SymlinkPolicy,
    /// Reanudación de transferencias interrumpidas (ADR 0012); default
    /// `Off` = contrato de M1 (cancelar deja destino limpio).
    pub resume: ResumePolicy,
    /// Verificación del parcial al reanudar (solo con `resume=On`).
    pub verify: VerifyPolicy,
    /// A la COLA en vez de en paralelo (ADR 0149): de una en una.
    pub queued: bool,
}

/// De dónde sale el journal de un [`Engine`], que desde #177 ya no es siempre
/// «lo tiene o no lo tiene».
///
/// Las tres variantes son los tres arranques que existen: un engine sin journal
/// (tests, embebedores), el del daemon —que lo abre él y se niega a arrancar si
/// no puede— y el de un frontend embebido, que no lo abre hasta que hace falta.
enum JournalSource {
    /// Sin journal: `undo_session` y `sync.apply` responden `Unsupported`, y el
    /// observer no registra nada.
    None,
    /// Ya abierto por quien construyó el engine (el daemon). Este proceso es el
    /// dueño de la cadena desde antes de existir el engine.
    Open(Arc<crate::journal::SqliteJournal>),
    /// El del directorio de estado, que se abrirá en la primera mutación —o en
    /// la primera pregunta que necesite la cadena— y quizá no se pueda abrir.
    Lazy(Arc<crate::embedded::LazyJournal>),
}

/// Núcleo embebido: registro de providers por scheme + operaciones.
/// Lecturas (`stat`/`list`) son directas; mutaciones (`copy`/`move_`/
/// `delete`) son Tasks con progreso y cancelación.
pub struct Engine {
    /// Caché de providers + ciclo de vida de las sesiones remotas (#47):
    /// providers de proceso por scheme, remotos por `scheme://authority`
    /// (una sesión por host, con single-flight/evicción/backoff) y archive
    /// compuestos por `fmt+scheme://authority`.
    sessions: SessionPool,
    /// Establece providers remotos bajo demanda (fase 6e, ADR 0015). Sin
    /// conector, un scheme sin provider registrado es `Unsupported` (M0/M1).
    connector: RwLock<Option<Arc<dyn crate::connect::RemoteConnector>>>,
    /// Observa los avisos de conexión (#44: degradación TLS). Sin observer, los
    /// avisos se dropean (el `tracing::warn!` del connector persiste en el log).
    connection_observer: RwLock<Option<Arc<dyn crate::connect::ConnectionObserver>>>,
    sched: Scheduler,
    observer: Arc<dyn MutationObserver>,
    /// Si alguien se llevó ya el despachador de hooks (ADR 0100): es de UNO.
    hooks_taken: std::sync::atomic::AtomicBool,
    /// De dónde sale el journal de este engine: fuente de LECTURA para el undo
    /// (M3-2) y para el gate de `sync.apply`. Es el MISMO objeto que `observer`
    /// en las dos variantes que tienen uno.
    ///
    /// No es un `Option<Arc<..>>` desde #177 porque el brazo embebido no sabe
    /// todavía si LO TIENE: lo abre en la primera mutación. Preguntárselo es
    /// [`Self::journal`], que es `async` justo por eso.
    journal: JournalSource,
    /// Las anclas de los directorios que el BACKEND EMBEBIDO ha listado (#301,
    /// ADR 0073), **si este engine es de un frontend** (#317).
    ///
    /// Vive aquí y no en `Backend` porque `Backend::Embedded` es un `Arc` del
    /// engine y nada más: dos clones suyos —el que lista un panel y el que
    /// copia— no comparten ninguna otra cosa, y una memoria por clon no vería
    /// nunca lo que listó el otro. Es el equivalente del `Inner` que el SDK
    /// usa para el camino remoto.
    ///
    /// # `Option`, y ese es el punto
    ///
    /// Un ancla dice **quién miró**, o sea un humano delante de una pantalla.
    /// Eso solo es cierto en un proceso con UN cliente: el de un frontend
    /// embebido. En el daemon hay muchos, cada uno con su propia idea de qué
    /// está mirando, y una caché compartida pasaría el listado del cliente A a
    /// la escritura del cliente B — que es lo que el ADR 0082 rechaza en sus
    /// alternativas.
    ///
    /// Era un campo siempre presente cuyo rustdoc decía «el daemon no lo
    /// toca». Lo decía y era verdad, pero lo sostenía una promesa en prosa y
    /// no el tipo: bastaba con que alguien montase un `Backend::Embedded` sobre
    /// el engine del daemon —para un job interno, para plugins— y la promesa
    /// caía sin que nada se pusiera rojo. Ahora la instala
    /// [`Self::with_client_anchors`], y la llama exactamente
    /// [`crate::embedded::engine_in`], que es la constructora de los engines de
    /// frontend y la que el daemon no usa.
    anchors: Option<std::sync::Mutex<crate::anchor::AnchorCache>>,
    /// Gate de policy consultado PRE-efecto en cada mutación (M3-3). Default
    /// [`AllowAll`](crate::policy::AllowAll): el engine embebido/humano no se
    /// sandboxea salvo que se instale una policy con [`Self::with_policy`].
    policy: Arc<dyn crate::policy::PolicyGate>,
    /// Si la `policy` de arriba la instaló ALGUIEN ([`Self::with_policy`]) o es
    /// la de por omisión.
    ///
    /// No cambia ninguna decisión: `AllowAll` gatea igual de permisivo en los
    /// dos casos. Existe porque el daemon tiene que poder AVISAR de la segunda
    /// (#166) — «policy permisiva a propósito» y «policy que nadie instaló» son
    /// la misma cosa para el gate y cosas distintas para el operador.
    policy_explicit: bool,
    /// Resuelve un `Ask` de policy. Default [`DenyAll`](crate::approval::DenyAll)
    /// (headless fail-closed).
    approvals: Arc<dyn crate::approval::ApprovalResolver>,
    /// Límites anti-bomba de los providers archive compuestos (#95.2).
    /// Default [`norte_vfs_archive::Limits::default`]; el operador los baja
    /// vía [`Self::set_archive_limits`] ANTES de la primera navegación a un
    /// contenedor (los providers compuestos se cachean con los límites
    /// vigentes en su primer uso).
    archive_limits: RwLock<norte_vfs_archive::Limits>,
    /// Ejecutable FIJADO para leer RAR (`[archive] rar_delegate`), o `None`
    /// para sondear `PATH`. Se pone en el arranque con
    /// [`Self::set_rar_delegate`], nunca desde la capa Project.
    rar_delegate: RwLock<Option<std::path::PathBuf>>,
    /// Proveedor de IA para el rename revisable (M4-A2, ADR 0031). `None` =
    /// sin IA (`ai_rename_plan` → `Unsupported`). Inyectado con
    /// [`Self::set_ai_provider`].
    ai_provider: RwLock<Option<norte_ai::SharedAiProvider>>,
    /// Proveedor de EMBEDDINGS (M4-IA-2, ADR 0031 A3). Separado del de chat:
    /// `[ai].embed_provider` puede nombrar otro proveedor/modelo. `None` =
    /// sin embeddings (`index.embed` → `Unsupported`). Inyectado con
    /// [`Self::set_ai_embed_provider`].
    ai_embed: RwLock<Option<norte_ai::SharedAiProvider>>,
    /// Config `[ai]` (opt-in/local-only/denied-paths). Default deshabilitado
    /// → el gate rechaza toda operación de IA.
    ai_config: RwLock<crate::ai::AiConfig>,
    /// Índice de búsqueda (M4, ADR 0034). `None` = sin índice
    /// (`index.*` → `Unsupported`, fail-closed como la IA). Inyectado con
    /// [`Self::with_index`]; lo instala el daemon.
    index: Option<Arc<norte_index::Index>>,
    /// EL spool de planes de sincronización retenidos (ADR 0049). `None` = sin
    /// retención, y entonces `sync.plan` responde `Unsupported` (fail-closed,
    /// como el índice): un plan que no se puede retener tampoco se puede
    /// aplicar, y servirlo sería enseñar un diálogo de aprobación sobre algo que
    /// después no existe.
    ///
    /// Lo instala el arranque con [`Self::set_spool`], **una sola vez y con un
    /// solo `Spool::new`**: el registro de planes emitidos vive detrás de un
    /// `Arc` dentro del handle, así que un segundo `Spool::new` sobre el mismo
    /// directorio no es otro handle sino un spool que no reconoce ni un plan.
    spool: RwLock<Option<crate::sync::Spool>>,
    /// Anillo ACOTADO de informes de lotes de renames, por `task_id`
    /// ([`Self::rename_batch_report`]).
    ///
    /// El informe se retiene AQUÍ, y no en el daemon, porque hay dos
    /// consumidores —el socket (`fs.rename_batch_report`) y el `Backend`
    /// embebido— y dos anillos serían dos políticas de retención que se
    /// contradicen a la primera. El actor guardado es el DUEÑO de la task: el
    /// daemon lo necesita para decidir quién puede leerlo.
    batch_reports: std::sync::Mutex<std::collections::VecDeque<BatchReportEntry>>,
    /// Anillo ACOTADO de informes de `sync.apply`, por `task_id`
    /// ([`Self::sync_report`]).
    ///
    /// Gemelo exacto de `batch_reports` y por el mismo motivo: hay dos
    /// consumidores —el socket (`sync.report`) y el `Backend` embebido— y dos
    /// anillos serían dos políticas de retención que se contradicen a la
    /// primera. El actor guardado es el DUEÑO de la Task; el daemon lo necesita
    /// para decidir quién puede leerlo.
    sync_reports: std::sync::Mutex<std::collections::VecDeque<SyncReportEntry>>,
    /// Anillo ACOTADO de informes de `archive.test`, por `task_id`
    /// ([`Engine::archive_test_report`]).
    ///
    /// Tercero de la misma familia y por la misma razón: una Task no puede
    /// devolver un valor, y lo que `archive.test` tiene que contar —qué entrada
    /// falló y por qué— no cabe en un `Failed`. El mismo desalojo, con la misma
    /// regla de «primero lo que no cuenta nada».
    test_reports: std::sync::Mutex<std::collections::VecDeque<TestReportEntry>>,
    /// Anillo ACOTADO de informes de undo, por `task_id`
    /// ([`Engine::undo_report`]).
    ///
    /// Vivía en el daemon, y era el único de la familia que no estaba aquí:
    /// el `Backend` embebido deshacía (`undo_after`) y no tenía de dónde leer
    /// qué había vuelto, así que contestaba `Unsupported` a su propio undo.
    undo_reports: std::sync::Mutex<std::collections::VecDeque<UndoReportEntry>>,
    /// Un undo a la vez (#358). Lo toma la Task de undo entera, del primer
    /// paso al último: dos undos que eligieron la misma pila (un doble clic,
    /// dos frontends, un reintento) no se pisan, y el segundo, al entrar,
    /// re-mira el journal y se salta lo que el primero ya devolvió.
    ///
    /// Dos límites, escritos para que nadie los dé por cerrados:
    /// - El rollback de un `fs.rename_batch` EN MARCHA también escribe
    ///   compensaciones y no toma este turno. Un undo que elija entradas de un
    ///   lote que aún corre puede cruzarse con él; lo acotan `is_free` y el
    ///   rename sin reemplazo, como antes de #358.
    /// - Un undo de agente cuya puerta de policy pregunta (`ask`) retiene el
    ///   turno mientras espera la aprobación (30 s en el daemon). Un undo
    ///   humano espera detrás. No hay interbloqueo —aprobar no pasa por un
    ///   undo—, solo espera.
    undo_en_curso: Arc<tokio::sync::Mutex<()>>,
    /// Anillo ACOTADO de informes de `archive.pack`, por `task_id`
    /// ([`Engine::archive_pack_report`]).
    ///
    /// Cuarto de la familia, y el que más lejos lleva su motivo: los otros tres
    /// cuentan lo que SALIÓ MAL, y este cuenta algo que salió BIEN y aun así
    /// hay que decir — un `a\b.txt` guardado, que en Windows es un `b.txt`
    /// dentro de una carpeta `a`. Un `Completed` es verdad y no lo cubre
    /// (#250).
    pack_reports: std::sync::Mutex<std::collections::VecDeque<PackReportEntry>>,
    /// Anillo ACOTADO de informes de `fs.checksum`, por `task_id`
    /// ([`Self::checksum_report`]).
    ///
    /// Gemelo de `pack_reports` y por el mismo motivo: hay dos consumidores —el
    /// socket (`fs.checksum_report`) y el `Backend` embebido— y dos anillos
    /// serían dos políticas de retención que se contradicen a la primera.
    checksum_reports: std::sync::Mutex<std::collections::VecDeque<ChecksumReportEntry>>,
    /// Anillo ACOTADO de informes de `fs.dir_usage`, por `task_id`
    /// ([`Self::dir_usage_report`]).
    ///
    /// El sexto de la familia. Aquí lo que no cabe en el desenlace de una Task
    /// es la LISTA de hijos medidos: `fs.dir_size` podía devolver su total por
    /// el progreso porque era un número, y un mapa no lo es.
    dir_usage_reports: std::sync::Mutex<std::collections::VecDeque<DirUsageReportEntry>>,
}

/// La puerta de policy, capturable (#171).
///
/// Existe para que el cuerpo de una Task pueda preguntar por SU cuenta, sin
/// `&self`: es lo que separa «gatear todo antes de empezar» de «gatear cada
/// unidad cuando le toca». Ver [`Engine::policy_checker`].
#[derive(Clone)]
struct PolicyChecker {
    policy: Arc<dyn crate::policy::PolicyGate>,
    approvals: Arc<dyn crate::approval::ApprovalResolver>,
}

impl PolicyChecker {
    /// La mitad de policy de [`Self::gate`].
    async fn check(
        &self,
        actor: &crate::journal::Actor,
        op: crate::policy::PolicyOp,
        paths: &[&VPath],
    ) -> Result<(), Error> {
        use crate::policy::{Decision, DenyReason};
        let denied = |reason: DenyReason| Error::PolicyDenied {
            rule: reason.rule_id().to_owned(),
        };
        match self.policy.evaluate(actor, op, paths) {
            Decision::Allow => Ok(()),
            Decision::Deny(reason) => {
                tracing::info!(?reason, op = op.kind(), "policy denegó la operación");
                Err(denied(reason))
            }
            // Un plugin corre sin nadie delante (ADR 0101): una regla `ask`
            // sobre él es un `deny` con su motivo, no un modal que nadie mira
            // y que vence por TTL igual.
            Decision::Ask if matches!(actor, crate::journal::Actor::Plugin { .. }) => {
                tracing::info!(
                    op = op.kind(),
                    "policy pide confirmación a un plugin: denegado"
                );
                Err(denied(DenyReason::NotApproved))
            }
            Decision::Ask => {
                let req = crate::approval::ApprovalRequest {
                    actor: actor.clone(),
                    op,
                    // Redactadas como los spans (regla 10): son SOLO display
                    // para el frontend que aprueba, jamás se reparsean.
                    //
                    // Y ACOTADAS. La DECISIÓN se toma sobre `paths` entero
                    // (arriba, `policy.evaluate`); lo que se recorta es lo que
                    // se le enseña al humano. Desde el rename por lotes un solo
                    // gate puede traer miles de rutas, y esta lista se difunde a
                    // cada conexión humana y se retiene durante el TTL: sin
                    // tope, un agente bajo una regla `ask` convierte cada
                    // petición en megabytes de notificación y expulsa a los
                    // suscriptores lentos por outbox lleno. Ningún frontend
                    // pinta tantas rutas de todos modos.
                    paths: paths
                        .iter()
                        .take(APPROVAL_PATHS_SHOWN)
                        .map(|p| span_path(p))
                        .collect(),
                    // Y el TOTAL viaja con ellas. Recortar la lista es
                    // necesario; recortarla EN SILENCIO convertiría el modal en
                    // una mentira — el humano aprobaría 32 rutas inocentes sin
                    // saber que la decisión cubría ocho mil.
                    paths_total: paths.len() as u64,
                };
                match self.approvals.request(req).await {
                    crate::approval::ApprovalOutcome::Approved => Ok(()),
                    crate::approval::ApprovalOutcome::Denied
                    | crate::approval::ApprovalOutcome::TimedOut => {
                        Err(denied(DenyReason::NotApproved))
                    }
                }
            }
        }
    }
}

impl Engine {
    /// Engine con el observador no-op (el journal llega en M3).
    #[must_use]
    pub fn new() -> Self {
        Self::with_observer(Arc::new(NoopObserver))
    }

    /// Engine con un observador de mutaciones propio (costura del journal), sin
    /// fuente de undo (`undo_session` → `Unsupported`).
    #[must_use]
    pub fn with_observer(observer: Arc<dyn MutationObserver>) -> Self {
        Self::build(observer, JournalSource::None)
    }

    /// El constructor de verdad: los tres públicos solo eligen QUÉ observer y
    /// qué fuente de journal, y el resto del engine es idéntico en los tres.
    /// Uno solo, y no tres copias de veinte campos, porque un campo nuevo que
    /// se olvide en una copia es un engine con la mitad de sus piezas.
    fn build(observer: Arc<dyn MutationObserver>, journal: JournalSource) -> Self {
        Self {
            sessions: SessionPool::new(),
            connector: RwLock::new(None),
            connection_observer: RwLock::new(None),
            sched: Scheduler::new(4),
            observer,
            journal,
            policy: Arc::new(crate::policy::AllowAll),
            policy_explicit: false,
            approvals: Arc::new(crate::approval::DenyAll),
            archive_limits: RwLock::new(norte_vfs_archive::Limits::default()),
            rar_delegate: RwLock::new(None),
            ai_provider: RwLock::new(None),
            ai_embed: RwLock::new(None),
            ai_config: RwLock::new(crate::ai::AiConfig::default()),
            index: None,
            spool: RwLock::new(None),
            batch_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            sync_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            test_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            undo_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            undo_en_curso: Arc::new(tokio::sync::Mutex::new(())),
            pack_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            checksum_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            dir_usage_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
            anchors: None,
            hooks_taken: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Le da a este engine la memoria de anclas de UN cliente (#301, #317).
    ///
    /// Solo para un engine de frontend embebido, donde el único que lista es el
    /// humano que mira. La llama [`crate::embedded::engine_in`]; un engine que
    /// no pase por ahí —el del daemon— no la tiene, y entonces
    /// `Backend::Embedded` sobre él no ancla nada y las escrituras se comportan
    /// como en 0.53. Fallar así es lo correcto: perder la comprobación es
    /// perder una comprobación, y compartirla entre clientes sería contestar
    /// «quién miró» con el nombre de otro.
    #[must_use]
    pub fn with_client_anchors(mut self) -> Self {
        self.anchors = Some(std::sync::Mutex::new(crate::anchor::AnchorCache::default()));
        self
    }

    /// ¿Tiene este engine memoria de anclas de cliente ([`Self::with_client_anchors`])?
    ///
    /// Lo pregunta el test que fija que el engine del daemon NO la tiene: la
    /// propiedad la sostiene el tipo, y esto es lo que la deja comprobar desde
    /// fuera en vez de por lectura del código.
    #[must_use]
    pub fn has_client_anchors(&self) -> bool {
        self.anchors.is_some()
    }

    /// Retiene el ancla del directorio que el BACKEND EMBEBIDO acaba de listar
    /// (#301). Sin memoria de cliente instalada no hace nada.
    ///
    /// Un lock envenenado se traga sin ruido, y es la respuesta correcta: lo
    /// que se pierde es la COMPROBACIÓN de una escritura, nunca la escritura.
    /// Hacerlo panicar convertiría un fallo de otro hilo en la muerte del
    /// listado.
    pub(crate) fn remember_dir_anchor(&self, dir: &VPath, anchor: Option<norte_proto::DirAnchor>) {
        let Some(anchors) = self.anchors.as_ref() else {
            return;
        };
        if let Ok(mut cache) = anchors.lock() {
            cache.remember(dir, anchor);
        }
    }

    /// El ancla retenida de `dir`, si el backend embebido lo listó (#301).
    pub(crate) fn remembered_dir_anchor(&self, dir: &VPath) -> Option<norte_proto::DirAnchor> {
        self.anchors.as_ref()?.lock().ok()?.get(dir)
    }

    /// Compone el provider de RAR para `aref`, o dice que no se puede.
    ///
    /// Las dos negativas son la frontera del ítem 11:
    ///
    /// - el interior tiene que ser `file://` **sin authority**: el delegado
    ///   recibe una ruta del sistema de ficheros, y no existe tal ruta para
    ///   un `sftp://` ni para una entrada dentro de otro archivo;
    /// - sin `7z` ni `unrar` instalados no hay lector: `Unsupported`, con la
    ///   frase que nombra qué instalar en el log (el wire no lleva prosa).
    fn rar_provider_for(
        &self,
        aref: &norte_proto::ArchiveRef,
        key: String,
    ) -> Result<Arc<dyn Provider>, Error> {
        if aref.outer.scheme() != "file" || aref.outer.authority().is_some() {
            tracing::warn!(
                outer = %aref.outer.scheme(),
                "rar solo se monta sobre un fichero LOCAL: el delegado necesita una ruta"
            );
            return Err(Error::Unsupported);
        }
        let archive = norte_vfs_local::vpath_to_native(&aref.outer)?;
        let pinned = self
            .rar_delegate
            .read()
            .expect("rar_delegate lock sano")
            .clone();
        let delegate = match pinned {
            Some(program) => norte_vfs_rar::Delegate::pinned(program),
            None => norte_vfs_rar::Delegate::discover().map_err(|e| {
                tracing::warn!(error = %e, "no hay lector de RAR instalado");
                Error::from(e)
            })?,
        };
        let provider: Arc<dyn Provider> = Arc::new(norte_vfs_rar::RarProvider::new(
            archive,
            delegate,
            norte_vfs_rar::RarLimits::default(),
        ));
        Ok(self.sessions.insert_composite(key, provider))
    }

    /// El ejecutable que lee RAR, si la configuración FIJA uno
    /// (`[archive] rar_delegate`). `None` = sondear `PATH`.
    ///
    /// La clave viene de las capas System/User y NUNCA de Project: un
    /// repositorio no elige qué binario se lanza al entrar en él.
    ///
    /// # Panics
    /// Si el lock interno está envenenado, como el resto de los del engine.
    pub fn set_rar_delegate(&self, program: Option<std::path::PathBuf>) {
        *self.rar_delegate.write().expect("rar_delegate lock sano") = program;
    }

    /// Fija los límites anti-bomba de los providers archive (#95.2, canal
    /// config→provider del ADR 0018). Llamar en el ARRANQUE, antes de la
    /// primera navegación a un contenedor: un `ArchiveProvider` ya compuesto
    /// (cacheado por scheme) conserva los límites con los que nació.
    ///
    /// # Panics
    /// Si el lock interno está envenenado (otro hilo hizo panic a mitad de
    /// escritura) — irrecuperable, mismo criterio que el resto de locks del
    /// engine.
    pub fn set_archive_limits(&self, limits: norte_vfs_archive::Limits) {
        *self
            .archive_limits
            .write()
            .expect("archive_limits lock sano") = limits;
    }

    /// Engine cuyo observer Y fuente de undo es el mismo `SqliteJournal` (M3-2).
    /// El journal es single-writer (spec §4): un solo Engine por fichero.
    #[must_use]
    pub fn with_journal(journal: Arc<crate::journal::SqliteJournal>) -> Self {
        Self::build(
            Arc::clone(&journal) as Arc<dyn MutationObserver>,
            JournalSource::Open(journal),
        )
    }

    /// Engine EMBEBIDO: su journal es el del directorio de estado, y se abre en
    /// la primera mutación (#177) — o en el primer `undo`/`sync.apply`, que es
    /// lo mismo por otro camino: son las tres cosas que necesitan la cadena.
    ///
    /// Igual que [`Self::with_journal`], el observer y la fuente de undo son EL
    /// MISMO objeto; la diferencia es que aquí ese objeto todavía no tiene un
    /// fichero abierto detrás, y quizá no llegue a tenerlo (otro proceso puede
    /// tener el lock). Construirlo no toca el disco ni le quita el journal a
    /// nadie: ese es todo el punto.
    ///
    /// Lo usan `norte-tui` y `norte-cli` sin daemon, vía
    /// [`crate::embedded::engine_in`].
    #[must_use]
    pub fn with_lazy_journal(journal: Arc<crate::embedded::LazyJournal>) -> Self {
        Self::build(
            Arc::clone(&journal) as Arc<dyn MutationObserver>,
            JournalSource::Lazy(journal),
        )
    }

    /// Enchufa los hooks (ADR 0100): cada fila que el journal de este engine
    /// comprometa se le ofrece a `tx`, venga de donde venga la mutación. Sin
    /// journal no hay filas y no hay hooks — un engine sin journal tampoco
    /// tiene undo, y es la misma razón.
    ///
    /// Con el journal perezoso del modo embebido el extremo se guarda y se
    /// le pone a cada handle que se abra; por eso es `async`.
    pub async fn enable_hooks(&self, tx: crate::hooks::HookSender) {
        match &self.journal {
            JournalSource::None => {}
            JournalSource::Open(j) => j.set_hook_sender(tx),
            JournalSource::Lazy(l) => l.set_hook_sender(tx).await,
        }
    }

    /// ¿Lleva este engine un journal (abierto o perezoso)? Sin él no hay
    /// filas, y sin filas no hay hooks que despachar.
    #[must_use]
    pub fn has_journal(&self) -> bool {
        !matches!(self.journal, JournalSource::None)
    }

    /// Reclama el hueco del despachador de hooks: `true` la PRIMERA vez, y
    /// solo esa. Un engine tiene un despachador; el segundo que se arrancara
    /// pisaría el extremo del primero en silencio.
    pub fn claim_hooks_slot(&self) -> bool {
        !self
            .hooks_taken
            .swap(true, std::sync::atomic::Ordering::AcqRel)
    }

    /// A dónde van los avisos de «esta sesión no queda registrada» (#177).
    ///
    /// No-op si este engine no lleva journal perezoso (el del daemon no puede
    /// quedarse sin journal: se niega a arrancar). Instalarlo es del arranque
    /// del frontend, y llega a tiempo aunque una mutación se le adelante — ver
    /// [`crate::embedded::LazyJournal::set_warning_sink`].
    /// Devuelve si el sink quedó INSTALADO: `false` cuando este engine no puede
    /// quedarse sin journal a mitad (el del daemon) ni puede tener uno (un
    /// `Engine::new()`). Lo mira el `Backend` para no entregarle a un frontend
    /// un canal que no va a sonar nunca y que le haría creerse cubierto.
    pub fn set_journal_warning_sink(
        &self,
        sink: Arc<dyn crate::embedded::JournalWarningSink>,
    ) -> bool {
        if let JournalSource::Lazy(l) = &self.journal {
            l.set_warning_sink(sink);
            return true;
        }
        false
    }

    /// El journal de este engine, ABRIÉNDOLO si es perezoso y es la primera vez
    /// que se pide.
    ///
    /// `async` a propósito, y es el nudo de #177: las tres preguntas que se le
    /// hacen a este campo —¿journalizo esta mutación?, ¿puedo deshacer?, ¿puedo
    /// aplicar un plan de sincronización?— llegan en momentos distintos, y dos
    /// de ellas ANTES de que el proceso haya mutado nada. Con la pereza metida
    /// solo en el observer, esas dos contestarían «no hay journal» sobre un
    /// engine que lo abriría sin problema. Aquí no: preguntar es abrir.
    ///
    /// Que no haya dos handles del mismo fichero —ni dos dueños de la cadena—
    /// lo garantiza la ventana del [`LazyJournal`](crate::embedded::LazyJournal):
    /// UNA, compartida con el observer, con los intentos serializados bajo su
    /// lock y el handle destruido al soltarlo. Desde #179 la apertura ya no es
    /// única; lo que sigue siendo único es el DUEÑO en cada instante.
    async fn journal(&self) -> Option<Arc<crate::journal::SqliteJournal>> {
        match &self.journal {
            JournalSource::None => None,
            JournalSource::Open(j) => Some(Arc::clone(j)),
            JournalSource::Lazy(l) => l.get().await,
        }
    }

    /// Abre ya el journal perezoso y dice si esta sesión queda registrada.
    ///
    /// Para el llamante que va a mutar y necesita DECÍRSELO al humano antes
    /// (hoy: `norte ai rename`, que pide confirmación para renombrar un
    /// directorio entero con los nombres que propuso un modelo). Sin esto, la
    /// respuesta llegaría después del sí.
    ///
    /// Toma el lock exclusivo AQUÍ, no en la primera mutación, y este proceso
    /// lo conserva hasta que lo suelte
    /// ([`LazyJournal::release`](crate::embedded::LazyJournal::release), que
    /// hoy no llama nadie por su cuenta): si lo que viene después es una
    /// pregunta al humano, `norte daemon run` no puede arrancar mientras él se
    /// lo piensa. Solo tiene sentido a un paso de mutar, y es el precio de que
    /// la respuesta llegue antes del sí y no después.
    ///
    /// **Se salta el freno de reintento de #179 a propósito.** Este es el único
    /// llamador para el que pagar los 250 ms de espera del lock vale
    /// obviamente la pena: contestar `false` desde un veredicto de hace medio
    /// minuto sería decirle al humano «esto no se va a registrar» sobre un
    /// journal que ahora mismo está libre, y con eso delante decidirá que no.
    pub async fn ensure_journal(&self) -> bool {
        match &self.journal {
            JournalSource::None => false,
            JournalSource::Open(_) => true,
            JournalSource::Lazy(l) => l.acquire_now().await.is_some(),
        }
    }

    /// Suelta el journal si lleva `ocioso` sin usarse (#179). `true` si al
    /// volver el fichero está libre.
    ///
    /// El engine del daemon y el que no journaliza contestan `true` sin hacer
    /// nada: no tienen ventana que soltar. El del daemon, además, es dueño a
    /// propósito — se niega a arrancar sin journal, así que soltarlo sería
    /// quitarse a sí mismo lo que exige tener.
    ///
    /// # Esto NO es cancel-safe (ver
    /// [`LazyJournal::release`](crate::embedded::LazyJournal::release)).
    /// Córrelo entero, en el CUERPO de una rama de `select!`, jamás en su
    /// condición.
    pub async fn release_journal_if_idle(&self, ocioso: std::time::Duration) -> bool {
        match &self.journal {
            JournalSource::None | JournalSource::Open(_) => true,
            JournalSource::Lazy(l) => l.release_if_idle(ocioso).await,
        }
    }

    /// Lo mismo, diciendo POR QUÉ no.
    ///
    /// `None` = esta sesión SÍ registra, o este engine no tiene ventana que
    /// perder (el del daemon, o un `Engine::new()` que no journaliza nada por
    /// construcción).
    ///
    /// Existe porque desde #178 los dos motivos ya no significan lo mismo y un
    /// `bool` los confunde: con `Busy` la operación OCURRE sin registro y hay
    /// que avisar; con `Failed` la operación va a ser REHUSADA por
    /// el gate del engine y avisar sería el preámbulo de una pregunta cuya premisa
    /// es falsa. Lo mira `norte ai rename`, que pregunta antes de dejar que un
    /// modelo renombre un directorio entero.
    ///
    /// Se salta el freno de reintento, como [`Self::ensure_journal`] y por la
    /// misma razón.
    pub async fn journal_obstacle(&self) -> Option<crate::embedded::NoJournal> {
        let JournalSource::Lazy(l) = &self.journal else {
            return None;
        };
        // UN solo intento, y por eso `resolve_now` y no `acquire_now` seguido de
        // `resolve`: aquel par pagaba dos aperturas, y si el texto del error de
        // `SQLite` difería entre ellas —lo escribe en parte quien pueda escribir
        // el fichero— el sink recibía dos avisos por una sola pregunta.
        l.resolve_now().await.err()
    }

    /// Instala el gate de policy y el resolver de aprobaciones (M3-3): a partir
    /// de aquí, las mutaciones de agentes se evalúan PRE-efecto.
    #[must_use]
    pub fn with_policy(
        mut self,
        policy: Arc<dyn crate::policy::PolicyGate>,
        approvals: Arc<dyn crate::approval::ApprovalResolver>,
    ) -> Self {
        self.policy = policy;
        self.approvals = approvals;
        self.policy_explicit = true;
        self
    }

    /// Si alguien llamó a [`Self::with_policy`] sobre este engine.
    ///
    /// Lo consulta el daemon en el arranque: montarse sobre un engine sin
    /// policy explícita deja pasar a CUALQUIER actor, agentes incluidos, y
    /// `sync.apply` bajo ese hueco es una llamada que reescribe un subárbol
    /// (#166). No es un gate — es lo que hace falta para que el hueco salga en
    /// el log en vez de en la sorpresa.
    #[must_use]
    pub fn has_explicit_policy(&self) -> bool {
        self.policy_explicit
    }

    /// Instala el índice de búsqueda (M4, ADR 0034). Sin él, `index.*` responde
    /// `Unsupported` (fail-closed). Lo instala el daemon (single-writer del DB).
    #[must_use]
    pub fn with_index(mut self, index: Arc<norte_index::Index>) -> Self {
        self.index = Some(index);
        self
    }

    /// Instala EL spool de planes de sincronización (ADR 0049). Sin él,
    /// [`Self::sync_plan_as`] responde [`Error::Unsupported`] (fail-closed).
    ///
    /// El handle se **clona**, jamás se vuelve a construir: el registro de lo
    /// que este proceso emitió vive dentro y es lo que hace que un plan solo lo
    /// pueda aplicar quien lo produjo. Quien llame a esto dos veces con dos
    /// `Spool::new` distintos deja huérfanos los planes del primero.
    ///
    /// # Panics
    /// Solo si el lock interno está envenenado (otro hilo hizo panic a mitad de
    /// escritura) — irrecuperable, mismo criterio que el resto de locks.
    pub fn set_spool(&self, spool: crate::sync::Spool) {
        *self.spool.write().expect("spool lock sano") = Some(spool);
    }

    /// El spool instalado, clonado. `None` = sin retención.
    ///
    /// # Panics
    /// Solo si el lock interno está envenenado.
    #[must_use]
    pub fn spool(&self) -> Option<crate::sync::Spool> {
        self.spool.read().expect("spool lock sano").clone()
    }

    /// La puerta ÚNICA de toda mutación de este engine: policy primero, journal
    /// después.
    ///
    /// Evalúa la policy PRE-efecto; un `Ask` suspende hasta aprobación. `Err`
    /// [`Error::PolicyDenied`] con la causa (`rule`) si se deniega — el wire lo
    /// distingue de un `PermissionDenied` del OS/provider (M3-3b). Y después,
    /// [`Self::journal_gate`]: un journal ILEGIBLE rehúsa (#178).
    ///
    /// **Ese orden, y no el otro.** El journal se pide DESPUÉS de que la policy
    /// haya dicho que sí, porque pedirlo toma el lock exclusivo del fichero
    /// (#177) y una operación que la policy iba a denegar no tiene por qué
    /// quitárselo al daemon.
    ///
    /// Que la comprobación viva AQUÍ y no en cada llamador es lo que la hace
    /// completa: los nueve puntos de mutación del engine pasan por esta función,
    /// y añadir el noveno no requiere acordarse de nada. Lo pinea
    /// `toda_mutacion_pasa_por_el_gate_del_journal` en
    /// `tests/embedded_journal.rs`, que es lo que impide que el noveno se
    /// olvide de todos modos.
    async fn gate(
        &self,
        actor: &crate::journal::Actor,
        op: crate::policy::PolicyOp,
        paths: &[&VPath],
    ) -> Result<(), Error> {
        self.policy_gate(actor, op, paths).await?;
        self.journal_gate().await
    }

    /// Rehúsa la mutación si el journal de esta sesión no se puede ABRIR
    /// (#178).
    ///
    /// Solo el caso `Failed`: sin permisos, corrupto, no-es-una-base-de-datos,
    /// o de una era anterior a la cadena de hoy. Ahí seguir sería mutar sin
    /// registro y sin undo, que es lo que la regla dura 4 prohíbe y lo que
    /// `norte daemon run` ya rehúsa con esa misma entrada — la asimetría era el
    /// bug.
    ///
    /// **`Busy` NO rehúsa**, y esa mitad es la que impide que el arreglo sea
    /// peor que el agujero: el ocupante habitual es benigno (un daemon vivo,
    /// otra ventana) o transitorio (otro `norte cp` de un script, un daemon
    /// reiniciándose), y negar ahí convertiría «hay un daemon» en «el gestor de
    /// ficheros no funciona» y dejaría a un ocupante de paso tumbando una
    /// sesión de tres horas.
    ///
    /// Los engines que no llevan journal perezoso pasan de largo: el del daemon
    /// (que no arranca sin journal, así que ya falló en cerrado antes) y el de
    /// un embebedor con `Engine::new()` (que no registra NADA por construcción
    /// y para el que no hay fichero que arreglar).
    ///
    /// # Lo que este gate garantiza, con su plazo
    /// **«No estaba ilegible la última vez que se miró», y eso puede ser hasta
    /// [`FRENO_TRAS_FALLO`](crate::embedded::FRENO_TRAS_FALLO) atrás.** El
    /// freno de #179 hace que un veredicto `Busy` se recuerde treinta segundos
    /// sin volver a abrir; si en esa ventana el fichero pasa
    /// de OCUPADO a ILEGIBLE —alguien suelta el lock y acto seguido lo
    /// corrompe— este gate sigue contestando `Ok(())` desde la clasificación
    /// vieja y las mutaciones de esa ventana pasan sin registro.
    ///
    /// Se acepta, y conviene entender por qué NO es una regresión: un `Busy`
    /// falla en abierto por diseño (arriba), y quien puede sostener el lock
    /// mantiene a la sesión sin registro **indefinidamente**, no treinta
    /// segundos — es la mitad de #178 que sigue abierta y que sigue #203. Un
    /// desfase de 30 s dentro de un agujero permanente no añade capacidad
    /// alguna. Lo que NO se puede hacer es cerrarlo saltándose el freno aquí:
    /// eso devuelve `ESPERA_POR_EL_LOCK` por CADA mutación mientras haya un
    /// daemon vivo, que es exactamente el coste que el freno existe para no
    /// pagar.
    async fn journal_gate(&self) -> Result<(), Error> {
        let JournalSource::Lazy(lazy) = &self.journal else {
            return Ok(());
        };
        match lazy.resolve().await {
            Ok(_) | Err(crate::embedded::NoJournal::Busy) => Ok(()),
            Err(crate::embedded::NoJournal::Failed(motivo)) => {
                // El motivo lleva el fichero y va al LOG del operador; la
                // categoría que cruza al frontend no lleva ninguno de los dos
                // (ver el rustdoc de la variante).
                // «Operación» y no «mutación»: desde la fase 7 esta puerta la
                // cruza también una LECTURA (`journal_page`, la línea de
                // tiempo), y decirle al operador que se rehusó una mutación
                // cuando alguien sólo abrió una pantalla es una línea de log
                // que manda a buscar un cambio que no existió.
                tracing::error!(
                    motivo = %motivo,
                    "operación rehusada: el journal de esta sesión no se puede abrir (#178)"
                );
                Err(Error::JournalUnavailable)
            } // SIN brazo comodín, y eso es el fail-closed: `NoJournal` es
              // `#[non_exhaustive]` de puertas afuera, pero aquí dentro el
              // compilador exige exhaustividad, así que un motivo NUEVO rompe la
              // compilación en vez de colarse como «adelante» por un `_`. Lo que
              // no se sabe clasificar no journaliza, y lo que no journaliza no
              // muta: que lo decida quien añada el motivo.
        }
    }

    async fn policy_gate(
        &self,
        actor: &crate::journal::Actor,
        op: crate::policy::PolicyOp,
        paths: &[&VPath],
    ) -> Result<(), Error> {
        self.policy_checker().check(actor, op, paths).await
    }

    /// La puerta de policy SIN `&self`: dos `Arc` que sí caben dentro del
    /// cuerpo `'static` de una Task (#171).
    ///
    /// Preguntarle a la policy DESDE DENTRO de la Task —que es lo que hace el
    /// ejecutor hacia delante y lo que el undo no hacía— exige poder capturar
    /// la puerta. Es lo mismo que `sync::exec::SyncTargets` ya se lleva para
    /// consultar paso a paso.
    fn policy_checker(&self) -> PolicyChecker {
        PolicyChecker {
            policy: Arc::clone(&self.policy),
            approvals: Arc::clone(&self.approvals),
        }
    }

    /// Registra un provider bajo su scheme (pisa el anterior si lo había).
    ///
    /// # Panics
    /// Nunca en la práctica: solo por envenenamiento del lock interno.
    pub fn register_provider(&self, provider: Arc<dyn Provider>) {
        self.sessions.register_process(provider);
    }

    /// Configura el conector de providers remotos (fase 6e, ADR 0015 A):
    /// ante un `VPath` remoto sin provider, el Engine le pide la conexión y
    /// cachea el resultado por `scheme://authority`.
    ///
    /// # Panics
    /// Nunca en la práctica: solo por envenenamiento del lock interno.
    pub fn set_connector(&self, connector: Arc<dyn crate::connect::RemoteConnector>) {
        *self.connector.write().expect("connector lock sano") = Some(connector);
    }

    /// Instala el observer de avisos de conexión (#44): el engine le entrega
    /// cada `ConnectionWarning` de un establecimiento remoto. Sin observer, los
    /// avisos se dropean (el `tracing::warn!` del connector sigue en el log).
    ///
    /// # Panics
    /// Nunca en la práctica: solo por envenenamiento del lock interno.
    pub fn set_connection_observer(&self, observer: Arc<dyn crate::connect::ConnectionObserver>) {
        *self
            .connection_observer
            .write()
            .expect("connection_observer lock sano") = Some(observer);
    }

    /// Instala un observer ENCADENADO al que ya estuviera: `hacer` recibe el
    /// anterior y devuelve el nuevo, que debe reenviarle lo que reciba.
    ///
    /// Existe porque la ranura es de UNO y hay DOS hechos que salen por ella
    /// —la degradación (#44) y el fallo (#322)— que el frontend toma por
    /// canales separados. Antes, el segundo instalador pisaba al primero y
    /// dejaba su canal mudo para siempre, en silencio.
    ///
    /// Y es UNA operación y no «lee y luego pon»: con dos llamadas, dos
    /// instaladores concurrentes leen el mismo anterior y el segundo pierde al
    /// primero — el mismo fallo mudo, ahora con carrera. Aquí el swap ocurre
    /// bajo el mismo candado de escritura.
    ///
    /// # Panics
    /// Nunca en la práctica: solo por envenenamiento del lock interno.
    pub fn chain_connection_observer<F>(&self, hacer: F)
    where
        F: FnOnce(
            Option<Arc<dyn crate::connect::ConnectionObserver>>,
        ) -> Arc<dyn crate::connect::ConnectionObserver>,
    {
        let mut ranura = self
            .connection_observer
            .write()
            .expect("connection_observer lock sano");
        let previo = ranura.take();
        *ranura = Some(hacer(previo));
    }

    /// Registra la host key de `host:port` tras confirmación explícita del
    /// usuario (flujo TOFU, método `connection.trust_host_key`).
    ///
    /// # Ni gate de policy ni entrada de journal, y por qué (#204, regla 4)
    ///
    /// Esto escribe `known_hosts`, que es el fichero más sensible que norte
    /// escribe aparte del journal y el keyring: decide qué claves de host
    /// aceptará a partir de ahora. Y no pasa ni por el gate ni por el journal.
    /// Las dos ausencias son deliberadas y se dicen aquí para que nadie
    /// vuelva a deducirlas:
    ///
    /// * **Quién puede llegar.** El despacho del daemon rechaza
    ///   `connection.trust_host_key` para cualquier actor que no sea
    ///   `Actor::User`, con `INVALID_REQUEST`, igual que
    ///   `policy.grant_scope`/`decide`/`undo_session` (#66): bendecir la
    ///   identidad de un host es un acto de gobierno humano, no una operación
    ///   de fichero. Un agente no lo alcanza, así que un gate de scopes aquí
    ///   defendería una puerta que ya está cerrada — y por rutas, que no es la
    ///   dimensión en la que este permiso se mide. Por la API embebida no hay
    ///   agentes: `Backend::Embedded` solo lo construyen la CLI y el TUI sin
    ///   `--daemon`, y el puente MCP va SIEMPRE por socket con
    ///   `agent_session`.
    ///
    /// * **Clasificación (regla 4): `Irreversible`, con motivo.** El journal
    ///   describe el árbol de ficheros del usuario y su undo lo devuelve a un
    ///   estado anterior; `known_hosts` no es parte de ese árbol, y «des-confiar
    ///   una clave» no es una operación que este programa ofrezca ni que un
    ///   `undo_session` deba poder hacer a ciegas — retirar una clave de host
    ///   en un `undo` que el usuario pidió por OTRA cosa rompería conexiones
    ///   que no tenían nada que ver. Lo que sí queda es rastro: el conector
    ///   re-verifica el fingerprint contra la clave que el host presenta AHORA
    ///   (anti-TOCTOU, ADR 0015 D) y la decisión la toma un humano delante del
    ///   fingerprint.
    ///
    /// Si algún día un agente necesitara esta puerta, lo que hace falta NO es
    /// un scope de rutas: es una op de policy propia, y entonces sí una
    /// entrada de journal que diga qué clave se aceptó y cuándo.
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin conector; los del conector (p. ej.
    /// [`Error::HostKeyMismatch`] si el host ya no presenta esa clave).
    ///
    /// # Panics
    /// Nunca en la práctica: solo por envenenamiento del lock interno.
    pub async fn trust_host_key(
        &self,
        host: &str,
        port: Option<u16>,
        fingerprint: &str,
    ) -> Result<(), Error> {
        let connector = self
            .connector
            .read()
            .expect("connector lock sano")
            .clone()
            .ok_or(Error::Unsupported)?;
        connector.trust_host_key(host, port, fingerprint).await
    }

    /// Guarda para ESTA sesión el secreto de la conexión `conn`, que un humano
    /// acaba de teclear (método `connection.provide_secret`, #325).
    ///
    /// # Por qué existe, y qué NO hace
    ///
    /// El resolutor de secretos mira variable de entorno, keyring y fichero
    /// `age` (ADR 0015). Cuando una entrada declara `secret = "prompt"` y
    /// ninguna de las tres tiene nada, el core no puede seguir solo: devuelve
    /// [`Error::SecretNeeded`] y el frontend pregunta. Esta puerta es por
    /// donde vuelve la respuesta.
    ///
    /// **El secreto vive en memoria y solo hasta que el daemon pare.** No se
    /// escribe a `connections.toml`, ni al keyring, ni al fichero `age`; ese
    /// «recordar» es otra decisión y no la toma este método.
    ///
    /// # Quién puede llegar, y clasificación
    ///
    /// Las mismas dos ausencias que [`Self::trust_host_key`], por las mismas
    /// razones: el despacho del daemon lo rechaza para todo actor que no sea
    /// `Actor::User` (teclear una contraseña es un acto humano; un agente que
    /// pudiera inyectar credenciales de sesión elegiría con qué identidad
    /// actúa el usuario), y no hay entrada de journal porque no toca el árbol
    /// de ficheros ni deja nada que deshacer — al parar el daemon desaparece.
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin conector; los del conector.
    ///
    /// # Panics
    /// Nunca en la práctica: solo por envenenamiento del lock interno.
    // `skip_all` y no `skip(self)`: el segundo argumento es una CONTRASEÑA, y
    // con `skip(self)` `tracing` la formatearía en el span. Está escrito aquí
    // y no solo en el conector porque este es el método público, y es el que
    // alguien ampliará.
    #[tracing::instrument(level = "info", skip_all, fields(conn = %conn))]
    pub async fn provide_secret(&self, conn: &str, secret: &str) -> Result<(), Error> {
        // La cadena vacía se rechaza AQUÍ y no solo en el diálogo del TUI.
        // Que un frontend deje inerte el confirmar con el campo vacío es
        // presentación; que un secreto vacío no entre en el resolutor es
        // política, vive en el core (regla 7), y sin esta guarda un cliente
        // con un bug mete un vacío que —al ir el escalón de sesión el
        // primero— tapa las otras tres fuentes hasta que alguien pare el
        // daemon.
        //
        // `PermissionDenied` y no una variante nueva: es a lo que degrada
        // `ConnectError::SecretEmpty` (#320) unos pasos más abajo, así que
        // decir lo mismo aquí no inventa taxonomía — una credencial vacía es
        // una credencial que no autentica.
        if secret.is_empty() {
            tracing::warn!("secreto vacío rechazado");
            return Err(Error::PermissionDenied);
        }
        let connector = self
            .connector
            .read()
            .expect("connector lock sano")
            .clone()
            .ok_or(Error::Unsupported)?;
        connector.provide_secret(conn, secret).await
    }

    /// La clave de registro/caché para `p`: providers de proceso van por
    /// scheme; los remotos por `scheme://authority`.
    fn provider_key(p: &VPath) -> String {
        match p.authority() {
            Some(a) => format!("{}://{a}", p.scheme()),
            None => p.scheme().to_owned(),
        }
    }

    /// Cierra la sesión remota de `p` (#140). `false` si no había ninguna.
    ///
    /// Un scheme de PROCESO —`file://`, `mem://`, el de un provider-plugin— no
    /// se cierra: no hay sesión que soltar, y decir que sí sería mentir sobre
    /// algo que sigue exactamente igual. La siguiente operación sobre esa
    /// autoridad vuelve a conectar por el camino de siempre: cerrar suelta, no
    /// prohíbe.
    pub fn close_connection(&self, p: &VPath) -> bool {
        // Registrado por scheme entero = provider de proceso, no una sesión.
        if self.sessions.lookup(p.scheme()).is_some() {
            return false;
        }
        self.sessions.close(&Self::provider_key(p))
    }

    async fn provider_for(&self, p: &VPath) -> Result<Arc<dyn Provider>, Error> {
        let key = Self::provider_key(p);
        // Primero el provider de proceso registrado para el scheme entero
        // (local, mem de tests): tiene prioridad y no dispara conexiones.
        if let Some(prov) = self.sessions.lookup(p.scheme()) {
            return Ok(prov);
        }
        if let Some(prov) = self.sessions.lookup(&key) {
            return Ok(prov);
        }
        // Archivos como directorios (ADR 0018): scheme compuesto = provider
        // por composición sobre el provider del CONTENEDOR. Antes del
        // connector: el interior puede ser local o una conexión ya viva.
        if let Some(aref) = p.archive_split().map_err(|_| Error::InvalidPath)? {
            // #56: tope de CAPAS anidadas ANTES de componer nada — cuenta
            // los tokens de formato del scheme (pelado izquierda→derecha,
            // mismo longest-match que el split).
            let mut layers = 0usize;
            let mut sch = p.scheme();
            while let Some(f) = norte_proto::scheme_archive_format(sch) {
                layers += 1;
                sch = &sch[f.len() + 1..];
            }
            let max_nesting = self
                .archive_limits
                .read()
                .expect("archive_limits lock sano")
                .max_nesting;
            if layers > max_nesting {
                tracing::warn!(layers, max_nesting, "anidamiento de archivo sobre el tope");
                return Err(Error::LimitExceeded {
                    limit: Error::LIMIT_NESTING.into(),
                });
            }
            // `rar` no compone sobre un provider interior: el delegado
            // externo necesita una RUTA de verdad, así que el interior tiene
            // que ser un `file://` local y sin authority. Cualquier otra cosa
            // —sftp, s3, o un archivo dentro de otro archivo— se niega AQUÍ,
            // antes de componer nada, en vez de traerse el contenedor entero
            // por una descarga que nadie pidió.
            if aref.format == "rar" {
                return self.rar_provider_for(&aref, key);
            }
            let format = match aref.format.as_str() {
                "tar" => norte_vfs_archive::Format::Tar,
                "zip" => norte_vfs_archive::Format::Zip,
                "tar+gz" => norte_vfs_archive::Format::TarGz,
                // Formato de la whitelist de proto sin provider aquí: una
                // versión de core más vieja que el proto. Honesto: no sé.
                _ => return Err(Error::Unsupported),
            };
            // #56: el exterior puede ser a su vez un path de archivo —
            // recursión capa a capa, acotada por el gate max_nesting de
            // arriba (jamás ilimitada).
            let inner = Box::pin(self.provider_for(&aref.outer)).await?;
            tracing::debug!(scheme = %p.scheme(), %key, "componiendo provider de archivo");
            // expect: envenenado = otro hilo panicó a mitad de escritura —
            // irrecuperable, misma convención que el resto de locks del
            // engine (ver `# Panics` de `set_archive_limits`).
            let limits = *self
                .archive_limits
                .read()
                .expect("archive_limits lock sano");
            let provider: Arc<dyn Provider> =
                Arc::new(norte_vfs_archive::ArchiveProvider::with_limits(
                    inner,
                    format,
                    p.scheme().to_owned(),
                    limits,
                ));
            // Double-check en el pool: si otra petición registró primero,
            // gana la suya (el ArchiveProvider extra solo es RAM).
            return Ok(self.sessions.insert_composite(key, provider));
        }
        let connector = self
            .connector
            .read()
            .expect("connector lock sano")
            .clone()
            .ok_or(Error::Unsupported)?;
        let Some(authority) = p.authority() else {
            return Err(Error::Unsupported);
        };
        let observer = self
            .connection_observer
            .read()
            .expect("connection_observer lock sano")
            .clone();
        // Dedup canónica (#47): resuelve la forma canónica ANTES de marcar
        // (lectura local de connections.toml) — un alias de una sesión viva
        // acierta aquí y jamás abre una segunda.
        let (cache_key, alias) = match connector.canonical_authority(p.scheme(), authority).await {
            Some(canonical) if canonical != authority => {
                let ckey = format!("{}://{canonical}", p.scheme());
                // alias_current re-lee la canónica BAJO el lock: si la
                // sesión cayó entre el lookup y aquí, jamás re-inserta un
                // Arc muerto como alias (sec MAJOR-2 del review #47).
                if let Some(prov) = self.sessions.alias_current(&ckey, key.clone()) {
                    return Ok(prov);
                }
                (ckey, Some(key))
            }
            _ => (key, None),
        };
        // El dial va en el pool (#47): single-flight por clave, timeout,
        // cancelable por drop del waiter, backoff de fallos transitorios.
        self.sessions
            .connect_remote(cache_key, alias, p.scheme(), authority, connector, observer)
            .await
    }

    /// Metadatos de un nodo (directo, sin Task).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay provider para el scheme; los del provider.
    pub async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.provider_for(p).await?.stat(p).await
    }

    /// Listado de un directorio (directo, sin Task).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay provider para el scheme; los del provider.
    pub async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.provider_for(p).await?.list(p).await
    }

    /// [`Self::stat`] con opciones (#108 bloque 2): atributos por entrada.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay provider para el scheme; los del provider.
    pub async fn stat_with(&self, p: &VPath, opt: &norte_vfs::ListOptions) -> Result<Entry, Error> {
        self.provider_for(p).await?.stat_with(p, opt).await
    }

    /// [`Self::list`] con opciones (#108 bloque 2): atributos por entrada.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay provider para el scheme; los del provider.
    pub async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
        self.provider_for(p).await?.list_with(p, opt).await
    }

    /// Catálogo de attrs del provider de `p`, SANEADO: `AttrCatalog::new` es
    /// el único camino al wire y también el del backend embebido (ADR 0039
    /// §4 — un catálogo en proceso jamás se cuela sin sanear).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay provider para el scheme.
    pub async fn attr_catalog(&self, p: &VPath) -> Result<norte_proto::AttrCatalog, Error> {
        Ok(norte_proto::AttrCatalog::new(
            self.provider_for(p).await?.attrs().to_vec(),
        ))
    }

    /// Inyecta el proveedor de IA para el rename revisable (M4-A2, ADR 0031).
    ///
    /// # Panics
    /// Solo por envenenamiento del lock interno (irrecuperable).
    pub fn set_ai_provider(&self, provider: norte_ai::SharedAiProvider) {
        *self.ai_provider.write().expect("ai_provider lock sano") = Some(provider);
    }

    /// Inyecta el proveedor de EMBEDDINGS para `index.embed` /
    /// `index.search_semantic` (M4-IA-2, ADR 0031 A3). Separado de
    /// [`Self::set_ai_provider`]: `[ai].embed_provider` puede nombrar otro
    /// proveedor/modelo que el del rename.
    ///
    /// # Panics
    /// Solo por envenenamiento del lock interno (irrecuperable).
    pub fn set_ai_embed_provider(&self, provider: norte_ai::SharedAiProvider) {
        *self.ai_embed.write().expect("ai_embed lock sano") = Some(provider);
    }

    /// Fija la config `[ai]` (opt-in/local-only/denied-paths). Sin ella el
    /// gate rechaza toda operación de IA (default deshabilitado).
    ///
    /// # Panics
    /// Solo por envenenamiento del lock interno.
    pub fn set_ai_config(&self, config: crate::ai::AiConfig) {
        *self.ai_config.write().expect("ai_config lock sano") = config;
    }

    /// Sugiere un plan de rename REVISABLE para los archivos de `dir` según
    /// `instruction` (spec §9, ADR 0031). NO muta nada — el plan es el
    /// producto; aplicarlo es N `fs.move` gobernados (journal + undo +
    /// policy). El gate opt-in se evalúa ANTES de que ningún nombre salga al
    /// proveedor; los nombres hostiles (no-UTF8) se rechazan fail-loud sin
    /// enviarse.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay proveedor de IA instalado;
    /// [`Error::PolicyDenied`] si el gate rechaza (IA off, local-only sobre
    /// remoto, o `dir` bajo un `denied_prefix`); los del listado o del
    /// proveedor mapeados a la taxonomía del wire.
    ///
    /// # Panics
    /// Solo por envenenamiento de un lock interno (irrecuperable).
    pub async fn ai_rename_plan(
        &self,
        dir: &VPath,
        instruction: &str,
    ) -> Result<crate::ai::RenamePlan, Error> {
        self.ai_rename_plan_for(dir, instruction, &[]).await
    }

    /// [`Self::ai_rename_plan`] sobre un SUBCONJUNTO de `dir` (#121).
    ///
    /// `only` son nombres BASE. Vacío = el directorio entero, que es lo que
    /// hacía antes de existir este parámetro.
    ///
    /// Lo que compra no es comodidad: con la selección de primera clase (#103),
    /// marcar cinco ficheros y pedir un plan mandaba los mil del directorio al
    /// proveedor. Eso es más de lo que el humano señaló, y el gate de IA existe
    /// justamente para acotar lo que sale de la máquina.
    ///
    /// Un nombre que no está en el listado se IGNORA en vez de rechazar el
    /// plan: entre marcar y pedir, un fichero puede haberse ido, y castigar al
    /// lector por esa carrera no arregla nada. Si tras filtrar no queda
    /// ninguno, no se llama al proveedor — un plan sobre nada no es una
    /// pregunta.
    ///
    /// # Errors
    /// Las de [`Self::ai_rename_plan`].
    ///
    /// # Panics
    /// Solo por envenenamiento de un lock interno (irrecuperable).
    pub async fn ai_rename_plan_for(
        &self,
        dir: &VPath,
        instruction: &str,
        only: &[String],
    ) -> Result<crate::ai::RenamePlan, Error> {
        use futures::StreamExt;

        /// Tope del reply acumulado (#M4 security): ver el bucle de drenado.
        const MAX_REPLY_BYTES: usize = 512 * 1024;

        // El tope se comprueba AQUÍ y no solo en el dispatch del daemon, como
        // el de `fs.set_mode`: `ntc` corre embebido por defecto, así que un
        // tope que solo vive en el wire no protege al camino que más se usa.
        // Lo que acota es un filtro O(nombres × entradas) sobre una llamada
        // DIRECTA —sin Task— que solo puede morir por timeout.
        if only.len() > norte_proto::methods::AI_RENAME_NAMES_MAX {
            tracing::debug!(n = only.len(), "ai.rename_plan por encima del tope");
            return Err(Error::InvalidPath);
        }

        let provider = self
            .ai_provider
            .read()
            .expect("ai_provider lock sano")
            .clone()
            .ok_or(Error::Unsupported)?;

        // Gate PRE-contenido: nada sale hasta que pasa (spec §9). El clon de
        // la config evita retener el lock a través de los await.
        {
            let config = self.ai_config.read().expect("ai_config lock sano").clone();
            crate::ai::AiGate::new(&config)
                .check(crate::ai::AiOp::Rename, provider.is_local(), &[dir])
                .map_err(|reason| ai_denied_to_error(&reason))?;
        }

        // Nombres base de los archivos del dir (a través del provider, jamás
        // el FS directo — regla 9). Se OMITE toda entrada cuya ruta caiga
        // bajo un `denied_prefix` (security MINOR del review #M4: el nombre
        // de un dir denegado que sea hijo directo de `dir` no debe salir —
        // el gate solo comprueba `dir`).
        let denied = {
            let config = self.ai_config.read().expect("ai_config lock sano");
            config.denied_prefixes.clone()
        };
        // El subconjunto, como CONJUNTO: el filtro de abajo corre por cada
        // entrada del listado, y una búsqueda lineal sobre 4096 nombres en un
        // directorio de un millón son minutos de CPU en una llamada que no es
        // una Task y que solo puede morir por timeout.
        let solo: std::collections::HashSet<&[u8]> =
            only.iter().map(std::string::String::as_bytes).collect();
        let mut stream = self.list(dir).await?;
        let mut names = Vec::new();
        while let Some(item) = stream.next().await {
            let entry = item?;
            if denied
                .iter()
                .any(|prefix| crate::policy::is_under(prefix, &entry.path))
            {
                continue;
            }
            if let Some(name) = entry.path.file_name() {
                // El subconjunto se filtra CONTRA EL LISTADO y por bytes
                // (#121): el nombre que el frontend marcó tiene que existir
                // aquí, y compararlo como texto perdería los que no son UTF-8
                // — que son justo los que más falta hace no confundir.
                if !solo.is_empty() && !solo.contains(name.as_bytes()) {
                    continue;
                }
                names.push(name.clone());
            }
        }
        if names.is_empty() && !only.is_empty() {
            // Lo que se marcó ya no está. No se llama al proveedor: un plan
            // sobre nada no es una pregunta, y mandar la instrucción con una
            // lista vacía gasta cuota para que conteste lo mismo.
            return Ok(crate::ai::RenamePlan::default());
        }

        let req = crate::ai::build_rename_prompt(&names, instruction)
            .map_err(|e| ai_to_proto_error(&e))?;
        // Lo que se mide del canje, y lo que NO. Aquí se sabe si el contrato
        // de salida tipada llegó a viajar (`estructurada`), y sin eso no hay
        // forma de saber si sirve de algo: una capacidad declarada y no
        // efectiva es justo lo que este camino tenía.
        //
        // Nunca la instrucción, nunca los nombres, nunca la respuesta: son
        // datos del usuario y el log no es sitio para ellos (regla 10). Sólo
        // el proveedor, el número de entradas, el tiempo y qué pasó.
        let estructurada = provider
            .capabilities()
            .contains(norte_ai::AiCaps::JSON_OUTPUT);
        let proveedor = provider.id();
        let empezo = std::time::Instant::now();
        // El fallo de establecimiento se mide TAMBIÉN: si sólo se midiera el
        // camino que llega a parsear, `estructurada` diría qué tal va el
        // contrato entre los intercambios que ya funcionaban, que es la
        // muestra equivocada — los que se caen en red o en auth son los que
        // más interesa contar.
        let mut chat = match provider.chat(req).await {
            Ok(c) => c,
            Err(e) => {
                tracing::info!(
                    proveedor,
                    estructurada,
                    entradas = names.len(),
                    ms = empezo.elapsed().as_millis(),
                    resultado = categoria_de_ai(&e),
                    "plan de renombrado por IA"
                );
                return Err(ai_to_proto_error(&e));
            }
        };
        // Cota del reply (security MAJOR del review #M4): un endpoint
        // comprometido/MITM puede stremear deltas sub-1MiB sin fin (el tope
        // por línea de http.rs no acota el ACUMULADO) → OOM. Un plan
        // `[{from,to}]` legítimo cabe de sobra en 512 KiB.
        let mut reply = String::new();
        while let Some(delta) = chat.next().await {
            let delta = delta.map_err(|e| ai_to_proto_error(&e))?;
            if reply.len() + delta.len() > MAX_REPLY_BYTES {
                tracing::warn!(
                    max = MAX_REPLY_BYTES,
                    "respuesta del proveedor de IA sobre el tope; abortando"
                );
                return Err(Error::Internal { panic: false });
            }
            reply.push_str(&delta);
        }
        let plan = crate::ai::validate_rename_reply(&reply, &names);
        let categoria = plan.as_ref().map_or_else(|e| categoria_de_ai(e), |_| "ok");
        tracing::info!(
            proveedor,
            estructurada,
            entradas = names.len(),
            bytes = reply.len(),
            ms = empezo.elapsed().as_millis(),
            resultado = categoria,
            "plan de renombrado por IA"
        );
        plan.map_err(|e| ai_to_proto_error(&e))
    }

    /// Plan de ORGANIZAR por IA (fase 8, `ai.organize_plan`).
    ///
    /// El gemelo de [`Self::ai_rename_plan_for`], con el mismo gate, el mismo
    /// filtrado de prefijos denegados, el mismo tope de respuesta y el mismo
    /// cinturón de validación — lo único que cambia es que el destino puede
    /// llevar subdirectorios, y esa diferencia la comprueba
    /// [`crate::ai::validate_organize_reply`] con la MISMA función que usa el
    /// core al ejecutar.
    ///
    /// **No muta nada.** El plan es el producto; aplicarlo es
    /// [`Self::organize`].
    ///
    /// # Errors
    /// Las de [`Self::ai_rename_plan_for`]: sin proveedor,
    /// [`Error::Unsupported`]; el gate de IA; lo que conteste el proveedor; y
    /// [`Error::InvalidPath`] si `only` pasa del tope.
    ///
    /// # Panics
    /// No: los `expect` son sobre locks propios.
    pub async fn ai_organize_plan_for(
        &self,
        dir: &VPath,
        instruction: &str,
        only: &[String],
    ) -> Result<crate::ai::OrganizePlanReply, Error> {
        use futures::StreamExt;

        /// El mismo tope de respuesta acumulada que el plan de renombrado, y
        /// por el mismo motivo: un endpoint comprometido puede stremear sin
        /// fin y el tope por línea no acota el acumulado.
        const MAX_REPLY_BYTES: usize = 512 * 1024;

        if only.len() > norte_proto::methods::AI_RENAME_NAMES_MAX {
            return Err(Error::InvalidPath);
        }
        let provider = self
            .ai_provider
            .read()
            .expect("ai_provider lock sano")
            .clone()
            .ok_or(Error::Unsupported)?;
        // Gate PRE-contenido: nada sale hasta que pasa. Organizar se evalúa
        // como `Rename` porque es lo que es —proponer nombres nuevos para
        // ficheros de este directorio—, y darle un `AiOp` propio obligaría a
        // cada configuración existente a permitirlo otra vez para algo que ya
        // había decidido.
        {
            let config = self.ai_config.read().expect("ai_config lock sano").clone();
            crate::ai::AiGate::new(&config)
                .check(crate::ai::AiOp::Rename, provider.is_local(), &[dir])
                .map_err(|reason| ai_denied_to_error(&reason))?;
        }
        let denied = {
            let config = self.ai_config.read().expect("ai_config lock sano");
            config.denied_prefixes.clone()
        };
        let solo: std::collections::HashSet<&[u8]> =
            only.iter().map(std::string::String::as_bytes).collect();
        let mut stream = self.list(dir).await?;
        let mut names = Vec::new();
        while let Some(item) = stream.next().await {
            let entry = item?;
            if denied
                .iter()
                .any(|prefix| crate::policy::is_under(prefix, &entry.path))
            {
                continue;
            }
            if let Some(name) = entry.path.file_name() {
                if !solo.is_empty() && !solo.contains(name.as_bytes()) {
                    continue;
                }
                names.push(name.clone());
            }
        }
        if names.is_empty() && !only.is_empty() {
            return Ok(crate::ai::OrganizePlanReply::default());
        }
        let req = crate::ai::build_organize_prompt(&names, instruction)
            .map_err(|e| ai_to_proto_error(&e))?;
        let estructurada = provider
            .capabilities()
            .contains(norte_ai::AiCaps::JSON_OUTPUT);
        let proveedor = provider.id();
        let empezo = std::time::Instant::now();
        let mut chat = match provider.chat(req).await {
            Ok(c) => c,
            Err(e) => {
                tracing::info!(
                    proveedor,
                    estructurada,
                    entradas = names.len(),
                    ms = empezo.elapsed().as_millis(),
                    resultado = categoria_de_ai(&e),
                    "plan de organizar por IA"
                );
                return Err(ai_to_proto_error(&e));
            }
        };
        let mut reply = String::new();
        while let Some(delta) = chat.next().await {
            let delta = delta.map_err(|e| ai_to_proto_error(&e))?;
            if reply.len() + delta.len() > MAX_REPLY_BYTES {
                tracing::warn!(
                    max = MAX_REPLY_BYTES,
                    "respuesta del proveedor de IA sobre el tope; abortando"
                );
                return Err(Error::Internal { panic: false });
            }
            reply.push_str(&delta);
        }
        let plan = crate::ai::validate_organize_reply(&reply, &names);
        let categoria = plan.as_ref().map_or_else(|e| categoria_de_ai(e), |_| "ok");
        // Nunca la instrucción, nunca los nombres, nunca la respuesta: son
        // datos del usuario y el log no es sitio para ellos (regla 10).
        tracing::info!(
            proveedor,
            estructurada,
            entradas = names.len(),
            bytes = reply.len(),
            ms = empezo.elapsed().as_millis(),
            resultado = categoria,
            "plan de organizar por IA"
        );
        plan.map_err(|e| ai_to_proto_error(&e))
    }

    /// Total de entradas omitidas del índice del contenedor de `p` (#93),
    /// `None` si el provider lista todo lo que existe (ver
    /// [`norte_vfs::Provider::list_skipped`]).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay provider para el scheme; los del provider.
    pub async fn list_skipped(&self, p: &VPath) -> Result<Option<u64>, Error> {
        self.provider_for(p).await?.list_skipped(p).await
    }

    /// El ancla del directorio `p` (#295): la identidad OPACA del nodo que un
    /// listado devuelve, para que la copia que escriba ahí después pueda decir
    /// cuál era.
    ///
    /// Se pregunta SIGUIENDO enlaces, porque lo que el humano estaba mirando es
    /// el directorio cuyo contenido se listó, no el enlace por el que se llegó.
    /// Un `~/copias -> /mnt/disco/copias` y `/mnt/disco/copias` dan la misma
    /// ancla, que es exactamente lo que hace falta: el mismo destino aprobado
    /// por dos nombres no puede ser dos destinos.
    ///
    /// `None` = este provider no sabe dar identidad de nodo (un bucket, un
    /// SFTP sin extensiones). Entonces no hay ancla, el cliente no manda
    /// ninguna y la escritura se comporta como en 0.53.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay provider para el scheme; los del provider.
    pub async fn dir_anchor(&self, p: &VPath) -> Result<Option<norte_proto::DirAnchor>, Error> {
        let id = self
            .provider_for(p)
            .await?
            .node_id(p, norte_vfs::FollowLinks::Yes)
            .await?;
        Ok(id.map(crate::anchor::de_nodo))
    }

    /// Lectura de un archivo como stream (directa, sin Task), con rango
    /// opcional — el viewer lee cabeceras de archivos enormes sin tragarse
    /// el resto (ADR 0005).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay provider para el scheme; los del provider.
    pub async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        self.provider_for(p).await?.read(p, range).await
    }

    /// Búsqueda viva bajo un subtree (spec §17.1a, M4): Task cancelable
    /// ([`TaskKind::Search`]) + canal `mpsc` de lotes de hits. El nombre casa
    /// por glob O regex, el contenido por literal multi-encoding O regex; al
    /// menos un criterio, glob/regex y `content`/`content_regex` excluyentes por
    /// eje. Los matchers se COMPILAN aquí, ANTES de la Task: un patrón inválido
    /// o una combinación ilegal es un error del REQUEST (no un fallo de la Task
    /// ya lanzada).
    ///
    /// **Gate de policy**: NINGUNO. `fs.search` es LECTURA pura y se trata
    /// EXACTAMENTE como `fs.list`/`fs.read`, que NO pasan por el gate del engine
    /// (las lecturas son directas — ver [`Self::list`]/[`Self::read`] y
    /// `handle_fs_list` en el daemon, que llaman al provider sin `PolicyOp`). No
    /// existe un `PolicyOp` de lectura; introducir uno solo para `search` haría
    /// que un agente pudiese LISTAR pero no BUSCAR el mismo subtree, una
    /// asimetría sin sentido. El `actor` se propaga a la Task (por consistencia
    /// con las mutaciones y para auditoría futura), pero no gatea nada.
    ///
    /// **Lo que el `actor` SÍ decide** (#165): las
    /// [`walk_exclusions`](crate::policy::walk_exclusions) del recorrido. El
    /// gate de lectura del daemon mira la RAÍZ, así que una búsqueda de un
    /// AGENTE sobre `$HOME` —legítima— bajaría al directorio de estado del
    /// daemon y devolvería `journal.db` y los spools de sync. No es un gate:
    /// es por dónde no se baja, y el humano no lleva ninguna.
    ///
    /// **Mapeo de progreso** (lo consume el TUI): `entries_done` = entradas
    /// examinadas (incluidas las saltadas por error); `bytes_done` = nº de hits
    /// acumulados (no hay bytes reales en una búsqueda — se reutiliza el campo);
    /// `current` = última entrada vista. `max_hits` alcanzado ⇒ `Completed` (no
    /// `Failed`), el cliente infiere "truncada" comparando el total con el tope.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] si los criterios no validan/compilan (el daemon lo
    /// traduce a `INVALID_PARAMS`; el detalle saneado lo obtiene con
    /// [`crate::search::SearchMatchers::compile`], que devuelve el mensaje del
    /// compilador de glob/regex). [`Error::Unsupported`] si el scheme del `root`
    /// no tiene provider registrado.
    ///
    /// **Lo que decide el actor, y lo que decide la petición** (0.81.0): las
    /// exclusiones de la POLÍTICA (`policy::walk_exclusions`) se calculan
    /// primero, y las que trae `params.exclude_roots` se AÑADEN a ellas. Se
    /// suman y no se sustituyen, así que una petición puede estrechar el
    /// recorrido y no puede ensancharlo: no hay forma de levantar un veto
    /// metiendo rutas en una lista.
    pub async fn search_as(
        &self,
        params: norte_proto::methods::FsSearchParams,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            tokio::sync::mpsc::Receiver<norte_proto::methods::SearchHits>,
        ),
        Error,
    > {
        let matchers = crate::search::SearchMatchers::compile(&params).map_err(|e| {
            tracing::debug!(error = %e, "criterios de fs.search inválidos");
            Error::InvalidPath
        })?;
        let provider = self.provider_for(&params.root).await?;
        let root = params.root;
        // Lo que un AGENTE no puede recorrer aunque su raíz sea legítima
        // (#165): el directorio de estado del daemon cuelga de `$HOME`, y el
        // gate de lectura del daemon solo mira la raíz de la búsqueda. El
        // humano no se sandboxea, así que busca en sus propios ficheros.
        let mut excluded = crate::policy::walk_exclusions(&actor);
        // Y lo que el LECTOR no quiere mirar (0.81.0). Se SUMAN, en este
        // orden y sin poder quitarse: lo de la política es lo que no se
        // puede leer, y esto es una preferencia. Una petición no levanta un
        // veto añadiendo rutas a una lista.
        excluded.extend(params.exclude_roots.iter().cloned());
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let key = root.scheme().to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::Search,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::search::run_walk(provider, root, matchers, excluded, tx, &ctx).await
                })
            }),
        );
        Ok((handle, rx))
    }

    /// Compara DOS árboles como Task cancelable (`fs.compare`, 0.39.0, ADR
    /// 0048): [`TaskKind::Compare`] + canal `mpsc` de lotes de filas
    /// ([`CompareRowsBatch`](norte_proto::methods::CompareRowsBatch)),
    /// acotados por
    /// [`COMPARE_ROWS_MAX_BATCH`](norte_proto::methods::COMPARE_ROWS_MAX_BATCH)
    /// y coalescidos, igual que [`Self::search_as`].
    ///
    /// **No muta nada**: sin journal, sin undo, no se escribe un byte (regla
    /// dura 4 no aplica; el porqué, largo, está en el módulo `compare`, que es
    /// privado y por eso no se enlaza).
    ///
    /// **Gate de policy**: NINGUNO aquí, y por el mismo motivo que
    /// [`Self::search_as`] — el gate de LECTURA vive en el daemon
    /// (`read_gate` sobre AMBAS raíces, más `content_gate` cuando el rung de
    /// hash está encendido), que es quien ata una conexión a un actor. Por la
    /// API embebida no existe un `Actor::Agent` que no se haya escrito el
    /// propio proceso.
    ///
    /// **Mapeo de progreso**: `entries_done` = FILAS emitidas (contrato de
    /// C1: es como un cliente detecta un `compare.rows` perdido); `bytes_done`
    /// se queda a cero — sin el rung de hash no se lee un solo byte.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] si las dos raíces son la MISMA (comparar algo
    /// contra sí mismo no es una petición, es una errata, y sale más caro que
    /// cualquier otro error de params: una hora de trabajo para contestar
    /// «todo igual»). [`Error::Unsupported`] si `follow_symlinks` viene a
    /// `true` —el motor acepta el campo y lo IGNORA, así que servir en
    /// silencio un recorrido distinto del pedido sería mentir— o si el scheme
    /// de alguna raíz no tiene provider registrado.
    pub async fn compare_as(
        &self,
        params: norte_proto::methods::FsCompareParams,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            tokio::sync::mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
        ),
        Error,
    > {
        // ANTES de resolver providers y de crear Task alguna: los dos rechazos
        // son del REQUEST, no fallos de una Task ya lanzada (mismo criterio
        // que la compilación de matchers de `search_as`).
        if params.follow_symlinks {
            tracing::debug!("fs.compare con follow_symlinks: no soportado");
            return Err(Error::Unsupported);
        }
        // Igualdad ESTRUCTURAL de `VPath` (scheme + authority + segmentos, ya
        // normalizados sin `.`/`..`). No detecta dos rutas que el sistema de
        // ficheros resuelva al mismo sitio —una raíz symlinkeada, un archivo
        // alcanzado por dos caminos, un mismo host SFTP bajo dos autoridades—:
        // eso exigiría resolver identidad real por provider, que hoy no está
        // en el trait. Comparar un árbol consigo mismo por esa vía no es
        // peligroso (no se escribe nada), solo caro y con todo `Same`.
        if params.left == params.right {
            tracing::debug!("fs.compare de una raíz contra sí misma");
            return Err(Error::InvalidPath);
        }
        let left = self.provider_for(&params.left).await?;
        let right = self.provider_for(&params.right).await?;
        let opts = norte_compare::CompareOptions {
            criteria: params.criteria,
            max_depth: params.max_depth,
            mtime_tolerance_ms: params.mtime_tolerance_ms,
            // Ya rechazado arriba; se pasa apagado explícitamente para que el
            // motor no dependa de esa comprobación remota.
            follow_symlinks: false,
            // `DescendSide` solo puede ser uno de los dos lados: la errata que
            // habría dado `Side::Unknown` —o sea, descender por NINGUNO sin
            // decirlo— muere en el deserializador del wire, y no hay chequeo
            // que este brazo (el embebido no pasa por el daemon) pueda
            // olvidarse.
            descend_orphans: params.descend_orphans.map(norte_proto::methods::Side::from),
        };
        let (left_root, right_root) = (params.left, params.right);
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        // La cola del scheduler es la de la raíz IZQUIERDA (la que lanzó la
        // comparación): una comparación cross-provider tiene que encolarse en
        // algún sitio, y elegir el otro lado no cambiaría nada.
        let key = left_root.scheme().to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::Compare,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    // El flujo se construye DENTRO: toma prestados los dos
                    // providers, así que el préstamo tiene que nacer aquí.
                    crate::compare::run_compare(left, left_root, right, right_root, opts, tx, &ctx)
                        .await
                })
            }),
        );
        Ok((handle, rx))
    }

    /// Cuánto ocupa lo que se le pase, como Task cancelable (`fs.dir_size`,
    /// 0.49.0, #139).
    ///
    /// **No muta**: recorre y suma. Sin journal y sin undo (la regla 4 no
    /// aplica), como [`Self::compare_as`].
    ///
    /// **El total no se devuelve aquí**: viaja en el progreso de la Task
    /// (`bytes_done`/`entries_done`), que es lo que un frontend ya sabe pintar,
    /// y el último snapshot es el resultado.
    ///
    /// **Gate de policy**: ninguno aquí, y por el mismo motivo que
    /// `compare_as` — el gate de LECTURA vive en el daemon, que es quien ata
    /// una conexión a un actor.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] si no se pasa ni una ruta —medir la nada no es
    /// una petición— y lo que devuelva la resolución de providers.
    pub async fn dir_size_as(
        &self,
        params: norte_proto::methods::FsDirSizeParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        // ANTES de crear Task alguna: es un rechazo del REQUEST, no el fallo de
        // una Task ya lanzada (mismo criterio que `compare_as`).
        let Some(primera) = params.paths.first() else {
            tracing::debug!("fs.dir_size sin rutas");
            return Err(Error::InvalidPath);
        };
        // Raíces que se solapan se RECHAZAN, como en `fs.compare` y
        // `sync.plan` (#247). Sin esto, `["file:///a", "file:///a/b"]` contaba
        // `b` dos veces y devolvía un número mayor que el sitio que ocupa —
        // justo lo contrario de para lo que existe el método, que es contestar
        // «¿cabe esto en el destino?». Se rechaza en vez de deduplicar porque
        // deduplicar es decidir por el llamante qué quiso decir, y la
        // selección de un panel jamás anida (son hermanos): quien manda raíces
        // anidadas lo hace desde un guion, y ahí un error es una respuesta.
        for (i, a) in params.paths.iter().enumerate() {
            for b in params.paths.iter().skip(i + 1) {
                if let Some(relation) = structural_overlap(a, b) {
                    tracing::debug!(?relation, "fs.dir_size con raíces solapadas");
                    return Err(Error::OverlappingRoots { relation });
                }
            }
        }
        // La cola del scheduler es la de la PRIMERA raíz. Una selección
        // mezclada de providers tiene que encolarse en algún sitio, y elegir
        // otro no cambiaría nada.
        let key = primera.scheme().to_owned();
        let mut roots = Vec::with_capacity(params.paths.len());
        for p in params.paths {
            let provider = self.provider_for(&p).await?;
            roots.push((provider, p));
        }
        let handle = self.sched.submit(
            &key,
            TaskKind::DirSize,
            Priority::Normal,
            actor,
            Box::new(move |ctx| Box::pin(async move { crate::ops::dir_size(roots, &ctx).await })),
        );
        Ok(handle)
    }

    /// El digest del contenido de un lote de ficheros, como Task cancelable
    /// (`fs.checksum`, 0.59.0, #311).
    ///
    /// **No muta nada**: leer no es escribir, así que la regla 4 no aplica —
    /// sin journal y sin undo. El gate de LECTURA vive en el daemon, que es
    /// quien ata una conexión a un actor, igual que en `fs.dir_size`.
    ///
    /// Los digests NO viajan en el retorno: se recogen con
    /// [`Self::checksum_report`], que es el único camino de lectura y el mismo
    /// reparto que `archive.pack`. Devolver además el `Arc` sería filtrar un
    /// asa a la API pública para comodidad de un test.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] con la lista vacía —resumir la nada no es una
    /// petición— o por encima de
    /// [`FS_CHECKSUM_MAX_PATHS`](norte_proto::methods::FS_CHECKSUM_MAX_PATHS),
    /// que se RECHAZA en vez de recortar: un informe recortado en silencio se
    /// lee como «todo comprobado» sobre ficheros que nadie miró.
    /// [`Error::Unsupported`] si algún scheme no tiene provider.
    ///
    /// # Panics
    /// Si el lock del anillo de informes está envenenado, que es un pánico
    /// previo de este mismo proceso — mismo criterio que `archive.pack`: el
    /// camino de ESCRITURA del anillo no sigue con un estado del que no se sabe
    /// nada. El de lectura ([`Self::checksum_report`]) sí lo tolera.
    pub async fn checksum_as(
        &self,
        params: norte_proto::methods::FsChecksumParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        // ANTES de crear Task alguna: es un rechazo del REQUEST, no el fallo de
        // una Task ya lanzada (mismo criterio que `dir_size_as`).
        let Some(primera) = params.paths.first() else {
            tracing::debug!("fs.checksum sin rutas");
            return Err(Error::InvalidPath);
        };
        if params.paths.len() > norte_proto::methods::FS_CHECKSUM_MAX_PATHS {
            tracing::debug!(n = params.paths.len(), "fs.checksum por encima del tope");
            return Err(Error::InvalidPath);
        }
        // La cola del scheduler es la de la PRIMERA ruta, como en `dir_size_as`:
        // una selección de varios providers tiene que encolarse en algún sitio.
        let key = primera.scheme().to_owned();
        let mut rutas = Vec::with_capacity(params.paths.len());
        for p in params.paths {
            let provider = self.provider_for(&p).await?;
            rutas.push((provider, p));
        }
        // El informe nace sabiendo con QUÉ se está calculando: se puede pedir
        // sin haber mandado la petición (`task.list` enseña las de otros), y un
        // lector que asumiera sha256 por omisión pintaría digests de otra cosa.
        let informe = Arc::new(std::sync::Mutex::new(
            norte_proto::methods::FsChecksumReportResult {
                algo: params.algo,
                ..Default::default()
            },
        ));
        let owner = actor.clone();
        let vivo = Arc::clone(&informe);
        let handle = self.sched.submit(
            &key,
            TaskKind::Checksum,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move { crate::ops::checksum(rutas, vivo, &ctx).await })
            }),
        );
        {
            let mut ring = self
                .checksum_reports
                .lock()
                .expect("checksum_reports lock sano");
            ring.push_back((handle.id(), owner, informe));
            evict_checksum_reports(&mut ring);
        }
        Ok(handle)
    }

    /// Informe de un `fs.checksum` ya lanzado, por `task_id`, más el ACTOR que
    /// lo pidió (#311). `None` si ese id nunca fue un lote de sumas de esta
    /// instancia o si el anillo ya lo desalojó.
    ///
    /// Es un SNAPSHOT: definitivo cuando la Task es terminal, parcial antes —
    /// que es justo lo que hace útil pedirlo mientras corre. El actor sale con
    /// él porque quien sirve esto por el wire tiene que decidir si el que
    /// pregunta podía ver esa task.
    ///
    /// Este camino NO panica ante un lock envenenado: es de LECTURA, igual que
    /// sus gemelos.
    #[must_use]
    pub fn checksum_report(
        &self,
        task_id: TaskId,
    ) -> Option<(
        crate::journal::Actor,
        norte_proto::methods::FsChecksumReportResult,
    )> {
        let ring = self
            .checksum_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// De qué está hecho un directorio, hijo a hijo, como Task cancelable
    /// (`fs.dir_usage`, 0.75.0, fase 4).
    ///
    /// **No muta nada**: medir no es escribir, así que la regla 4 no aplica —
    /// sin journal y sin undo. El gate de LECTURA vive en el daemon, que es
    /// quien ata una conexión a un actor, igual que en `fs.dir_size`.
    ///
    /// Los hijos NO viajan en el retorno: se recogen con
    /// [`Self::dir_usage_report`], que es el único camino de lectura. Es la
    /// diferencia con `fs.dir_size`, cuyo total cabía en el progreso porque era
    /// un número; una LISTA no cabe ahí.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] con `depth` en cero —describir cero niveles no es
    /// una petición— o por encima de
    /// [`DIR_USAGE_MAX_DEPTH`](norte_proto::methods::DIR_USAGE_MAX_DEPTH).
    /// [`Error::Unsupported`] con cualquier `depth` mayor que uno: hoy se sirve
    /// un nivel, y **se RECHAZA en vez de recortar** — un servidor que recorta
    /// en silencio deja al cliente creyendo que tiene los dos niveles que pidió.
    /// [`Error::Unsupported`] también si el scheme no tiene provider.
    ///
    /// # Panics
    /// Si el lock del anillo de informes está envenenado, que es un pánico
    /// previo de este mismo proceso — mismo criterio que sus gemelos: el camino
    /// de ESCRITURA del anillo no sigue con un estado del que no se sabe nada.
    /// El de lectura ([`Self::dir_usage_report`]) sí lo tolera.
    pub async fn dir_usage_as(
        &self,
        params: norte_proto::methods::FsDirUsageParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        // ANTES de crear Task alguna: es un rechazo del REQUEST, no el fallo de
        // una Task ya lanzada (mismo criterio que `checksum_as`).
        if params.depth == 0 || params.depth > norte_proto::methods::DIR_USAGE_MAX_DEPTH {
            tracing::debug!(
                depth = params.depth,
                "fs.dir_usage con profundidad fuera de rango"
            );
            return Err(Error::InvalidPath);
        }
        if params.depth > 1 {
            tracing::debug!(
                depth = params.depth,
                "fs.dir_usage: hoy solo se sirve un nivel"
            );
            return Err(Error::Unsupported);
        }
        let key = params.path.scheme().to_owned();
        let provider = self.provider_for(&params.path).await?;
        let informe = Arc::new(std::sync::Mutex::new(
            norte_proto::methods::FsDirUsageReportResult::default(),
        ));
        let owner = actor.clone();
        let vivo = Arc::clone(&informe);
        let root = params.path;
        let handle = self.sched.submit(
            &key,
            TaskKind::DirUsage,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move { crate::ops::dir_usage(provider, root, vivo, &ctx).await })
            }),
        );
        {
            let mut ring = self
                .dir_usage_reports
                .lock()
                .expect("dir_usage_reports lock sano");
            ring.push_back((handle.id(), owner, informe));
            evict_dir_usage_reports(&mut ring);
        }
        Ok(handle)
    }

    /// El mapa que lleva medido un `fs.dir_usage` ya lanzado, por `task_id`, más
    /// el ACTOR que lo pidió (fase 4). `None` si ese id nunca fue un mapa de
    /// esta instancia o si el anillo ya lo desalojó.
    ///
    /// Es un SNAPSHOT: definitivo cuando la Task es terminal, parcial antes —
    /// que es justo lo que hace útil pedirlo mientras corre, porque un mapa se
    /// puede ir pintando. El actor sale con él porque quien sirve esto por el
    /// wire tiene que decidir si el que pregunta podía ver esa task.
    ///
    /// Este camino NO panica ante un lock envenenado: es de LECTURA, igual que
    /// sus gemelos.
    #[must_use]
    pub fn dir_usage_report(
        &self,
        task_id: TaskId,
    ) -> Option<(
        crate::journal::Actor,
        norte_proto::methods::FsDirUsageReportResult,
    )> {
        let ring = self
            .dir_usage_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// Planifica una sincronización de UN sentido como Task cancelable
    /// (`sync.plan`, 0.40.0, ADR 0049): [`TaskKind::SyncPlan`] + canal de
    /// [`SyncPlanEvent`](crate::sync::SyncPlanEvent) — lotes de pasos acotados
    /// por [`SYNC_STEPS_MAX_BATCH`](norte_proto::methods::SYNC_STEPS_MAX_BATCH)
    /// y, al final, UN
    /// [`SyncPlanDone`](norte_proto::methods::SyncPlanDone). Un solo canal para
    /// las dos cosas: el orden «los pasos, y después el cierre» es contrato, y
    /// una cola FIFO lo garantiza sin que nadie tenga que reordenar.
    ///
    /// **No muta nada**: planificar es la comparación de [`Self::compare_as`]
    /// con una decisión por fila. Quien escribe es `sync.apply` (regla dura 4 no
    /// aplica aquí; la explicación larga está en el módulo `sync`).
    ///
    /// **Retiene**, en cambio: el plan se escribe en el spool según se planifica
    /// y queda atado a `conn_id`, que es lo que hace que `sync.apply` no lleve
    /// más que un hash. Sin spool instalado ([`Self::set_spool`]) esto es
    /// [`Error::Unsupported`].
    ///
    /// **Gate de policy**: NINGUNO aquí, y por el mismo motivo que
    /// [`Self::compare_as`] — el gate de LECTURA vive en el daemon (`read_gate`
    /// sobre AMBAS raíces, más `content_gate` cuando el rung de hash está
    /// encendido), que es quien ata una conexión a un actor.
    ///
    /// # Las dos raíces no se pueden solapar, y se comprueba DOS veces
    /// Copiar `/a` sobre `/a/sub` copia un árbol dentro de sí mismo.
    /// [`FS_COMPARE`](norte_proto::methods::FS_COMPARE) sí admite ese par
    /// —comparar cuesta un walk y no escribe un byte—; planificar escrituras
    /// dentro del propio origen no tiene esa licencia.
    ///
    /// 1. **Estructural**: scheme, authority y segmentos, literales. Coge las
    ///    dos raíces iguales y una dentro de la otra.
    /// 2. **Identidad real**: [`Provider::node_id`] de las dos raíces,
    ///    resolviendo enlaces. Dos rutas distintas que nombran el mismo
    ///    directorio —`/data` que es un symlink a `/srv/data`— responden con el
    ///    mismo id, y eso es [`RootOverlap::Same`](norte_proto::RootOverlap::Same). Es lo
    ///    que la comprobación
    ///    estructural no puede ver y lo que el guard del walk tampoco: las filas
    ///    de un walk sobre `/data` cuelgan todas de `/data` y jamás «alcanzan»
    ///    la otra raíz.
    /// 3. **Contención PLEGADA**, cuando alguno de los dos providers no declara
    ///    `CASE_SENSITIVE`: los mismos segmentos, comparados por la clave con la
    ///    que `norte-compare` empareja nombres. Es lo que caza `source=/Data`
    ///    contra `dest=/data/backup` sobre APFS o NTFS — tres directorios
    ///    distintos para los bytes y uno dentro de otro para el volumen.
    ///
    /// Lo que sigue SIN cubrir, dicho aquí en vez de prometido de más:
    ///
    /// - **La contención plegada en un volumen que distingue caja pero pliega
    ///   otra cosa.** ext4 con `+F` pliega y ext4 sin él no, y `Capabilities` no
    ///   lo distingue por directorio; y el plegado de #145 (`ß`→`ss`) expande, o
    ///   sea que no lo cubre ninguna de las dos tablas.
    /// - **Un mismo host bajo dos autoridades**: `node_id` es `None` en SFTP y
    ///   en FTP, así que ahí no hay identidad que comparar.
    /// - **Dos providers distintos**: solo se comparan ids del MISMO objeto
    ///   provider, porque el [`NodeId`](norte_vfs::NodeId) de dos backends no es
    ///   comparable (el índice de un provider sintético y un inodo de ext4
    ///   pueden coincidir sin tener nada que ver).
    ///
    /// Lo que queda lo acotan el guard del walk (`OverlapDetected` poda el
    /// subárbol en cuanto una fila alcanza la otra raíz) y la revalidación por
    /// paso del ejecutor.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay spool instalado, si el scheme de alguna
    /// raíz no tiene provider, o si el llamante mandó
    /// `compare.follow_symlinks`/`compare.descend_orphans` — que en `sync.plan`
    /// **no son suyos**: el planificador fija el segundo al lado del ORIGEN, y
    /// servir en silencio un recorrido distinto del pedido es peor que no
    /// ofrecerlo. [`Error::InvalidPath`] si `include` pasa de
    /// [`SYNC_MAX_INCLUDE`](norte_proto::methods::SYNC_MAX_INCLUDE) (se rehúsa,
    /// jamás se recorta: una lista acortada en silencio sincroniza algo que
    /// nadie pidió). [`Error::OverlappingRoots`] si las raíces se solapan.
    pub async fn sync_plan_as(
        &self,
        params: norte_proto::methods::SyncPlanParams,
        conn_id: u64,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            tokio::sync::mpsc::Receiver<crate::sync::SyncPlanEvent>,
        ),
        Error,
    > {
        use norte_proto::methods::{SYNC_MAX_INCLUDE, Side};

        // Sin retención no hay plan que aprobar: fail-closed ANTES de tocar un
        // provider.
        let spool = self.spool().ok_or(Error::Unsupported)?;
        // Rechazos del REQUEST, antes de resolver providers y de crear Task
        // alguna (mismo criterio que `compare_as`). Van aquí y no solo en el
        // daemon porque el brazo EMBEBIDO llama a este método sin pasar por él.
        if params.compare.follow_symlinks {
            tracing::debug!("sync.plan con follow_symlinks: no soportado");
            return Err(Error::Unsupported);
        }
        if params.compare.descend_orphans.is_some() {
            tracing::debug!("sync.plan con descend_orphans: no es del llamante");
            return Err(Error::Unsupported);
        }
        if params
            .include
            .as_ref()
            .is_some_and(|inc| inc.len() > SYNC_MAX_INCLUDE)
        {
            tracing::debug!("sync.plan: include por encima del tope");
            return Err(Error::InvalidPath);
        }
        if let Some(relation) = structural_overlap(&params.source, &params.dest) {
            tracing::debug!(?relation, "sync.plan con raíces solapadas");
            return Err(Error::OverlappingRoots { relation });
        }

        let source = self.provider_for(&params.source).await?;
        let dest = self.provider_for(&params.dest).await?;
        if same_node(&source, &params.source, &dest, &params.dest).await {
            tracing::debug!("sync.plan: las dos raíces son el mismo directorio");
            return Err(Error::OverlappingRoots {
                relation: norte_proto::RootOverlap::Same,
            });
        }
        // 3ª puerta: la contención que ve un volumen que PLIEGA. Se le pregunta
        // a CADA RAÍZ y no al provider (ADR 0054): las dos pueden estar en
        // mounts distintos del mismo `file://`, y es el mount que no distingue
        // caja —o el que además EXPANDE, un ext4 en `+F`— el que decide si
        // estas dos raíces se solapan.
        let (source_caps, dest_caps) = tokio::join!(
            source.capabilities_at(&params.source),
            dest.capabilities_at(&params.dest)
        );
        // Una raíz que no sabe responder NO tumba la planificación: se declara
        // lo del provider, igual que en `compare::probed_sides`, y la raíz
        // sigue fallando donde tiene que fallar —su propio listado, con su fila
        // de error—. Planificar hacia un destino que todavía no existe es el
        // caso corriente de un mirror, y tumbarlo aquí sería un método del wire
        // que empieza a fallar donde antes respondía.
        let caps = crate::compare::degradada(dest_caps, dest.as_ref(), &params.dest);
        let sides = norte_compare::Sides::from_capabilities(
            crate::compare::degradada(source_caps, source.as_ref(), &params.source),
            caps,
        );
        if sides.folds_case()
            && let Some(relation) = folded_overlap(&params.source, &params.dest, sides)
        {
            tracing::debug!(?relation, "sync.plan con raíces solapadas al plegar");
            return Err(Error::OverlappingRoots { relation });
        }
        let opts = norte_sync::SyncOptions {
            source_root: params.source.clone(),
            dest_root: params.dest,
            mode: params.mode,
            on_unknown: params.on_unknown,
            // El frontend ya tradujo la dirección: aquí el origen es el lado
            // IZQUIERDO de la comparación por construcción, y `SyncPlanParams`
            // no lleva ningún `Side` con el que pudiera contradecirlo.
            source_side: Side::Left,
            dest_has_trash: caps.flags.contains(norte_proto::CapabilityFlags::TRASH),
            // Lo que el provider PROMETE sobre su papelera, no lo que se
            // supone de ella: una que no nombra su destino deja al undo sin
            // `reversal_ref`, y el plan tiene que marcarlo IRREVERSIBLE antes
            // de que nadie apruebe nada (regla dura 4). Sale del mismo objeto
            // provider del que salen las capabilities, después de la operación
            // async que forzó el sondeo perezoso.
            dest_trash_restorable: dest.trash_restorable(),
            dest_writable: !caps.flags.contains(norte_proto::CapabilityFlags::READ_ONLY),
        };
        let job = crate::sync::SyncPlanJob {
            source,
            dest,
            opts,
            compare: params.compare,
            include: params.include,
            spool,
            conn_id,
        };
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        // La cola del scheduler es la del ORIGEN, por lo mismo que en
        // `compare_as`: hay que encolar en algún sitio y el otro lado no cambia
        // nada.
        let key = params.source.scheme().to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::SyncPlan,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move { crate::sync::run_sync_plan(job, tx, &ctx).await })
            }),
        );
        Ok((handle, rx))
    }

    /// Ejecuta un plan APROBADO (`sync.apply`, 0.40.0, ADR 0049) como Task
    /// cancelable: [`TaskKind::Sync`] más el informe que se llena según avanza.
    ///
    /// **El único parámetro es el hash**, y de ahí sale todo lo demás. Las dos
    /// raíces, el modo y los criterios se leen del SPOOL, que es donde el plan
    /// aprobado quedó retenido y atado a `conn_id`: por la FORMA de la petición,
    /// no se puede ejecutar nada que no sea lo que un humano vio.
    ///
    /// # El gate corre sobre las raíces que salen del SPOOL, aquí y ahora
    /// El de `sync.plan` NO vale: entre planificar y aplicar pasan hasta
    /// [`SYNC_PLAN_TTL_MS`](norte_proto::methods::SYNC_PLAN_TTL_MS) milisegundos,
    /// y dentro de esa ventana un scope caduca y una regla de `policy.toml`
    /// cambia. Así que se piden aquí, con las rutas leídas del fichero:
    ///
    /// - [`PolicyOp::Copy`](crate::policy::PolicyOp::Copy) sobre las DOS raíces
    ///   —copiar es leer el origen y escribir el destino, y es exactamente lo que
    ///   [`Self::copy_with_as`] pide para copiar un árbol—,
    /// - [`PolicyOp::Mkdir`](crate::policy::PolicyOp::Mkdir) sobre el destino si
    ///   el plan crea algún directorio,
    /// - [`PolicyOp::Delete`](crate::policy::PolicyOp::Delete) sobre el destino
    ///   si el plan sobrescribe o borra, con el modo que de verdad se va a usar
    ///   (papelera o permanente, según lo que el destino declare).
    ///
    /// **Se gatea sobre las RAÍCES, no sobre cada paso**, igual que una copia
    /// recursiva de `fs.copy`: la frontera de scope de un agente es por raíz, así
    /// que un paso no puede escapar de un scope que cubra `dest_root`. Lo que sí
    /// queda fuera es una regla `deny` de `policy.toml` sobre una ruta CONCRETA
    /// de dentro del árbol — un gate por paso metería un `ask` por paso en un
    /// plan de medio millón, que no es una interfaz. Es la misma cobertura que
    /// `fs.copy` de un árbol tiene hoy.
    ///
    /// # Regla dura 4: sin journal no se aplica
    /// [`Error::Unsupported`], fail-closed como el spool. El plan promete una
    /// [`StepReversal`](norte_proto::methods::StepReversal) por paso y solo el
    /// journal la puede cumplir; aplicarlo sin él sería sobrescribir y enterrar
    /// sin dejar rastro ni vuelta atrás.
    ///
    /// La tercera pata NO es fail-closed y conviene no suponerlo: el gate por
    /// defecto de un `Engine` es [`AllowAll`](crate::policy::AllowAll), así que
    /// un daemon montado sobre un engine SIN `with_policy` deja que cualquier
    /// actor reescriba un árbol entero con una sola llamada. Es la misma puerta
    /// abierta que `fs.copy` y `fs.delete` tienen desde M3 —y ningún binario de
    /// este árbol la deja así— pero aquí el radio es otro.
    ///
    /// # El plan se gasta, pase lo que pase
    /// Al terminar la Task —completada, fallida o cancelada— el spool se borra.
    /// Mientras no se borre, ese hash cuenta como «aplicándose» y replanificar el
    /// mismo árbol con las mismas opciones (que da el mismo digest) se rehúsa.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] si el journal de esta sesión no se puede
    /// ABRIR (#178: ilegible ≠ ocupado — con el fichero simplemente ocupado se
    /// contesta `Unsupported`, que es lo de siempre).
    /// [`Error::Unsupported`] sin spool o sin journal, o si el scheme de alguna
    /// raíz no tiene provider; [`Error::PlanStale`] si el hash no nombra un plan
    /// vivo de ESTA conexión (no existe, caducó, está manipulado, o ya se está
    /// aplicando); [`Error::PlanNotExecutable`] si el plan traía bloqueos;
    /// [`Error::PolicyDenied`] si el gate deniega; [`Error::Io`] si el spool no
    /// se deja leer por un fallo del daemon.
    ///
    /// # Panics
    /// Solo por envenenamiento del `Mutex` del informe (otro hilo entró en
    /// pánico sosteniéndolo) — irrecuperable, mismo criterio que el resto del
    /// core.
    #[tracing::instrument(skip(self, actor), fields(conn_id = conn_id, plan_hash = plan_hash.as_str()))]
    pub async fn sync_apply_as(
        &self,
        plan_hash: &PlanHash,
        conn_id: u64,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<norte_proto::methods::SyncReportResult>>,
        ),
        Error,
    > {
        let spool = self.spool().ok_or(Error::Unsupported)?;
        // Un journal ILEGIBLE se dice con su categoría (#178) y no como
        // «no soportado»: hay un fichero concreto que arreglar y el usuario
        // tiene derecho a que se lo digan. Va antes del `ok_or` porque los dos
        // caminos acaban sin journal y solo uno de ellos es accionable.
        //
        // Este es el ÚNICO sitio donde el journal se pide antes que la policy,
        // al revés de lo que dice el rustdoc de `gate`, y hace falta: el lote
        // (`alloc_batch`) y el `BatchJournal` se arman abajo con este handle, y
        // el plan es de un solo uso — descubrir aquí que no hay journal después
        // de haber gastado la aprobación dejaría al humano sin plan y sin
        // sincronización.
        //
        // Lo que ese orden costaría —tener el lock exclusivo tomado mientras un
        // `Ask` de policy espera a un humano— no puede ocurrir, y no por
        // suerte: el único engine con journal PEREZOSO es el embebido, y un
        // `Backend::Embedded` contesta `Unsupported` a `policy_decide`, así que
        // no hay nadie que pueda aprobar y nada que suspender. El del daemon
        // tiene `JournalSource::Open`: ya está abierto desde el arranque y
        // `journal_gate` lo deja pasar sin tocar el fichero.
        self.journal_gate().await?;
        let journal = self.journal().await.ok_or_else(|| {
            tracing::warn!("sync.apply sin journal: no hay lote que deshacer, no se aplica");
            Error::Unsupported
        })?;
        // `open` se lleva el DERECHO a aplicar este plan (es de un solo uso), así
        // que a partir de aquí toda salida tiene que devolverlo con `remove`: si
        // no, ese hash se queda «aplicándose» y no se puede replanificar.
        let reader = spool
            .open(conn_id, plan_hash)
            .await
            .map_err(|e| spool_open_error(&e))?;
        // Y si este future se DROPEA antes de volver, el derecho se suelta
        // igual. Pasa de verdad: el gate de abajo puede quedarse suspendido en
        // un `ask` de policy durante un minuto, y el daemon retira ese despacho
        // con un `rpc.cancel`. Sin esto, dropear ahí dejaría el hash
        // «aplicándose» para siempre — ni aplicable ni replanificable, y
        // diagnosticado como error interno.
        let mut claim = ApplyClaim {
            spool: &spool,
            conn_id,
            plan_hash,
            armed: true,
        };
        let outcome = self
            .sync_apply_opened(reader, plan_hash, conn_id, actor, &spool, &journal)
            .await;
        // Se volvió: a partir de aquí mandan los caminos de siempre — el cuerpo
        // de la Task gasta el plan al terminar, y un error lo gasta aquí.
        claim.armed = false;
        if outcome.is_err() {
            let _ = spool.remove(conn_id, plan_hash).await;
        }
        outcome
    }

    /// La parte de [`Self::sync_apply_as`] que corre con el plan ya abierto.
    ///
    /// Está separada para que TODA salida de error devuelva el derecho a aplicar
    /// (el `remove` del llamante): con un solo cuerpo habría que acordarse en
    /// cada `?`, que es exactamente la clase de cosa que se olvida.
    async fn sync_apply_opened(
        &self,
        reader: crate::sync::SpoolReader,
        plan_hash: &PlanHash,
        conn_id: u64,
        actor: crate::journal::Actor,
        spool: &crate::sync::Spool,
        journal: &Arc<crate::journal::SqliteJournal>,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<norte_proto::methods::SyncReportResult>>,
        ),
        Error,
    > {
        use crate::policy::PolicyOp;
        use norte_proto::DeleteMode;
        use norte_proto::methods::DestTrash;

        // Un plan con bloqueos no se ejecuta aunque su hash case: el hash dice
        // «este es el plan que se te enseñó», jamás «este plan se puede
        // ejecutar» (ver la nota del módulo `hash` de `norte-sync`).
        if !reader.summary().executable {
            return Err(Error::PlanNotExecutable);
        }
        let counts = reader.summary().counts;
        let source_root = reader.header().options.source_root.clone();
        let dest_root = reader.header().options.dest_root.clone();
        let trash = plan_dest_trash(&reader);

        self.gate(&actor, PolicyOp::Copy, &[&source_root, &dest_root])
            .await?;
        if counts.create_dir > 0 {
            self.gate(&actor, PolicyOp::Mkdir, &[&dest_root]).await?;
        }
        // La clase de borrado que de verdad va a ocurrir: la misma de la que sale
        // la reversa de cada paso, así que el gate pregunta por lo que pasa.
        // `Absent` es exactamente «el destino no tiene papelera» — ver
        // [`plan_dest_trash`] —, así que el gate sigue preguntando por lo mismo
        // que antes de que este valor existiera.
        let mode = if trash == DestTrash::Absent {
            DeleteMode::Permanent
        } else {
            DeleteMode::Trash
        };
        if counts.overwrite > 0 || counts.delete_tree > 0 {
            self.gate(&actor, PolicyOp::Delete { mode }, &[&dest_root])
                .await?;
        }

        let source = self.provider_for(&source_root).await?;
        let dest = self.provider_for(&dest_root).await?;
        // El lote se reserva DESPUÉS del gate: un plan denegado no consume id.
        let batch_id = journal.journal().alloc_batch().await.map_err(Error::from)?;
        let recorder = crate::sync::exec::BatchJournal {
            journal: Arc::clone(journal),
            actor: actor.clone(),
            batch_id,
        };
        let targets = crate::sync::exec::SyncTargets {
            source,
            dest,
            source_root,
            dest_root,
            // La MISMA policy que acaba de gatear las raíces, para volver a
            // preguntarle paso a paso: el gate de la raíz resuelve la frontera de
            // scope, pero una regla `deny` de `policy.toml` sobre una ruta de
            // dentro del árbol solo se ve preguntando por esa ruta.
            policy: Arc::clone(&self.policy),
            delete_mode: mode,
            // Se abre dentro de la Task, que es donde hay `task_id` con el que
            // decir en el log que este destino no sabe confinarse.
            dest_confined: None,
        };
        let report = Arc::new(std::sync::Mutex::new(crate::sync::exec::new_report(
            batch_id, trash,
        )));
        let report_task = Arc::clone(&report);
        let spool_task = spool.clone();
        let hash_task = plan_hash.clone();
        // Los pasos del plan, más lo que el diálogo ya sabía: el total de pasos
        // y de bytes, para que la barra tenga denominador desde el primer
        // instante. `current` NO se toca, por lo mismo que en `sync.plan`:
        // llevaría un `VPath` a un broadcast que ven todas las conexiones
        // humanas, y el gate de esta Task es por RAÍZ.
        let total = counts
            .create_dir
            .saturating_add(counts.copy)
            .saturating_add(counts.overwrite)
            .saturating_add(counts.delete_tree)
            .saturating_add(counts.skip);
        let key = targets.dest_root.scheme().to_owned();
        // El DUEÑO, para el anillo de abajo: el daemon decide con él quién puede
        // leer este informe, y `submit` se lleva el `actor` por valor.
        let owner = actor.clone();
        let handle = self.sched.submit(
            &key,
            TaskKind::Sync,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    // El denominador se puede SUPERAR, y hay que saberlo: los
                    // bytes del plan son una cota inferior (un provider que
                    // lista sin tamaño no aporta ninguno — sobre `file://` es el
                    // caso normal) mientras que los del informe se cuentan al
                    // escribirlos. `TaskProgress::bytes_total` es «estimado» por
                    // contrato, así que esto está dentro de lo prometido, pero
                    // una barra que divida sin acotar pasará del 100%.
                    ctx.progress.update(|p| {
                        p.entries_total = Some(total);
                        p.bytes_total = Some(counts.bytes);
                    });
                    let steps = futures::StreamExt::map(reader.steps(), |item| {
                        item.map_err(|e| spool_read_error(&e))
                    });
                    // El `catch_unwind` no es paranoia: el scheduler ya envuelve
                    // el cuerpo entero en uno, así que un pánico de un provider
                    // se llevaría por delante el `remove` de abajo y dejaría ese
                    // hash marcado «aplicándose» PARA SIEMPRE — replanificar el
                    // mismo árbol da el mismo digest y se rehusaría durante toda
                    // la vida de la conexión. Aquí el pánico se recoge, el plan
                    // se gasta, y después se contesta lo que el scheduler habría
                    // contestado.
                    let out = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(crate::sync::exec::run(
                        targets,
                        &recorder,
                        steps,
                        &ctx,
                        &report_task,
                    )))
                    .await;
                    // El plan se gasta en CUALQUIER estado terminal. Mientras no
                    // se borre, su hash cuenta como «aplicándose» y el mismo
                    // árbol no se puede replanificar.
                    if let Err(e) = spool_task.remove(conn_id, &hash_task).await {
                        tracing::warn!(error = %e, "sync.apply: el plan aplicado no se pudo borrar");
                    }
                    match out {
                        Err(_) => {
                            tracing::error!("sync.apply: pánico en el ejecutor");
                            Err(Error::Internal { panic: true })
                        }
                        // `into_wire` y no un `?`: la conversión es LOSSY (el
                        // estado de #160 no tiene categoría en la taxonomía) y
                        // es ella la que lo deja dicho en el log antes de
                        // perderlo.
                        Ok(r) => r.map_err(crate::sync::exec::ApplyError::into_wire),
                    }
                })
            }),
        );
        // Retiene el informe para quien solo tiene el `task_id`: el socket
        // (`sync.report`) y el `Backend` embebido. El llamante directo ya se
        // lleva el `Arc` VIVO en la mano; esto es para los otros dos, que solo
        // pueden pedirlo después. Anillo acotado, igual que el de renames.
        {
            let mut ring = self.sync_reports.lock().expect("sync_reports lock sano");
            ring.push_back((handle.id(), owner, Arc::clone(&report)));
            evict_sync_reports(&mut ring);
        }
        Ok((handle, report))
    }

    /// Fabrica un archivo como Task cancelable (`archive.pack`, 0.50.0, #132).
    ///
    /// **No escribe dentro de ningún contenedor**: el provider de archivos
    /// sigue siendo `READ_ONLY` (ADR 0018). Lee las fuentes por su provider y
    /// escribe UN fichero nuevo por el del destino, que puede ser otro. Muta,
    /// así que va al journal (regla 4) como una creación, y deshacerlo es
    /// borrar el archivo.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] sin fuentes, o si alguna no cuelga de `base` —el
    /// nombre guardado se calcula contra ella, y sin nombre no hay entrada—, y
    /// lo que devuelva la resolución de providers.
    ///
    /// # Panics
    ///
    /// Si el mutex del anillo de informes está envenenado, que solo pasa si
    /// otro hilo panicó teniéndolo. Mismo trato que sus tres gemelos en el
    /// camino de ESCRITURA del anillo: aquí sí se panica, porque un anillo a
    /// medias de actualizar no es un informe rancio sino una entrada que nadie
    /// va a poder leer nunca.
    pub async fn pack_as(
        &self,
        params: norte_proto::methods::ArchivePackParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        if params.sources.is_empty() {
            return Err(Error::InvalidPath);
        }
        // Gate de MUTACIÓN (regla 9 y ADR 0025): empaquetar LEE las fuentes y
        // ESCRIBE el destino, que es exactamente lo que hace una copia, así
        // que se pregunta por lo mismo y con la misma op. Antes de resolver
        // providers: una petición denegada no abre conexiones.
        let mut refs: Vec<&VPath> = params.sources.iter().collect();
        refs.push(&params.dest);
        self.gate(&actor, crate::policy::PolicyOp::Copy, &refs)
            .await?;
        let provider_destino = self.provider_for(&params.dest).await?;
        let mut fuentes = Vec::with_capacity(params.sources.len());
        for p in params.sources {
            let provider = self.provider_for(&p).await?;
            fuentes.push((provider, p));
        }
        // La cola es la del DESTINO: es el único provider por el que esta Task
        // escribe, y encolar por el origen mezclaría escrituras de un mismo
        // destino en colas distintas.
        let key = params.dest.scheme().to_owned();
        let observer = Arc::clone(&self.observer);
        let dest = params.dest;
        let base = params.base;
        let format = params.format;
        let level = params.level;
        let informe = Arc::new(std::sync::Mutex::new(
            norte_proto::methods::ArchivePackReportResult::default(),
        ));
        let owner = actor.clone();
        let vivo = Arc::clone(&informe);
        let handle = self.sched.submit(
            &key,
            TaskKind::Pack,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::pack::pack(
                        fuentes,
                        crate::pack::Destino {
                            provider: provider_destino,
                            dest,
                        },
                        crate::pack::Empaquetado {
                            base,
                            format,
                            level,
                        },
                        observer,
                        vivo,
                        &ctx,
                    )
                    .await
                })
            }),
        );
        {
            let mut ring = self.pack_reports.lock().expect("pack_reports lock sano");
            ring.push_back((handle.id(), owner, informe));
            evict_pack_reports(&mut ring);
        }
        // El informe NO viaja en el retorno, al revés que en `test_archive_as`:
        // aquí el único camino de lectura es [`Self::archive_pack_report`], y
        // devolver además el `Arc<Mutex<…>>` era filtrar un asa a la API
        // pública para comodidad de un test.
        Ok(handle)
    }

    /// Informe de un `archive.pack` ya lanzado, por `task_id`, más el ACTOR que
    /// lo pidió (#250). `None` si ese id nunca fue un empaquetado de esta
    /// instancia o si el anillo ya lo desalojó.
    ///
    /// Snapshot, como sus tres gemelos, y se sirve también antes del terminal:
    /// el informe se calcula sobre la lista de entradas ANTES de escribir, así
    /// que ya es definitivo cuando el archivo aún se está escribiendo — y lo
    /// que dice sigue siendo verdad de un archivo cancelado a medias.
    #[must_use]
    pub fn archive_pack_report(
        &self,
        task_id: TaskId,
    ) -> Option<(
        crate::journal::Actor,
        norte_proto::methods::ArchivePackReportResult,
    )> {
        let ring = self
            .pack_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// Comprueba un archivo como Task cancelable (`archive.test`, 0.50.0,
    /// #132). Devuelve el handle y el informe VIVO.
    ///
    /// No muta: sin journal, sin undo, ni un byte escrito. Lo que comprueba lo
    /// hace el LECTOR del formato —el de zip verifica el CRC de cada entrada
    /// que se lee entera—, así que esto recorre y recoge en vez de tener una
    /// segunda opinión sobre la misma integridad.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] si el nombre no corresponde a ningún formato de
    /// [`ARCHIVE_FORMATS`](norte_proto::ARCHIVE_FORMATS) —comprobar «un
    /// fichero cualquiera» no significa nada— y lo que devuelva la resolución
    /// de providers.
    ///
    /// # Panics
    ///
    /// Si el mutex del anillo de informes está envenenado, que solo pasa si
    /// otro hilo panicó teniéndolo. Es el mismo trato que sus dos gemelos en
    /// el camino de ESCRITURA del anillo: aquí sí se panica, porque un anillo
    /// a medias de actualizar no es un informe rancio, es una entrada que
    /// nadie va a poder leer nunca.
    pub async fn test_archive_as(
        &self,
        params: norte_proto::methods::ArchiveTestParams,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<norte_proto::methods::ArchiveTestResult>>,
        ),
        Error,
    > {
        let nombre = params
            .path
            .file_name()
            .map(|s| s.as_bytes().to_vec())
            .ok_or(Error::InvalidPath)?;
        let token = crate::pack::formato_de_nombre(&nombre).ok_or(Error::Unsupported)?;
        let raiz = norte_proto::VPath::archive_compose(token, &params.path, &[])
            .map_err(|_| Error::InvalidPath)?;
        let provider = self.provider_for(&raiz).await?;
        let checked = crate::pack::que_se_comprueba(token);
        let informe = Arc::new(std::sync::Mutex::new(
            norte_proto::methods::ArchiveTestResult::default(),
        ));
        let key = params.path.scheme().to_owned();
        let owner = actor.clone();
        let vivo = Arc::clone(&informe);
        let handle = self.sched.submit(
            &key,
            TaskKind::TestArchive,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::pack::test_archive(provider, raiz, checked, vivo, &ctx).await
                })
            }),
        );
        {
            let mut ring = self.test_reports.lock().expect("test_reports lock sano");
            ring.push_back((handle.id(), owner, Arc::clone(&informe)));
            evict_test_reports(&mut ring);
        }
        Ok((handle, informe))
    }

    /// Informe de un `archive.test` ya lanzado, por `task_id`, más el ACTOR que
    /// lo pidió. `None` si ese id nunca fue un test de esta instancia o si el
    /// anillo ya lo desalojó (su tope es el mismo que el de sus dos gemelos).
    ///
    /// Snapshot, como sus dos gemelos, y por la misma razón se sirve también
    /// antes del terminal: comprobar un archivo grande tarda, y el informe
    /// parcial es lo único que dice por dónde va.
    #[must_use]
    pub fn archive_test_report(
        &self,
        task_id: TaskId,
    ) -> Option<(
        crate::journal::Actor,
        norte_proto::methods::ArchiveTestResult,
    )> {
        let ring = self
            .test_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// Parte un fichero en trozos numerados como Task cancelable
    /// (`file.split`, 0.50.0, #132). Muta: una creación por trozo en el
    /// journal.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] con un trozo por debajo de
    /// [`FILE_SPLIT_MIN_BYTES`](norte_proto::methods::FILE_SPLIT_MIN_BYTES),
    /// [`Error::LimitExceeded`] si saldrían más de
    /// [`FILE_SPLIT_MAX_PARTS`](norte_proto::methods::FILE_SPLIT_MAX_PARTS)
    /// —comprobado ANTES de escribir nada—, y lo que devuelva la resolución de
    /// providers.
    pub async fn split_as(
        &self,
        params: norte_proto::methods::FileSplitParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.gate(
            &actor,
            crate::policy::PolicyOp::Copy,
            &[&params.path, &params.dest_dir],
        )
        .await?;
        let src = self.provider_for(&params.path).await?;
        // Se mide ANTES de crear la Task: un rechazo del REQUEST es un error
        // del RPC y no el fallo de algo que ya estaba corriendo.
        crate::pack::mide_el_reparto(&*src, &params.path, params.part_bytes).await?;
        let provider_destino = self.provider_for(&params.dest_dir).await?;
        let key = params.dest_dir.scheme().to_owned();
        let observer = Arc::clone(&self.observer);
        let handle = self.sched.submit(
            &key,
            TaskKind::Split,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::pack::split(
                        src,
                        params.path,
                        params.part_bytes,
                        provider_destino,
                        params.dest_dir,
                        observer,
                        &ctx,
                    )
                    .await
                })
            }),
        );
        Ok(handle)
    }

    /// Junta los trozos de un split como Task cancelable (`file.combine`,
    /// 0.50.0, #132). Muta: una creación en el journal.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] si el primero no acaba en `.NNN`,
    /// [`Error::NotFound`] si no hay ni un trozo, [`Error::Conflict`] ante un
    /// hueco o un trozo intermedio corto, y lo que devuelva la resolución de
    /// providers.
    pub async fn combine_as(
        &self,
        params: norte_proto::methods::FileCombineParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        // El gate va sobre el DIRECTORIO de los trozos, no solo sobre el
        // primero: juntar lee `.002`…`.999` como hermanos derivados por
        // convención, y `is_under` es prefijo exacto de segmentos, así que un
        // scope clavado en el fichero `…/x.bin.001` cubría el primero y ningún
        // otro. Un agente con ese scope se llevaba el conjunto entero a un
        // destino que sí podía leer.
        let dir_trozos = params.first.parent().ok_or(Error::InvalidPath)?;
        self.gate(
            &actor,
            crate::policy::PolicyOp::Copy,
            &[&params.first, &dir_trozos, &params.dest],
        )
        .await?;
        let src = self.provider_for(&params.first).await?;
        let provider_destino = self.provider_for(&params.dest).await?;
        let key = params.dest.scheme().to_owned();
        let observer = Arc::clone(&self.observer);
        let handle = self.sched.submit(
            &key,
            TaskKind::Combine,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::pack::combine(
                        src,
                        params.first,
                        provider_destino,
                        params.dest,
                        observer,
                        &ctx,
                    )
                    .await
                })
            }),
        );
        Ok(handle)
    }

    /// Informe de una aplicación de plan ya lanzada, por `task_id`, más el ACTOR
    /// que la pidió (`sync.report`, ADR 0049). `None` si ese id nunca fue una
    /// aplicación de esta instancia o si el anillo ya lo desalojó
    /// ([`SYNC_REPORTS_MAX`]).
    ///
    /// Es un SNAPSHOT: definitivo cuando la Task es terminal, parcial antes — y
    /// se sirve igual antes del terminal, porque un plan de medio millón de
    /// pasos tarda y el informe parcial es lo único que dice por dónde va.
    ///
    /// El actor sale con él por lo mismo que en
    /// [`Self::rename_batch_report`]: quien sirve esto por el wire tiene que
    /// decidir si el que pregunta podía ver esa Task, y decidirlo aquí obligaría
    /// al engine a conocer las reglas de visibilidad del daemon.
    ///
    /// Como su gemelo, este camino NO panica ante un lock envenenado: es de
    /// LECTURA y lo atraviesa cada `sync.report`.
    #[must_use]
    pub fn sync_report(
        &self,
        task_id: TaskId,
    ) -> Option<(
        crate::journal::Actor,
        norte_proto::methods::SyncReportResult,
    )> {
        let ring = self
            .sync_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// (Re)construye el índice de `root` como Task cancelable (M4, ADR 0034).
    /// Camina el provider (lectura; como `fs.search`, sin gate de mutación) y
    /// alimenta [`norte_index::Index::build`]. El `report` se rellena al
    /// completar (patrón del `undo_session`).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay índice instalado ([`Self::with_index`])
    /// o el scheme del `root` no tiene provider.
    ///
    /// # Panics
    /// Solo si el lock interno del `report` está envenenado (otro hilo hizo panic
    /// a mitad de escritura) — irrecuperable, mismo criterio que los demás locks.
    pub async fn index_build_as(
        &self,
        root: VPath,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<Option<norte_index::BuildReport>>>,
        ),
        Error,
    > {
        let index = self.index.clone().ok_or(Error::Unsupported)?;
        let provider = self.provider_for(&root).await?;
        let key = root.scheme().to_owned();
        let report = Arc::new(std::sync::Mutex::new(None));
        let report_task = Arc::clone(&report);
        let handle = self.sched.submit(
            &key,
            TaskKind::Index,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    let entries =
                        crate::index_build::walk_for_index(provider, root.clone(), &ctx).await?;
                    let r = index
                        .build(&root, entries, &ctx.cancel)
                        .await
                        .map_err(|e| {
                            tracing::warn!(error = %e, "index.build falló");
                            // BUSY/LOCKED de SQLite = transitorio → retryable
                            // (rust review MAJOR).
                            Error::Io {
                                retryable: e.is_retryable(),
                            }
                        })?;
                    *report_task.lock().expect("report lock sano") = Some(r);
                    Ok(())
                })
            }),
        );
        Ok((handle, report))
    }

    /// Task `index.embed`: embeddings de los ficheros ya indexados de `root`
    /// (M4-IA-2, ADR 0031 A3). Filtrado ANTES de leer (`denied_prefixes`,
    /// heurística de texto), prefijos acotados, skip por hash — ver el módulo
    /// privado `index_embed` (sin enlace: el gate de docs rechaza enlazar a
    /// item privado desde doc pública).
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin índice instalado ([`Self::with_index`]),
    /// sin proveedor de embeddings ([`Self::set_ai_embed_provider`]) o sin
    /// proveedor resoluble en la config; [`Error::NotFound`] sin `index.build`
    /// previo de `root` (fail-loud en la RESPUESTA, no en el join);
    /// [`Error::PolicyDenied`] si el gate de IA rechaza (IA off, local-only
    /// sobre remoto, `root` bajo un `denied_prefix`).
    ///
    /// # Panics
    /// Solo por envenenamiento de un lock interno (irrecuperable).
    #[tracing::instrument(skip(self, actor), fields(root = %span_path(&root)))]
    pub async fn index_embed_as(
        &self,
        root: VPath,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        let index = self.index.clone().ok_or(Error::Unsupported)?;
        let embedder = self
            .ai_embed
            .read()
            .expect("ai_embed lock sano")
            .clone()
            .ok_or(Error::Unsupported)?;
        // Gate PRE-contenido (spec §9): nada se lee ni sale hasta que pasa.
        // El clon de la config evita retener el lock a través de los await.
        let (model, denied) = {
            let config = self.ai_config.read().expect("ai_config lock sano").clone();
            let model = config
                .embed_provider_config()
                .ok_or(Error::Unsupported)?
                .model
                .clone();
            crate::ai::AiGate::new(&config)
                .check(crate::ai::AiOp::Embed, embedder.is_local(), &[&root])
                .map_err(|reason| ai_denied_to_error(&reason))?;
            (model, config.denied_prefixes.clone())
        };
        // Pre-check fail-loud en la RESPUESTA: sin build previo no hay
        // universo que embeber — mejor `NotFound` inmediato que una Task que
        // falla al join.
        // Por `has_files_for_embed` y no por `files_for_embed(...).is_empty()`
        // (#122): la pregunta es «¿hay universo?», y contestarla materializando
        // la lista entera de candidatos —con su `VPath::parse` por fila—
        // barría el árbol dos veces por embed, una de ellas para tirarla.
        let no_build = !index.has_files_for_embed(&root).await.map_err(|e| {
            tracing::warn!(error = %e, "index.embed: pre-check del índice falló");
            Error::Io {
                retryable: e.is_retryable(),
            }
        })?;
        if no_build {
            return Err(Error::NotFound);
        }
        let provider = self.provider_for(&root).await?;
        let key = root.scheme().to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::Embed,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::index_embed::embed_for_index(
                        provider, embedder, index, root, model, denied, &ctx,
                    )
                    .await
                })
            }),
        );
        Ok(handle)
    }

    /// Consulta el índice de `root` por `text` (M4). Lectura directa (no Task).
    ///
    /// Los hits de un subárbol protegido se CAEN para un agente o un plugin
    /// (#165): el índice lo construye normalmente el humano, así que puede
    /// contener el directorio de estado del daemon aunque un agente no pueda
    /// listarlo. El filtro va DESPUÉS del `limit` del índice, así que una
    /// consulta cuyos hits caen todos en lo protegido devuelve menos filas de
    /// las pedidas — que es exactamente lo que debe devolver.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay índice; error del índice mapeado a `Io`.
    pub async fn index_query_as(
        &self,
        root: &VPath,
        text: &str,
        limit: u32,
        actor: crate::journal::Actor,
    ) -> Result<Vec<norte_index::IndexHit>, Error> {
        let index = self.index.clone().ok_or(Error::Unsupported)?;
        let hits = index.query(root, text, limit).await.map_err(|e| {
            tracing::debug!(error = %e, "index.query falló");
            Error::Io {
                retryable: e.is_retryable(),
            }
        })?;
        Ok(drop_excluded(hits, &crate::policy::walk_exclusions(&actor)))
    }

    /// `index.search_semantic`: UNA llamada de embed para la query + barrido
    /// coseno en Rust sobre los vectores del root (`None` ⇒ todos). Sin ANN
    /// (ADR 0031: solo si un corpus real lo justifica). `k` se recorta a
    /// `[1, INDEX_SEMANTIC_MAX_K]`. Los scores son SIEMPRE finitos (cinturón
    /// anti-NaN: `serde_json` serializa `NaN` como `null` y envenenaría la
    /// respuesta entera en el cliente).
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin índice, sin proveedor de embeddings o sin
    /// proveedor resoluble en la config; [`Error::PolicyDenied`] si el gate de
    /// IA rechaza (IA off, local-only sobre remoto, `root` denegado); errores
    /// del proveedor mapeados a la taxonomía del wire; del índice a `Io`.
    ///
    /// # Panics
    /// Solo por envenenamiento de un lock interno (irrecuperable).
    #[tracing::instrument(
        skip(self, query),
        fields(root = root.map_or_else(|| "<all>".into(), span_path))
    )]
    pub async fn index_search_semantic(
        &self,
        root: Option<&VPath>,
        query: &str,
        k: u32,
    ) -> Result<Vec<(VPath, f64)>, Error> {
        let index = self.index.clone().ok_or(Error::Unsupported)?;
        let embedder = self
            .ai_embed
            .read()
            .expect("ai_embed lock sano")
            .clone()
            .ok_or(Error::Unsupported)?;
        // Gate PRE-embed (spec §9): la query no sale al proveedor hasta que
        // pasa. El clon de la config evita retener el lock a través del await.
        let model = {
            let config = self.ai_config.read().expect("ai_config lock sano").clone();
            let model = config
                .embed_provider_config()
                .ok_or(Error::Unsupported)?
                .model
                .clone();
            let paths: Vec<&VPath> = root.into_iter().collect();
            crate::ai::AiGate::new(&config)
                .check(crate::ai::AiOp::Embed, embedder.is_local(), &paths)
                .map_err(|reason| ai_denied_to_error(&reason))?;
            model
        };
        let k = crate::index_embed::clamp_k(k);
        let qvec = embedder
            .embed(&[query.to_owned()])
            .await
            .map_err(|e| ai_to_proto_error(&e))?
            .into_iter()
            .next()
            // Proveedor mentiroso (0 vectores por 1 input): mala conducta del
            // PROVEEDOR, no retryable — mismo criterio que `flush_batch`.
            .ok_or(Error::ProviderUnavailable { retryable: false })?;
        // Cinturón proveedor-basura: un vector de query vacío, con componentes
        // no finitos o de norma cero no puede puntuar nada — mejor un error
        // honesto que 0 hits en silencio. (Norma finita ⇒ componentes finitos.)
        let norm2: f32 = qvec.iter().map(|x| x * x).sum();
        if qvec.is_empty() || !norm2.is_finite() || norm2 <= 0.0 {
            return Err(Error::ProviderUnavailable { retryable: false });
        }
        let vectors = index
            .embeddings_for_root(root, &model)
            .await
            .map_err(|e| crate::index_embed::index_to_proto(&e))?;
        // Montículo acotado y norma de la query HOISTED (#122): la cuenta es
        // la misma, la memoria es O(k) en vez de O(índice), y la query deja de
        // renormalizarse una vez por fila. El orden final desempata por path,
        // así que dos ficheros con el mismo score salen siempre igual.
        let norm_q = norm2.sqrt();
        let puntuados = vectors.into_iter().filter_map(|(path, v)| {
            crate::index_embed::cosine_prenormed(&qvec, norm_q, &v).map(|s| {
                crate::index_embed::Puntuado {
                    score: f64::from(s),
                    path,
                }
            })
        });
        Ok(crate::index_embed::mejores_k(puntuados, k))
    }

    /// Copia (recursiva si es dir) como Task, con las políticas por defecto
    /// (`Fail` + `Preserve`).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si algún scheme no tiene provider registrado.
    pub async fn copy(&self, from: &VPath, to: &VPath) -> Result<TaskHandle, Error> {
        self.copy_with(from, to, TransferOptions::default()).await
    }

    /// Copia con políticas explícitas de colisión y symlinks (ADR 0005), como
    /// `User` (camino humano, sin sandbox).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si algún scheme no tiene provider registrado.
    pub async fn copy_with(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskHandle, Error> {
        self.copy_with_as(from, to, opts, crate::journal::Actor::User)
            .await
    }

    /// Copia con políticas y ACTOR explícito (camino agéntico, M3-3): gatea por
    /// policy PRE-efecto y registra el actor real en el journal.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] si el journal de esta sesión
    /// no se puede abrir (#178); [`Error::PolicyDenied`] si la policy
    /// deniega; [`Error::Unsupported`] si algún scheme no tiene provider
    /// registrado.
    pub async fn copy_with_as(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.copy_anchored(from, to, opts, actor, None).await
    }

    /// Como [`Engine::copy_with_as`], pero con el ANCLA que el cliente observó
    /// para el directorio destino cuando lo listó (#295, ADR 0073).
    ///
    /// Con ancla, la transferencia se niega a escribir si ese directorio ya no
    /// es el nodo que el humano estaba mirando cuando aprobó — que es lo único
    /// capaz de separar un `dest/sub -> /etc` plantado de antemano de un
    /// `~/copias -> /mnt/disco/copias` legítimo, porque desde dentro del core
    /// los dos se ven igual (ADR 0072).
    ///
    /// Sin ancla (`None`) hace exactamente lo de 0.53: se confina igual y esa
    /// comprobación no ocurre.
    ///
    /// # Errors
    /// Las de [`Engine::copy_with_as`], más [`Error::Conflict`] con
    /// [`ConflictKind::EscapesRoot`](norte_proto::ConflictKind::EscapesRoot) si
    /// el directorio destino ya no es el nodo anclado.
    #[tracing::instrument(skip(self, actor, dest_anchor), fields(from = %span_path(from), to = %span_path(to), anclado = dest_anchor.is_some()))]
    pub async fn copy_anchored(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
        actor: crate::journal::Actor,
        dest_anchor: Option<norte_proto::DirAnchor>,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Copy, &[from, to])
            .await?;
        let src = self.provider_for(from).await?;
        let dst = self.provider_for(to).await?;
        let observer = Arc::clone(&self.observer);
        let (from, to) = (from.clone(), to.clone());
        let key = to.scheme().to_owned();
        // A la cola o en paralelo, según lo pidiera quien la lanzó (ADR 0149).
        let lane = if opts.queued {
            crate::scheduler::Lane::Cola
        } else {
            crate::scheduler::Lane::Paralelo
        };
        Ok(self.sched.submit_en(
            lane,
            &key,
            TaskKind::Copy,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    ops::copy_task(src, dst, from, to, opts, dest_anchor, observer, &ctx).await
                })
            }),
        ))
    }

    /// Sube o baja en la COLA en serie una task que aún no empezó (ADR 0149).
    ///
    /// `false` si ya corría, si no estaba en la cola o si ya estaba en la
    /// punta hacia la que se la mueve.
    #[must_use]
    pub fn mover_en_cola(&self, id: norte_proto::TaskId, arriba: bool) -> bool {
        self.sched.mover_en_cola(id, arriba)
    }

    /// Move como Task con las políticas por defecto: rename si mismo
    /// provider; copy+delete con plan único si es cross-provider o el
    /// rename devuelve `Unsupported` (EXDEV).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si algún scheme no tiene provider registrado.
    pub async fn move_(&self, from: &VPath, to: &VPath) -> Result<TaskHandle, Error> {
        self.move_with(from, to, TransferOptions::default()).await
    }

    /// Move con políticas explícitas de colisión y symlinks (ADR 0005), como
    /// `User`.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si algún scheme no tiene provider registrado.
    pub async fn move_with(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskHandle, Error> {
        self.move_with_as(from, to, opts, crate::journal::Actor::User)
            .await
    }

    /// Move con políticas y ACTOR explícito (camino agéntico, M3-3).
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] si el journal de esta sesión
    /// no se puede abrir (#178); [`Error::PolicyDenied`] si la policy
    /// deniega; [`Error::Unsupported`] si algún scheme no tiene provider
    /// registrado.
    pub async fn move_with_as(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.move_anchored(from, to, opts, actor, None).await
    }

    /// Como [`Engine::move_with_as`], con el ancla del directorio destino
    /// (#295, ADR 0073; ver [`Engine::copy_anchored`]).
    ///
    /// Vale para los dos caminos: el de copia escribe igual que una copia, y
    /// el rename resuelve su destino por ruta una vez —con el directorio hecho
    /// enlace, deja el fichero al otro lado igual—.
    ///
    /// # Errors
    /// Las de [`Engine::move_with_as`], más [`Error::Conflict`] si el
    /// directorio destino ya no es el nodo anclado.
    #[tracing::instrument(skip(self, actor, dest_anchor), fields(from = %span_path(from), to = %span_path(to), anclado = dest_anchor.is_some()))]
    pub async fn move_anchored(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
        actor: crate::journal::Actor,
        dest_anchor: Option<norte_proto::DirAnchor>,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Move, &[from, to])
            .await?;
        let src = self.provider_for(from).await?;
        let dst = self.provider_for(to).await?;
        let observer = Arc::clone(&self.observer);
        let (from, to) = (from.clone(), to.clone());
        let key = to.scheme().to_owned();
        let lane = if opts.queued {
            crate::scheduler::Lane::Cola
        } else {
            crate::scheduler::Lane::Paralelo
        };
        Ok(self.sched.submit_en(
            lane,
            &key,
            TaskKind::Move,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    ops::move_task(src, dst, from, to, opts, dest_anchor, observer, &ctx).await
                })
            }),
        ))
    }

    /// El plan REVISABLE de un lote de renames dentro de `dir` (spec §17, ADR
    /// 0042). LEE el directorio y sus capabilities; no muta ni journaliza nada.
    ///
    /// `pairs` son `(from, to)` en BYTES crudos de nombre base (regla 1): el
    /// llamante manda INTENCIÓN. El orden, los temporales y los veredictos los
    /// decide el core, así que un cliente —que puede ser un agente— no puede
    /// colar un orden que el humano no vio.
    ///
    /// El token que devuelve ([`crate::rename::DirPlan::hash`]) va ATADO a
    /// `dir`: un hash aprobado para un directorio no vale contra otro cuyo
    /// re-plan produzca los mismos pasos.
    ///
    /// **Gate de policy: NINGUNO AQUÍ**, igual que [`Self::list`],
    /// [`Self::stat`] y [`Self::search_as`]. No hay `PolicyOp` de lectura: el
    /// gate de LECTURA vive en el daemon (`read_gate`, #80), que es quien
    /// decide si un actor puede mirar dentro de un directorio, y el de
    /// MUTACIÓN se aplica entero en [`Self::rename_batch`], que es donde hay
    /// efecto.
    ///
    /// **OBLIGACIÓN DEL DAEMON**: `fs.rename_batch_plan` **y también
    /// `fs.rename_batch`** tienen que pasar por `read_gate` como `fs.list`. Sin
    /// él, este método es un oráculo de nombres —cada nombre del directorio,
    /// más la estructura de gemelos NFC/NFD que `fs.list` ni siquiera expone—
    /// para un agente sin scope, y además revela
    /// `Unsupported`/`NotFound`/`PlanStale` de un directorio sobre el que no
    /// tiene derechos.
    ///
    /// Que el segundo sea una MUTACIÓN no lo exime, y es el error fácil de
    /// cometer: [`Self::rename_batch_as`] empieza planificando, o sea leyendo,
    /// y su gate de mutación no puede correr antes porque no se sabe qué rutas
    /// hay que gatear hasta que el plan existe. Sin `read_gate` delante, un
    /// agente sin scope obtiene por ahí exactamente el oráculo que este párrafo
    /// cierra aquí — y uno más fino, porque el `plan_hash` es determinista y
    /// calculable offline, así que la respuesta contesta a una hipótesis
    /// concreta sobre un nombre concreto.
    ///
    /// El gate vive en el daemon y no aquí porque es el daemon quien ata una
    /// conexión a un actor: por la API embebida no existe un `Actor::Agent` que
    /// no se haya escrito el propio proceso (mismo criterio que documenta
    /// `Backend::plugins_set_approval`). Duplicarlo aquí, además,
    /// preguntaría DOS veces bajo una regla `ask`.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] si algún nombre no es una entrada de directorio
    /// legal o si `pairs` supera
    /// [`FS_RENAME_BATCH_MAX_PAIRS`](norte_proto::methods::FS_RENAME_BATCH_MAX_PAIRS);
    /// [`Error::LimitExceeded`] si `dir` tiene más de
    /// [`RENAME_BATCH_MAX_LISTING`] entradas; [`Error::Unsupported`] si el
    /// scheme de `dir` no tiene provider o el provider es de solo lectura; los
    /// del provider al listar.
    pub async fn rename_batch_plan(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<crate::rename::DirPlan, Error> {
        self.rename_batch_plan_as(dir, pairs, crate::journal::Actor::User)
            .await
    }

    /// [`Self::rename_batch_plan`] con ACTOR explícito (camino agéntico).
    ///
    /// El actor viaja por simetría con las mutaciones y para auditoría futura,
    /// pero NO gatea nada: planificar es una lectura, y un agente que puede
    /// listar un directorio puede ver qué haría un rename en él. Lo que se
    /// gatea es ejecutarlo ([`Self::rename_batch_as`]).
    ///
    /// # Errors
    /// Las de [`Self::rename_batch_plan`].
    // `dir` por `span_path`, como en todos sus hermanos: con `skip(self,
    // pairs)` a secas se registraba por `Debug`, que es el wire entero —con
    // un `user:pass@` si la ruta lo lleva— y colgado ahora de cada línea de
    // la petición (ADR 0127).
    #[tracing::instrument(
        skip(self, dir, pairs),
        fields(dir = %span_path(dir), actor = ?actor, pairs = pairs.len())
    )]
    pub async fn rename_batch_plan_as(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
        actor: crate::journal::Actor,
    ) -> Result<crate::rename::DirPlan, Error> {
        // El actor no GATEA nada aquí (eso es el `read_gate` del daemon), pero
        // sí se traza: un plan que sale bien enumera un directorio entero y el
        // `read_gate` solo deja rastro cuando DENIEGA — sin esto, la lectura
        // agéntica que sí se permitió no es atribuible en la auditoría (M3-5).
        let (plan, _provider) = self.plan_for(dir, pairs).await?;
        Ok(plan)
    }

    /// Planifica contra el directorio TAL COMO ESTÁ AHORA y devuelve también el
    /// provider que lo sirvió, para que el camino de ejecución re-planifique
    /// con exactamente las mismas entradas.
    #[tracing::instrument(skip(self, pairs), fields(dir = %span_path(dir), pairs = pairs.len()))]
    async fn plan_for(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<(crate::rename::DirPlan, Arc<dyn Provider>), Error> {
        // Tope del wire, comprobado también aquí: el engine embebido es una API
        // pública y el planificador es lineal en pares Y en entradas del
        // listado — un lote sin cota es trabajo sin cota.
        if pairs.len() > norte_proto::methods::FS_RENAME_BATCH_MAX_PAIRS {
            tracing::debug!(pairs = pairs.len(), "lote de renames por encima del tope");
            return Err(Error::InvalidPath);
        }
        // La precondición del planificador (cada lado es UNA entrada de
        // directorio) se comprueba AQUÍ, en el único punto impuro por el que
        // pasan los nombres. `plan_batch` es `pub` y trata los nombres como
        // bytes opacos: en release planificaría feliz un `to = "../.."`.
        for name in pairs.iter().flat_map(|(f, t)| [f, t]) {
            if Segment::new(name.clone()).is_err() {
                tracing::debug!("nombre de rename que no es una entrada de directorio");
                return Err(Error::InvalidPath);
            }
        }
        let provider = self.provider_for(dir).await?;
        // El LISTADO va primero, y el orden sigue siendo la corrección aunque
        // `capabilities_at` ya sondee por su cuenta (ADR 0054): un directorio
        // que no se deja listar no tiene plan que calcular, y preguntar por sus
        // capabilities antes solo adelantaría trabajo para tirarlo.
        //
        // Y se pregunta por el DIRECTORIO, no por el provider, que es lo que la
        // doc de `NameCaps` promete: planificar sobre un volumen que pliega
        // como si distinguiera caja es una colisión `External` que no se
        // reporta y un plan que el humano aprueba sin la línea que le
        // importaba.
        let names = list_base_names(&*provider, dir).await?;
        let caps = provider.capabilities_at(dir).await?;
        if caps.flags.contains(CapabilityFlags::READ_ONLY) {
            return Err(Error::Unsupported);
        }
        let name_caps = crate::rename::NameCaps::from_capabilities(caps);
        let owned: Vec<(Vec<u8>, Vec<u8>)> = pairs.to_vec();
        // El planificador es SÍNCRONO y asigna una clave de comparación por
        // entrada del listado: sobre un directorio de cien mil ficheros eso es
        // trabajo de CPU medible, y `fs.rename_batch_plan` es una respuesta
        // DIRECTA (ADR 0042) que corre en el executor async. Fuera de él
        // (reglas 2 y 3): un hilo de bloqueo no puede dejar sin atender al
        // resto de conexiones del daemon.
        crate::blocking::spawn_blocking(move || {
            crate::rename::plan_batch(&owned, &names, name_caps)
        })
        .await
        .map(|plan| (crate::rename::DirPlan::bind(dir, plan), provider))
        .map_err(|e| {
            let panic = e.is_panic();
            tracing::error!(error = %e, panic, "el planificador de renames no terminó");
            Error::Internal { panic }
        })
    }

    /// Ejecuta un lote de renames dentro de `dir` como UNA Task y UNA unidad
    /// deshacible del journal (spec §17, ADR 0042).
    ///
    /// `plan_hash` es el token de FRESCURA del plan
    /// ([`Self::rename_batch_plan`]), atado al directorio. El directorio se
    /// re-planifica aquí y los tokens tienen que coincidir: una deriva que
    /// cambie algún veredicto responde [`Error::PlanStale`] en vez de ejecutar
    /// un plan distinto del que se pidió.
    ///
    /// Ojo con lo que NO garantiza. El digest es una función pública y
    /// determinista de `(dir, pasos, veredictos)`, sin secreto ni estado en el
    /// server, así que cualquiera puede calcularlo sin haber llamado nunca a
    /// [`Self::rename_batch_plan`]: casar el token NO demuestra que un humano
    /// viera el plan. Lo que demuestra es que el plan que se ejecuta es el que
    /// el re-plan produce AHORA. El consentimiento lo aporta el gate de policy,
    /// no este hash.
    ///
    /// Devuelve la Task y su [`crate::rename::BatchReport`], que se rellena
    /// mientras corre y queda completo al terminar. **Míralo aunque la Task
    /// falle**: si el rollback se quedó a medias, ahí está el paso que siguió
    /// aplicado y con qué nombres — el `Failed{error}` solo cuenta la causa.
    ///
    /// Ningún paso sobrescribe nada: son [`norte_vfs::Provider::rename`]
    /// pelados, y el planificador rechaza el plan entero ante cualquier
    /// colisión.
    ///
    /// # Errors
    /// [`Error::PlanNotExecutable`] si el plan tiene colisiones;
    /// [`Error::PlanStale`] si el directorio derivó; [`Error::PolicyDenied`]
    /// si el gate deniega; [`Error::JournalUnavailable`] si el journal de esta
    /// sesión no se puede abrir (#178); las de [`Self::rename_batch_plan`].
    pub async fn rename_batch(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
        plan_hash: &PlanHash,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<crate::rename::BatchReport>>,
        ),
        Error,
    > {
        self.rename_batch_as(dir, pairs, plan_hash, crate::journal::Actor::User)
            .await
    }

    /// [`Self::rename_batch`] con ACTOR explícito (camino agéntico, M3-3).
    ///
    /// # Errors
    /// Las de [`Self::rename_batch`].
    ///
    /// # Panics
    /// Solo por envenenamiento del `Mutex` del reporte (otro hilo panicó
    /// sosteniéndolo) — irrecuperable, mismo criterio que el resto del core.
    #[tracing::instrument(skip(self, pairs, plan_hash, actor), fields(dir = %span_path(dir)))]
    pub async fn rename_batch_as(
        &self,
        dir: &VPath,
        pairs: &[(Vec<u8>, Vec<u8>)],
        plan_hash: &PlanHash,
        actor: crate::journal::Actor,
    ) -> Result<
        (
            TaskHandle,
            Arc<std::sync::Mutex<crate::rename::BatchReport>>,
        ),
        Error,
    > {
        let (plan, provider) = self.plan_for(dir, pairs).await?;
        // La DERIVA se comprueba primero, y el orden es contractual (ver la
        // doc de `Error::PlanStale`): «re-planificar las mismas parejas produjo
        // un plan distinto del que se aprobó». Un directorio que derivó hasta
        // volverse colisionante contestando `PlanNotExecutable` le diría al
        // humano que el plan que leyó tenía colisiones — y no las tenía. Con
        // este orden, `PlanNotExecutable` queda para lo que de verdad nombra:
        // el llamante aprobó un plan que ya venía muerto (ignoró
        // `executable: false`), y su token casa.
        if plan.hash() != plan_hash {
            return Err(Error::PlanStale);
        }
        if !plan.executable() {
            return Err(Error::PlanNotExecutable);
        }
        let steps: Vec<crate::rename::exec::PlannedStep> = plan
            .plan()
            .steps
            .iter()
            .map(|s| crate::rename::exec::absolute(dir, s))
            .collect::<Result<_, _>>()?;
        // UN gate para el lote entero, con TODAS las rutas que toca — los
        // temporales incluidos, que también son ficheros creados en el
        // directorio. La policy resuelve el slice a lo MÁS restrictivo, así que
        // un lote que roza un nombre denegado se deniega ENTERO y jamás a
        // medias. Va antes de reservar el id de lote: un lote denegado no
        // consume nada.
        let mut gate_paths: Vec<&VPath> = Vec::with_capacity(steps.len() * 2);
        for s in &steps {
            gate_paths.push(&s.from);
            gate_paths.push(&s.to);
        }
        self.gate(&actor, crate::policy::PolicyOp::Move, &gate_paths)
            .await?;
        drop(gate_paths);

        let recorder: Arc<dyn crate::rename::exec::StepJournal> = match self.journal().await {
            Some(j) => {
                let batch_id = j.journal().alloc_batch().await.map_err(Error::from)?;
                Arc::new(crate::rename::exec::BatchJournal {
                    journal: Arc::clone(&j),
                    actor: actor.clone(),
                    batch_id,
                })
            }
            // Sin journal (tests embebidos, `Engine::new`): los renames ocurren
            // y llegan al observer, pero no hay lote que agrupar ni undo que
            // servir. Honesto: sin journal tampoco hay undo.
            //
            // OJO al día en que el observer deje de ser el journal: con
            // `with_journal` los dos son el MISMO objeto (ver su constructor),
            // así que escribir directo en el journal no se salta a nadie. Si
            // alguna vez se instala un observer en abanico JUNTO a un journal,
            // esta rama tiene que emitir a los dos o los renames por lotes
            // serán los únicos invisibles para él.
            // FIJADO (#205), y aquí hacía falta tanto como en `ops`: sin fijar,
            // cada rename del lote volvía a preguntarle al observer, así que un
            // lote largo que empieza con el fichero ocupado dejaba filas a
            // partir de la mitad. Peor que en `ops`, además: esas filas van sin
            // `batch_id`, o sea que el lote que el wire anuncia como UNA unidad
            // deshacible quedaba medio registrado Y sin agrupar, y el undo
            // desandaba la cola dejando la cabeza renombrada.
            //
            // Se fija SIN volver a resolver: `self.journal()` ya preguntó, y
            // preguntar otra vez podría contestar que sí —el freno es corto en
            // los tests, y `with_retry_brake` es público— con lo que el lote
            // quedaría registrado entero pero sin lote, que es el otro modo de
            // romper la misma promesa.
            None => Arc::new(crate::rename::exec::ObserverJournal {
                // Un journal perezoso ausente NO registra; un observer de
                // embebedor sí recibe, aunque no haya journal detrás. El
                // informe dice la verdad en los dos casos (#205).
                records: !matches!(self.journal, JournalSource::Lazy(_)),
                observer: if matches!(self.journal, JournalSource::Lazy(_)) {
                    // Journal perezoso que ahora mismo no está: este lote no
                    // registra nada, y no volverá a preguntar.
                    Arc::new(crate::observer::NoopObserver)
                } else {
                    // `Engine::new()`/`with_observer`: no hay ventana que
                    // perder y el observer del embebedor tiene que seguir
                    // recibiendo sus renames.
                    Arc::clone(&self.observer)
                },
                actor: actor.clone(),
            }),
        };

        let report = Arc::new(std::sync::Mutex::new(crate::rename::BatchReport::default()));
        let report_task = Arc::clone(&report);
        let key = dir.scheme().to_owned();
        let owner = actor.clone();
        let handle = self.sched.submit(
            &key,
            TaskKind::RenameBatch,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    let out = crate::rename::exec::run(
                        &*provider,
                        &*recorder,
                        &steps,
                        &ctx.cancel,
                        &ctx.progress,
                        &report_task,
                    )
                    .await;
                    // Un rollback que no pudo terminar no es un detalle: nombra
                    // el paso que se quedó aplicado.
                    let stuck = report_task.lock().expect("batch report lock").stuck.clone();
                    if let Some(s) = stuck {
                        tracing::error!(
                            from = %span_path(&s.from),
                            to = %span_path(&s.to),
                            pair_index = s.pair_index,
                            still_applied = s.still_applied,
                            "el lote de renames dejó un rename aplicado que no pudo deshacerse",
                        );
                    }
                    out
                })
            }),
        );
        // Retiene el informe para quien solo tiene el `task_id`: el socket
        // (`fs.rename_batch_report`) y el `Backend` embebido. El llamante
        // directo ya se lleva el `Arc` VIVO en la mano; esto es para los otros
        // dos, que solo pueden pedirlo después. Anillo acotado: la memoria de
        // un daemon de meses no crece con cada lote.
        {
            let mut ring = self.batch_reports.lock().expect("batch_reports lock sano");
            ring.push_back((handle.id(), owner, Arc::clone(&report)));
            evict_batch_reports(&mut ring);
        }
        Ok((handle, report))
    }

    /// Aplica un plan de ORGANIZAR (fase 8, `fs.organize`): crea las carpetas
    /// que falten y mueve, TODO bajo un solo lote.
    ///
    /// Es un método y no N llamadas del cliente por una razón concreta: los
    /// `fs.create` y los `fs.move` tienen que compartir `batch_id`. Sueltos,
    /// deshacer el lote devolvería los ficheros y se olvidaría las carpetas —
    /// y el humano se quedaría con un árbol de directorios vacíos que él no
    /// hizo.
    ///
    /// Los movimientos se journalizan con [`crate::OP_ORGANIZED`] y no
    /// con `renamed`, y eso también es correctitud: un lote de `renamed` lo
    /// deshace el ejecutor de renombrados, que supone UN directorio común y
    /// construye la cadena inversa a partir de él. Aquí no hay directorio
    /// común — mover a subdirectorios es justo lo que esto hace.
    ///
    /// # Errors
    /// [`Error::PlanStale`] si el `plan_hash` no es el del plan que se
    /// revisó; [`Error::InvalidPath`] si algún destino no pasa la validación
    /// (un `..`, una ruta absoluta, un plan que se contradice);
    /// [`Error::PolicyDenied`] si la policy deniega cualquiera de las rutas
    /// que toca — el lote se deniega ENTERO, jamás a medias; las de
    /// [`Self::mkdir_as`] y las del provider.
    ///
    /// # Panics
    /// No: los `expect` son sobre locks propios.
    pub async fn organize(
        &self,
        dir: &VPath,
        moves: &[norte_proto::methods::OrganizeMove],
        plan_hash: &PlanHash,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        let plan = crate::organize::OrganizePlan::bind(dir, moves)?;
        // La DERIVA primero, igual que en el lote de renombrados y por el
        // mismo motivo: lo que se aplica tiene que ser lo que un humano leyó.
        if plan.hash() != plan_hash {
            return Err(Error::PlanStale);
        }
        let provider = self.provider_for(dir).await?;
        let carpetas = plan.carpetas(dir);
        let pasos: Vec<(VPath, VPath)> = plan
            .pasos()
            .iter()
            .map(|p| (dir.clone().join(p.current.clone()), p.destino(dir)))
            .collect();

        // UN gate para todo lo que toca —las carpetas que va a crear y los dos
        // lados de cada movimiento—, antes de reservar el lote. La policy
        // resuelve el slice a lo más restrictivo, así que un plan que roza un
        // nombre denegado se deniega entero.
        let mut gate: Vec<&VPath> = Vec::with_capacity(carpetas.len() + pasos.len() * 2);
        gate.extend(carpetas.iter());
        for (de, a) in &pasos {
            gate.push(de);
            gate.push(a);
        }
        self.gate(&actor, crate::policy::PolicyOp::Move, &gate)
            .await?;
        drop(gate);

        let journal = self.journal().await;
        let batch_id = match &journal {
            Some(j) => Some(j.journal().alloc_batch().await.map_err(Error::from)?),
            // Sin journal no hay lote que agrupar ni undo que servir, y se
            // dice así: honesto, como en el lote de renombrados.
            None => None,
        };
        let key = dir.scheme().to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::RenameBatch,
            Priority::Normal,
            actor.clone(),
            Box::new(move |ctx| {
                Box::pin(async move {
                    let total = (carpetas.len() + pasos.len()) as u64;
                    ctx.progress.update(|p| p.entries_total = Some(total));
                    // Primero las carpetas, de la más alta a la más honda: un
                    // provider no inventa padres, y el core tiene que saber
                    // cuáles creó para poder borrarlas al deshacer.
                    for c in &carpetas {
                        if ctx.cancel.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        // Una carpeta que YA existe no es un fallo ni se
                        // journaliza: no la creó este lote, así que deshacerlo
                        // no puede borrarla.
                        if provider.stat(c).await.is_ok() {
                            continue;
                        }
                        provider.mkdir(c).await?;
                        if let (Some(j), Some(b)) = (journal.as_ref(), batch_id) {
                            j.journal()
                                .record_entry(&crate::journal::NewEntry {
                                    op: "created",
                                    path: c.to_wire().as_bytes(),
                                    path_to: None,
                                    reversal: crate::journal::Reversal::Delete,
                                    reversal_ref: None,
                                    actor: &actor,
                                    undoes_seq: None,
                                    batch_id: Some(b),
                                })
                                .await
                                .map_err(Error::from)?;
                        }
                        ctx.progress.update(|p| p.entries_done += 1);
                    }
                    for (de, a) in &pasos {
                        if ctx.cancel.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        provider.rename(de, a).await?;
                        if let (Some(j), Some(b)) = (journal.as_ref(), batch_id) {
                            j.journal()
                                .record_entry(&crate::journal::NewEntry {
                                    op: crate::undo::OP_ORGANIZED,
                                    path: a.to_wire().as_bytes(),
                                    path_to: Some(de.to_wire().as_bytes()),
                                    reversal: crate::journal::Reversal::RenameBack,
                                    reversal_ref: None,
                                    actor: &actor,
                                    undoes_seq: None,
                                    batch_id: Some(b),
                                })
                                .await
                                .map_err(Error::from)?;
                        }
                        ctx.progress.update(|p| p.entries_done += 1);
                    }
                    Ok(())
                })
            }),
        );
        Ok(handle)
    }

    /// Informe de un lote ya lanzado, por `task_id`, más el ACTOR que lo pidió
    /// (spec §17). `None` si ese id nunca fue un lote de esta instancia o si el
    /// anillo ya lo desalojó ([`BATCH_REPORTS_MAX`]).
    ///
    /// Es un SNAPSHOT: definitivo cuando la Task es terminal, parcial antes.
    /// El actor sale con él porque quien sirve esto por el wire tiene que
    /// decidir si el que pregunta podía ver esa task — decidirlo aquí obligaría
    /// al engine a conocer las reglas de visibilidad del daemon.
    ///
    /// Este camino NO panica ante un lock envenenado: es de LECTURA y lo
    /// atraviesa cada `fs.rename_batch_report`, así que un panic ajeno con el
    /// mutex en la mano convertiría el método entero en una bomba por
    /// conexión. Un informe posiblemente rancio le sirve más a quien busca su
    /// fichero que un error — y el informe es datos, no un invariante que un
    /// panic a medias pudiera haber roto.
    #[must_use]
    pub fn rename_batch_report(
        &self,
        task_id: TaskId,
    ) -> Option<(crate::journal::Actor, crate::rename::BatchReport)> {
        let ring = self
            .batch_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }

    /// Borrado PERMANENTE (recursivo post-order) como Task. La papelera
    /// es [`Self::delete_with`] con [`DeleteMode::Trash`].
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider registrado.
    pub async fn delete(&self, path: &VPath) -> Result<TaskHandle, Error> {
        self.delete_with(path, DeleteMode::Permanent).await
    }

    /// Borrado con modo explícito (ADR 0009): `Trash` mueve el árbol
    /// entero a la papelera del provider (una sola operación; sin la
    /// capability `TRASH` la task falla `Unsupported` — el engine JAMÁS
    /// degrada a permanente por su cuenta); `Permanent` borra de verdad.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider registrado.
    pub async fn delete_with(&self, path: &VPath, mode: DeleteMode) -> Result<TaskHandle, Error> {
        self.delete_with_as(path, mode, crate::journal::Actor::User)
            .await
    }

    /// Borrado con modo y ACTOR explícito (camino agéntico, M3-3).
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] si el journal de esta sesión
    /// no se puede abrir (#178); [`Error::PolicyDenied`] si la policy
    /// deniega; [`Error::Unsupported`] si el scheme no tiene provider
    /// registrado.
    #[tracing::instrument(skip(self, actor), fields(path = %span_path(path), ?mode))]
    pub async fn delete_with_as(
        &self,
        path: &VPath,
        mode: DeleteMode,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Delete { mode }, &[path])
            .await?;
        let provider = self.provider_for(path).await?;
        let observer = Arc::clone(&self.observer);
        let path = path.clone();
        let key = path.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Delete,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(
                    async move { ops::delete_task(provider, path, mode, observer, &ctx).await },
                )
            }),
        ))
    }

    /// Creación de UN directorio como Task (#104, F7). Sin `-p` (padre
    /// ausente = `NotFound`), destino ocupado = `Conflict{Exists}`. Journal
    /// `Created` con undo (regla 4).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider registrado.
    pub async fn mkdir(&self, path: &VPath) -> Result<TaskHandle, Error> {
        self.mkdir_as(path, crate::journal::Actor::User).await
    }

    /// [`Self::mkdir`] con ACTOR explícito (camino agéntico, M3-3): gateado
    /// por [`crate::policy::PolicyOp::Mkdir`] PRE-efecto.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] si el journal de esta sesión
    /// no se puede abrir (#178); [`Error::PolicyDenied`] si la policy
    /// deniega; [`Error::Unsupported`] si el scheme no tiene provider
    /// registrado.
    #[tracing::instrument(skip(self, actor), fields(path = %span_path(path)))]
    pub async fn mkdir_as(
        &self,
        path: &VPath,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Mkdir, &[path])
            .await?;
        let provider = self.provider_for(path).await?;
        let observer = Arc::clone(&self.observer);
        let path = path.clone();
        let key = path.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Mkdir,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move { ops::mkdir_task(provider, path, observer, &ctx).await })
            }),
        ))
    }

    /// Crea un fichero VACÍO (#290).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider registrado.
    pub async fn create_file(&self, path: &VPath) -> Result<TaskHandle, Error> {
        self.create_file_as(path, None, crate::journal::Actor::User)
            .await
    }

    /// [`Self::create_file`] con ACTOR explícito: gateado por
    /// [`crate::policy::PolicyOp::Create`] PRE-efecto.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] si el journal de esta sesión no se puede
    /// abrir (#178); [`Error::PolicyDenied`] si la policy deniega;
    /// [`Error::Unsupported`] si el scheme no tiene provider registrado.
    #[tracing::instrument(skip(self, actor), fields(path = %span_path(path)))]
    pub async fn create_file_as(
        &self,
        path: &VPath,
        dest_anchor: Option<norte_proto::DirAnchor>,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Create, &[path])
            .await?;
        let provider = self.provider_for(path).await?;
        let observer = Arc::clone(&self.observer);
        let path = path.clone();
        let key = path.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Create,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    ops::create_task(provider, path, dest_anchor, observer, &ctx).await
                })
            }),
        ))
    }

    /// Escribe un fichero con contenido desde memoria como Task (ADR 0101):
    /// el sidecar de un hook. Gateado PRE-efecto por
    /// [`crate::policy::PolicyOp::Create`] y, si `on_exists` es
    /// [`OnExists::Replace`], también por `Delete{Trash}`: reemplazar es
    /// enterrar lo que había y crear, dos permisos.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] si `content` supera
    /// [`norte_plugin_host::MAX_SIDECAR_BYTES`]; [`Error::JournalUnavailable`]
    /// si el journal no se puede abrir (#178); [`Error::PolicyDenied`] si la
    /// policy deniega; [`Error::Unsupported`] sin provider para el scheme.
    #[tracing::instrument(skip(self, actor, content), fields(path = %span_path(path)))]
    pub async fn write_file_as(
        &self,
        path: &VPath,
        content: Vec<u8>,
        on_exists: OnExists,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        if content.len() > norte_plugin_host::MAX_SIDECAR_BYTES {
            return Err(Error::InvalidPath);
        }
        self.gate(&actor, crate::policy::PolicyOp::Create, &[path])
            .await?;
        if on_exists == OnExists::Replace {
            self.gate(
                &actor,
                crate::policy::PolicyOp::Delete {
                    mode: norte_proto::DeleteMode::Trash,
                },
                &[path],
            )
            .await?;
        }
        let provider = self.provider_for(path).await?;
        let observer = Arc::clone(&self.observer);
        let path = path.clone();
        let key = path.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Create,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    ops::write_task(provider, path, content, on_exists, observer, &ctx).await
                })
            }),
        ))
    }

    /// Cambia los permisos POSIX de un lote de rutas (#314, ADR 0081).
    ///
    /// # Errors
    /// [`Error::InvalidPath`] sin rutas, por encima del tope, o con bits que
    /// no son de permiso; [`Error::PolicyDenied`] si la política deniega;
    /// [`Error::Unsupported`] si algún scheme no tiene provider.
    pub async fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> Result<TaskHandle, Error> {
        self.set_mode_as(params, crate::journal::Actor::User).await
    }

    /// [`Self::set_mode`] con ACTOR explícito: gateado por
    /// [`crate::policy::PolicyOp::SetMode`] PRE-efecto, sobre TODAS las rutas.
    ///
    /// El gate va sobre la lista entera y antes de la primera escritura: un
    /// lote que empezara a cambiar permisos y se topara con la política a
    /// mitad dejaría media selección cambiada por una petición que estaba
    /// denegada.
    ///
    /// # Errors
    /// Los de [`Self::set_mode`].
    #[tracing::instrument(skip(self, params, actor), fields(n = params.paths.len()))]
    pub async fn set_mode_as(
        &self,
        params: norte_proto::methods::FsSetModeParams,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        let Some(primera) = params.paths.first() else {
            tracing::debug!("fs.set_mode sin rutas");
            return Err(Error::InvalidPath);
        };
        if params.paths.len() > norte_proto::methods::FS_SET_MODE_MAX_PATHS {
            tracing::debug!(n = params.paths.len(), "fs.set_mode por encima del tope");
            return Err(Error::InvalidPath);
        }
        // Los bits de arriba dicen de qué CLASE es el nodo. Recortarlos en
        // silencio dejaría un permiso que nadie pidió; se rechaza.
        if params.mode & !norte_proto::methods::MODE_PERMISSION_BITS != 0 {
            tracing::debug!(
                mode = params.mode,
                "fs.set_mode con bits que no son permiso"
            );
            return Err(Error::InvalidPath);
        }
        // setuid y setgid, solo a mano (ADR 0081). No porque esos bits sean el
        // peligro —un `chmod 0777` sobre `~/.ssh` hace mucho más daño y no
        // lleva ninguno—, sino porque son los que quien aprueba NO PUEDE VER:
        // la petición de aprobación lleva la op y las rutas, y no el modo, así
        // que un humano diría que sí a «set-mode sobre 12 rutas» sin saber si
        // era `0600` o `4777`. Mientras el modo no viaje en esa pregunta, un
        // agente no los fija; el humano sí, desde un diálogo que sí los enseña.
        const ESPECIALES: u32 = 0o6000;
        if params.mode & ESPECIALES != 0 && !matches!(actor, crate::journal::Actor::User) {
            tracing::warn!(
                mode = params.mode,
                "fs.set_mode con setuid/setgid de un actor que no es el humano: denegado"
            );
            return Err(Error::PolicyDenied {
                rule: "set-mode.special-bits".to_owned(),
            });
        }
        // `dir_mode` pasa por las MISMAS dos comprobaciones que `mode`, y ANTES
        // del gate como ellas (#315): un modo que se va a rechazar no puede
        // gastar antes la aprobación de un humano — que además la vería sin
        // este valor dentro.
        //
        // Y solo significa algo con `recursive`: sin bajar por el árbol no hay
        // directorios a los que aplicárselo, y aplicarlo a las rutas pedidas
        // les daría un permiso que quien llamó no pidió. Se descarta aquí, que
        // es donde el wire dice que se ignora.
        let dir_mode = params.recursive.then_some(params.dir_mode).flatten();
        if dir_mode.is_some_and(|m| m & !norte_proto::methods::MODE_PERMISSION_BITS != 0) {
            tracing::debug!("fs.set_mode con un dir_mode que no son bits de permiso");
            return Err(Error::InvalidPath);
        }
        if dir_mode.is_some_and(|m| m & ESPECIALES != 0)
            && !matches!(actor, crate::journal::Actor::User)
        {
            tracing::warn!("fs.set_mode: dir_mode con setuid/setgid de un actor no humano");
            return Err(Error::PolicyDenied {
                rule: "set-mode.special-bits".to_owned(),
            });
        }
        let refs: Vec<&VPath> = params.paths.iter().collect();
        // La pregunta lleva el ALCANCE, no solo el modo (#315): un recursivo
        // sobre una raíz son cien mil nodos y `paths_total` dice 1.
        self.gate(
            &actor,
            crate::policy::PolicyOp::SetMode {
                mode: params.mode,
                recursive: params.recursive,
                dir_mode,
            },
            &refs,
        )
        .await?;
        let key = primera.scheme().to_owned();
        let mut rutas = Vec::with_capacity(params.paths.len());
        for p in &params.paths {
            let provider = self.provider_for(p).await?;
            rutas.push((provider, p.clone()));
        }
        let observer = Arc::clone(&self.observer);
        // Un recursivo son N entradas de diario que fueron UNA acción, así que
        // van bajo un lote (#315) — como el ejecutor de renames. Sin journal no
        // hay lote que pedir, y entonces las entradas van sueltas: lo que se
        // pierde es poder decir que fueron una, no el undo.
        let batch = if params.recursive {
            match self.journal().await {
                Some(j) => j.journal().alloc_batch().await.ok(),
                None => None,
            }
        } else {
            None
        };
        let opciones = ops::SetModeOptions {
            mode: params.mode,
            recursive: params.recursive,
            dir_mode,
            batch,
        };
        Ok(self.sched.submit(
            &key,
            TaskKind::SetMode,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move { ops::set_mode(rutas, opciones, observer, &ctx).await })
            }),
        ))
    }

    /// Capabilities de LA UBICACIÓN `p` (para que el frontend decida, p. ej.,
    /// si el F8 va a papelera o avisa de permanente).
    ///
    /// Desde ADR 0054 responde por la ubicación y no por el provider entero:
    /// el método del wire (`fs.capabilities`) siempre tomó un path y hasta
    /// ahora contestaba lo mismo para cualquiera de ellos, lo cual es falso en
    /// cuanto una máquina monta dos filesystems distintos.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider registrado, y lo
    /// que produzca `p` en el provider ([`Error::NotFound`] si no existe).
    pub async fn capabilities(&self, p: &VPath) -> Result<norte_proto::Capabilities, Error> {
        self.provider_for(p).await?.capabilities_at(p).await
    }

    /// Barre el staging `.norte-partial` huérfano (crashes previos, ADR 0012 /
    /// #11) bajo `dir`, delegando en el provider que lo sirve. Operación
    /// PUNTUAL (no una Task) y NO registrada en el journal (no es una mutación
    /// de usuario). Los providers sin staging local devuelven 0.
    ///
    /// No hay barrido automático al arranque: `gc_partials` es single-dir y no
    /// existe una raíz gestionada fiable hasta que el journal registre el
    /// staging in-flight (deuda, futuro increment de M3).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider; los del provider.
    pub async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        self.provider_for(dir)
            .await?
            .gc_partials(dir, older_than)
            .await
    }

    /// Deshace en LIFO las mutaciones revertibles de la sesión `actor`, sin
    /// pisar el trabajo del usuario (estricto: para en el primer conflicto).
    /// Cada undo se registra como entrada compensatoria (append-only). Task
    /// cancelable con progreso; el [`crate::UndoReport`] se llena en el
    /// `Arc<Mutex<…>>` devuelto y queda completo al terminar la Task.
    ///
    /// El `actor` SELECCIONA qué sesión deshacer Y actúa como ejecutor: el gate
    /// de policy se evalúa con él y las compensaciones lo registran (así, un
    /// agente que deshace su propia sesión queda sujeto a su scope/policy, y las
    /// compensaciones llevan el actor real). El caso «humano deshace la sesión
    /// de un agente» (performer ≠ target) es [`Self::undo_session_for`].
    ///
    /// El undo pasa por el gate de policy (M3-3, regla 9): cada reversa se
    /// evalúa como su `PolicyOp` inverso, unidad a unidad, DENTRO de la Task
    /// (#171); una denegación bloquea esa unidad y el bucle sigue con las
    /// demás (`report.denied`), mientras que un conflicto real sí para el
    /// LIFO (`report.blocked`).
    ///
    /// **El provider se resuelve ANTES que el gate**, en la planificación, y
    /// eso hay que escribirlo porque el texto de aquí decía lo contrario: al
    /// mover el gate dentro de la Task, `provider_for` se quedó fuera. O sea
    /// que el journal sí puede dirigir la apertura de una conexión remota
    /// antes de que la policy opine sobre la reversa. Se acepta porque
    /// resolver un provider no muta nada y las rutas del journal las escribió
    /// este mismo core; lo que no se acepta es que el rustdoc afirme una
    /// garantía que el código no da.
    ///
    /// # Errors
    /// [`Error::JournalUnavailable`] si el journal de esta sesión existe pero no
    /// se puede ABRIR (#178).
    /// [`Error::Unsupported`] si el Engine no tiene journal ([`Self::with_journal`]),
    /// o si algún path del journal no tiene provider registrado.
    ///
    /// # Panics
    /// Si el `Mutex` interno del reporte queda envenenado (solo si un poseedor
    /// previo paniqueó sosteniéndolo — no ocurre en la práctica).
    pub async fn undo_session(
        &self,
        actor: crate::journal::Actor,
    ) -> Result<(TaskHandle, Arc<std::sync::Mutex<crate::UndoReport>>), Error> {
        self.undo_session_for(&actor.clone(), actor).await
    }

    /// Como [`Self::undo_session`] pero separando QUIÉN se deshace de QUIÉN
    /// ejecuta (M3-4, deuda M3-2): `target` selecciona las entradas del
    /// journal a revertir; `executor` pasa el gate de policy y firma las
    /// compensaciones. El caso humano-deshace-agente usa
    /// `(Agent{session}, User)`: el undo no muere porque el scope del agente
    /// haya expirado — lo ejecuta el humano.
    ///
    /// # Errors
    /// Las de [`Self::undo_session`].
    ///
    /// # Panics
    /// Como [`Self::undo_session`] (Mutex del reporte envenenado; no ocurre).
    pub async fn undo_session_for(
        &self,
        target: &crate::journal::Actor,
        executor: crate::journal::Actor,
    ) -> Result<(TaskHandle, Arc<std::sync::Mutex<crate::UndoReport>>), Error> {
        // Un journal ILEGIBLE se dice con su categoría y no como «no soportado»
        // (#178): los dos caminos acaban sin cadena que leer, pero solo uno de
        // ellos tiene un fichero concreto que arreglar.
        self.journal_gate().await?;
        let journal = self.journal().await.ok_or(Error::Unsupported)?;
        let entries = journal
            .journal()
            .revertible_for(target)
            .await
            .map_err(Error::from)?;
        self.undo_entries(entries, executor).await
    }

    /// Deshace, en LIFO, lo que el HUMANO hizo DESPUÉS de `after_seq` (fase 7
    /// del programa WOW, `journal.undo_after`).
    ///
    /// La entrada `after_seq` se queda: es el estado al que se quiere volver.
    ///
    /// Es [`Self::undo_session_for`] con otro criterio de SELECCIÓN y nada
    /// más — las mismas unidades, el mismo gate de policy por unidad, el
    /// mismo LIFO estricto que para en el primer bloqueo, los mismos
    /// contadores y el mismo informe—. Eso no es una coincidencia que haya
    /// que mantener a mano: las dos llaman al mismo cuerpo privado, donde
    /// vive todo eso una sola vez. Un segundo undo «parecido» escrito aparte
    /// habría divergido en la primera regla que alguien afinara.
    ///
    /// Un LOTE entra entero o no entra: si el corte cae en medio de uno, ese
    /// lote se queda fuera completo. Partirlo revertiría media unidad
    /// creyéndola entera, y meterlo entero desharía entradas anteriores al
    /// corte. El porqué, con la forma de la consulta, está sobre
    /// `SELECT_REVERTIBLE_AFTER`.
    ///
    /// `upto_seq` es el TECHO (0.80.0): nada con `seq` mayor se deshace. Es lo
    /// más nuevo que el humano tenía contado delante; sin él, lo que se hizo
    /// después de pintar la línea de tiempo entraba en un undo que no lo
    /// había contado. `None` = sin techo.
    ///
    /// # Errors
    /// Las de [`Self::undo_session_for`], y [`Error::NotFound`] si `seq` no
    /// nombra ninguna entrada — un corte que no existe no se interpreta como
    /// «desde el principio».
    ///
    /// # Panics
    /// Como [`Self::undo_session`] (Mutex del reporte envenenado; no ocurre).
    pub async fn undo_after(
        &self,
        after_seq: i64,
        upto_seq: Option<i64>,
    ) -> Result<(TaskHandle, Arc<std::sync::Mutex<crate::UndoReport>>), Error> {
        self.journal_gate().await?;
        let journal = self.journal().await.ok_or(Error::Unsupported)?;
        // El corte tiene que NOMBRAR una entrada que existe, y no sólo ser un
        // número. `seq > 0` seleccionaría TODO lo del humano desde el
        // principio de los tiempos, y ese cero es justo lo que sale de un
        // cursor rancio o de un cliente que traduce «no hay nada señalado» a
        // cero. Deshacer de menos se vuelve a pedir; deshacer la historia
        // entera de alguien porque su cliente mandó un cero, no.
        //
        // Comprobarlo contra el journal, y no con un `>= 1` a secas, cubre
        // además el cursor de una entrada que ya no está.
        if journal
            .journal()
            .entry_hash_at(after_seq)
            .await
            .map_err(Error::from)?
            .is_none()
        {
            return Err(Error::NotFound);
        }
        let entries = journal
            .journal()
            .revertible_for_after(&crate::journal::Actor::User, after_seq, upto_seq)
            .await
            .map_err(Error::from)?;
        self.undo_entries(entries, crate::journal::Actor::User)
            .await
    }

    /// Una PÁGINA del journal hacia atrás (fase 7, `journal.list`).
    ///
    /// Lectura pura: no toca nada y no pasa por el gate de policy, que
    /// gobierna mutaciones. Quién puede preguntarlo lo decide el borde —el
    /// daemon sólo se lo sirve a una conexión humana—, que es donde se sabe
    /// quién está al otro lado del socket.
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin journal; la categoría propia de un journal
    /// ILEGIBLE (#178), que no es lo mismo; [`Error::Internal`] de sqlite.
    pub async fn journal_page(
        &self,
        before_seq: Option<i64>,
        limit: u32,
        actor_kind: Option<&str>,
    ) -> Result<Vec<crate::journal::PageEntry>, Error> {
        self.journal_gate().await?;
        let journal = self.journal().await.ok_or(Error::Unsupported)?;
        // El tope se aplica AQUÍ y no sólo en cada borde: los dos que hay hoy
        // lo hacen, y el tercero que venga heredaría la cota en vez de tener
        // que acordarse de ponerla.
        let limit = limit.clamp(1, norte_proto::methods::JOURNAL_LIST_MAX_PAGE);
        journal
            .journal()
            .page(before_seq, limit, actor_kind)
            .await
            .map_err(Error::from)
    }

    /// El cuerpo COMPARTIDO de un undo: agrupa en unidades, resuelve el
    /// provider de cada una, y lanza la Task que las revierte en LIFO con el
    /// gate de policy unidad a unidad.
    ///
    /// Lo que varía entre deshacer una sesión y deshacer hasta un punto es
    /// QUÉ entradas entran, y eso lo decide el llamante. Todo lo demás —y es
    /// donde están las reglas que duelen si divergen— vive aquí.
    ///
    /// # Errors
    /// Las de [`Self::undo_session_for`].
    ///
    /// # Panics
    /// Como [`Self::undo_session`] (Mutex del reporte envenenado; no ocurre).
    #[expect(
        clippy::too_many_lines,
        reason = "el cuerpo de la Task es UNA secuencia —turno, re-comprobación, gate, \
                  reversa, informe— y partirla esconde el orden, que es la regla"
    )]
    async fn undo_entries(
        &self,
        entries: Vec<crate::journal::JournalEntry>,
        executor: crate::journal::Actor,
    ) -> Result<(TaskHandle, Arc<std::sync::Mutex<crate::UndoReport>>), Error> {
        // El journal se vuelve a pedir aquí y no se recibe del llamante: es
        // el que viaja DENTRO de la Task para escribir las compensaciones, y
        // cada llamante ya comprobó el suyo para poder leer las entradas.
        let journal = self.journal().await.ok_or(Error::Unsupported)?;
        let report = Arc::new(std::sync::Mutex::new(crate::UndoReport::default()));

        // Un lote (`fs.rename_batch`) es UNA unidad: se revierte entero o no se
        // toca (diseño §7). El agrupado va antes del gate para que la policy vea
        // el lote completo, igual que en la ida.
        let units = crate::undo::undo_units(entries);

        // Planning: SOLO resuelve el provider de cada unidad, que es lo único
        // que necesita `&self` (el cuerpo de la Task es `'static`). El gate se
        // pregunta DENTRO, unidad a unidad — #171.
        //
        // Lo que esto quita del hilo del llamante es lo que crecía sin tope:
        // `undo_gate_targets` parsea hasta `unit.len() * 2` `VPath`s, y una
        // unidad de sincronización tiene un paso por entrada. Con un `Mirror`
        // de medio millón, `policy.undo_session` se pasaba medio millón de
        // parseos antes de devolver un `task_id`, sin progreso y sin poder
        // cancelarse. Ahora devuelve el id de inmediato y el trabajo va dentro,
        // con el token en el bucle (regla dura 3).
        //
        // El ANCLA sí se parsea aquí, pero es UNA ruta por unidad y no dos por
        // entrada: es lo que dice a qué provider preguntar.
        let mut plan: Vec<(Vec<crate::journal::JournalEntry>, Arc<dyn Provider>)> =
            Vec::with_capacity(units.len());
        for unit in units {
            let Some(first) = unit.first() else {
                continue; // imposible: `undo_units` no produce unidades vacías.
            };
            let anchor = wire_engine(&first.path)?;
            let provider = self.provider_for(&anchor).await?;
            plan.push((unit, provider));
        }
        let checker = self.policy_checker();

        let report_task = Arc::clone(&report);
        let owner = executor.clone();
        let en_curso = Arc::clone(&self.undo_en_curso);
        let key = "undo".to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::Undo,
            Priority::Normal,
            executor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    // El total son UNIDADES: un lote avanza el contador una vez,
                    // porque para el humano es un solo paso del undo.
                    let total = plan.len() as u64;
                    let task_id = ctx.progress.snapshot().task_id;
                    ctx.progress.update(|p| p.entries_total = Some(total));
                    // Un undo a la vez (#358), y la espera se puede cancelar:
                    // un undo en cola detrás de otro largo no retiene a nadie.
                    let _turno = tokio::select! {
                        g = en_curso.lock() => g,
                        () = ctx.cancel.cancelled() => return Err(Error::Cancelled),
                    };
                    // #358: lo elegido pudo deshacerlo otro undo entretanto. Se
                    // pregunta una vez, ya con el turno: desde aquí solo esta
                    // Task escribe compensaciones de undo.
                    let seqs: Vec<i64> = plan
                        .iter()
                        .flat_map(|(u, _)| u.iter().map(|e| e.seq))
                        .collect();
                    let deshechas = crate::undo::deshechas(&journal, &seqs).await?;
                    for (unit, provider) in plan {
                        if ctx.cancel.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        let unit = match crate::undo::vigencia(unit, &deshechas) {
                            crate::undo::Vigencia::Entera(u)
                            | crate::undo::Vigencia::EnParte(u) => u,
                            crate::undo::Vigencia::Deshecha => {
                                tracing::info!(
                                    %task_id,
                                    "unidad ya deshecha por otro undo; se salta"
                                );
                                ctx.progress.update(|p| p.entries_done += 1);
                                continue;
                            }
                            // Un lote a medio deshacer por otro undo: ni se
                            // sigue ni se salta — se para, como en un drift.
                            crate::undo::Vigencia::Parada(seq) => {
                                report_task.lock().expect("undo report lock").blocked =
                                    Some((seq, Error::PlanStale));
                                break;
                            }
                        };
                        // La puerta, AQUÍ y por unidad (#171). Dos cosas
                        // cambian respecto a preguntarla toda por adelantado:
                        // el trabajo de parsear va dentro de la Task, y el
                        // veredicto es el de AHORA — un scope que vence a
                        // mitad lo ve esta unidad, no una foto de hace media
                        // hora.
                        //
                        // Y una denegación NO mata el undo: bloquea SU unidad,
                        // deja fila en el informe y sigue. Es la misma
                        // decisión que el ejecutor hacia delante tomó en la
                        // tarea 9 —`Deny` y `Ask` son fila, no modal— y por el
                        // mismo motivo: un plan de medio millón de pasos no se
                        // puede parar en seco por uno. `blocked` sigue
                        // significando lo que significaba: el LIFO paró por
                        // DRIFT, y el árbol quedó consistente.
                        let denegada = undo_unit_denial(&checker, &ctx.actor, &unit).await;
                        if let Some(err) = denegada {
                            if let Some(first) = unit.first() {
                                let mut r = report_task.lock().expect("undo report lock");
                                r.denied_total = r.denied_total.saturating_add(1);
                                if r.denied.len() < crate::undo::UNDO_MAX_DENIED_REPORTED {
                                    r.denied.push((first.seq, err));
                                }
                            }
                            ctx.progress.update(|p| p.entries_done += 1);
                            continue;
                        }
                        // `undone` sí cuenta ENTRADAS: es lo que la unidad
                        // deshizo del journal, y un lote deshace las suyas.
                        let members = unit.len() as u64;
                        // Qué undo le toca a la unidad lo decide `revert_unit`,
                        // por la FORMA de sus entradas: una suelta, un lote de
                        // renombrados (entero o nada) o uno de sincronización
                        // (lo que se pueda, nombrando lo que no).
                        let outcome = crate::undo::revert_unit(
                            &*provider,
                            &journal,
                            &unit,
                            &ctx.actor,
                            &ctx.cancel,
                            task_id,
                            &report_task,
                        )
                        .await?;
                        match outcome {
                            crate::undo::Reverted::Done => {
                                report_task.lock().expect("undo report lock").undone += members;
                            }
                            // La unidad ya repartió sus entradas entre los
                            // contadores (un lote de sync revierte parte y
                            // salta parte): sumar `members` aquí contaría como
                            // deshecho lo que no volvió.
                            crate::undo::Reverted::Accounted => {}
                            crate::undo::Reverted::SkippedIrreversible => {
                                report_task
                                    .lock()
                                    .expect("undo report lock")
                                    .skipped_irreversible += 1;
                            }
                            crate::undo::Reverted::SkippedNoTrash => {
                                report_task
                                    .lock()
                                    .expect("undo report lock")
                                    .skipped_created_no_trash += 1;
                            }
                            // Se cuenta y se SIGUE, al revés que un bloqueo
                            // (#371): el nodo lo cambió el lector, no es una
                            // divergencia sin explicar, y pararlo todo por un
                            // fichero que él mismo editó dejaría sin deshacer
                            // el resto de la copia.
                            crate::undo::Reverted::SkippedNotOurs => {
                                report_task
                                    .lock()
                                    .expect("undo report lock")
                                    .skipped_not_ours += 1;
                            }
                            crate::undo::Reverted::Blocked { seq, error } => {
                                report_task.lock().expect("undo report lock").blocked =
                                    Some((seq, error));
                                break; // estricto: para en el primer bloqueo.
                            }
                            // El árbol NO volvió: la Task FALLA. `Completed`
                            // promete un árbol restaurado y aquí no lo está;
                            // el paso que se quedó puesto va con nombres en
                            // `UndoReport::batch_stuck`.
                            crate::undo::Reverted::Stuck { seq, error } => {
                                report_task.lock().expect("undo report lock").blocked =
                                    Some((seq, error.clone()));
                                return Err(error);
                            }
                        }
                        ctx.progress.update(|p| p.entries_done += 1);
                    }
                    Ok(())
                })
            }),
        );
        {
            let mut ring = self.undo_reports.lock().expect("undo_reports lock sano");
            ring.push_back((handle.id(), owner, Arc::clone(&report)));
            evict_undo_reports(&mut ring);
        }
        Ok((handle, report))
    }

    /// Suelta el informe de un undo cuyo id no llegó a nadie.
    ///
    /// El engine retiene el informe al LANZAR la Task, y el daemon puede
    /// contestar después OVERLOADED sin entregar el id. Ese informe ya no
    /// tiene a quién servirse, y guardarlo solo dejaba en el anillo un id que
    /// cualquier otra conexión humana podía pedir por enumeración.
    pub fn forget_undo_report(&self, task_id: TaskId) {
        self.undo_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(id, _, _)| *id != task_id);
    }

    /// El informe de un undo (`policy.undo_report`, #71): snapshot, definitivo
    /// cuando la Task es terminal. `None` si ese id no fue un undo o el anillo
    /// ya lo desalojó ([`UNDO_REPORTS_MAX`]). El actor es quien lo ejecutó: el
    /// daemon lo necesita para decidir quién puede leerlo.
    ///
    /// # Panics
    /// Nunca en la práctica: el lock del informe sólo se envenena si la Task
    /// entra en pánico a mitad de escribirlo, y se lee igual.
    #[must_use]
    pub fn undo_report(
        &self,
        task_id: TaskId,
    ) -> Option<(crate::journal::Actor, crate::UndoReport)> {
        let ring = self
            .undo_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter()
            .find(|(id, _, _)| *id == task_id)
            .map(|(_, owner, r)| {
                (
                    owner.clone(),
                    r.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            })
    }
}

/// Forma de un `VPath` para SPANS de tracing: `display_lossy`, salvo que el
/// userinfo de la authority contenga `:` — un password inline en la URL
/// (`sftp://u:pass@h`) se rechaza al parsear la conexión, pero el span se
/// abre ANTES: jamás debe llegar a un log (regla 10). `pub(crate)`: también
/// redacta los logs de `index_embed`.
pub(crate) fn span_path(p: &VPath) -> String {
    match p.authority() {
        Some(a) if a.rsplit_once('@').is_some_and(|(ui, _)| ui.contains(':')) => {
            format!("<{} ***>", p.scheme())
        }
        _ => p.display_lossy().clone(),
    }
}

/// Traduce un fallo de [`Spool::open`](crate::sync::Spool::open) a la taxonomía
/// del wire.
///
/// Las tres formas de «no hay plan vivo con ese hash» —no está, caducó, está
/// manipulado— son la MISMA respuesta para el cliente, y es una respuesta útil:
/// que vuelva a planificar. Solo un fallo de I/O de verdad es culpa del daemon.
fn spool_open_error(e: &crate::sync::SpoolError) -> Error {
    if e.is_stale() {
        Error::PlanStale
    } else {
        Error::Io { retryable: false }
    }
}

/// Lo mismo, pero A MITAD de la ejecución: el fichero se truncó o se editó
/// después de abrirlo.
///
/// Aquí ya no se puede contestar `PlanStale` —la Task existe y ha escrito— así
/// que sale como un fallo de la Task. Lo aplicado hasta ese punto se queda
/// journalizado bajo su lote y es deshacible.
fn spool_read_error(e: &crate::sync::SpoolError) -> Error {
    match e {
        // Un fichero que este binario escribió hace minutos y que deja de
        // parsearse a mitad no es un fallo del daemon: es un fichero truncado o
        // tocado. `Io` lo dice y `Internal` lo enterraría como avería nuestra.
        crate::sync::SpoolError::Io(_) | crate::sync::SpoolError::Malformed(_) => {
            Error::Io { retryable: false }
        }
        _ => Error::Internal { panic: false },
    }
}

/// ¿Está `path` EN `root` o por debajo?
///
/// Delegado en [`RelPath::under`] (`norte-proto`), que hace la misma
/// comparación por segmentos y bytes crudos (regla dura 1) y es la única
/// implementación desde #172 — antes había tres copias hechas a mano. La raíz
/// misma SÍ cuenta como contenida, que es lo que este llamante necesita (ver
/// [`structural_overlap`] y [`folded_overlap`]).
fn is_at_or_under(root: &VPath, path: &VPath) -> bool {
    RelPath::under(root, path).is_some()
}

/// Cómo se solapan dos raíces MIRÁNDOLAS, sin tocar el disco.
///
/// Los tres casos son distintos y ninguno es un caso degenerado de otro: «son el
/// mismo árbol» no es «una está dentro de la otra», y es justo la frase que un
/// frontend pinta. `None` = no se solapan estructuralmente, que **no** es lo
/// mismo que no solaparse (ver [`Engine::sync_plan_as`]).
fn structural_overlap(source: &VPath, dest: &VPath) -> Option<norte_proto::RootOverlap> {
    use norte_proto::RootOverlap;
    if source == dest {
        Some(RootOverlap::Same)
    } else if is_at_or_under(dest, source) {
        Some(RootOverlap::SourceInsideDest)
    } else if is_at_or_under(source, dest) {
        Some(RootOverlap::DestInsideSource)
    } else {
        None
    }
}

/// [`is_at_or_under`] con la clave que EMPAREJA los nombres, en vez de con sus
/// bytes.
///
/// Misma forma —scheme, authority y después segmento a segmento— y una sola
/// diferencia: cada segmento se compara por su
/// [`key_for`](norte_compare::key_for), que es exactamente la clave con la que
/// `norte-compare` decide que dos nombres son el MISMO nombre. Reutilizada y no
/// reescrita: una tercera copia de la tabla de plegado sería una tercera
/// respuesta a «¿colisionan estos dos nombres?» (#151, #129).
fn folded_is_at_or_under(root: &VPath, path: &VPath, sides: norte_compare::Sides) -> bool {
    if path.scheme() != root.scheme() || path.authority() != root.authority() {
        return false;
    }
    let mut rest = path.segments();
    root.segments().all(|segment| {
        rest.next().is_some_and(|other| {
            norte_compare::key_for(other, sides) == norte_compare::key_for(segment, sides)
        })
    })
}

/// [`structural_overlap`] sobre un par de raíces que NO distingue caja.
///
/// Existe porque la comprobación literal no caza la contención que un volumen
/// que pliega sí ve: sobre APFS o NTFS, `source=/Data` y `dest=/data/backup` no
/// son iguales byte a byte, no cuelga una de la otra byte a byte, y sus
/// [`NodeId`](norte_vfs::NodeId) son distintos porque son directorios
/// distintos — pasan las tres puertas, y después el plan copia un árbol dentro
/// de sí mismo. El guard del walk compara igual y tampoco lo ve.
///
/// Se aplica cuando ALGUNO de los dos providers no declara
/// [`CapabilityFlags::CASE_SENSITIVE`](norte_proto::CapabilityFlags::CASE_SENSITIVE),
/// que es el mismo criterio con el que
/// [`Sides`](norte_compare::Sides) decide plegar una clave de emparejamiento: un
/// lado que no distingue caja no puede sostener las dos grafías, así que
/// comparar CONTRA él es plegar aunque el otro sea ext4. Y es plegado de caja de
/// verdad, no un `to_lowercase` (los 22 code points de #129).
///
/// Con el plegado viene la normalización a NFC, porque es la misma clave: sobre
/// un volumen que pliega caja, `/Café` NFC y `/cafe\u{301}` NFD nombran el mismo
/// directorio para cualquier propósito que le importe a esta comprobación.
fn folded_overlap(
    source: &VPath,
    dest: &VPath,
    sides: norte_compare::Sides,
) -> Option<norte_proto::RootOverlap> {
    use norte_proto::RootOverlap;
    let source_in_dest = folded_is_at_or_under(dest, source, sides);
    let dest_in_source = folded_is_at_or_under(source, dest, sides);
    match (source_in_dest, dest_in_source) {
        // Cada una dentro de la otra solo puede ser la misma ruta plegada.
        (true, true) => Some(RootOverlap::Same),
        (true, false) => Some(RootOverlap::SourceInsideDest),
        (false, true) => Some(RootOverlap::DestInsideSource),
        (false, false) => None,
    }
}

/// ¿Son las dos raíces el MISMO directorio, se escriban como se escriban?
///
/// Un `stat` por raíz, una vez por plan. Tres cosas, todas deliberadas:
///
/// - **Solo si el provider es el MISMO objeto.** Un [`NodeId`](norte_vfs::NodeId)
///   de dos backends distintos no es comparable —el índice de un provider
///   sintético y un inodo de ext4 pueden coincidir sin tener nada que ver— y una
///   coincidencia falsa aquí rechaza un plan legítimo.
/// - **[`FollowLinks::Yes`](norte_vfs::FollowLinks::Yes)**, porque el caso que
///   esto existe para coger es exactamente el de una raíz que es un symlink: con
///   la identidad del propio enlace, `/data` y `/srv/data` responden distinto y
///   la comprobación no sirve de nada. Listar un directorio atraviesa el enlace
///   igual, así que es la identidad que el walk va a recorrer de verdad.
/// - **Un error o un `None` NO son un solape.** `node_id` es `None` en SFTP y en
///   FTP (residual conocido), y un `NotFound` significa que la raíz no está —
///   cosa que la comparación de debajo dirá con una fila de error, como hace hoy
///   `fs.compare`, en vez de matar la petición con una taxonomía distinta.
async fn same_node(
    source_provider: &Arc<dyn Provider>,
    source: &VPath,
    dest_provider: &Arc<dyn Provider>,
    dest: &VPath,
) -> bool {
    use norte_vfs::FollowLinks;
    if !Arc::ptr_eq(source_provider, dest_provider) {
        return false;
    }
    let a = source_provider.node_id(source, FollowLinks::Yes).await;
    let b = dest_provider.node_id(dest, FollowLinks::Yes).await;
    match (a, b) {
        (Ok(Some(a)), Ok(Some(b))) => a == b,
        _ => false,
    }
}

/// Mapea un rechazo del gate de IA a [`Error::PolicyDenied`] con el
/// vocabulario CERRADO del wire (M4-A2/IA-2): la categoría, jamás el detalle
/// de la config. Compartido por `ai_rename_plan` e `index_embed_as`.
fn ai_denied_to_error(reason: &crate::ai::AiDenied) -> Error {
    Error::PolicyDenied {
        rule: match reason {
            crate::ai::AiDenied::Disabled => "ai-disabled",
            crate::ai::AiDenied::LocalOnly => "ai-local-only",
            crate::ai::AiDenied::DeniedPath => "ai-denied-path",
        }
        .to_owned(),
    }
}

/// La CATEGORÍA de un fallo de IA, para la métrica. Nunca su texto: el
/// `Display` de un `Protocol` lleva el fragmento que motivó el rechazo, y ese
/// fragmento lo escribió el modelo sobre nombres del usuario —o es un nombre
/// real del directorio, en el error de colisión—.
fn categoria_de_ai(e: &norte_ai::AiError) -> &'static str {
    use norte_ai::AiError as A;
    match e {
        A::Protocol(_) => "parse",
        A::Auth => "auth",
        A::RateLimited { .. } => "rate_limit",
        A::Transport(_) => "transport",
        A::Cancelled => "cancelada",
        A::Unsupported => "no_soportado",
        _ => "otro",
    }
}

/// Mapea un [`norte_ai::AiError`] a la taxonomía del wire (M4-A2). El wire
/// lleva la categoría; al log va lo que se puede decir sin repetir datos del
/// usuario.
///
/// **`Protocol` no se loguea con su texto.** Su `Display` lleva el fragmento
/// que motivó el rechazo, y en el camino del rename ese fragmento puede ser
/// un nombre que escribió el modelo —o uno REAL del directorio, en el error
/// de colisión de `validate_rename_reply`—. Es contenido del usuario, y un
/// log del daemon o un volcado de diagnóstico no es sitio para él (regla 10).
/// Lo que sí se dice es que fue de protocolo: la categoría es lo que un log
/// necesita para que alguien sepa dónde mirar.
///
/// El resto de variantes sí llevan texto: `Http` es el estado y el cuerpo del
/// proveedor, que no ha visto ningún nombre del usuario.
pub(crate) fn ai_to_proto_error(e: &norte_ai::AiError) -> Error {
    use norte_ai::AiError as A;
    match e {
        A::Auth => Error::PermissionDenied,
        A::Cancelled => Error::Cancelled,
        A::Unsupported => Error::Unsupported,
        A::RateLimited { .. } | A::Transport(_) => Error::ProviderUnavailable { retryable: true },
        A::Protocol(_) => {
            tracing::warn!("proveedor de IA: respuesta que no cumple el formato");
            Error::Internal { panic: false }
        }
        // Http y cualquier variante futura (AiError es non_exhaustive):
        // categoría gruesa, detalle al log.
        _ => {
            tracing::warn!(error = %e, "proveedor de IA: respuesta o estado inesperado");
            Error::Internal { panic: false }
        }
    }
}

/// Cuántas rutas de una operación llegan al frontend que la aprueba.
///
/// Un tope de PRESENTACIÓN, no de decisión: la policy se evalúa siempre sobre
/// la lista completa. Ver el comentario en [`Engine::gate`].
const APPROVAL_PATHS_SHOWN: usize = 32;

/// Tope de entradas de directorio que un rename por lotes planifica.
///
/// La cota de parejas (`FS_RENAME_BATCH_MAX_PAIRS`) no acota la otra dimensión,
/// y el planificador es lineal en AMBAS: una clave de comparación por entrada
/// del listado, más dos índices. `fs.rename_batch_plan` es respuesta DIRECTA
/// (ADR 0042): sin Task, sin token de cancelación y dentro del despacho, así
/// que un directorio de millones de entradas sería trabajo sin cota ni forma de
/// pararlo. Un directorio que un humano va a revisar entrada por entrada cabe
/// de sobra aquí; por encima, `LimitExceeded` es honesto y barato.
pub const RENAME_BATCH_MAX_LISTING: usize = 100_000;

/// Devuelve el derecho a aplicar un plan si el `sync.apply` que lo cobró se
/// abandona a mitad (ver el comentario de [`Engine::sync_apply_as`]).
///
/// Solo suelta la marca en memoria: es un `Drop`, así que no puede esperar a un
/// borrado de fichero, y el fichero lo recogen el TTL, el barrido de arranque o
/// el cierre de la conexión.
struct ApplyClaim<'a> {
    spool: &'a crate::sync::Spool,
    conn_id: u64,
    plan_hash: &'a PlanHash,
    armed: bool,
}

impl Drop for ApplyClaim<'_> {
    fn drop(&mut self) {
        if self.armed {
            tracing::debug!(
                conn_id = self.conn_id,
                plan_hash = self.plan_hash.as_str(),
                "sync.apply abandonado antes de crear la Task: se devuelve el plan"
            );
            self.spool.abandon(self.conn_id, self.plan_hash);
        }
    }
}

/// Una entrada de un anillo de informes: la Task, el ACTOR que la pidió (para
/// que quien sirva el informe por el wire pueda decidir si el que pregunta
/// podía ver esa task) y el informe VIVO, que la Task sigue rellenando.
type ReportEntry<T> = (TaskId, crate::journal::Actor, Arc<std::sync::Mutex<T>>);

/// El anillo de informes de lotes de renames ([`Engine::rename_batch_report`]).
type BatchReportEntry = ReportEntry<crate::rename::BatchReport>;

/// El anillo de informes de aplicaciones de plan ([`Engine::sync_report`]).
type SyncReportEntry = ReportEntry<norte_proto::methods::SyncReportResult>;

/// El anillo de informes de `archive.test` ([`Engine::archive_test_report`]).
type TestReportEntry = ReportEntry<norte_proto::methods::ArchiveTestResult>;

/// El anillo de informes de undo ([`Engine::undo_report`]).
type UndoReportEntry = ReportEntry<crate::UndoReport>;

/// El anillo de informes de `archive.pack` ([`Engine::archive_pack_report`]).
type PackReportEntry = ReportEntry<norte_proto::methods::ArchivePackReportResult>;

/// El anillo de informes de `fs.checksum` ([`Engine::checksum_report`]).
type ChecksumReportEntry = ReportEntry<norte_proto::methods::FsChecksumReportResult>;

/// Entrada del anillo de `fs.dir_usage` (0.75.0, fase 4).
type DirUsageReportEntry = ReportEntry<norte_proto::methods::FsDirUsageReportResult>;

/// Poda el anillo de informes de lote hasta sus topes, sacrificando SIEMPRE lo
/// que menos hace falta.
///
/// Dos reglas, y las dos existen porque este anillo es el ÚNICO canal por el
/// que alguien se entera de que su directorio quedó a medio renombrar:
///
/// 1. **Sub-tope por clase** ([`BATCH_REPORTS_AGENTS_MAX`], mismo patrón que el
///    de scopes de M3-3b): los lotes de agentes no agotan el anillo. Sin él,
///    `fs.rename_batch` es alcanzable por un agente con scope y 33 lotes
///    triviales desalojan el informe que el humano no ha leído todavía —
///    incluido el del lote que ese mismo agente dejó aplicado a medias.
/// 2. **Se desaloja primero lo que no cuenta nada**: entre dos informes, se tira
///    el que dice que todo fue bien antes que el que nombra un paso atascado.
///    Un informe limpio es reconstruible mirando el directorio; uno atascado no.
///
/// La ANTIGÜEDAD sigue siendo el criterio dentro de cada categoría, y si todo lo
/// retenido es ruidoso se tira lo más viejo igualmente: la memoria de un daemon
/// de meses no puede crecer con cada lote, ni siquiera con los malos.
fn evict_batch_reports(ring: &mut std::collections::VecDeque<BatchReportEntry>) {
    evict_reports(
        ring,
        BATCH_REPORTS_MAX,
        BATCH_REPORTS_AGENTS_MAX,
        // ¿Este informe tiene algo que solo él sabe? Se evalúa AHORA y no al
        // insertarlo: al insertarlo todo informe está vacío — es al desalojar
        // cuando los viejos ya terminaron y se sabe cuáles duelen.
        |r: &crate::rename::BatchReport| {
            r.stuck.is_some() || r.uncertain.is_some() || r.compensations_lost > 0
        },
    );
}

/// Poda el anillo de informes de `sync.apply` con las mismas dos reglas.
///
/// Aquí «ruidoso» es un informe con FALLOS: un plan que se aplicó entero deja
/// el árbol como el diálogo prometió y su informe es reconstruible mirándolo,
/// mientras que uno con pasos caídos nombra ficheros que no se copiaron y un
/// lote del journal donde buscarlos. `failures` está recortado y `failed` no,
/// así que el contador es el que decide.
fn evict_sync_reports(ring: &mut std::collections::VecDeque<SyncReportEntry>) {
    evict_reports(
        ring,
        SYNC_REPORTS_MAX,
        SYNC_REPORTS_AGENTS_MAX,
        |r: &norte_proto::methods::SyncReportResult| r.failed > 0,
    );
}

/// El desalojo COMPARTIDO de los dos anillos de informes, con las dos reglas
/// que ambos necesitan porque los dos son el ÚNICO canal por el que alguien se
/// entera de que su árbol quedó a medias:
///
/// 1. **Sub-tope por clase** (mismo patrón que el de scopes de M3-3b): los
///    informes de agentes ocupan como mucho `agents_max` del anillo, así que
///    siempre quedan `max - agents_max` sitios que un agente no puede llenar.
///    Sin él, un agente con scope y 33 operaciones triviales desaloja el informe
///    que el humano no ha leído todavía — incluido el de la operación que ese
///    mismo agente dejó a medias. Lo que el sub-tope da es un SUELO, no
///    inmunidad: por encima de él la segunda pasada desaloja por antigüedad sin
///    mirar el dueño, así que un agente sí puede empujar fuera informes humanos
///    viejos y limpios. Los ruidosos sobreviven, que es lo que importa.
/// 2. **Se desaloja primero lo que no cuenta nada**: entre dos informes, se tira
///    el que dice que todo fue bien antes que el que nombra algo roto. Un
///    informe limpio es reconstruible mirando el árbol; uno roto no.
///
/// La ANTIGÜEDAD sigue siendo el criterio dentro de cada categoría, y si todo lo
/// retenido es ruidoso se tira lo más viejo igualmente: la memoria de un daemon
/// de meses no puede crecer con cada operación, ni siquiera con las malas.
fn evict_reports<T>(
    ring: &mut std::collections::VecDeque<ReportEntry<T>>,
    max: usize,
    agents_max: usize,
    loud: fn(&T) -> bool,
) {
    fn drop_one<T>(
        ring: &mut std::collections::VecDeque<ReportEntry<T>>,
        agents_only: bool,
        loud: fn(&T) -> bool,
    ) {
        let candidates = || {
            ring.iter()
                .enumerate()
                .filter(|(_, e)| !agents_only || !matches!(e.1, crate::journal::Actor::User))
        };
        let quiet = candidates()
            .find(|(_, e)| !loud(&e.2.lock().expect("report lock")))
            .map(|(i, _)| i);
        let victim = quiet.or_else(|| candidates().map(|(i, _)| i).next());
        if let Some(i) = victim {
            ring.remove(i);
        }
    }
    while ring
        .iter()
        .filter(|e| !matches!(e.1, crate::journal::Actor::User))
        .count()
        > agents_max
    {
        drop_one(ring, true, loud);
    }
    while ring.len() > max {
        drop_one(ring, false, loud);
    }
}

/// Cuántos informes de lote retiene [`Engine::rename_batch_report`].
///
/// Anillo, no mapa: un informe se pide una vez, justo después del terminal de
/// su Task, y el que nadie recoja tiene que caducar solo o el daemon acumula
/// memoria por cada lote que corrió en su vida. El mismo criterio (y el mismo
/// orden de magnitud) que el anillo de informes de undo ([`UNDO_REPORTS_MAX`]).
pub const BATCH_REPORTS_MAX: usize = 32;

/// Cuántos de los [`BATCH_REPORTS_MAX`] puede ocupar el conjunto de los actores
/// NO humanos. Sub-tope por clase, como el de scopes por conexión de M3-3b: el
/// humano conserva su margen pase lo que pase al otro lado. Ver
/// [`evict_batch_reports`].
pub const BATCH_REPORTS_AGENTS_MAX: usize = 16;

/// Cuántos informes de aplicación retiene [`Engine::sync_report`]. Mismo
/// criterio y mismo orden de magnitud que [`BATCH_REPORTS_MAX`]: se pide una
/// vez, justo después del terminal de su Task, y el que nadie recoja tiene que
/// caducar solo.
pub const SYNC_REPORTS_MAX: usize = 32;

/// El sub-tope por clase del anillo de `sync.apply`, gemelo de
/// [`BATCH_REPORTS_AGENTS_MAX`]. No se reexporta en la raíz del crate por lo
/// mismo que su gemelo: el tope que un cliente necesita conocer es el total.
pub(crate) const SYNC_REPORTS_AGENTS_MAX: usize = 16;

/// Cuántos informes de `archive.test` se retienen. El tercero de la familia,
/// mismo criterio.
pub const TEST_REPORTS_MAX: usize = 32;

/// Sub-tope por clase del anillo de `archive.test`.
pub(crate) const TEST_REPORTS_AGENTS_MAX: usize = 16;

/// Cuántos informes de undo retiene [`Engine::undo_report`]. Ocho, el número
/// que tenía cuando vivía en el daemon: los undos son raros y los pide una
/// persona.
pub const UNDO_REPORTS_MAX: usize = 8;

/// Sub-tope por clase del anillo de undo. Hoy todo undo lo ejecuta el humano,
/// así que no muerde; está para que eso siga siendo cierto el día que deje de
/// serlo — `Engine::undo_session` acepta cualquier actor —, igual que en los
/// anillos gemelos: la mitad.
pub(crate) const UNDO_REPORTS_AGENTS_MAX: usize = 4;

/// Desalojo del anillo de undo: primero lo que no cuenta nada. Un undo que
/// paró, se quedó a medias, perdió compensaciones o tuvo unidades denegadas
/// es el único sitio donde el humano se entera de que el árbol no volvió
/// entero.
fn evict_undo_reports(ring: &mut std::collections::VecDeque<UndoReportEntry>) {
    evict_reports(
        ring,
        UNDO_REPORTS_MAX,
        UNDO_REPORTS_AGENTS_MAX,
        |r: &crate::UndoReport| {
            r.blocked.is_some()
                || r.batch_stuck.is_some()
                || r.compensations_lost > 0
                || r.denied_total > 0
        },
    );
}

/// Desalojo del anillo de `archive.test`: lo que no cuenta nada —un archivo
/// que pasó entero— se sacrifica antes que un informe con fallos, que es el
/// único sitio donde alguien puede enterarse de qué entrada está corrupta.
fn evict_test_reports(ring: &mut std::collections::VecDeque<TestReportEntry>) {
    evict_reports(
        ring,
        TEST_REPORTS_MAX,
        TEST_REPORTS_AGENTS_MAX,
        |r: &norte_proto::methods::ArchiveTestResult| !r.failed.is_empty() || r.truncated,
    );
}

/// Tope del anillo de `archive.pack`. Mismo número que el de `archive.test` y
/// **atado a propósito**: son informes de la misma familia y del mismo tamaño,
/// y que compartan cota es una decisión, no el reflejo de haber copiado la
/// constante de al lado. Cambiar una y no la otra debería costar escribir por
/// qué.
pub(crate) const PACK_REPORTS_MAX: usize = TEST_REPORTS_MAX;

/// Sub-tope por clase del anillo de `archive.pack`, atado igual.
pub(crate) const PACK_REPORTS_AGENTS_MAX: usize = TEST_REPORTS_AGENTS_MAX;

/// Desalojo del anillo de `archive.pack` (#250), con la misma regla que sus
/// tres gemelos: primero cae lo que no cuenta nada. Aquí «no cuenta nada» es
/// un archivo cuyos nombres viajan todos intactos, y lo que se protege es el
/// informe que dice que alguno no.
fn evict_pack_reports(ring: &mut std::collections::VecDeque<PackReportEntry>) {
    evict_reports(
        ring,
        PACK_REPORTS_MAX,
        PACK_REPORTS_AGENTS_MAX,
        |r: &norte_proto::methods::ArchivePackReportResult| !r.risky.is_empty() || r.truncated,
    );
}

/// Tope del anillo de `fs.checksum`. Mismo número que sus gemelos y con
/// nombre propio: atarlo al de `archive.pack` haría que tocar el tope de
/// empaquetar moviera este sin que nadie lo pidiera.
pub(crate) const CHECKSUM_REPORTS_MAX: usize = TEST_REPORTS_MAX;

/// Sub-tope por clase del anillo de `fs.checksum`.
pub(crate) const CHECKSUM_REPORTS_AGENTS_MAX: usize = TEST_REPORTS_AGENTS_MAX;

/// Desalojo del anillo de `fs.checksum` (#311), con la misma regla que sus
/// gemelos: primero cae lo que no cuenta nada.
///
/// Aquí «cuenta algo» es un informe con alguna ruta SIN digest: el que dice que
/// todo se pudo leer se reconstruye volviendo a pedirlo, y el que dice que uno
/// no se pudo leer es el que alguien está buscando. Es la inversión respecto a
/// sus gemelos —allí «ruidoso» es un fallo, aquí también, pero el informe
/// limpio es el que carga los digests que costaron horas de I/O— y se conserva
/// porque el que falta se recalcula y el motivo del fallo no se recuerda solo.
fn evict_checksum_reports(ring: &mut std::collections::VecDeque<ChecksumReportEntry>) {
    evict_reports(
        ring,
        CHECKSUM_REPORTS_MAX,
        CHECKSUM_REPORTS_AGENTS_MAX,
        |r: &norte_proto::methods::FsChecksumReportResult| {
            r.entries.iter().any(|e| e.digest.is_none())
        },
    );
}

/// Tope del anillo de `fs.dir_usage`. Mismo número que sus gemelos y con nombre
/// propio, por lo mismo que el de `fs.checksum`: atarlo al de al lado haría que
/// tocar aquel moviera este sin que nadie lo pidiera.
pub(crate) const DIR_USAGE_REPORTS_MAX: usize = TEST_REPORTS_MAX;

/// Sub-tope por clase del anillo de `fs.dir_usage`.
pub(crate) const DIR_USAGE_REPORTS_AGENTS_MAX: usize = TEST_REPORTS_AGENTS_MAX;

/// Desalojo del anillo de `fs.dir_usage` (fase 4), con la regla de la familia:
/// primero cae lo que no cuenta nada.
///
/// «Cuenta algo» es un mapa que NO es el mapa entero: uno que se quedó sin
/// listar (`listed` en `false`), uno con hijos que no se dejaron medir del todo
/// (`partial`), o uno al que el tope le comió nombres (`omitted`). Un mapa
/// completo se reconstruye volviendo a medir; lo que no se recuerda solo es
/// POR QUÉ este está incompleto.
fn evict_dir_usage_reports(ring: &mut std::collections::VecDeque<DirUsageReportEntry>) {
    evict_reports(
        ring,
        DIR_USAGE_REPORTS_MAX,
        DIR_USAGE_REPORTS_AGENTS_MAX,
        |r: &norte_proto::methods::FsDirUsageReportResult| {
            !r.listed || r.omitted > 0 || r.children.iter().any(|c| c.partial)
        },
    );
}

/// Todos los nombres base de `dir`, en bytes crudos (regla 1).
///
/// MATERIALIZA el listado entero: el planificador necesita ver el directorio
/// completo para juzgar las colisiones, y un listado a medias produciría
/// veredictos a medias. Por eso un error a mitad de stream PROPAGA en vez de
/// devolver lo que se alcanzó a leer.
pub(crate) async fn list_base_names(
    provider: &dyn Provider,
    dir: &VPath,
) -> Result<Vec<Vec<u8>>, Error> {
    use futures::StreamExt;
    let mut stream = provider.list(dir).await?;
    let mut names = Vec::with_capacity(64);
    while let Some(item) = stream.next().await {
        let entry = item?;
        if names.len() >= RENAME_BATCH_MAX_LISTING {
            tracing::warn!(
                max = RENAME_BATCH_MAX_LISTING,
                "directorio por encima del tope planificable para un rename por lotes",
            );
            return Err(Error::LimitExceeded {
                limit: Error::LIMIT_ENTRIES.into(),
            });
        }
        if let Some(n) = entry.path.file_name() {
            names.push(n.as_bytes().to_vec());
        }
    }
    Ok(names)
}

/// Qué evaluar en la policy para deshacer UNA unidad de undo.
///
/// Reversa de un `Created` BORRA → `Delete` (una ruta), y desde #65 va SIEMPRE
/// a papelera (o se salta): se gatea como `Trash`, no como `Permanent` — un
/// deny de «solo-permanente» no debe parar el LIFO por una reversa que jamás
/// borra permanente. `rename_back` / `restore_trash` REUBICAN → `Move` con DOS
/// endpoints (origen y destino de la restauración): ambos deben pasar el gate,
/// igual que un Move normal (security M2). El segundo endpoint es `path_to`
/// (rename) o `reversal_ref` (trash).
///
/// De un LOTE se recogen los endpoints de TODOS sus miembros y se pregunta UNA
/// vez POR CLASE de operación: la policy resuelve cada slice a lo más
/// restrictivo, así que un lote que roza un nombre denegado se deniega entero
/// y jamás a medias — la misma regla que [`Engine::rename_batch_as`] aplica en
/// la ida.
///
/// **Por clase, y no una sola pregunta con la clase más restrictiva.** Un lote
/// de renames trae la misma reversa en todas sus entradas, pero uno de
/// SINCRONIZACIÓN mezcla: restaurar lo enterrado es un `Move` y borrar lo
/// creado es un `Delete`. Fundirlos en `Delete` no era conservador —`delete` y
/// `move` son permisos INDEPENDIENTES en `OpSet` y en `policy.toml`, no uno
/// dentro del otro—, así que dejaba pasar bajo permiso de `delete` un `move`
/// que la policy deniega; y al revés, denegaba el lote entero por un miembro
/// cuya reversa nadie había prohibido.
///
/// `None` solo para una unidad vacía, que [`crate::undo::undo_units`] no
/// produce.
fn undo_gate_targets(unit: &[crate::journal::JournalEntry]) -> Result<Option<UndoGates>, Error> {
    let mut anchor: Option<VPath> = None;
    let mut moves: Vec<VPath> = Vec::new();
    let mut deletes: Vec<VPath> = Vec::new();
    let mut set_modes: std::collections::BTreeMap<u32, Vec<VPath>> =
        std::collections::BTreeMap::new();
    for e in unit {
        let path = wire_engine(&e.path)?;
        if anchor.is_none() {
            anchor = Some(path.clone());
        }
        match e.reversal.as_str() {
            // Una entrada `irreversible` no tiene reversa que ejecutar
            // (`revert_entry` la salta sin tocar nada), así que no hay puerta
            // que abrir. Gatearla dejaba que un `deny` sobre una ruta que este
            // undo NO va a tocar bloqueara la unidad entera y, con el LIFO
            // estricto, la sesión entera detrás: el secuestro que
            // `revert_sync_batch` existe para impedir, una capa más arriba.
            "irreversible" => {}
            "rename_back" => {
                moves.push(path);
                if let Some(to) = e.path_to.as_deref() {
                    moves.push(wire_engine(to)?);
                }
            }
            "restore_trash" => {
                moves.push(path);
                if let Some(from) = e.reversal_ref.as_deref() {
                    moves.push(wire_engine(from)?);
                }
            }
            // #314: la reversa de un cambio de permisos es OTRO cambio de
            // permisos, y `set-mode` es un permiso INDEPENDIENTE. Caía en el
            // comodín de abajo, y eso reproducía exactamente el bug que el
            // rustdoc de arriba cuenta para `delete`/`move`: un actor con
            // `delete` deshacía un chmod que la política no le concede, y uno
            // con `set-mode` no podía deshacer el suyo — y con el LIFO
            // estricto, eso bloquea la sesión entera detrás.
            // #314: el modo que la reversa VA A PONER viaja en `reversal_ref`,
            // y se agrupa por él: la pregunta que se le hace al humano dice
            // cuál es, así que una unidad que restaure dos modos distintos
            // tiene que preguntar dos veces en vez de enseñar uno por los dos.
            "set_mode_back" => {
                let modo = e
                    .reversal_ref
                    .as_deref()
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                set_modes.entry(modo).or_default().push(path);
            }
            // `delete`, y cualquier etiqueta que este core no conozca: la
            // desconocida no llega a actuar (`revert_entry` la bloquea), pero
            // se pregunta igual por la clase que MÁS quita.
            _ => deletes.push(path),
        }
    }
    if anchor.is_none() {
        return Ok(None);
    }
    let mut gates: Vec<(crate::policy::PolicyOp, Vec<VPath>)> = Vec::with_capacity(3);
    for (mode, paths) in set_modes {
        gates.push((
            // Deshacer NUNCA es recursivo, sea lo que fuera la ida: el diario
            // guarda una entrada por NODO, así que las rutas de esta pregunta
            // son exactamente las que se van a tocar. Poner `recursive: true`
            // aquí diría «y todo lo que cuelgue», que es más de lo que este
            // undo hace.
            crate::policy::PolicyOp::SetMode {
                mode,
                recursive: false,
                dir_mode: None,
            },
            paths,
        ));
    }
    if !deletes.is_empty() {
        gates.push((
            crate::policy::PolicyOp::Delete {
                mode: DeleteMode::Trash,
            },
            deletes,
        ));
    }
    if !moves.is_empty() {
        gates.push((crate::policy::PolicyOp::Move, moves));
    }
    Ok(Some(UndoGates { gates }))
}

/// Pregunta a la policy por UNA unidad del undo, y devuelve el motivo si la
/// deniega (#171).
///
/// Corre DENTRO de la Task: aquí es donde se paga el parseo de hasta
/// `unit.len() * 2` `VPath`s, que es lo que antes se hacía entero en el hilo
/// de quien llamaba, antes de que existiera la Task y sin nada que pudiera
/// cancelarlo.
///
/// Una ruta del journal que no parsea cuenta como denegación de SU unidad y de
/// nadie más: tumbar el undo entero por una fila rota le quitaría al humano
/// las demás, que están bien.
async fn undo_unit_denial(
    checker: &PolicyChecker,
    actor: &crate::journal::Actor,
    unit: &[crate::journal::JournalEntry],
) -> Option<Error> {
    let targets = match undo_gate_targets(unit) {
        Ok(Some(targets)) => targets,
        Ok(None) => return None,
        Err(e) => return Some(e),
    };
    for (undo_op, paths) in &targets.gates {
        let gate_paths: Vec<&VPath> = paths.iter().collect();
        if let Err(err) = checker.check(actor, *undo_op, &gate_paths).await {
            return Some(err);
        }
    }
    None
}

/// Lo que la policy tiene que aprobar antes de deshacer una unidad, y dónde
/// vive esa unidad.
struct UndoGates {
    /// Las puertas, por clase de operación. Vacío = la unidad no actúa (todas
    /// sus entradas son `irreversible`), y entonces no hay nada que preguntar.
    ///
    /// El provider ya NO sale de aquí: lo resuelve quien planifica, desde la
    /// primera entrada de la unidad (#171). Todas las rutas de una unidad
    /// viven en el mismo provider — lo comprueban `inverse_chain` para un lote
    /// de renombrados y `one_provider` para uno de sincronización — y la
    /// primera entrada sirve aunque la unidad entera sea `irreversible` y no
    /// gatee ninguna ruta.
    gates: Vec<(crate::policy::PolicyOp, Vec<VPath>)>,
}

/// Deja fuera los hits que caen en un subárbol excluido (#165). Separado de
/// [`Engine::index_query_as`] para poder probarlo sin índice y sin entorno:
/// las exclusiones reales las resuelve
/// [`crate::policy::walk_exclusions`] del proceso.
fn drop_excluded(
    hits: Vec<norte_index::IndexHit>,
    excluded: &[VPath],
) -> Vec<norte_index::IndexHit> {
    if excluded.is_empty() {
        return hits;
    }
    hits.into_iter()
        .filter(|h| !excluded.iter().any(|x| crate::policy::is_under(x, &h.path)))
        .collect()
}

/// Reconstruye un `VPath` desde los bytes `to_wire` del journal (undo M3-2).
fn wire_engine(bytes: &[u8]) -> Result<VPath, Error> {
    let s = std::str::from_utf8(bytes).map_err(|_| Error::InvalidPath)?;
    VPath::parse(s).map_err(|_| Error::InvalidPath)
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod batch_report_ring_tests {
    use super::*;
    use crate::journal::Actor;
    use crate::rename::{BatchReport, StuckStep};

    fn entry(id: u64, owner: Actor, loud: bool) -> BatchReportEntry {
        let mut r = BatchReport::default();
        if loud {
            r.stuck = Some(StuckStep {
                from: VPath::parse("mem:///a").expect("path"),
                to: VPath::parse("mem:///b").expect("path"),
                pair_index: 0,
                error: Error::Io { retryable: false },
                journalled: true,
                still_applied: 1,
            });
        }
        (TaskId::new(id), owner, Arc::new(std::sync::Mutex::new(r)))
    }

    fn agent() -> Actor {
        Actor::Agent {
            session: "s1".into(),
        }
    }

    fn ids(ring: &std::collections::VecDeque<BatchReportEntry>) -> Vec<u64> {
        ring.iter().map(|e| e.0.get()).collect()
    }

    /// Un informe que NOMBRA un paso atascado sobrevive a uno que dice que todo
    /// fue bien, aunque sea más viejo: el limpio se puede reconstruir mirando el
    /// directorio y el otro no.
    #[test]
    fn el_desalojo_sacrifica_primero_el_informe_que_no_cuenta_nada() {
        let mut ring: std::collections::VecDeque<BatchReportEntry> = (0..BATCH_REPORTS_MAX as u64)
            .map(|i| entry(i, Actor::User, i == 0))
            .collect();
        ring.push_back(entry(999, Actor::User, false));
        evict_batch_reports(&mut ring);
        assert_eq!(ring.len(), BATCH_REPORTS_MAX);
        assert!(ids(&ring).contains(&0), "el atascado se queda");
        assert!(!ids(&ring).contains(&1), "el limpio más viejo se va");
    }

    /// Sub-tope por clase: los lotes de un AGENTE no desalojan el informe que el
    /// humano todavía no ha leído — ni aunque el agente mande muchos más.
    #[test]
    fn los_lotes_de_un_agente_no_desalojan_el_informe_del_humano() {
        let mut ring: std::collections::VecDeque<BatchReportEntry> =
            std::collections::VecDeque::new();
        ring.push_back(entry(1, Actor::User, true));
        for i in 0..(BATCH_REPORTS_MAX as u64 * 2) {
            ring.push_back(entry(100 + i, agent(), false));
            evict_batch_reports(&mut ring);
        }
        assert!(ids(&ring).contains(&1), "el informe del humano sigue ahí");
        assert!(
            ring.iter().filter(|e| !matches!(e.1, Actor::User)).count() <= BATCH_REPORTS_AGENTS_MAX,
            "el sub-tope de agentes se respeta",
        );
    }

    /// Si TODO lo retenido es ruidoso se tira lo más viejo igualmente: la
    /// memoria de un daemon de meses no puede crecer ni con los lotes malos.
    #[test]
    fn con_todo_ruidoso_el_anillo_sigue_acotado() {
        let mut ring: std::collections::VecDeque<BatchReportEntry> =
            (0..(BATCH_REPORTS_MAX as u64 + 5))
                .map(|i| entry(i, Actor::User, true))
                .collect();
        evict_batch_reports(&mut ring);
        assert_eq!(ring.len(), BATCH_REPORTS_MAX);
        assert!(!ids(&ring).contains(&0), "se fue el más viejo");
    }
}

/// La papelera del DESTINO de un plan retenido, del mismo par de opciones del
/// que salió el [`DestTrash`](norte_proto::methods::DestTrash) que se le enseñó a quien lo aprobó (#170).
///
/// Una función y no una línea dentro de `sync_apply_opened` porque ahí manda dos
/// veces: el informe la lleva —para que «¿se puede devolver este lote?» se
/// conteste con el informe delante y sin haber conservado el `sync.plan_done`—
/// y el gate de borrado pregunta por la clase de borrado que de verdad va a
/// ocurrir. Las dos tienen que salir del MISMO sitio o el informe puede acabar
/// diciendo una papelera y el gate preguntando por otra.
fn plan_dest_trash(reader: &crate::sync::SpoolReader) -> norte_proto::methods::DestTrash {
    norte_proto::methods::DestTrash::of(
        reader.header().options.dest_has_trash,
        reader.header().options.dest_trash_restorable,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(wire: &str) -> norte_index::IndexHit {
        norte_index::IndexHit {
            path: VPath::parse(wire).expect("wire"),
            kind: norte_proto::EntryKind::File,
            size: None,
            mtime_ms: None,
        }
    }

    /// #165: el índice lo construye el humano y puede contener el directorio
    /// de estado del daemon; un agente que lo consulta no se lo lleva.
    #[test]
    fn los_hits_de_un_subarbol_protegido_no_salen() {
        let hits = vec![
            hit("file:///home/u/docs/carta.txt"),
            hit("file:///home/u/.config/norte/journal.db"),
            hit("file:///home/u/.config/norte"),
            hit("file:///home/u/.config/norte-backup/journal.db"),
        ];
        let excluido = vec![VPath::parse("file:///home/u/.config/norte").expect("wire")];
        let quedan: Vec<String> = drop_excluded(hits.clone(), &excluido)
            .into_iter()
            .map(|h| h.path.to_wire())
            .collect();
        assert_eq!(
            quedan,
            vec![
                "file:///home/u/docs/carta.txt".to_owned(),
                "file:///home/u/.config/norte-backup/journal.db".to_owned(),
            ]
        );
        // Sin exclusiones (el humano) no se cae ni una.
        assert_eq!(drop_excluded(hits, &[]).len(), 4);
    }

    /// #166: un `Engine` sin `with_policy` gatea con `AllowAll`, y eso no se
    /// distingue de una policy permisiva a propósito. El daemon avisa en el
    /// arranque, y para avisar necesita poder PREGUNTARLO.
    #[test]
    fn una_policy_por_omision_se_distingue_de_una_instalada() {
        let sin = Engine::new();
        assert!(
            !sin.has_explicit_policy(),
            "un Engine recién hecho no tiene policy instalada"
        );

        let con = Engine::new().with_policy(
            Arc::new(crate::policy::AllowAll),
            Arc::new(crate::approval::DenyAll),
        );
        assert!(
            con.has_explicit_policy(),
            "AllowAll instalada A PROPÓSITO sí cuenta como policy"
        );
    }
}
