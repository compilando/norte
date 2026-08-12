//! [`Engine`]: la API embebida del core (M0). El daemon JSON-RPC (M1)
//! envolverá esta misma API; los frontends no contienen lógica de negocio.

use std::sync::{Arc, RwLock};

use norte_proto::{
    CapabilityFlags, CollisionPolicy, DeleteMode, Entry, Error, ResumePolicy, Segment,
    SymlinkPolicy, TaskId, TaskKind, VPath, VerifyPolicy, methods::PlanHash,
};
use norte_vfs::{EntryStream, Provider};

use crate::observer::{MutationObserver, NoopObserver};
use crate::ops;
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
    /// Fuente de LECTURA del journal para el undo (M3-2). `None` = sin journal
    /// (el undo devuelve `Unsupported`). Es el MISMO objeto que `observer`
    /// cuando se construye con [`Self::with_journal`].
    journal: Option<Arc<crate::journal::SqliteJournal>>,
    /// Gate de policy consultado PRE-efecto en cada mutación (M3-3). Default
    /// [`AllowAll`](crate::policy::AllowAll): el engine embebido/humano no se
    /// sandboxea salvo que se instale una policy con [`Self::with_policy`].
    policy: Arc<dyn crate::policy::PolicyGate>,
    /// Resuelve un `Ask` de policy. Default [`DenyAll`](crate::approval::DenyAll)
    /// (headless fail-closed).
    approvals: Arc<dyn crate::approval::ApprovalResolver>,
    /// Límites anti-bomba de los providers archive compuestos (#95.2).
    /// Default [`norte_vfs_archive::Limits::default`]; el operador los baja
    /// vía [`Self::set_archive_limits`] ANTES de la primera navegación a un
    /// contenedor (los providers compuestos se cachean con los límites
    /// vigentes en su primer uso).
    archive_limits: RwLock<norte_vfs_archive::Limits>,
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
        Self {
            sessions: SessionPool::new(),
            connector: RwLock::new(None),
            connection_observer: RwLock::new(None),
            sched: Scheduler::new(4),
            observer,
            journal: None,
            policy: Arc::new(crate::policy::AllowAll),
            approvals: Arc::new(crate::approval::DenyAll),
            archive_limits: RwLock::new(norte_vfs_archive::Limits::default()),
            ai_provider: RwLock::new(None),
            ai_embed: RwLock::new(None),
            ai_config: RwLock::new(crate::ai::AiConfig::default()),
            index: None,
            spool: RwLock::new(None),
            batch_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
        }
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
        Self {
            sessions: SessionPool::new(),
            connector: RwLock::new(None),
            connection_observer: RwLock::new(None),
            sched: Scheduler::new(4),
            observer: Arc::clone(&journal) as Arc<dyn MutationObserver>,
            journal: Some(journal),
            policy: Arc::new(crate::policy::AllowAll),
            approvals: Arc::new(crate::approval::DenyAll),
            archive_limits: RwLock::new(norte_vfs_archive::Limits::default()),
            ai_provider: RwLock::new(None),
            ai_embed: RwLock::new(None),
            ai_config: RwLock::new(crate::ai::AiConfig::default()),
            index: None,
            spool: RwLock::new(None),
            batch_reports: std::sync::Mutex::new(std::collections::VecDeque::new()),
        }
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
        self
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

    /// Evalúa la policy PRE-efecto; un `Ask` suspende hasta aprobación. `Err`
    /// [`Error::PolicyDenied`] con la causa (`rule`) si se deniega — el wire lo
    /// distingue de un `PermissionDenied` del OS/provider (M3-3b).
    async fn gate(
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

    /// Registra la host key de `host:port` tras confirmación explícita del
    /// usuario (flujo TOFU, método `connection.trust_host_key`).
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

    /// La clave de registro/caché para `p`: providers de proceso van por
    /// scheme; los remotos por `scheme://authority`.
    fn provider_key(p: &VPath) -> String {
        match p.authority() {
            Some(a) => format!("{}://{a}", p.scheme()),
            None => p.scheme().to_owned(),
        }
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
        use futures::StreamExt;

        /// Tope del reply acumulado (#M4 security): ver el bucle de drenado.
        const MAX_REPLY_BYTES: usize = 512 * 1024;

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
                names.push(name.clone());
            }
        }

        let req = crate::ai::build_rename_prompt(&names, instruction)
            .map_err(|e| ai_to_proto_error(&e))?;
        let mut chat = provider
            .chat(req)
            .await
            .map_err(|e| ai_to_proto_error(&e))?;
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
        crate::ai::validate_rename_reply(&reply, &names).map_err(|e| ai_to_proto_error(&e))
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
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let key = root.scheme().to_owned();
        let handle = self.sched.submit(
            &key,
            TaskKind::Search,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move {
                    crate::search::run_walk(provider, root, matchers, tx, &ctx).await
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
        // DESPUÉS de una operación async sobre el provider: `capabilities` es
        // síncrono (regla 2: no puede hacer I/O) y `norte-vfs-local` sondea de
        // forma perezosa dentro de la primera operación async. Preguntar antes
        // devuelve el default de `cfg!(target_os)`.
        let caps = dest.capabilities();
        // 3ª puerta: la contención que ve un volumen que PLIEGA. Solo puede
        // disparar cuando scheme y authority coinciden, y entonces las dos
        // raíces salen del mismo objeto provider (el pool cachea por
        // `scheme://authority`), así que el `node_id` de arriba ya forzó el
        // sondeo perezoso de capacidades y estas son las de verdad.
        let sides = norte_compare::Sides::from_capabilities(source.capabilities(), caps);
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
        let no_build = index
            .files_for_embed(&root)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "index.embed: pre-check del índice falló");
                Error::Io {
                    retryable: e.is_retryable(),
                }
            })?
            .is_empty();
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
    /// # Errors
    /// [`Error::Unsupported`] si no hay índice; error del índice mapeado a `Io`.
    pub async fn index_query_as(
        &self,
        root: &VPath,
        text: &str,
        limit: u32,
        _actor: crate::journal::Actor,
    ) -> Result<Vec<norte_index::IndexHit>, Error> {
        let index = self.index.clone().ok_or(Error::Unsupported)?;
        index.query(root, text, limit).await.map_err(|e| {
            tracing::debug!(error = %e, "index.query falló");
            Error::Io {
                retryable: e.is_retryable(),
            }
        })
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
        let mut scored: Vec<(VPath, f64)> = vectors
            .into_iter()
            .filter_map(|(path, v)| {
                crate::index_embed::cosine(&qvec, &v).map(|s| (path, f64::from(s)))
            })
            .collect();
        // `total_cmp` es orden TOTAL: jamás el panic de `sort_by` con un
        // comparador no total (Rust ≥1.81).
        scored.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(k);
        Ok(scored)
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
    /// [`Error::PolicyDenied`] si la policy deniega; [`Error::Unsupported`]
    /// si algún scheme no tiene provider registrado.
    #[tracing::instrument(skip(self, actor), fields(from = %span_path(from), to = %span_path(to)))]
    pub async fn copy_with_as(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Copy, &[from, to])
            .await?;
        let src = self.provider_for(from).await?;
        let dst = self.provider_for(to).await?;
        let observer = Arc::clone(&self.observer);
        let (from, to) = (from.clone(), to.clone());
        let key = to.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Copy,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(
                    async move { ops::copy_task(src, dst, from, to, opts, observer, &ctx).await },
                )
            }),
        ))
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
    /// [`Error::PolicyDenied`] si la policy deniega; [`Error::Unsupported`]
    /// si algún scheme no tiene provider registrado.
    #[tracing::instrument(skip(self, actor), fields(from = %span_path(from), to = %span_path(to)))]
    pub async fn move_with_as(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Move, &[from, to])
            .await?;
        let src = self.provider_for(from).await?;
        let dst = self.provider_for(to).await?;
        let observer = Arc::clone(&self.observer);
        let (from, to) = (from.clone(), to.clone());
        let key = to.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Move,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(
                    async move { ops::move_task(src, dst, from, to, opts, observer, &ctx).await },
                )
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
    #[tracing::instrument(skip(self, pairs), fields(actor = ?actor, pairs = pairs.len()))]
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
        // El LISTADO va primero, y el orden es la corrección, no un detalle de
        // estilo. `Provider::capabilities` es síncrono (regla 2: no puede hacer
        // I/O) y `norte-vfs-local` sondea el régimen de mayúsculas de forma
        // PEREZOSA, dentro de la primera operación async; hasta entonces
        // contesta el default de `cfg!(target_os)` — justo lo que la doc de
        // `NameCaps` promete que no se hace. Preguntar antes de listar
        // planifica un volumen que pliega el caso como si lo distinguiera: una
        // colisión `External` que no se reporta, y un plan que el humano
        // aprueba sin la línea que le importaba. Es el caso normal de un
        // puente MCP o un CLI de un solo tiro, donde planificar ES la primera
        // operación del provider.
        let names = list_base_names(&*provider, dir).await?;
        let caps = provider.capabilities();
        if caps.flags.contains(CapabilityFlags::READ_ONLY) {
            return Err(Error::Unsupported);
        }
        let name_caps = crate::rename::NameCaps {
            case_sensitive: caps.flags.contains(CapabilityFlags::CASE_SENSITIVE),
        };
        let owned: Vec<(Vec<u8>, Vec<u8>)> = pairs.to_vec();
        // El planificador es SÍNCRONO y asigna una clave de comparación por
        // entrada del listado: sobre un directorio de cien mil ficheros eso es
        // trabajo de CPU medible, y `fs.rename_batch_plan` es una respuesta
        // DIRECTA (ADR 0042) que corre en el executor async. Fuera de él
        // (reglas 2 y 3): un hilo de bloqueo no puede dejar sin atender al
        // resto de conexiones del daemon.
        tokio::task::spawn_blocking(move || crate::rename::plan_batch(&owned, &names, name_caps))
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
    /// si el gate deniega; las de [`Self::rename_batch_plan`].
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

        let recorder: Arc<dyn crate::rename::exec::StepJournal> = match &self.journal {
            Some(j) => {
                let batch_id = j.journal().alloc_batch().await.map_err(Error::from)?;
                Arc::new(crate::rename::exec::BatchJournal {
                    journal: Arc::clone(j),
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
            None => Arc::new(crate::rename::exec::ObserverJournal {
                observer: Arc::clone(&self.observer),
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
    /// [`Error::PolicyDenied`] si la policy deniega; [`Error::Unsupported`]
    /// si el scheme no tiene provider registrado.
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
    /// [`Error::PolicyDenied`] si la policy deniega; [`Error::Unsupported`]
    /// si el scheme no tiene provider registrado.
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

    /// Capabilities del provider que sirve `p` (para que el frontend
    /// decida, p. ej., si el F8 va a papelera o avisa de permanente).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider registrado.
    pub async fn capabilities(&self, p: &VPath) -> Result<norte_proto::Capabilities, Error> {
        Ok(self.provider_for(p).await?.capabilities())
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
    /// El undo pasa por el gate de policy (M3-3, regla 9): cada reversa se evalúa
    /// como su `PolicyOp` inverso ANTES de resolver el provider; una denegación
    /// para el bucle LIFO (`report.blocked`). Gatear antes de `provider_for`
    /// evita además establecer conexiones remotas dirigidas por el journal sin
    /// pasar por policy.
    ///
    /// # Errors
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
        let journal = self.journal.clone().ok_or(Error::Unsupported)?;
        let entries = journal
            .journal()
            .revertible_for(target)
            .await
            .map_err(Error::from)?;

        let report = Arc::new(std::sync::Mutex::new(crate::UndoReport::default()));

        // Un lote (`fs.rename_batch`) es UNA unidad: se revierte entero o no se
        // toca (diseño §7). El agrupado va antes del gate para que la policy vea
        // el lote completo, igual que en la ida.
        let units = crate::undo::undo_units(entries);

        // Planning: gatea cada reversa por policy y resuelve su provider ANTES de
        // spawnear (el cuerpo de la Task es 'static y no puede tener `&self`).
        // Gate → provider (para no conectar a schemes del journal sin policy).
        let mut plan: Vec<(Vec<crate::journal::JournalEntry>, Arc<dyn Provider>)> =
            Vec::with_capacity(units.len());
        for unit in units {
            let Some((undo_op, paths)) = undo_gate_targets(&unit)? else {
                continue; // imposible: `undo_units` no produce unidades vacías.
            };
            let Some(first) = unit.first() else { continue };
            let gate_paths: Vec<&VPath> = paths.iter().collect();
            if let Err(err) = self.gate(&executor, undo_op, &gate_paths).await {
                report.lock().expect("undo report lock").blocked = Some((first.seq, err));
                break; // estricto: para al primer bloqueo de policy (LIFO).
            }
            let provider = self.provider_for(&paths[0]).await?;
            plan.push((unit, provider));
        }

        let report_task = Arc::clone(&report);
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
                    for (unit, provider) in plan {
                        if ctx.cancel.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        // `undone` sí cuenta ENTRADAS: es lo que la unidad
                        // deshizo del journal, y un lote deshace las suyas.
                        let members = unit.len() as u64;
                        let outcome = match unit.split_first() {
                            // Unidad de una: el camino de siempre, intacto.
                            Some((entry, [])) => {
                                crate::undo::revert_entry(&*provider, &journal, entry, &ctx.actor)
                                    .await?
                            }
                            // Unidad de varias: un lote, entero o nada.
                            _ => {
                                crate::undo::revert_batch(
                                    &*provider,
                                    &journal,
                                    &unit,
                                    &ctx.actor,
                                    &ctx.cancel,
                                    task_id,
                                    &report_task,
                                )
                                .await?
                            }
                        };
                        match outcome {
                            crate::undo::Reverted::Done => {
                                report_task.lock().expect("undo report lock").undone += members;
                            }
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
        Ok((handle, report))
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

/// ¿Está `path` EN `root` o por debajo?
///
/// Scheme, authority y luego los segmentos uno a uno por sus BYTES crudos —
/// nunca por prefijo de cadena, que haría colgar `…/cafétière` de `…/café`
/// (regla dura 1). Gemela de la que `norte_sync::plan` usa para su guard de
/// solape, que es privada de aquel crate.
fn is_at_or_under(root: &VPath, path: &VPath) -> bool {
    if path.scheme() != root.scheme() || path.authority() != root.authority() {
        return false;
    }
    let mut rest = path.segments();
    root.segments().all(|segment| rest.next() == Some(segment))
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

/// Mapea un [`norte_ai::AiError`] a la taxonomía del wire (M4-A2). El detalle
/// (mensajes del proveedor, que jamás contienen la clave por construcción)
/// queda en el log; el wire lleva la categoría.
pub(crate) fn ai_to_proto_error(e: &norte_ai::AiError) -> Error {
    use norte_ai::AiError as A;
    match e {
        A::Auth => Error::PermissionDenied,
        A::Cancelled => Error::Cancelled,
        A::Unsupported => Error::Unsupported,
        A::RateLimited { .. } | A::Transport(_) => Error::ProviderUnavailable { retryable: true },
        // Http/Protocol y cualquier variante futura (AiError es
        // non_exhaustive): categoría gruesa, detalle al log.
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

/// Una entrada del anillo de informes: la Task, el ACTOR que la pidió (para
/// que quien sirva el informe por el wire pueda decidir si el que pregunta
/// podía ver esa task) y el informe VIVO, que la Task sigue rellenando.
type BatchReportEntry = (
    TaskId,
    crate::journal::Actor,
    Arc<std::sync::Mutex<crate::rename::BatchReport>>,
);

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
    // ¿Este informe tiene algo que solo él sabe? Se evalúa AHORA y no al
    // insertarlo: al insertarlo todo informe está vacío — es al desalojar
    // cuando los viejos ya terminaron y se sabe cuáles duelen.
    fn loud(e: &BatchReportEntry) -> bool {
        let r = e.2.lock().expect("batch report lock");
        r.stuck.is_some() || r.uncertain.is_some() || r.compensations_lost > 0
    }
    fn drop_one(ring: &mut std::collections::VecDeque<BatchReportEntry>, agents_only: bool) {
        let candidates = || {
            ring.iter()
                .enumerate()
                .filter(|(_, e)| !agents_only || !matches!(e.1, crate::journal::Actor::User))
        };
        let quiet = candidates().find(|(_, e)| !loud(e)).map(|(i, _)| i);
        let victim = quiet.or_else(|| candidates().map(|(i, _)| i).next());
        if let Some(i) = victim {
            ring.remove(i);
        }
    }
    while ring
        .iter()
        .filter(|e| !matches!(e.1, crate::journal::Actor::User))
        .count()
        > BATCH_REPORTS_AGENTS_MAX
    {
        drop_one(ring, true);
    }
    while ring.len() > BATCH_REPORTS_MAX {
        drop_one(ring, false);
    }
}

/// Cuántos informes de lote retiene [`Engine::rename_batch_report`].
///
/// Anillo, no mapa: un informe se pide una vez, justo después del terminal de
/// su Task, y el que nadie recoja tiene que caducar solo o el daemon acumula
/// memoria por cada lote que corrió en su vida. El mismo criterio (y el mismo
/// orden de magnitud) que el anillo de informes de undo del daemon.
pub const BATCH_REPORTS_MAX: usize = 32;

/// Cuántos de los [`BATCH_REPORTS_MAX`] puede ocupar el conjunto de los actores
/// NO humanos. Sub-tope por clase, como el de scopes por conexión de M3-3b: el
/// humano conserva su margen pase lo que pase al otro lado. Ver
/// [`evict_batch_reports`].
pub const BATCH_REPORTS_AGENTS_MAX: usize = 16;

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

/// Qué evaluar en la policy para deshacer UNA unidad de undo: la operación y
/// TODAS las rutas que tocará.
///
/// Reversa de un `Created` BORRA → `Delete` (una ruta), y desde #65 va SIEMPRE
/// a papelera (o se salta): se gatea como `Trash`, no como `Permanent` — un
/// deny de «solo-permanente» no debe parar el LIFO por una reversa que jamás
/// borra permanente. `rename_back` / `restore_trash` REUBICAN → `Move` con DOS
/// endpoints (origen y destino de la restauración): ambos deben pasar el gate,
/// igual que un Move normal (security M2). El segundo endpoint es `path_to`
/// (rename) o `reversal_ref` (trash).
///
/// De un LOTE se recogen los endpoints de TODOS sus miembros para gatear UNA
/// vez: la policy resuelve el slice a lo más restrictivo, así que un lote que
/// roza un nombre denegado se deniega entero y jamás a medias — la misma regla
/// que [`Engine::rename_batch_as`] aplica en la ida.
///
/// `None` solo para una unidad vacía, que [`crate::undo::undo_units`] no
/// produce.
fn undo_gate_targets(
    unit: &[crate::journal::JournalEntry],
) -> Result<Option<(crate::policy::PolicyOp, Vec<VPath>)>, Error> {
    let mut paths: Vec<VPath> = Vec::with_capacity(unit.len() * 2);
    let mut op: Option<crate::policy::PolicyOp> = None;
    for e in unit {
        let (undo_op, second) = match e.reversal.as_str() {
            "rename_back" => (
                crate::policy::PolicyOp::Move,
                e.path_to.as_deref().map(wire_engine).transpose()?,
            ),
            "restore_trash" => (
                crate::policy::PolicyOp::Move,
                e.reversal_ref.as_deref().map(wire_engine).transpose()?,
            ),
            _ => (
                crate::policy::PolicyOp::Delete {
                    mode: DeleteMode::Trash,
                },
                None,
            ),
        };
        paths.push(wire_engine(&e.path)?);
        if let Some(s) = second {
            paths.push(s);
        }
        // Una unidad de varias es un lote de renames: mismo op para todos, así
        // que este merge no se ejerce hoy. Si un journal corrupto los mezclara,
        // gana `Delete` sobre `Move` porque quita un nodo de en medio y `Move`
        // solo lo reubica: ante la duda, la que un usuario querría que le
        // preguntaran. `revert_batch` bloquea la unidad de todas formas.
        op = Some(match (op, undo_op) {
            (Some(prev @ crate::policy::PolicyOp::Delete { .. }), _) => prev,
            _ => undo_op,
        });
    }
    Ok(op.map(|o| (o, paths)))
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
