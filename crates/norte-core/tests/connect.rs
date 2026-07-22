//! Fase 6e (ADR 0015 A/G): el Engine resuelve providers REMOTOS bajo demanda
//! vía un [`RemoteConnector`] — cacheados por `scheme://authority`, con el
//! flujo TOFU (`HostKeyUnknown` → `trust_host_key`) pasando por el conector.
//! La lógica de matching de `connections.toml` se prueba aparte (unit del
//! módulo); aquí va el contrato Engine↔conector con un conector FALSO.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use norte_core::Engine;
use norte_core::connect::{
    Connected, ConnectionObserver, ConnectionWarning, ConnectionWarningReason, RemoteConnector,
};
use norte_proto::{Entry, EntryKind, Error, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

/// Provider trivial que responde a CUALQUIER scheme (el fake de conexión).
struct EcoProvider;

#[async_trait]
impl Provider for EcoProvider {
    fn scheme(&self) -> &'static str {
        "sftp"
    }
    fn capabilities(&self) -> norte_proto::Capabilities {
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::empty(),
            max_path: None,
        }
    }
    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        Ok(Entry {
            path: p.clone(),
            kind: EntryKind::Dir,
            size: None,
            mtime_ms: None,
        })
    }
    async fn list(&self, _p: &VPath) -> Result<norte_vfs::EntryStream, Error> {
        Err(Error::Unsupported)
    }
    async fn read(
        &self,
        _p: &VPath,
        _range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        Err(Error::Unsupported)
    }
    async fn write(&self, _p: &VPath) -> Result<Box<dyn norte_vfs::ByteSink>, Error> {
        Err(Error::Unsupported)
    }
    async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    async fn remove(&self, _p: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    async fn rename(&self, _from: &VPath, _to: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// Conector falso: cuenta llamadas y puede fallar con `HostKeyUnknown` hasta
/// que se confíe la clave.
struct FakeConnector {
    connects: AtomicUsize,
    trusts: AtomicUsize,
    /// `true` = el primer connect devuelve `HostKeyUnknown` hasta trust.
    tofu: std::sync::Mutex<bool>,
    /// Avisos que el connect devuelve al establecer (#44: default vacío).
    warn_on_connect: Vec<ConnectionWarning>,
}

impl FakeConnector {
    fn new(tofu: bool) -> Self {
        Self {
            connects: AtomicUsize::new(0),
            trusts: AtomicUsize::new(0),
            tofu: std::sync::Mutex::new(tofu),
            warn_on_connect: Vec::new(),
        }
    }

    /// Como [`Self::new`] pero cada connect devuelve estos avisos (#44).
    fn with_warnings(warnings: Vec<ConnectionWarning>) -> Self {
        Self {
            warn_on_connect: warnings,
            ..Self::new(false)
        }
    }
}

#[async_trait]
impl RemoteConnector for FakeConnector {
    async fn connect(&self, scheme: &str, authority: &str) -> Result<Connected, Error> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scheme, "sftp");
        if *self.tofu.lock().unwrap() {
            return Err(Error::HostKeyUnknown {
                host: authority.to_string(),
                port: Some(22),
                algo: "ssh-ed25519".into(),
                fingerprint: "SHA256:xyz".into(),
            });
        }
        Ok(Connected {
            provider: Arc::new(EcoProvider),
            warnings: self.warn_on_connect.clone(),
        })
    }

    async fn trust_host_key(
        &self,
        _host: &str,
        _port: Option<u16>,
        _fingerprint: &str,
    ) -> Result<(), Error> {
        self.trusts.fetch_add(1, Ordering::SeqCst);
        *self.tofu.lock().unwrap() = false;
        Ok(())
    }
}

fn vp(s: &str) -> VPath {
    VPath::parse(s).expect("wire válido")
}

/// El provider remoto se establece UNA vez y se cachea por authority: dos
/// stats al mismo host = un connect; otro host = otro connect.
#[tokio::test]
async fn conecta_bajo_demanda_y_cachea_por_authority() {
    let engine = Engine::new();
    let conn = Arc::new(FakeConnector::new(false));
    engine.set_connector(conn.clone());

    engine
        .stat(&vp("sftp://a.example/x"))
        .await
        .expect("stat 1");
    engine
        .stat(&vp("sftp://a.example/y"))
        .await
        .expect("stat 2");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1, "cacheado");

    engine
        .stat(&vp("sftp://b.example/x"))
        .await
        .expect("stat 3");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 2, "otra authority");
}

/// Sin conector, un scheme sin provider sigue siendo Unsupported (M0/M1).
#[tokio::test]
async fn sin_conector_sigue_unsupported() {
    let engine = Engine::new();
    let err = engine.stat(&vp("sftp://h/x")).await.unwrap_err();
    assert!(matches!(err, Error::Unsupported), "fue {err:?}");
}

