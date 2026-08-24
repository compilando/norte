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
/// # Un daemon que MUERE al arrancar
///
/// Es un caso distinto de «tarda», y antes se leían igual: el `stderr` del
/// hijo iba a `/dev/null`, así que la única frase que explicaba el fallo —«el
/// journal es de antes de `undoes_seq`», «el socket lo tiene otro»— se perdía,
/// y el llamante esperaba los 3,2 s enteros para recibir un `SpawnTimeout` que
/// invita a reintentar algo que no va a cambiar nunca.
///
/// Ahora el `stderr` se captura y el hijo se vigila con `try_wait` en cada
/// vuelta: si murió, se devuelve [`ClientError::SpawnFailed`] con lo que dijo,
/// **sin agotar el backoff**.
///
/// Con un daemon que SÍ arranca queda una tubería que nadie lee, y eso se
/// llena: un daemon que escriba un aviso por stderr con el buffer lleno se
/// BLOQUEA en el `write`, o sea que la ventana lo colgaría por haberlo
/// arrancado. Por eso el éxito deja un hilo drenándola.
///
/// # Errors
/// I/O; [`ClientError::SpawnFailed`] si el daemon arrancó y murió; o
/// [`ClientError::SpawnTimeout`] si sigue vivo y no llega a aceptar.
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
    // El daemon es un proceso INDEPENDIENTE del frontend que lo parió, salvo
    // por el `stderr`: es por donde dice por qué no pudo arrancar, y tirarlo
    // deja al llamante sin la única explicación que existe.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    // Backoff total ≈ 3,2 s (documentado: "hasta ~3 s").
    for backoff_ms in [25u64, 50, 100, 200, 400, 800, 1600] {
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        if let Ok(stream) = UnixStream::connect(socket).await {
            drenar_stderr(child.stderr.take());
            return authenticated(stream).await;
        }
        // ¿Murió? Entonces esperar el resto del backoff no cambia nada, y lo
        // que escribió es lo único que explica el fallo. `try_wait` no
        // bloquea, y tras la muerte el `stderr` está cerrado: leerlo termina.
        if let Ok(Some(status)) = child.try_wait() {
            return Err(ClientError::SpawnFailed {
                status: status.code(),
                stderr: leer_stderr(child.stderr.take()).await,
            });
        }
    }
    drenar_stderr(child.stderr.take());
    Err(ClientError::SpawnTimeout)
}

/// Lo que el daemon dijo antes de morir, recortado y sin bytes de control.
///
/// Acotado a 4 KiB: es un mensaje de error para enseñar, no un log, y lo que
/// llega es la salida de otro proceso. Se lee en `spawn_blocking` porque es
/// I/O síncrona (regla 2) — y termina, porque el hijo ya murió y el extremo
/// de escritura está cerrado.
async fn leer_stderr(stderr: Option<std::process::ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let leido = tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        let mut buf = Vec::new();
        let _ = std::io::Read::by_ref(&mut stderr)
            .take(4096)
            .read_to_end(&mut buf);
        buf
    })
    .await
    .unwrap_or_default();
    String::from_utf8_lossy(&leido).trim().to_owned()
}

/// Deja la tubería vaciándose para siempre, tirando lo que llegue.
///
/// Sin esto, un daemon que arranca bien y luego escribe por `stderr` se
/// bloquea en cuanto llena el buffer de la tubería, porque en este proceso no
/// hay nadie leyendo. El hilo muere solo cuando el daemon cierra su extremo.
fn drenar_stderr(stderr: Option<std::process::ChildStderr>) {
    let Some(mut stderr) = stderr else {
        return;
    };
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn socket_que_no_existe() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("norte-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("no-hay-nadie.sock")
    }

    /// **Un daemon que arranca y MUERE devuelve lo que dijo**, y no el
    /// `SpawnTimeout` que invita a esperar (#300).
    ///
    /// La frase por `stderr` es lo único que explica por qué no va a arrancar
    /// nunca —un journal que no se puede migrar es el caso real—, y antes se
    /// tiraba a `/dev/null`.
    #[tokio::test]
    async fn un_daemon_que_muere_devuelve_lo_que_dijo() {
        let socket = socket_que_no_existe();
        let e = connect_or_spawn(&socket, || {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", "echo 'el journal no se puede migrar' >&2; exit 3"]);
            cmd
        })
        .await
        .expect_err("el daemon murió");

        match e {
            ClientError::SpawnFailed { status, stderr } => {
                assert_eq!(status, Some(3), "el código de salida se conserva");
                assert!(
                    stderr.contains("el journal no se puede migrar"),
                    "lo que dijo tiene que llegar entero: {stderr:?}"
                );
            }
            otro => panic!("tenía que ser SpawnFailed, fue {otro:?}"),
        }
    }

    /// Y no se agota el backoff esperándolo: ~3,2 s de espera sobre algo que
    /// ya murió son 3,2 s de ventana en blanco por nada.
    #[tokio::test]
    async fn morir_no_agota_el_backoff() {
        let socket = socket_que_no_existe();
        let antes = std::time::Instant::now();
        let _ = connect_or_spawn(&socket, || {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", "exit 1"]);
            cmd
        })
        .await;
        assert!(
            antes.elapsed() < Duration::from_secs(2),
            "se esperó el backoff entero: {:?}",
            antes.elapsed()
        );
    }

    /// Un daemon que arranca y NO dice nada por `stderr` sigue siendo un
    /// fallo con su código: el error existe aunque no haya frase que enseñar.
    #[tokio::test]
    async fn morir_en_silencio_tambien_es_un_fallo() {
        let socket = socket_que_no_existe();
        let e = connect_or_spawn(&socket, || {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", "exit 9"]);
            cmd
        })
        .await
        .expect_err("murió");
        assert!(
            matches!(
                e,
                ClientError::SpawnFailed {
                    status: Some(9),
                    ref stderr
                } if stderr.is_empty()
            ),
            "{e:?}"
        );
    }

    /// Un proceso que arranca, NO muere y tampoco escucha agota el backoff y
    /// da `SpawnTimeout`: eso sí es «todavía no», y el consejo de reintentar
    /// es el bueno. Es la distinción entera de #300 en una aserción.
    #[tokio::test]
    async fn el_que_sigue_vivo_y_no_escucha_sigue_siendo_timeout() {
        let socket = socket_que_no_existe();
        let e = connect_or_spawn(&socket, || {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", "sleep 30"]);
            cmd
        })
        .await
        .expect_err("nadie escucha");
        assert!(matches!(e, ClientError::SpawnTimeout), "{e:?}");
    }
}
