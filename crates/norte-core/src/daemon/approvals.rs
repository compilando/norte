//! Router de aprobaciones del daemon (M3-3b Task 4): resuelve un `Ask` de
//! policy con un round-trip humano por el wire. El gate del engine llama a
//! [`ApprovalResolver::request`]; este router registra la pendiente, difunde
//! `policy.approval_required` a los frontends suscritos y suspende la llamada
//! hasta el `policy.decide` correspondiente (o el TTL, que deniega).
//!
//! Orden de construcción (el "chicken-egg" del plan): el resolver se crea
//! ANTES que el engine y el daemon; el engine lo recibe en
//! [`Engine::with_policy`](crate::Engine::with_policy) y el daemon en
//! [`Daemon::bind_with_policy`](super::Daemon::bind_with_policy), que le
//! inyecta el broadcaster (la salida hacia los suscriptores). Sin broadcaster
//! instalado (engine sin daemon), un `Ask` se deniega INMEDIATO fail-closed:
//! nadie podría oír la pregunta, colgar el TTL solo retrasaría lo inevitable.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use norte_proto::methods::{PendingApproval, PolicyApprovalRequired};
use tokio::sync::oneshot;

use crate::approval::{ApprovalOutcome, ApprovalRequest, ApprovalResolver};
use crate::journal::Actor;

/// TTL por defecto de una aprobación pendiente: vencido, el `Ask` se deniega
/// (`not-approved`). Un humano ausente no deja la operación colgada eterna.
pub const DEFAULT_APPROVAL_TTL: Duration = Duration::from_mins(1);

/// Aprobaciones pendientes simultáneas toleradas. Estructuralmente ya están
/// acotadas (cada pendiente suspende el dispatch SERIAL de una conexión, y las
/// conexiones tienen su propio tope), pero el cinturón es barato: por encima,
/// un `Ask` nuevo se deniega fail-closed en vez de crecer sin límite.
const MAX_PENDING_APPROVALS: usize = 256;

/// Salida del router hacia los frontends: encapsula el encode + broadcast del
/// daemon sin que este módulo conozca su `Shared` (lo instala
/// `bind_with_policy` con un closure que captura un `Weak`).
type Broadcaster = Box<dyn Fn(PolicyApprovalRequired) + Send + Sync>;

/// Una aprobación en vuelo: metadatos para `policy.pending` (resync) y el
/// canal por el que `policy.decide` despierta al gate suspendido.
struct PendingEntry {
    session: Option<String>,
    op: String,
    paths: Vec<String>,
    decide: oneshot::Sender<bool>,
}

struct Inner {
    pending: Mutex<HashMap<u64, PendingEntry>>,
    next_id: AtomicU64,
    broadcaster: Mutex<Option<Broadcaster>>,
}

/// Resolver de `Ask` del daemon (M3-3b). Se comparte por `Arc` entre el engine
/// (que lo llama desde el gate) y el `Shared` del daemon (que le enruta
/// `policy.decide`/`policy.pending`).
pub struct DaemonApprovalResolver {
    inner: Arc<Inner>,
    ttl: Duration,
}

impl Default for DaemonApprovalResolver {
    fn default() -> Self {
        Self::new(DEFAULT_APPROVAL_TTL)
    }
}

