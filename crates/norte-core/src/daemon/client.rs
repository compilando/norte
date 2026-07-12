//! Cliente del daemon (ADR 0011): conexión UDS, handshake `initialize`,
//! requests correlacionadas por id y stream de notificaciones. Los
//! frontends lo cablean en la fase 3; hasta entonces lo ejercitan la CLI
//! (`norte daemon stop`) y los tests E2E.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use norte_proto::methods;
use norte_proto::wire::{
    FrameDecoder, JsonRpcVersion, Message, Notification, Request, RequestId, RpcError, codes,
    encode_frame,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

/// Errores del cliente.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// I/O del socket.
    #[error("i/o del cliente: {0}")]
    Io(#[from] std::io::Error),
    /// El servidor respondió con error (protocolo o aplicación).
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// La conexión murió con la request en vuelo.
    #[error("conexión cerrada con la request en vuelo")]
    ConnectionClosed,
    /// El result no deserializa al tipo esperado (server de otra versión).
    #[error("result malformado: {0}")]
    BadResult(#[from] serde_json::Error),
    /// No se pudo arrancar el daemon (autoarranque).
    #[error("el daemon no arrancó a tiempo")]
    SpawnTimeout,
    /// El socket lo sirve OTRO usuario: jamás se le habla (spoof en el
    /// fallback /tmp — hallazgo M2 del security-reviewer).
    #[error("el daemon del socket pertenece a otro usuario")]
    ForeignDaemon,
}

/// Respuestas en vuelo, por id de request.
type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value, RpcError>>>>>;

/// Conexión autenticada (implícitamente: mismo uid o el server la corta)
/// con el daemon.
pub struct Client {
    frames_out: mpsc::UnboundedSender<Vec<u8>>,
    notifications: mpsc::UnboundedReceiver<Notification>,
    pending: PendingMap,
    next_id: AtomicU64,
    /// Marcado por el reader ANTES de vaciar `pending`: una `call()`
    /// posterior al cierre falla en vez de colgarse (B1 del rust-reviewer).
    closed: Arc<AtomicBool>,
}

impl Client {
    /// Conecta al socket del daemon (sin handshake: ver [`Self::initialize`]).
    ///
    /// # Errors
    /// I/O de conexión.
    pub async fn connect(socket: &Path) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(socket).await?;
        Self::authenticated(stream).await
    }

    /// El cliente TAMBIÉN autentica al servidor (simetría de la spec
    /// §17.6): el peer del socket debe ser NUESTRO uid — en el fallback
    /// /tmp, un dir pre-creado por otro usuario podría servir un daemon
    /// impostor (M2 del security-reviewer).
    async fn authenticated(stream: UnixStream) -> Result<Self, ClientError> {
        let peer = stream.peer_cred()?;
        // uid propio sin unsafe (regla 5), en spawn_blocking (regla 2).
        let my_uid = tokio::task::spawn_blocking(super::process_uid_best_effort)
            .await
            .map_err(|e| ClientError::Io(std::io::Error::other(e)))?;
        if peer.uid() != my_uid {
            return Err(ClientError::ForeignDaemon);
        }
        Ok(Self::from_stream(stream))
    }

    /// Conecta, y si el socket no existe o nadie escucha, ARRANCA el daemon
    /// (`spawn` produce el `Command` ya configurado — los frontends deciden
    /// binario y flags) y reintenta con backoff hasta ~3 s.
    ///
    /// El hijo no se espera (`wait`): si el daemon muere antes que este
    /// proceso queda un zombie hasta que salgamos — coste asumido de no
    /// hacer double-fork (exigiría unsafe).
    ///
    /// # Errors
    /// I/O, o [`ClientError::SpawnTimeout`] si el daemon no llega a aceptar.
    pub async fn connect_or_spawn(
        socket: &Path,
        spawn: impl FnOnce() -> std::process::Command,
    ) -> Result<Self, ClientError> {
        match UnixStream::connect(socket).await {
            Ok(stream) => return Self::authenticated(stream).await,
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
                return Self::authenticated(stream).await;
            }
        }
        Err(ClientError::SpawnTimeout)
    }

    fn from_stream(stream: UnixStream) -> Self {
        let (mut reader, mut writer) = stream.into_split();
        let (frames_out, mut frames_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (notif_tx, notifications) = mpsc::unbounded_channel::<Notification>();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));

        tokio::spawn(async move {
            while let Some(frame) = frames_rx.recv().await {
                if writer.write_all(&frame).await.is_err() {
                    break;
                }
            }
            let _ = writer.shutdown().await;
        });

        let pending_reader = Arc::clone(&pending);
        let closed_reader = Arc::clone(&closed);
        tokio::spawn(async move {
            let mut decoder = FrameDecoder::new();
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n) = reader.read(&mut buf).await {
                if n == 0 || decoder.push(&buf[..n]).is_err() {
                    break;
                }
                while let Some(frame) = decoder.next_frame() {
                    let Ok(msg) = serde_json::from_slice::<Message>(&frame) else {
                        continue; // frame corrupto del server: se ignora
                    };
                    match msg {
                        Message::Response(resp) => {
                            let Some(RequestId::Num(id)) = resp.id else {
                                continue;
                            };
                            let waiter = pending_reader
                                .lock()
                                .expect("pending lock sano")
                                .remove(&id);
                            if let Some(tx) = waiter {
                                let outcome = resp.outcome().cloned();
                                let _ = tx.send(outcome);
                            }
                        }
                        Message::Notification(n) => {
                            let _ = notif_tx.send(n);
                        }
                        // El server no nos manda requests en M2.
                        Message::Request(_) => {}
                    }
                }
            }
            // Conexión muerta: primero el flag (una call() nueva ya no se
            // registra), después despertar a los que esperaban. Ese orden
            // cierra la carrera insert/clear (B1 del rust-reviewer).
            closed_reader.store(true, Ordering::SeqCst);
            pending_reader.lock().expect("pending lock sano").clear();
        });

        Self {
            frames_out,
            notifications,
            pending,
            next_id: AtomicU64::new(1),
            closed,
        }
    }

    /// Handshake obligatorio (ADR 0011).
    ///
    /// # Errors
    /// [`ClientError::Rpc`] si el core rechaza versión o encoding.
    pub async fn initialize(
        &mut self,
        client_info: methods::ClientInfo,
    ) -> Result<methods::InitializeResult, ClientError> {
        self.call(
            methods::INITIALIZE,
            &methods::InitializeParams {
                client_info,
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
            },
        )
        .await
    }

    /// Request con respuesta tipada.
    ///
    /// # Errors
    /// [`ClientError`]: I/O, error RPC del server, conexión muerta o result
    /// que no deserializa.
    ///
    /// # Panics
    /// Nunca: el lock interno no puede envenenarse (nadie panica con él).
    pub async fn call<P, R>(&self, method: &str, params: &P) -> Result<R, ClientError>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let params = serde_json::to_value(params)?;
        let req = Request {
            jsonrpc: JsonRpcVersion,
            id: RequestId::Num(id),
            method: method.to_owned(),
            params: Some(params),
        };
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending lock sano")
            .insert(id, tx);
        // El orden importa: insertar y DESPUÉS mirar el flag — si el reader
        // cerró entre medias, su clear() ya nos barrió o lo hacemos aquí.
        if self.closed.load(Ordering::SeqCst) {
            self.pending.lock().expect("pending lock sano").remove(&id);
            return Err(ClientError::ConnectionClosed);
        }
        let frame = encode_frame(&req)?;
        if self.frames_out.send(frame).is_err() {
            self.pending.lock().expect("pending lock sano").remove(&id);
            return Err(ClientError::ConnectionClosed);
        }
        let value = rx.await.map_err(|_| ClientError::ConnectionClosed)??;
        Ok(serde_json::from_value(value)?)
    }

    /// Siguiente notificación del server (`task.progress`…). `None` =
    /// conexión cerrada.
    pub async fn notification(&mut self) -> Option<Notification> {
        self.notifications.recv().await
    }
}

/// El código RPC que señala "el server no habla nuestra versión" —
/// distinguible para que un frontend lo explique bien.
#[must_use]
pub fn is_version_mismatch(e: &ClientError) -> bool {
    // Por CÓDIGO, jamás parseando message (B1 del protocol-guardian).
    matches!(e, ClientError::Rpc(rpc) if rpc.code == codes::VERSION_MISMATCH)
}
