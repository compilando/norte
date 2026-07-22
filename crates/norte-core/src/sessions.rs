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
    /// Claves-alias que registrar junto a la canónica al publicar (la del
    /// spawner + las de los waiters que se SUMARON al dial en vuelo con otra
    /// forma de la misma identidad — rust MINOR-1 del review #47).
    aliases: Vec<String>,
}

struct Cooldown {
    until: tokio::time::Instant,
    next: Duration,
    last_err: Error,
}

const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Tope de entradas de la negative-cache (security MAJOR-3 del review #47):
/// las claves las elige el caller (`scheme://authority` arbitrario) — sin
/// tope, enumerar authorities infla memoria. Mismo orden que los topes
/// anti-DoS del daemon (listings 256, scopes 256).
const COOLDOWN_MAX: usize = 256;

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
    /// petición registró primero, gana la suya (el extra solo es RAM). El
    /// compuesto entra ENVUELTO en su propio [`SessionProvider`]: si el
    /// barrido de una evicción se lo salta (carrera compose-vs-evict,
    /// security MAJOR-2), un compuesto zombi sobre una sesión muerta se
    /// auto-evicta a la primera operación fallida — jamás inmortal.
    pub(crate) fn insert_composite(
        &self,
        key: String,
        provider: Arc<dyn Provider>,
    ) -> Arc<dyn Provider> {
        let wrapped: Arc<dyn Provider> = Arc::new(SessionProvider {
            inner: provider,
            key: key.clone(),
            pool: Arc::downgrade(&self.inner),
        });
        let mut providers = self.inner.providers.write().expect("providers lock sano");
        let entry = providers.entry(key).or_insert(wrapped);
        Arc::clone(entry)
    }

    /// Registra `alias_key` → la sesión VIGENTE de `canonical_key` y la
    /// devuelve; `None` si la canónica ya no está (evictada entre el lookup
    /// del caller y aquí — security MAJOR-2: jamás re-insertar un Arc muerto
    /// que el caller retenga de un lookup viejo).
    pub(crate) fn alias_current(
        &self,
        canonical_key: &str,
        alias_key: String,
    ) -> Option<Arc<dyn Provider>> {
        let mut providers = self.inner.providers.write().expect("providers lock sano");
        let current = Arc::clone(providers.get(canonical_key)?);
        providers
            .entry(alias_key)
            .or_insert_with(|| Arc::clone(&current));
        Some(current)
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
        // los errores accionables — TOFU/auth no entran en cooldown). De
        // paso PODA las entradas muertas (tope COOLDOWN_MAX, sec MAJOR-3) y
        // DECAE el backoff de una clave que lleva mucho sin fallar (rust
        // MINOR-2: dos fallos separados por horas no son consecutivos).
        {
            let now = tokio::time::Instant::now();
            let mut cooldown = self.inner.cooldown.lock().expect("cooldown lock sano");
            cooldown.retain(|_, c| now < c.until + BACKOFF_MAX);
            if let Some(c) = cooldown.get_mut(&cache_key) {
                if now < c.until {
                    return Err(c.last_err.clone());
                }
                if now > c.until + c.next {
                    c.next = BACKOFF_INITIAL;
                }
            }
        }
        let (mut rx, _guard) = {
            let mut connecting = self.inner.connecting.lock().expect("connecting lock sano");
            // Double-check bajo el lock (orden connecting→providers, fijo):
            // un job pudo publicar entre el fast path del caller y aquí —
            // sin esto, el segundo caller re-marcaría un handshake de más.
            if let Some(prov) = self
                .inner
                .providers
                .read()
                .expect("providers lock sano")
                .get(&cache_key)
            {
                return Ok(Arc::clone(prov));
            }
            if let Some(job) = connecting.get_mut(&cache_key) {
                job.waiters += 1;
                // El waiter que se SUMA aporta su forma-alias: se registra
                // al publicar (rust MINOR-1).
                if let Some(a) = alias
                    && a != cache_key
                    && !job.aliases.contains(&a)
                {
                    job.aliases.push(a);
                }
                (
                    job.rx.clone(),
                    WaiterGuard {
                        pool: Arc::clone(&self.inner),
                        key: cache_key.clone(),
                        id: job.id,
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
                        aliases: alias.into_iter().filter(|a| *a != cache_key).collect(),
                    },
                );
                spawn_dial_job(DialJob {
                    pool: Arc::downgrade(&self.inner),
                    id,
                    cache_key: cache_key.clone(),
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
                        id,
                    },
                )
            }
        };
        loop {
            if let Some(result) = rx.borrow_and_update().clone() {
                return result;
            }
            if rx.changed().await.is_err() {
                // El job murió sin publicar. Con el guard por-id (BLOCKER
                // del review), un job con waiters vivos ya no puede ser
                // cancelado por un guard rancio: si el tx cayó sin publicar
                // teniendo nosotros un waiter vivo, el job PANICÓ de verdad.
                return Err(Error::Internal { panic: true });
            }
        }
    }
}