impl DaemonApprovalResolver {
    /// Con un TTL explícito por aprobación (los tests usan uno corto).
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        // Arranque NO-cero (security MINOR-1): tras un restart del daemon, un
        // frontend con un modal rancio que reconecta no debe acertar por
        // colisión con una pendiente NUEVA (ambas secuencias arrancarían en
        // 0). El reloj basta como separador best-effort; la unicidad intra-
        // proceso la da el fetch_add.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| {
                u64::try_from(d.as_nanos() & u128::from(u64::MAX)).unwrap_or(0)
            });
        Self {
            inner: Arc::new(Inner {
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(seed),
                broadcaster: Mutex::new(None),
            }),
            ttl,
        }
    }

    /// Instala la salida hacia los frontends. La llama el daemon al enlazar
    /// (`bind_with_policy`); pisar una previa es un no-op razonable (último
    /// daemon gana — en la práctica hay uno).
    ///
    /// # Panics
    /// Solo si el lock interno queda envenenado.
    pub fn set_broadcaster(&self, b: Broadcaster) {
        *self
            .inner
            .broadcaster
            .lock()
            .expect("broadcaster lock sano") = Some(b);
    }

    /// Resuelve la pendiente `approval_id` con `approve` y despierta al gate.
    /// `false` si el id no existe, ya se decidió, o el peticionario ya no
    /// escucha (TTL recién vencido / dispatch cancelado — la decisión NO tuvo
    /// efecto y el ack no debe mentir, ni el log de auditoría M3-5): el
    /// llamante lo traduce a `INVALID_PARAMS`. Una decisión consume la
    /// pendiente — un segundo `decide` del mismo id falla (anti
    /// doble-decisión).
    ///
    /// # Panics
    /// Solo si el lock interno queda envenenado.
    #[must_use]
    pub fn decide(&self, approval_id: u64, approve: bool) -> bool {
        let entry = self
            .inner
            .pending
            .lock()
            .expect("pending approvals lock sano")
            .remove(&approval_id);
        match entry {
            // `send` falla si el receptor murió en la carrera (timeout ya
            // consumido, guard aún sin dropear): se reporta como vencida.
            Some(e) => e.decide.send(approve).is_ok(),
            None => false,
        }
    }

    /// Instantánea de las aprobaciones pendientes (resync de `policy.pending`
    /// para un frontend que conecta después del broadcast). Orden estable por
    /// `approval_id`.
    ///
    /// # Panics
    /// Solo si el lock interno queda envenenado.
    #[must_use]
    pub fn pending(&self) -> Vec<PendingApproval> {
        let map = self
            .inner
            .pending
            .lock()
            .expect("pending approvals lock sano");
        let mut out: Vec<PendingApproval> = map
            .iter()
            .map(|(&approval_id, e)| PendingApproval {
                approval_id,
                session: e.session.clone(),
                op: e.op.clone(),
                paths: e.paths.clone(),
            })
            .collect();
        out.sort_by_key(|p| p.approval_id);
        out
    }
}

/// Guard RAII de la pendiente: si el future de [`request`] se cancela (el
/// shutdown del daemon corta el dispatch) o el TTL vence, la entrada sale del
/// mapa al soltarse el guard — jamás una pendiente huérfana que
/// `policy.pending` mostraría para siempre. Tras un `decide` el remove es un
/// no-op benigno.
///
/// La MUERTE de la conexión peticionaria SÍ cancela este future (#64,
/// resuelto): la lectura del socket vive en su propia task y un EOF/reset
/// cancela `peer_gone`, dropeando el dispatch suspendido → este guard retira
/// la pendiente. (Punto ciego residual acotado: si el peer dejó >`INBOX_FRAMES`
/// frames en vuelo, el reader queda bloqueado en el envío al inbox y no
/// observa el EOF hasta que el Ask resuelva por TTL — los clientes de norte
/// son request/response, así que no aplica.)
///
/// [`request`]: ApprovalResolver::request
struct PendingGuard {
    inner: Arc<Inner>,
    id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.inner
            .pending
            .lock()
            .expect("pending approvals lock sano")
            .remove(&self.id);
    }
}

