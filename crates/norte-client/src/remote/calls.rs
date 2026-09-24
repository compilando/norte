//! What wraps ONE call: its deadline and its cancellation.
//!
//! Two things that are not the protocol's but are the client's. The
//! deadline, because a daemon that is alive-but-stuck — a `stat` over a dead
//! NFS — must never freeze a frontend. And cancellation on ABANDONMENT: if a
//! call's future is dropped (the user pressed Esc, `select!` chose another
//! branch), the daemon has to find out, or it would keep working for
//! nobody.

use std::sync::Arc;
use std::time::Duration;

use norte_proto::{Error, methods};

use crate::rpc::{Client, ClientError};

/// Cap on an RPC call: a daemon that is alive-but-stuck (a stat over a dead
/// NFS) never freezes the frontend (rust-reviewer M4).
pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Maps the RPC client's error onto the taxonomy (frontends' contract is
/// ALWAYS the taxonomy, spec §17.7).
/// Drop-based guard for a remote submit (#74): if it drops ARMED with a
/// captured id, it notifies `rpc.cancel {id}` (sync, best-effort — `notify`
/// only queues the frame; a dead channel = no-op). `armed=false` after
/// receiving the response.
pub(super) struct CancelOnAbandon {
    pub(super) client: Arc<Client>,
    /// Id of the request in flight; `0` = not yet assigned (the [`Client`]'s
    /// counter starts at 1 — it never emits 0).
    pub(super) id: Arc<std::sync::atomic::AtomicU64>,
    pub(super) armed: bool,
}

impl Drop for CancelOnAbandon {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let id = self.id.load(std::sync::atomic::Ordering::SeqCst);
        if id == 0 {
            return;
        }
        let _ = self.client.notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(id),
            },
        );
    }
}

/// Translates a client error into the wire's taxonomy.
///
/// This is what [`super::RemoteBackend::connect`] does internally, and it is
/// public because a frontend using [`super::RemoteBackend::connect_detallado`]
/// — to be able to show what a daemon that died said — needs to translate
/// all the OTHER cases exactly as they would translate themselves.
///
/// The loss is deliberate and goes one way: the taxonomy carries no free
/// text, so what the daemon wrote does not survive this function.
///
/// ```
/// use norte_client::{ClientError, to_taxonomy};
/// // It started and died: retrying does not fix it, and it says so.
/// let e = to_taxonomy(ClientError::SpawnFailed {
///     status: Some(1),
///     stderr: "corrupt journal".to_owned(),
/// });
/// assert_eq!(e, norte_proto::Error::ProviderUnavailable { retryable: false });
/// // Not accepting yet: that one IS retried.
/// let e = to_taxonomy(ClientError::SpawnTimeout);
/// assert_eq!(e, norte_proto::Error::ProviderUnavailable { retryable: true });
/// ```
#[must_use]
pub fn to_taxonomy(e: ClientError) -> Error {
    match e {
        // The taxonomy travels in data (ADR 0011): delivered as is.
        ClientError::Rpc(rpc) => rpc.data.unwrap_or(Error::Internal { panic: false }),
        ClientError::Io(_) | ClientError::ConnectionClosed | ClientError::SpawnTimeout => {
            Error::ProviderUnavailable { retryable: true }
        }
        // It started and died: retrying is not going to fix it, so
        // `retryable: false`. What it SAID does not fit the taxonomy — it
        // carries no free text, and it is not an error of the wire but of
        // this machine — so whoever needs it connects with
        // [`super::RemoteBackend::connect_detallado`].
        ClientError::SpawnFailed { .. } => Error::ProviderUnavailable { retryable: false },
        ClientError::BadResult(_) => Error::Internal { panic: false },
        ClientError::ForeignDaemon => Error::PermissionDenied,
    }
}