/// Waiter RAII de un dial en vuelo: al caer el ÚLTIMO, cancela el job y
/// limpia la entrada (atómico bajo el lock de `connecting` — un waiter nuevo
/// jamás ve un job ya cancelado). Lleva el `id` del job al que se suscribió:
/// un guard rancio (su job ya terminó) JAMÁS descuenta waiters de un job
/// nuevo bajo la misma clave (BLOCKER del review #47).
struct WaiterGuard {
    pool: Arc<PoolInner>,
    key: String,
    id: u64,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let mut connecting = self.pool.connecting.lock().expect("connecting lock sano");
        if let Some(job) = connecting.get_mut(&self.key)
            && job.id == self.id
        {
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
                // La sesión entra al pool ENVUELTA: se auto-evicta cuando
                // una operación la encuentre muerta (ProviderUnavailable).
                let provider: Arc<dyn Provider> = Arc::new(SessionProvider {
                    inner: connected.provider,
                    key: job.cache_key.clone(),
                    pool: job.pool.clone(),
                });
                // Publica SOLO si este job sigue vigente (sec MINOR-3: un
                // job abandonado cuyo select! tomó el brazo del connect no
                // pisa la sesión de un job de reemplazo). Orden de locks
                // FIJO del pool: connecting → providers → cooldown.
                let mut connecting = pool.connecting.lock().expect("connecting lock sano");
                let current = connecting
                    .get(&job.cache_key)
                    .is_some_and(|j| j.id == job.id);
                if !current {
                    // Reemplazado/abandonado: la sesión recién nacida se
                    // suelta (Drop cierra) y nadie escucha el tx.
                    return;
                }
                let aliases = connecting
                    .get(&job.cache_key)
                    .map(|j| j.aliases.clone())
                    .unwrap_or_default();
                {
                    let mut providers = pool.providers.write().expect("providers lock sano");
                    providers.insert(job.cache_key.clone(), Arc::clone(&provider));
                    for a in aliases {
                        providers.insert(a, Arc::clone(&provider));
                    }
                }
                pool.cooldown
                    .lock()
                    .expect("cooldown lock sano")
                    .remove(&job.cache_key);
                connecting.remove(&job.cache_key);
                drop(connecting);
                // #44: avisos UNA vez por establecimiento publicado (fuera
                // de los locks: el observer es código ajeno).
                if let Some(obs) = &job.observer {
                    for w in &connected.warnings {
                        obs.on_connection_warning(w);
                    }
                }
                let _ = job.tx.send(Some(Ok(provider)));
            }
            Some(Err(e)) => {
                let mut connecting = pool.connecting.lock().expect("connecting lock sano");
                let current = connecting
                    .get(&job.cache_key)
                    .is_some_and(|j| j.id == job.id);
                if current {
                    // El cooldown se fija ANTES de soltar la entrada (bajo
                    // el mismo lock): un re-dial no puede colarse en la
                    // ventana sin ver la negative-cache. Solo un job VIGENTE
                    // escala el backoff (un job abandonado que muere tarde
                    // no castiga el próximo intento del usuario).
                    let mut cooldown = pool.cooldown.lock().expect("cooldown lock sano");
                    if matches!(e, Error::ProviderUnavailable { .. }) {
                        let now = tokio::time::Instant::now();
                        if cooldown.len() >= COOLDOWN_MAX
                            && !cooldown.contains_key(&job.cache_key)
                            && let Some(oldest) = cooldown
                                .iter()
                                .min_by_key(|(_, c)| c.until)
                                .map(|(k, _)| k.clone())
                        {
                            // Tope: cae la entrada más antigua (sec MAJOR-3).
                            cooldown.remove(&oldest);
                        }
                        let entry = cooldown.entry(job.cache_key.clone()).or_insert(Cooldown {
                            until: now,
                            next: BACKOFF_INITIAL,
                            last_err: e.clone(),
                        });
                        entry.until = now + entry.next;
                        entry.next = (entry.next * 2).min(BACKOFF_MAX);
                        entry.last_err = e.clone();
                    } else {
                        // Error accionable (TOFU, auth, path): sin cooldown —
                        // el usuario corrige y reintenta al instante.
                        cooldown.remove(&job.cache_key);
                    }
                    drop(cooldown);
                    connecting.remove(&job.cache_key);
                }
                drop(connecting);
                let _ = job.tx.send(Some(Err(e)));
            }
        }
    });
}

