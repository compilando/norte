//! Ciclo de vida de las sesiones remotas del Engine (#47, evolución de la
//! fase 6e / ADR 0015): un [`SessionPool`] posee la caché de providers y le
//! añade single-flight del connect, dial cancelable por drop, backoff de
//! reconexión y evicción de sesiones muertas (con arrastre de los providers
//! archive compuestos, #62).
//!
//! Contrato del ciclo de vida (ADR 0029):
//! - **Perezoso**: sin health-checks de fondo ni TTL. Una sesión se evicta
//!   cuando una operación devuelve [`Error::ProviderUnavailable`]; el
//!   siguiente acceso reconecta.
//! - **Single-flight**: dos peticiones concurrentes a la misma clave esperan
//!   el MISMO dial (una sesión por clave, sin handshakes duplicados).
//! - **Cancelable por drop**: los waiters llevan un guard RAII; cuando el
//!   último se suelta a mitad del dial, el connect se cancela (compone con
//!   `rpc.cancel`, #72 — dropear el dispatch suelta el waiter).
//! - **Backoff**: un fallo `ProviderUnavailable` del dial entra en
//!   negative-cache (1s, ×2, tope 30s); los errores accionables por el
//!   usuario (TOFU, auth) JAMÁS se cachean.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use norte_proto::Error;
use norte_vfs::Provider;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Tope para ESTABLECER una conexión remota (dial+TOFU+auth+subsistema).
/// Generoso a propósito: cubre redes lentas sin colgar indefinidamente.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// La caché de providers del Engine + el ciclo de vida de las sesiones
/// remotas. Clon barato (Arc interno): los jobs de connect y los wrappers de
/// evicción referencian el interior vía `Weak`.
#[derive(Clone)]
pub(crate) struct SessionPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    /// Clave: el scheme (`"file"`, providers de proceso), `scheme://authority`
    /// (sesiones remotas; puede haber varias claves-alias para el MISMO Arc,
    /// dedup canónica) o `fmt+scheme://authority` (archive compuestos).
    providers: RwLock<HashMap<String, Arc<dyn Provider>>>,
    /// Dials en vuelo, por clave de caché (single-flight).
    connecting: Mutex<HashMap<String, ConnectJob>>,
    /// Negative-cache de fallos de dial (solo `ProviderUnavailable`).
    cooldown: Mutex<HashMap<String, Cooldown>>,
    /// Ids únicos de job (el job solo limpia SU entrada de `connecting`).
    next_job: AtomicU64,
}

/// Resultado compartido de un dial en vuelo.
type DialResult = Option<Result<Arc<dyn Provider>, Error>>;

struct ConnectJob {
    id: u64,
    rx: watch::Receiver<DialResult>,
    waiters: usize,
    cancel: CancellationToken,
}

struct Cooldown {
    until: tokio::time::Instant,
    next: Duration,
    last_err: Error,
}

const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