/// Un provider registrado por scheme (p. ej. mem en tests, file local) tiene
/// prioridad y NO dispara el conector.
#[tokio::test]
async fn provider_registrado_no_dispara_conector() {
    let engine = Engine::new();
    let conn = Arc::new(FakeConnector::new(false));
    engine.set_connector(conn.clone());
    let mem = MemProvider::new();
    mem.mkdir(&vp("mem://x/d")).await.expect("mkdir");
    engine.register_provider(Arc::new(mem));

    engine.stat(&vp("mem://x/d")).await.expect("stat mem");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 0);
}

/// Flujo TOFU completo a través del Engine: primer contacto →
/// `HostKeyUnknown` (con fingerprint), `trust_host_key`, reintento conecta.
#[tokio::test]
async fn tofu_error_trust_y_reintento() {
    let engine = Engine::new();
    let conn = Arc::new(FakeConnector::new(true));
    engine.set_connector(conn.clone());

    let err = engine.stat(&vp("sftp://h/x")).await.unwrap_err();
    let Error::HostKeyUnknown { fingerprint, .. } = &err else {
        panic!("esperaba HostKeyUnknown, fue {err:?}");
    };
    assert_eq!(fingerprint, "SHA256:xyz");

    engine
        .trust_host_key("h", Some(22), "SHA256:xyz")
        .await
        .expect("trust");
    assert_eq!(conn.trusts.load(Ordering::SeqCst), 1);

    // Reintento: ahora conecta (y un fallo previo NO quedó cacheado).
    engine
        .stat(&vp("sftp://h/x"))
        .await
        .expect("stat tras trust");
}

/// Conector que nunca resuelve: simula un servidor que acepta TCP y calla.
struct HangingConnector;

#[async_trait]
impl RemoteConnector for HangingConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, Error> {
        std::future::pending().await
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        std::future::pending().await
    }
}

/// El establecimiento de conexión tiene TIMEOUT: un host que acepta TCP y
/// calla no cuelga la operación para siempre (el connect ocurre ANTES de que
/// exista una Task cancelable — regla 3 exige que no sea indefinido).
#[tokio::test(start_paused = true)]
async fn connect_colgado_expira_con_timeout() {
    let engine = Engine::new();
    engine.set_connector(Arc::new(HangingConnector));
    let err = engine.stat(&vp("sftp://h/x")).await.unwrap_err();
    assert!(
        matches!(err, Error::ProviderUnavailable { retryable: true }),
        "fue {err:?}"
    );
}

/// Dos peticiones concurrentes al mismo host: la SEGUNDA en registrarse no
/// pisa a la primera (double-check bajo el write lock) — no quedan dos
/// sesiones vivas indistinguibles.
#[tokio::test]
async fn connect_concurrente_no_duplica_registro() {
    let engine = Arc::new(Engine::new());
    let conn = Arc::new(FakeConnector::new(false));
    engine.set_connector(conn.clone());
    let (px, py) = (vp("sftp://h/x"), vp("sftp://h/y"));
    let (a, b) = tokio::join!(engine.stat(&px), engine.stat(&py));
    a.expect("stat a");
    b.expect("stat b");
    // Pueden haberse disparado 1 o 2 connects (carrera), pero un tercer
    // acceso reutiliza SIEMPRE el registrado (no re-conecta).
    let antes = conn.connects.load(Ordering::SeqCst);
    engine.stat(&vp("sftp://h/z")).await.expect("stat c");
    assert_eq!(conn.connects.load(Ordering::SeqCst), antes);
}

/// Conector con puerta: `connect` cuenta la llamada y espera a que el test
/// abra la puerta — permite tener DOS waiters pendientes del mismo dial.
struct GatedConnector {
    connects: AtomicUsize,
    gate: tokio::sync::Semaphore,
}

impl GatedConnector {
    fn new() -> Self {
        Self {
            connects: AtomicUsize::new(0),
            gate: tokio::sync::Semaphore::new(0),
        }
    }
}

#[async_trait]
impl RemoteConnector for GatedConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, Error> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let _permit = self.gate.acquire().await.expect("gate viva");
        Ok(Connected {
            provider: Arc::new(EcoProvider),
            warnings: Vec::new(),
        })
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// #47: dos peticiones concurrentes al mismo host esperan el MISMO dial —
/// exactamente UN connect, jamás una sesión duplicada transitoria.
#[tokio::test]
async fn connect_concurrente_es_single_flight() {
    let engine = Arc::new(Engine::new());
    let conn = Arc::new(GatedConnector::new());
    engine.set_connector(conn.clone());

    let e1 = Arc::clone(&engine);
    let t1 = tokio::spawn(async move { e1.stat(&vp("sftp://h/x")).await });
    let e2 = Arc::clone(&engine);
    let t2 = tokio::spawn(async move { e2.stat(&vp("sftp://h/y")).await });
    // Espera a que el dial haya arrancado (los dos stats ya en vuelo).
    while conn.connects.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    tokio::task::yield_now().await;
    conn.gate.add_permits(2);
    t1.await.expect("join").expect("stat 1");
    t2.await.expect("join").expect("stat 2");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1, "un solo dial");
}

