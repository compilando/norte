//! El transporte de hoy: un socket UNIX autenticado por credenciales del peer.
//!
//! Aquí vive TODO lo que sabe que hay un `UnixStream` debajo. El JSON-RPC
//! enmarcado ([`crate::rpc`]) solo ve una mitad de lectura y otra de
//! escritura, así que añadir un named pipe de Windows —milestone aparte, ver
//! ADR 0066— es escribir otro módulo como este, no copiar la correlación ni
//! el enmarcado.

use std::path::Path;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::ClientError;

/// Conecta al socket y comprueba que lo sirve NUESTRO uid.
///
/// El cliente también autentica al servidor (simetría de la spec §17.6): en
/// el fallback de `/tmp`, un directorio pre-creado por otro usuario podría
/// servir un daemon impostor (hallazgo M2 del security-reviewer).
///
/// # Errors
/// I/O de conexión, o [`ClientError::ForeignDaemon`] si el peer no es nuestro.
pub(crate) async fn connect(socket: &Path) -> Result<(OwnedReadHalf, OwnedWriteHalf), ClientError> {
    let stream = UnixStream::connect(socket).await?;
    authenticated(stream).await
}

/// Conecta y, si no hay nadie escuchando, ARRANCA el daemon y reintenta con
/// backoff hasta ~3 s.
///
/// El hijo no se espera (`wait`): si el daemon muere antes que este proceso
/// queda un zombie hasta que salgamos — coste asumido de no hacer double-fork
/// (exigiría unsafe).
///
/// # Errors
/// I/O, o [`ClientError::SpawnTimeout`] si el daemon no llega a aceptar.
pub(crate) async fn connect_or_spawn(
    socket: &Path,
    spawn: impl FnOnce() -> std::process::Command,
) -> Result<(OwnedReadHalf, OwnedWriteHalf), ClientError> {
    match UnixStream::connect(socket).await {
        Ok(stream) => return authenticated(stream).await,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) => {}
        Err(e) => return Err(e.into()),
    }
    let mut cmd = spawn();
    // El daemon es un proceso INDEPENDIENTE del frontend que lo parió.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _child = cmd.spawn()?;
    // Backoff total ≈ 3,2 s (documentado: "hasta ~3 s").
    for backoff_ms in [25u64, 50, 100, 200, 400, 800, 1600] {
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        if let Ok(stream) = UnixStream::connect(socket).await {
            return authenticated(stream).await;
        }
    }
    Err(ClientError::SpawnTimeout)
}

async fn authenticated(stream: UnixStream) -> Result<(OwnedReadHalf, OwnedWriteHalf), ClientError> {
    let peer = stream.peer_cred()?;
    // uid propio sin unsafe (regla 5), en spawn_blocking (regla 2).
    let my_uid = tokio::task::spawn_blocking(crate::socket::process_uid_best_effort)
        .await
        .map_err(|e| ClientError::Io(std::io::Error::other(e)))?;
    if peer.uid() != my_uid {
        return Err(ClientError::ForeignDaemon);
    }
    Ok(stream.into_split())
}
