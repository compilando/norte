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
                let provider = Arc::clone(&connected.provider);
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