/// Marca `cancelled` cuando el future del connect se DROPEA a mitad.
struct DropProbe(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Conector colgado que detecta la cancelación (drop del future en vuelo).
struct ProbedHangingConnector {
    started: AtomicUsize,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl RemoteConnector for ProbedHangingConnector {
    async fn connect(&self, _s: &str, _a: &str) -> Result<Connected, Error> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let _probe = DropProbe(self.cancelled.clone());
        std::future::pending().await
    }
    async fn trust_host_key(&self, _h: &str, _p: Option<u16>, _f: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// #47: si TODOS los waiters abandonan (drop del future — p. ej. `rpc.cancel`
/// dropea el dispatch, #72), el dial en vuelo se CANCELA y la clave queda
/// limpia: un acceso posterior vuelve a marcar.
#[tokio::test]
async fn abandono_de_todos_los_waiters_cancela_el_dial() {
    let engine = Arc::new(Engine::new());
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let conn = Arc::new(ProbedHangingConnector {
        started: AtomicUsize::new(0),
        cancelled: cancelled.clone(),
    });
    engine.set_connector(conn.clone());

    let e1 = Arc::clone(&engine);
    let waiter = tokio::spawn(async move { e1.stat(&vp("sftp://h/x")).await });
    while conn.started.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    waiter.abort();
    let _ = waiter.await;
    // El job procesa la cancelación en su propia task: dale turnos.
    for _ in 0..50 {
        if cancelled.load(Ordering::SeqCst) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(cancelled.load(Ordering::SeqCst), "el dial se canceló");

    // La clave quedó limpia: el siguiente acceso vuelve a marcar (no se
    // queda esperando a un job zombi).
    let e2 = Arc::clone(&engine);
    let again = tokio::spawn(async move { e2.stat(&vp("sftp://h/x")).await });
    for _ in 0..500 {
        if conn.started.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        conn.started.load(Ordering::SeqCst),
        2,
        "re-marca tras limpiar"
    );
    again.abort();
    let _ = again.await;
}

/// `trust_host_key` sin conector configurado es Unsupported, no un panic.
#[tokio::test]
async fn trust_sin_conector_es_unsupported() {
    let engine = Engine::new();
    let err = engine
        .trust_host_key("h", Some(22), "SHA256:x")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported), "fue {err:?}");
}

/// Observer de avisos de conexión que acumula lo recibido (#44).
struct RecordingObserver {
    seen: Arc<std::sync::Mutex<Vec<ConnectionWarning>>>,
}

impl ConnectionObserver for RecordingObserver {
    fn on_connection_warning(&self, warning: &ConnectionWarning) {
        self.seen
            .lock()
            .expect("lock de test")
            .push(warning.clone());
    }
}

/// #44: un connect que degrada TLS entrega el aviso al observer instalado,
/// EXACTAMENTE una vez por establecimiento (el provider se cachea después).
#[tokio::test]
async fn observer_recibe_el_aviso_de_degradacion() {
    let engine = Engine::new();
    let warning = ConnectionWarning {
        scheme: "sftp".into(),
        host: "a.example".into(),
        reason: ConnectionWarningReason::TlsAuthRejected,
    };
    let conn = Arc::new(FakeConnector::with_warnings(vec![warning.clone()]));
    engine.set_connector(conn.clone());
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    engine.set_connection_observer(Arc::new(RecordingObserver { seen: seen.clone() }));

    // Primer acceso: establece la conexión y emite el aviso.
    engine
        .stat(&vp("sftp://a.example/x"))
        .await
        .expect("stat 1");
    // Segundo acceso al mismo host: cacheado, NO re-establece → no re-avisa.
    engine
        .stat(&vp("sftp://a.example/y"))
        .await
        .expect("stat 2");

    let got = seen.lock().expect("lock de test");
    assert_eq!(*got, vec![warning], "exactamente un aviso, una vez");
    assert_eq!(conn.connects.load(Ordering::SeqCst), 1, "cacheado");
}

/// #44: un connect SIN avisos jamás llama al observer.
#[tokio::test]
async fn observer_no_se_llama_sin_avisos() {
    let engine = Engine::new();
    let conn = Arc::new(FakeConnector::new(false));
    engine.set_connector(conn.clone());
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    engine.set_connection_observer(Arc::new(RecordingObserver { seen: seen.clone() }));

    engine.stat(&vp("sftp://a.example/x")).await.expect("stat");
    assert!(
        seen.lock().expect("lock de test").is_empty(),
        "sin degradación no hay aviso"
    );
}
