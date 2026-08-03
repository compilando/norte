//! [`Engine`]: la API embebida del core (M0). El daemon JSON-RPC (M1)
//! envolverá esta misma API; los frontends no contienen lógica de negocio.

use std::sync::{Arc, RwLock};

use norte_proto::{
    CollisionPolicy, DeleteMode, Entry, Error, ResumePolicy, SymlinkPolicy, TaskKind, VPath,
    VerifyPolicy,
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
                    paths: paths.iter().map(|p| span_path(p)).collect(),
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
    /// heurística de texto), prefijos acotados, skip por hash — ver
    /// [`crate::index_embed`].
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

        // Planning: gatea cada reversa por policy y resuelve su provider ANTES de
        // spawnear (el cuerpo de la Task es 'static y no puede tener `&self`).
        // Gate → provider (para no conectar a schemes del journal sin policy).
        let mut plan: Vec<(crate::journal::JournalEntry, Arc<dyn Provider>)> =
            Vec::with_capacity(entries.len());
        for e in entries {
            let p = wire_engine(&e.path)?;
            // Reversa de un Created BORRA → Delete (una ruta), y desde #65 va
            // SIEMPRE a papelera (o se salta): se gatea como `Trash`, no como
            // `Permanent` — un deny de "solo-permanente" no debe parar el LIFO
            // por una reversa que jamás borra permanente. rename_back /
            // restore_trash REUBICAN → Move con DOS endpoints (origen+destino de
            // la restauración): ambos deben pasar el gate, igual que un Move
            // normal (security M2). El segundo endpoint es `path_to` (rename) o
            // `reversal_ref` (trash).
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
            let mut gate_paths: Vec<&VPath> = vec![&p];
            if let Some(s) = &second {
                gate_paths.push(s);
            }
            if let Err(err) = self.gate(&executor, undo_op, &gate_paths).await {
                report.lock().expect("undo report lock").blocked = Some((e.seq, err));
                break; // estricto: para al primer bloqueo de policy (LIFO).
            }
            let provider = self.provider_for(&p).await?;
            plan.push((e, provider));
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
                    let total = plan.len() as u64;
                    ctx.progress.update(|p| p.entries_total = Some(total));
                    for (entry, provider) in plan {
                        if ctx.cancel.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        match crate::undo::revert_entry(&*provider, &journal, &entry, &ctx.actor)
                            .await?
                        {
                            crate::undo::Reverted::Done => {
                                report_task.lock().expect("undo report lock").undone += 1;
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
                            crate::undo::Reverted::Blocked(err) => {
                                report_task.lock().expect("undo report lock").blocked =
                                    Some((entry.seq, err));
                                break; // estricto: para en el primer bloqueo.
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
/// abre ANTES: jamás debe llegar a un log (regla 10).
fn span_path(p: &VPath) -> String {
    match p.authority() {
        Some(a) if a.rsplit_once('@').is_some_and(|(ui, _)| ui.contains(':')) => {
            format!("<{} ***>", p.scheme())
        }
        _ => p.display_lossy().clone(),
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
