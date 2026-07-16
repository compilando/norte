//! [`Engine`]: la API embebida del core (M0). El daemon JSON-RPC (M1)
//! envolverá esta misma API; los frontends no contienen lógica de negocio.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use norte_proto::{
    CollisionPolicy, DeleteMode, Entry, Error, ResumePolicy, SymlinkPolicy, TaskKind, VPath,
    VerifyPolicy,
};
use norte_vfs::{EntryStream, Provider};

use crate::observer::{MutationObserver, NoopObserver};
use crate::ops;
use crate::scheduler::{Priority, Scheduler, TaskHandle};

/// Tope para ESTABLECER una conexión remota (dial+TOFU+auth+subsistema).
/// Generoso a propósito: cubre redes lentas sin colgar indefinidamente.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
    /// Clave: el scheme (`"file"`, providers de proceso) o
    /// `"scheme://authority"` (remotos establecidos bajo demanda — un
    /// provider remoto envuelve UNA sesión a UN host, fase 6e).
    providers: RwLock<HashMap<String, Arc<dyn Provider>>>,
    /// Establece providers remotos bajo demanda (fase 6e, ADR 0015). Sin
    /// conector, un scheme sin provider registrado es `Unsupported` (M0/M1).
    connector: RwLock<Option<Arc<dyn crate::connect::RemoteConnector>>>,
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
            providers: RwLock::new(HashMap::new()),
            connector: RwLock::new(None),
            sched: Scheduler::new(4),
            observer,
            journal: None,
            policy: Arc::new(crate::policy::AllowAll),
            approvals: Arc::new(crate::approval::DenyAll),
        }
    }

    /// Engine cuyo observer Y fuente de undo es el mismo `SqliteJournal` (M3-2).
    /// El journal es single-writer (spec §4): un solo Engine por fichero.
    #[must_use]
    pub fn with_journal(journal: Arc<crate::journal::SqliteJournal>) -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
            connector: RwLock::new(None),
            sched: Scheduler::new(4),
            observer: Arc::clone(&journal) as Arc<dyn MutationObserver>,
            journal: Some(journal),
            policy: Arc::new(crate::policy::AllowAll),
            approvals: Arc::new(crate::approval::DenyAll),
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
        let scheme = provider.scheme().to_owned();
        self.providers
            .write()
            .expect("providers lock sano")
            .insert(scheme, provider);
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
        {
            let providers = self.providers.read().expect("providers lock sano");
            // Primero el provider de proceso registrado para el scheme entero
            // (local, mem de tests): tiene prioridad y no dispara conexiones.
            if let Some(prov) = providers.get(p.scheme()) {
                return Ok(Arc::clone(prov));
            }
            if let Some(prov) = providers.get(&key) {
                return Ok(Arc::clone(prov));
            }
        }
        // Archivos como directorios (ADR 0018): scheme compuesto = provider
        // por composición sobre el provider del CONTENEDOR. Antes del
        // connector: el interior puede ser local o una conexión ya viva.
        if let Some(aref) = p.archive_split().map_err(|_| Error::InvalidPath)? {
            let format = match aref.format.as_str() {
                "tar" => norte_vfs_archive::Format::Tar,
                "zip" => norte_vfs_archive::Format::Zip,
                // Formato de la whitelist de proto sin provider aquí: una
                // versión de core más vieja que el proto. Honesto: no sé.
                _ => return Err(Error::Unsupported),
            };
            // El exterior jamás lleva prefijo de formato (archive_split
            // rechaza anidamiento en v1): recursión de profundidad 1.
            let inner = Box::pin(self.provider_for(&aref.outer)).await?;
            tracing::debug!(scheme = %p.scheme(), %key, "componiendo provider de archivo");
            let provider: Arc<dyn Provider> = Arc::new(norte_vfs_archive::ArchiveProvider::new(
                inner,
                format,
                p.scheme().to_owned(),
            ));
            // Mismo double-check que los remotos: si otra petición registró
            // primero, gana la suya (el ArchiveProvider extra solo es RAM).
            let mut providers = self.providers.write().expect("providers lock sano");
            let entry = providers
                .entry(key)
                .or_insert_with(|| Arc::clone(&provider));
            return Ok(Arc::clone(entry));
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
        // Fuera del lock: conectar puede tardar (red, TOFU). Con TIMEOUT: el
        // connect ocurre ANTES de existir una Task cancelable — sin límite,
        // un host que acepta TCP y calla colgaría la operación para siempre
        // (regla 3). La cancelación fina llegará al mover el connect dentro
        // de la Task (issue #47).
        let provider = tokio::time::timeout(
            CONNECT_TIMEOUT,
            connector.connect(p.scheme(), authority),
        )
        .await
        .map_err(|_| {
            tracing::warn!(scheme = %p.scheme(), "timeout estableciendo la conexión remota");
            Error::ProviderUnavailable { retryable: true }
        })??;
        // Double-check bajo el write lock: si otra petición concurrente
        // registró primero, se conserva LA SUYA (una clave = una sesión) y la
        // recién creada se suelta — su Drop cierra la sesión de más (deuda de
        // single-flight/evicción: issue #47; la sesión duplicada es coste,
        // no riesgo — mismo uid, mismas credenciales).
        let mut providers = self.providers.write().expect("providers lock sano");
        let entry = providers
            .entry(key)
            .or_insert_with(|| Arc::clone(&provider));
        Ok(Arc::clone(entry))
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
            // Reversa de un Created BORRA → Delete (una ruta). rename_back /
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
                        mode: DeleteMode::Permanent,
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