impl SessionPool {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(PoolInner {
                providers: RwLock::new(HashMap::new()),
                connecting: Mutex::new(HashMap::new()),
                cooldown: Mutex::new(HashMap::new()),
                next_job: AtomicU64::new(1),
            }),
        }
    }

    /// Registra un provider de proceso bajo su scheme (pisa el anterior).
    pub(crate) fn register_process(&self, provider: Arc<dyn Provider>) {
        let scheme = provider.scheme().to_owned();
        self.inner
            .providers
            .write()
            .expect("providers lock sano")
            .insert(scheme, provider);
    }

    /// El provider cacheado bajo `key`, si lo hay.
    pub(crate) fn lookup(&self, key: &str) -> Option<Arc<dyn Provider>> {
        self.inner
            .providers
            .read()
            .expect("providers lock sano")
            .get(key)
            .map(Arc::clone)
    }

    /// Inserta un provider compuesto (archive) con double-check: si otra
    /// petición registró primero, gana la suya (el extra solo es RAM).
    pub(crate) fn insert_composite(
        &self,
        key: String,
        provider: Arc<dyn Provider>,
    ) -> Arc<dyn Provider> {
        let mut providers = self.inner.providers.write().expect("providers lock sano");
        let entry = providers.entry(key).or_insert(provider);
        Arc::clone(entry)
    }

    /// Alias `key` → la MISMA sesión `provider` (dedup canónica: un acceso
    /// posterior por la forma no-canónica acierta el fast path).
    pub(crate) fn alias(&self, key: String, provider: &Arc<dyn Provider>) {
        self.inner
            .providers
            .write()
            .expect("providers lock sano")
            .entry(key)
            .or_insert_with(|| Arc::clone(provider));
    }

    /// Establece (o espera) la sesión remota de `cache_key` — el corazón del
    /// ciclo de vida (#47).
    ///
    /// Single-flight: si ya hay un dial en vuelo para la clave, esta llamada
    /// se SUBSCRIBE y espera su resultado. Si no, spawnea el job de dial
    /// (timeout [`CONNECT_TIMEOUT`]). Dropear el future de esta función
    /// suelta el waiter; cuando cae el último, el dial se cancela.
    ///
    /// `alias`: clave extra bajo la que registrar la MISMA sesión al
    /// conectar (dedup canónica: la forma que pidió el caller).
    ///
    /// # Errors
    /// Los del dial; un fallo `ProviderUnavailable` reciente se responde
    /// desde la negative-cache sin volver a marcar (backoff).
    pub(crate) async fn connect_remote(
        &self,
        cache_key: String,
        alias: Option<String>,
        scheme: &str,
        authority: &str,
        connector: Arc<dyn crate::connect::RemoteConnector>,
        observer: Option<Arc<dyn crate::connect::ConnectionObserver>>,
    ) -> Result<Arc<dyn Provider>, Error> {
        // Backoff: un fallo transitorio reciente responde cacheado (jamás
        // los errores accionables — TOFU/auth no entran en cooldown).
        {
            let cooldown = self.inner.cooldown.lock().expect("cooldown lock sano");
            if let Some(c) = cooldown.get(&cache_key)
                && tokio::time::Instant::now() < c.until
            {
                return Err(c.last_err.clone());
            }
        }
        let (mut rx, _guard) = {
            let mut connecting = self.inner.connecting.lock().expect("connecting lock sano");
            if let Some(job) = connecting.get_mut(&cache_key) {
                job.waiters += 1;
                (
                    job.rx.clone(),
                    WaiterGuard {
                        pool: Arc::clone(&self.inner),
                        key: cache_key.clone(),
                    },
                )
            } else {
                let (tx, rx) = watch::channel(None);
                let cancel = CancellationToken::new();
                let id = self.inner.next_job.fetch_add(1, Ordering::Relaxed);
                connecting.insert(
                    cache_key.clone(),
                    ConnectJob {
                        id,
                        rx: rx.clone(),
                        waiters: 1,
                        cancel: cancel.clone(),
                    },
                );
                spawn_dial_job(DialJob {
                    pool: Arc::downgrade(&self.inner),
                    id,
                    cache_key: cache_key.clone(),
                    alias,
                    scheme: scheme.to_owned(),
                    authority: authority.to_owned(),
                    connector,
                    observer,
                    cancel,
                    tx,
                });
                (
                    rx,
                    WaiterGuard {
                        pool: Arc::clone(&self.inner),
                        key: cache_key.clone(),
                    },
                )
            }
        };
        loop {
            if let Some(result) = rx.borrow_and_update().clone() {
                return result;
            }
            if rx.changed().await.is_err() {
                // El job murió sin publicar (panic): honesto, no un cuelgue.
                return Err(Error::Internal { panic: true });
            }
        }
    }
}

/// Waiter RAII de un dial en vuelo: al caer el ÚLTIMO, cancela el job y
/// limpia la entrada (atómico bajo el lock de `connecting` — un waiter nuevo
/// jamás ve un job ya cancelado).
struct WaiterGuard {
    pool: Arc<PoolInner>,
    key: String,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let mut connecting = self.pool.connecting.lock().expect("connecting lock sano");
        if let Some(job) = connecting.get_mut(&self.key) {
            job.waiters -= 1;
            if job.waiters == 0 {
                job.cancel.cancel();
                connecting.remove(&self.key);
            }
        }
    }
}

/// Todo lo que necesita el job de dial (spawneado: sobrevive a los waiters).
struct DialJob {
    pool: Weak<PoolInner>,
    id: u64,
    cache_key: String,
    alias: Option<String>,
    scheme: String,
    authority: String,
    connector: Arc<dyn crate::connect::RemoteConnector>,
    observer: Option<Arc<dyn crate::connect::ConnectionObserver>>,
    cancel: CancellationToken,
    tx: watch::Sender<DialResult>,
}