impl PoolInner {
    /// Evicta TODA entrada cuyo Arc sea exactamente el wrapper que lo pide
    /// (ptr-driven: una sesión más nueva bajo cualquier clave jamás se pisa,
    /// y un alias huérfano cuya canónica ya cayó TAMBIÉN se barre — sec
    /// MAJOR-2: sin esto, un alias re-insertado por una carrera quedaba
    /// muerto para siempre). Arrastra los providers archive compuestos
    /// sobre las claves barridas Y sobre `key` (`{fmt}+{clave}`, #62 — el
    /// wrapper de archivo cachea el Arc de la sesión: dejarlo sería servir
    /// un índice de una conexión muerta).
    fn evict_session(&self, key: &str, wrapper_ptr: *const ()) {
        let mut providers = self.providers.write().expect("providers lock sano");
        let mut swept_keys: Vec<String> = providers
            .iter()
            .filter(|(_, v)| Arc::as_ptr(v).cast::<()>() == wrapper_ptr)
            .map(|(k, _)| k.clone())
            .collect();
        if swept_keys.is_empty() {
            // Evicción rancia (una sesión nueva ya vive bajo estas claves):
            // sin barrido de sesión, pero los compuestos sobre `key` que
            // envuelvan ESTE wrapper se limpian vía su propio wrapper
            // (insert_composite envuelve — se auto-evictan al fallar).
            return;
        }
        for k in &swept_keys {
            providers.remove(k);
        }
        // Base del barrido de compuestos: las claves barridas + la canónica
        // del wrapper (cubre el alias barrido cuya canónica ya no estaba).
        if !swept_keys.iter().any(|k| k == key) {
            swept_keys.push(key.to_owned());
        }
        let composite_keys: Vec<String> = providers
            .keys()
            .filter(|ck| {
                swept_keys.iter().any(|k| {
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
            clave = %redact_key(key),
            barridas = swept_keys.len(),
            compuestos = composite_keys.len(),
            "sesión remota evictada (ProviderUnavailable); el siguiente acceso reconecta"
        );
    }
}

/// `scheme://user@host` → `scheme://host` para logs (regla 10: el userinfo
/// jamás entra al log — misma convención que `ConnectionWarning.host`). El
/// corte usa el ÚLTIMO `@` (regla de #46: lo anterior es userinfo).
fn redact_key(key: &str) -> String {
    match key.split_once("://") {
        Some((scheme, auth)) => match auth.rfind('@') {
            Some(i) => format!("{scheme}://{}", &auth[i + 1..]),
            None => key.to_owned(),
        },
        None => key.to_owned(),
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
        if let Err(Error::ProviderUnavailable { .. }) = &r
            && let Some(pool) = self.pool.upgrade()
        {
            pool.evict_session(&self.key, std::ptr::from_ref(self).cast::<()>());
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

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::VPath;

    fn pu() -> Error {
        Error::ProviderUnavailable { retryable: true }
    }

    /// Provider cuyo TODO devuelve `ProviderUnavailable` (sesión muerta):
    /// pinea que el wrapper delega y evicta en CADA método del trait.
    struct AllPu;

    #[async_trait::async_trait]
    impl Provider for AllPu {
        // La firma la fija el trait (&self → &str): el literal es del test.
        #[allow(clippy::unnecessary_literal_bound)]
        fn scheme(&self) -> &str {
            "sftp"
        }
        fn capabilities(&self) -> norte_proto::Capabilities {
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::empty(),
                max_path: None,
            }
        }
        async fn stat(&self, _p: &VPath) -> Result<norte_proto::Entry, Error> {
            Err(pu())
        }
        async fn list(&self, _p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
            Err(pu())
        }
        async fn list_skipped(&self, _p: &VPath) -> Result<Option<u64>, Error> {
            Err(pu())
        }
        async fn read(
            &self,
            _p: &VPath,
            _r: Option<norte_proto::ByteRange>,
        ) -> Result<norte_vfs::ByteStream, Error> {
            Err(pu())
        }
        async fn node_id(
            &self,
            _p: &VPath,
            _f: norte_vfs::FollowLinks,
        ) -> Result<Option<norte_vfs::NodeId>, Error> {
            Err(pu())
        }
        async fn read_link(&self, _p: &VPath) -> Result<Vec<u8>, Error> {
            Err(pu())
        }
        async fn trash(&self, _p: &VPath) -> Result<Option<VPath>, Error> {
            Err(pu())
        }
        async fn gc_partials(&self, _d: &VPath, _o: Duration) -> Result<usize, Error> {
            Err(pu())
        }
        async fn restore_trashed(&self, _o: &VPath) -> Result<(), Error> {
            Err(pu())
        }
        async fn symlink(
            &self,
            _l: &VPath,
            _t: &[u8],
            _k: norte_vfs::SymlinkKind,
        ) -> Result<(), Error> {
            Err(pu())
        }
        async fn write(&self, _p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
            Err(pu())
        }
        async fn open_resumable(
            &self,
            _p: &VPath,
        ) -> Result<(Box<dyn norte_vfs::ByteSink>, u64), Error> {
            Err(pu())
        }
        async fn partial_digest(&self, _p: &VPath, _l: u64) -> Result<Option<[u8; 32]>, Error> {
            Err(pu())
        }
        async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
            Err(pu())
        }
        async fn remove(&self, _p: &VPath) -> Result<(), Error> {
            Err(pu())
        }
        async fn rename(&self, _f: &VPath, _t: &VPath) -> Result<(), Error> {
            Err(pu())
        }
        async fn copy_native(&self, _f: &VPath, _t: &VPath) -> Option<Result<(), Error>> {
            Some(Err(pu()))
        }
    }

    fn wrapper(pool: &SessionPool, key: &str) -> Arc<dyn Provider> {
        Arc::new(SessionProvider {
            inner: Arc::new(AllPu),
            key: key.to_owned(),
            pool: Arc::downgrade(&pool.inner),
        })
    }

    fn insert(pool: &SessionPool, key: &str, w: &Arc<dyn Provider>) {
        pool.inner
            .providers
            .write()
            .expect("lock de test")
            .insert(key.to_owned(), Arc::clone(w));
    }

    fn contains(pool: &SessionPool, key: &str) -> bool {
        pool.inner
            .providers
            .read()
            .expect("lock de test")
            .contains_key(key)
    }

    /// sec MAJOR-2 (review #47): un alias HUÉRFANO (su canónica ya cayó del
    /// mapa) también se barre — la evicción es ptr-driven, sin gate por
    /// clave — y arrastra los compuestos colgados de esa forma (#62).
    #[tokio::test]
    async fn alias_huerfano_se_barre_ptr_driven() {
        let pool = SessionPool::new();
        let w = wrapper(&pool, "sftp://oscar@h");
        // Vive SOLO bajo la forma alias; la canónica no está en el mapa.
        insert(&pool, "sftp://h", &w);
        let comp: Arc<dyn Provider> = Arc::new(AllPu);
        insert(&pool, "zip+sftp://h", &comp);

        let _ = w.stat(&VPath::parse("sftp://h/x").expect("wire")).await;
        assert!(!contains(&pool, "sftp://h"), "alias barrido");
        assert!(!contains(&pool, "zip+sftp://h"), "compuesto arrastrado");
    }

    /// `alias_current` con la canónica ausente NO re-inserta nada (el Arc
    /// muerto que el caller retenga de un lookup viejo jamás vuelve al mapa).
    #[test]
    fn alias_current_sin_canonica_es_none() {
        let pool = SessionPool::new();
        assert!(
            pool.alias_current("sftp://oscar@h", "sftp://h".to_owned())
                .is_none()
        );
        assert!(!contains(&pool, "sftp://h"));
    }

    /// Una evicción RANCIA (otro Arc ya vive bajo la clave) no toca nada.
    #[tokio::test]
    async fn eviccion_rancia_no_pisa_la_sesion_nueva() {
        let pool = SessionPool::new();
        let viejo = wrapper(&pool, "sftp://h");
        let nuevo = wrapper(&pool, "sftp://h");
        insert(&pool, "sftp://h", &nuevo);
        // El VIEJO (ya fuera del mapa) falla y pide evicción: no barre.
        let _ = viejo.stat(&VPath::parse("sftp://h/x").expect("wire")).await;
        assert!(contains(&pool, "sftp://h"), "la sesión nueva sigue");
    }

    /// rust MINOR-3 (review #47): TODA la superficie del trait delega en el
    /// interior y OBSERVA el error — cada método sobre una sesión muerta
    /// evicta la clave. Si un método nuevo del trait no se delega, este test
    /// es el recordatorio (junto al comentario en el trait).
    #[tokio::test]
    async fn wrapper_observa_todos_los_metodos() {
        let pool = SessionPool::new();
        let key = "sftp://h";
        let p = VPath::parse("sftp://h/x").expect("wire");
        let w = wrapper(&pool, key);

        macro_rules! evicta {
            ($llamada:expr) => {{
                insert(&pool, key, &w);
                let _ = $llamada;
                assert!(!contains(&pool, key), stringify!($llamada));
            }};
        }

        evicta!(w.stat(&p).await);
        evicta!(w.list(&p).await);
        evicta!(w.list_skipped(&p).await);
        evicta!(w.read(&p, None).await);
        evicta!(w.node_id(&p, norte_vfs::FollowLinks::No).await);
        evicta!(w.read_link(&p).await);
        evicta!(w.trash(&p).await);
        evicta!(w.gc_partials(&p, Duration::from_secs(1)).await);
        evicta!(w.restore_trashed(&p).await);
        evicta!(w.symlink(&p, b"t", norte_vfs::SymlinkKind::File).await);
        evicta!(w.write(&p).await);
        evicta!(w.open_resumable(&p).await);
        evicta!(w.partial_digest(&p, 0).await);
        evicta!(w.mkdir(&p).await);
        evicta!(w.remove(&p).await);
        evicta!(w.rename(&p, &p).await);
        evicta!(w.copy_native(&p, &p).await);
    }
}