#[async_trait]
impl ApprovalResolver for DaemonApprovalResolver {
    /// Suspende hasta `policy.decide` o TTL. La suspensión retiene SOLO el
    /// dispatch (serial) de la conexión que pidió la op — no el pool de
    /// workers del scheduler ni a otras conexiones.
    async fn request(&self, req: ApprovalRequest) -> ApprovalOutcome {
        // La sesión de display sale del ACTOR fijado server-side en el
        // handshake, jamás de nada que el peticionario declare aquí.
        let session = match &req.actor {
            Actor::User => None,
            Actor::Agent { session } => Some(session.clone()),
            // M4: el id de plugin viaja en `session` como identificador de
            // display — si el modelo de plugins pide distinguirlo en el wire,
            // será campo nuevo en proto, no sobrecarga de este.
            Actor::Plugin { id } => Some(id.clone()),
        };
        let op = req.op.kind().to_owned();
        let (tx, rx) = oneshot::channel();
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut pending = self
                .inner
                .pending
                .lock()
                .expect("pending approvals lock sano");
            if pending.len() >= MAX_PENDING_APPROVALS {
                tracing::warn!("aprobaciones pendientes al tope; Ask denegado fail-closed");
                return ApprovalOutcome::Denied;
            }
            pending.insert(
                id,
                PendingEntry {
                    session: session.clone(),
                    op: op.clone(),
                    paths: req.paths.clone(),
                    decide: tx,
                },
            );
        }
        let _guard = PendingGuard {
            inner: Arc::clone(&self.inner),
            id,
        };
        // Difunde DESPUÉS de registrar: un `policy.pending` concurrente la ve
        // por uno de los dos caminos, jamás por ninguno.
        {
            let broadcaster = self
                .inner
                .broadcaster
                .lock()
                .expect("broadcaster lock sano");
            let Some(broadcast) = broadcaster.as_ref() else {
                tracing::warn!("Ask sin broadcaster instalado: denegado fail-closed");
                return ApprovalOutcome::Denied;
            };
            broadcast(PolicyApprovalRequired {
                approval_id: id,
                session,
                op,
                paths: req.paths,
                ttl_ms: u64::try_from(self.ttl.as_millis()).unwrap_or(u64::MAX),
            });
        }
        match tokio::time::timeout(self.ttl, rx).await {
            Ok(Ok(true)) => ApprovalOutcome::Approved,
            // El sender solo muere sin enviar si el resolver entero se está
            // desmontando: denegar es lo único honesto.
            Ok(Ok(false) | Err(_)) => ApprovalOutcome::Denied,
            Err(_elapsed) => ApprovalOutcome::TimedOut,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::PolicyOp;

    fn ask(resolver: &Arc<DaemonApprovalResolver>) -> tokio::task::JoinHandle<ApprovalOutcome> {
        let r = Arc::clone(resolver);
        tokio::spawn(async move {
            r.request(ApprovalRequest {
                actor: Actor::Agent {
                    session: "s1".into(),
                },
                op: PolicyOp::Copy,
                paths: vec!["mem:///a".into()],
            })
            .await
        })
    }

    /// Espera (con tope) a que haya exactamente `n` pendientes registradas.
    async fn wait_pending(resolver: &DaemonApprovalResolver, n: usize) {
        for _ in 0..200 {
            if resolver.pending().len() == n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("nunca hubo {n} pendientes");
    }

    #[tokio::test]
    async fn sin_broadcaster_deniega_inmediato_fail_closed() {
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
        let out = ask(&r).await.expect("join");
        assert_eq!(out, ApprovalOutcome::Denied);
        assert!(r.pending().is_empty(), "el guard limpió la pendiente");
    }

    #[tokio::test]
    async fn decide_aprueba_y_consume_la_pendiente() {
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
        r.set_broadcaster(Box::new(|_| {}));
        let task = ask(&r);
        wait_pending(&r, 1).await;
        let id = r.pending()[0].approval_id;
        assert!(r.decide(id, true), "existía");
        assert_eq!(task.await.expect("join"), ApprovalOutcome::Approved);
        assert!(!r.decide(id, true), "una decisión consume el id");
        assert!(r.pending().is_empty());
    }

    #[tokio::test]
    async fn decide_deniega() {
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
        r.set_broadcaster(Box::new(|_| {}));
        let task = ask(&r);
        wait_pending(&r, 1).await;
        let id = r.pending()[0].approval_id;
        assert!(r.decide(id, false));
        assert_eq!(task.await.expect("join"), ApprovalOutcome::Denied);
    }

    #[tokio::test]
    async fn ttl_vencido_es_timed_out_y_limpia() {
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_millis(50)));
        r.set_broadcaster(Box::new(|_| {}));
        let task = ask(&r);
        assert_eq!(task.await.expect("join"), ApprovalOutcome::TimedOut);
        assert!(r.pending().is_empty(), "el TTL no deja huérfanas");
    }

    #[tokio::test]
    async fn cancelar_el_future_limpia_la_pendiente() {
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_secs(30)));
        r.set_broadcaster(Box::new(|_| {}));
        let task = ask(&r);
        wait_pending(&r, 1).await;
        // El future se cancela (en el daemon real: el shutdown corta el
        // dispatch — la muerte del peer NO llega aquí, ver rustdoc del guard
        // e issue #64). El guard debe sacar la pendiente del mapa.
        task.abort();
        let _ = task.await;
        wait_pending(&r, 0).await;
    }

    #[tokio::test]
    async fn la_notificacion_lleva_lo_registrado() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PolicyApprovalRequired>();
        let r = Arc::new(DaemonApprovalResolver::new(Duration::from_millis(50)));
        r.set_broadcaster(Box::new(move |n| {
            let _ = tx.send(n);
        }));
        let task = ask(&r);
        let notif = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("el broadcast salió")
            .expect("canal vivo");
        assert_eq!(notif.session.as_deref(), Some("s1"));
        assert_eq!(notif.op, "copy");
        assert_eq!(notif.paths, vec!["mem:///a".to_string()]);
        assert_eq!(notif.ttl_ms, 50);
        let _ = task.await;
    }
}