fn spawn_dial_job(job: DialJob) {
    tokio::spawn(async move {
        let dialed: Option<Result<crate::connect::Connected, Error>> = tokio::select! {
            () = job.cancel.cancelled() => None,
            r = tokio::time::timeout(
                CONNECT_TIMEOUT,
                job.connector.connect(&job.scheme, &job.authority),
            ) => Some(r.unwrap_or_else(|_| {
                tracing::warn!(scheme = %job.scheme, "timeout estableciendo la conexión remota");
                Err(Error::ProviderUnavailable { retryable: true })
            })),
        };
        let Some(pool) = job.pool.upgrade() else {
            return; // el Engine murió: nada que registrar
        };
        match dialed {
            // Abandonado: el último WaiterGuard ya canceló Y limpió la
            // entrada bajo el lock — aquí no queda nada que hacer.
            None => {}
            Some(Ok(connected)) => {
                // #44: los avisos se emiten UNA vez por establecimiento.
                if let Some(obs) = &job.observer {
                    for w in &connected.warnings {
                        obs.on_connection_warning(w);
                    }
                }
                // La sesión entra al pool ENVUELTA: se auto-evicta cuando
                // una operación la encuentre muerta (ProviderUnavailable).
                let provider: Arc<dyn Provider> = Arc::new(SessionProvider {
                    inner: connected.provider,
                    key: job.cache_key.clone(),
                    pool: job.pool.clone(),
                });
                {
                    let mut providers = pool.providers.write().expect("providers lock sano");
                    providers.insert(job.cache_key.clone(), Arc::clone(&provider));
                    if let Some(alias) = &job.alias {
                        providers.insert(alias.clone(), Arc::clone(&provider));
                    }
                }
                pool.cooldown
                    .lock()
                    .expect("cooldown lock sano")
                    .remove(&job.cache_key);
                remove_own_entry(&pool, &job.cache_key, job.id);
                let _ = job.tx.send(Some(Ok(provider)));
            }
            Some(Err(e)) => {
                let mut cooldown = pool.cooldown.lock().expect("cooldown lock sano");
                if matches!(e, Error::ProviderUnavailable { .. }) {
                    // Fallo transitorio: escala la negative-cache.
                    let now = tokio::time::Instant::now();
                    let entry = cooldown.entry(job.cache_key.clone()).or_insert(Cooldown {
                        until: now,
                        next: BACKOFF_INITIAL,
                        last_err: e.clone(),
                    });
                    entry.until = now + entry.next;
                    entry.next = (entry.next * 2).min(BACKOFF_MAX);
                    entry.last_err = e.clone();
                } else {
                    // Error accionable (TOFU, auth, path): sin cooldown — el
                    // usuario corrige y reintenta al instante.
                    cooldown.remove(&job.cache_key);
                }
                drop(cooldown);
                remove_own_entry(&pool, &job.cache_key, job.id);
                let _ = job.tx.send(Some(Err(e)));
            }
        }
    });
}

/// Quita la entrada de `connecting` SOLO si sigue siendo la de este job (un
/// job viejo jamás borra el dial nuevo de otro).
fn remove_own_entry(pool: &PoolInner, key: &str, id: u64) {
    let mut connecting = pool.connecting.lock().expect("connecting lock sano");
    if connecting.get(key).is_some_and(|j| j.id == id) {
        connecting.remove(key);
    }
}

impl PoolInner {
    /// Evicta la sesión de `key` si el wrapper que lo pide SIGUE siendo la
    /// entrada vigente (ptr-check: una sesión más nueva bajo la misma clave
    /// jamás se pisa). Barre también las claves-alias (mismo Arc, dedup
    /// canónica) y los providers archive compuestos sobre esta sesión
    /// (`{fmt}+{clave}`, #62 — el wrapper de archivo cachea el Arc de la
    /// sesión: dejarlo sería servir un índice de una conexión muerta).
    fn evict_session(&self, key: &str, wrapper_ptr: *const ()) {
        let mut providers = self.providers.write().expect("providers lock sano");
        let Some(current) = providers.get(key) else {
            return;
        };
        if Arc::as_ptr(current).cast::<()>() != wrapper_ptr {
            return;
        }
        let session_keys: Vec<String> = providers
            .iter()
            .filter(|(_, v)| Arc::as_ptr(v).cast::<()>() == wrapper_ptr)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &session_keys {
            providers.remove(k);
        }
        let composite_keys: Vec<String> = providers
            .keys()
            .filter(|ck| {
                session_keys.iter().any(|k| {
                    ck.len() > k.len() + 1 && ck.ends_with(k) && {
                        // sufijo `+{k}` exacto (el scheme compuesto es
                        // `fmt+scheme`): evita falsos positivos.
                        ck.as_bytes()[ck.len() - k.len() - 1] == b'+'
                    }
                })
            })
            .cloned()
            .collect();
        for ck in &composite_keys {
            providers.remove(ck);
        }
        tracing::warn!(
            clave = %key,
            alias = session_keys.len().saturating_sub(1),
            compuestos = composite_keys.len(),
            "sesión remota evictada (ProviderUnavailable); el siguiente acceso reconecta"
        );
    }
}

