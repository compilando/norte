//! Lo que envuelve a UNA llamada: su plazo y su cancelación.
//!
//! Dos cosas que no son del protocolo pero sí del cliente. El plazo, porque
//! un daemon vivo-pero-atascado —un `stat` sobre un NFS muerto— jamás debe
//! congelar un frontend. Y la cancelación al ABANDONAR: si el future de una
//! llamada se dropea (el usuario pulsó Esc, el `select!` eligió otra rama),
//! el daemon tiene que enterarse, o seguiría trabajando para nadie.

use std::sync::Arc;
use std::time::Duration;

use norte_proto::{Error, methods};

use crate::rpc::{Client, ClientError};

/// Tope de una llamada RPC: un daemon vivo-pero-atascado (stat sobre un
/// NFS muerto) jamás congela el frontend (M4 del rust-reviewer).
pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Mapea el error del cliente RPC a la taxonomía (el contrato de los
/// frontends es SIEMPRE la taxonomía, spec §17.7).
/// Guard drop-based de un submit remoto (#74): si cae ARMADO con un id
/// capturado, notifica `rpc.cancel {id}` (sync, best-effort — `notify`
/// solo encola el frame; canal muerto = no-op). `armed=false` tras
/// recibir la respuesta.
pub(super) struct CancelOnAbandon {
    pub(super) client: Arc<Client>,
    /// Id de la request en vuelo; `0` = aún sin asignar (el contador del
    /// [`Client`] arranca en 1 — jamás emite 0).
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

/// Traduce un error del cliente a la taxonomía del wire.
///
/// Es lo que hace [`super::RemoteBackend::connect`] por dentro, y es público
/// porque un frontend que use [`super::RemoteBackend::connect_detallado`]
/// —para poder enseñar lo que dijo un daemon que murió— necesita traducir
/// todos los DEMÁS casos igual que se traducirían solos.
///
/// La pérdida es deliberada y va en un sentido: la taxonomía no lleva texto
/// libre, así que lo que el daemon escribió no sobrevive a esta función.
///
/// ```
/// use norte_client::{ClientError, to_taxonomy};
/// // Arrancó y murió: reintentar no lo arregla, y se dice.
/// let e = to_taxonomy(ClientError::SpawnFailed {
///     status: Some(1),
///     stderr: "journal corrupto".to_owned(),
/// });
/// assert_eq!(e, norte_proto::Error::ProviderUnavailable { retryable: false });
/// // Todavía no acepta: eso sí se reintenta.
/// let e = to_taxonomy(ClientError::SpawnTimeout);
/// assert_eq!(e, norte_proto::Error::ProviderUnavailable { retryable: true });
/// ```
#[must_use]
pub fn to_taxonomy(e: ClientError) -> Error {
    match e {
        // La taxonomía viaja en data (ADR 0011): se entrega tal cual.
        ClientError::Rpc(rpc) => rpc.data.unwrap_or(Error::Internal { panic: false }),
        ClientError::Io(_) | ClientError::ConnectionClosed | ClientError::SpawnTimeout => {
            Error::ProviderUnavailable { retryable: true }
        }
        // Arrancó y murió: reintentar no lo va a arreglar, así que
        // `retryable: false`. Lo que DIJO no cabe en la taxonomía —no lleva
        // texto libre, y no es un error del wire sino de esta máquina—, así
        // que quien lo necesite conecta con
        // [`super::RemoteBackend::connect_detallado`].
        ClientError::SpawnFailed { .. } => Error::ProviderUnavailable { retryable: false },
        ClientError::BadResult(_) => Error::Internal { panic: false },
        ClientError::ForeignDaemon => Error::PermissionDenied,
    }
}