/// Envuelve la sesión remota cacheada: si una operación devuelve
/// [`Error::ProviderUnavailable`], se auto-evicta del pool (la sesión está
/// muerta; el siguiente acceso reconecta). El error se propaga TAL CUAL.
///
/// v1: solo los `Result` de las LLAMADAS evictan — un error dentro de un
/// stream (`list`/`read`) o sink ya entregado no llega aquí (la siguiente
/// llamada directa sobre la sesión muerta sí evicta).
struct SessionProvider {
    inner: Arc<dyn Provider>,
    /// La clave CANÓNICA bajo la que vive en el pool.
    key: String,
    pool: Weak<PoolInner>,
}

impl SessionProvider {
    fn observe<T>(&self, r: Result<T, Error>) -> Result<T, Error> {
        if let Err(Error::ProviderUnavailable { .. }) = &r {
            if let Some(pool) = self.pool.upgrade() {
                pool.evict_session(&self.key, std::ptr::from_ref(self).cast::<()>());
            }
        }
        r
    }
}

#[async_trait::async_trait]
impl Provider for SessionProvider {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &norte_proto::VPath) -> Result<norte_proto::Entry, Error> {
        self.observe(self.inner.stat(p).await)
    }
    async fn list(&self, p: &norte_proto::VPath) -> Result<norte_vfs::EntryStream, Error> {
        self.observe(self.inner.list(p).await)
    }
    async fn list_skipped(&self, p: &norte_proto::VPath) -> Result<Option<u64>, Error> {
        self.observe(self.inner.list_skipped(p).await)
    }
    async fn read(
        &self,
        p: &norte_proto::VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        self.observe(self.inner.read(p, range).await)
    }
    async fn node_id(
        &self,
        p: &norte_proto::VPath,
        follow: norte_vfs::FollowLinks,
    ) -> Result<Option<norte_vfs::NodeId>, Error> {
        self.observe(self.inner.node_id(p, follow).await)
    }
    async fn read_link(&self, p: &norte_proto::VPath) -> Result<Vec<u8>, Error> {
        self.observe(self.inner.read_link(p).await)
    }
    async fn trash(&self, p: &norte_proto::VPath) -> Result<Option<norte_proto::VPath>, Error> {
        self.observe(self.inner.trash(p).await)
    }
    async fn gc_partials(
        &self,
        dir: &norte_proto::VPath,
        older_than: Duration,
    ) -> Result<usize, Error> {
        self.observe(self.inner.gc_partials(dir, older_than).await)
    }
    async fn restore_trashed(&self, original: &norte_proto::VPath) -> Result<(), Error> {
        self.observe(self.inner.restore_trashed(original).await)
    }
    async fn symlink(
        &self,
        link: &norte_proto::VPath,
        target: &[u8],
        kind: norte_vfs::SymlinkKind,
    ) -> Result<(), Error> {
        self.observe(self.inner.symlink(link, target, kind).await)
    }
    async fn write(&self, p: &norte_proto::VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        self.observe(self.inner.write(p).await)
    }
    async fn open_resumable(
        &self,
        p: &norte_proto::VPath,
    ) -> Result<(Box<dyn norte_vfs::ByteSink>, u64), Error> {
        self.observe(self.inner.open_resumable(p).await)
    }
    async fn partial_digest(
        &self,
        p: &norte_proto::VPath,
        len: u64,
    ) -> Result<Option<[u8; 32]>, Error> {
        self.observe(self.inner.partial_digest(p, len).await)
    }
    async fn mkdir(&self, p: &norte_proto::VPath) -> Result<(), Error> {
        self.observe(self.inner.mkdir(p).await)
    }
    async fn remove(&self, p: &norte_proto::VPath) -> Result<(), Error> {
        self.observe(self.inner.remove(p).await)
    }
    async fn rename(
        &self,
        from: &norte_proto::VPath,
        to: &norte_proto::VPath,
    ) -> Result<(), Error> {
        self.observe(self.inner.rename(from, to).await)
    }
    async fn copy_native(
        &self,
        from: &norte_proto::VPath,
        to: &norte_proto::VPath,
    ) -> Option<Result<(), Error>> {
        self.inner
            .copy_native(from, to)
            .await
            .map(|r| self.observe(r))
    }
}
