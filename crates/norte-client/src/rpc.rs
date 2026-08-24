//! El JSON-RPC ENMARCADO del daemon (ADR 0011): handshake `initialize`,
//! requests correlacionadas por id y stream de notificaciones.
//!
//! No sabe por dónde viaja. Recibe una mitad de lectura y otra de escritura
//! —quién las abrió y cómo autenticó al peer es asunto del módulo privado
//! `transport`— para que un transporte nuevo no obligue a copiar la
//! correlación ni el enmarcado (ADR 0066).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use norte_proto::methods;
use norte_proto::wire::{
    FrameDecoder, JsonRpcVersion, Message, Notification, Request, RequestId, RpcError, codes,
    encode_frame,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::transport::unix;

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
    ///
    /// Es «sigue vivo y todavía no acepta», no «no va a aceptar nunca»: lo
    /// segundo es [`ClientError::SpawnFailed`].
    #[error("el daemon no arrancó a tiempo")]
    SpawnTimeout,
    /// El daemon arrancó y MURIÓ, con lo que dijo por `stderr`.
    ///
    /// Existe separado de [`ClientError::SpawnTimeout`] porque son consejos
    /// opuestos: uno invita a esperar y el otro a leer. Un daemon que muere al
    /// abrir un journal que no puede migrar no va a arrancar por mucho que se
    /// reintente, y la frase que dice qué hacer solo la tiene él.
    #[error("el daemon no pudo arrancar{}: {}",
        match .status { Some(c) => format!(" (salió con {c})"), None => String::new() },
        if .stderr.is_empty() { "no dijo por qué" } else { .stderr })]
    SpawnFailed {
        /// Código de salida, si lo hubo (`None` = lo mató una señal).
        status: Option<i32>,
        /// Lo que escribió por `stderr`, recortado. Puede venir vacío.
        stderr: String,
    },
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
    /// I/O de conexión, o que el socket lo sirva otro usuario.
    pub async fn connect(socket: &Path) -> Result<Self, ClientError> {
        let (reader, writer) = unix::connect(socket).await?;
        Ok(Self::from_halves(reader, writer))
    }

    /// Conecta, y si el socket no existe o nadie escucha, ARRANCA el daemon
    /// (`spawn` produce el `Command` ya configurado — los frontends deciden
    /// binario y flags) y reintenta con backoff hasta ~3 s.
    ///
    /// # Errors
    /// I/O, o [`ClientError::SpawnTimeout`] si el daemon no llega a aceptar.
    pub async fn connect_or_spawn(
        socket: &Path,
        spawn: impl FnOnce() -> std::process::Command,
    ) -> Result<Self, ClientError> {
        let (reader, writer) = unix::connect_or_spawn(socket, spawn).await?;
        Ok(Self::from_halves(reader, writer))
    }

    /// Monta el cliente sobre un transporte YA abierto y autenticado.
    fn from_halves<R, W>(mut reader: R, mut writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
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

    /// Handshake obligatorio (ADR 0011). Conexión HUMANA: sin
    /// `agent_session`, el daemon la liga a `Actor::User` (sin gate de
    /// agente). Para una conexión de agente,
    /// [`Self::initialize_as_agent`] — ya enlazable: dejó de ser `pub(crate)`
    /// cuando el puente MCP pasó a negociar por ella.
    ///
    /// # Errors
    /// [`ClientError::Rpc`] si el core rechaza versión o encoding.
    pub async fn initialize(
        &mut self,
        client_info: methods::ClientInfo,
    ) -> Result<methods::InitializeResult, ClientError> {
        self.handshake(client_info, None).await
    }

    /// Handshake declarando `agent_session`: el daemon liga la conexión a
    /// `Actor::Agent { session }` y todo lo que pase por ella queda bajo el
    /// gate de agente (scopes por sesión, journal con ese actor).
    ///
    /// `pub` porque la usan los DOS lados de una misma sesión de agente:
    /// `backend::remote::RemoteBackend::connect_as_agent` (el brazo que drena
    /// notificaciones) y el puente MCP (`norte_mcp::bridge::Bridge::connect`,
    /// su conexión de tools). Que negocien por aquí y no cada uno con su
    /// literal es el punto: un `encodings` ampliado o una capability nueva
    /// tiene que llegarles a los dos o a ninguno.
    ///
    /// # Errors
    /// Las de [`Self::initialize`], más sesión rechazada por el daemon
    /// (charset `[A-Za-z0-9._-]`, 1..=64).
    pub async fn initialize_as_agent(
        &mut self,
        client_info: methods::ClientInfo,
        agent_session: String,
    ) -> Result<methods::InitializeResult, ClientError> {
        self.handshake(client_info, Some(agent_session)).await
    }

    /// El handshake, UNA sola vez: dos copias de estos params es como
    /// divergen (una gana un campo nuevo y la otra no).
    async fn handshake(
        &mut self,
        client_info: methods::ClientInfo,
        agent_session: Option<String>,
    ) -> Result<methods::InitializeResult, ClientError> {
        self.call(
            methods::INITIALIZE,
            &methods::InitializeParams {
                client_info,
                protocol_version: methods::PROTOCOL_VERSION.into(),
                encodings: vec!["json".into()],
                agent_session,
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
        self.call_tracked(method, params, |_| {}).await
    }

    /// Como [`Self::call`], pero invoca `on_id` con el id JSON-RPC asignado
    /// ANTES de esperar la respuesta — para que el llamante lo correlacione
    /// (p. ej. enviar un `rpc.cancel` de esa request mientras sigue en vuelo,
    /// #72). `on_id` corre ANTES de serializar/enviar: en los caminos de error
    /// (conexión ya cerrada, canal muerto) el id reportado NO llegó al wire, así
    /// que no corresponde a ninguna request en vuelo — un `rpc.cancel` de ese id
    /// es un no-op benigno en el daemon.
    ///
    /// # Errors
    /// Iguales que [`Self::call`].
    ///
    /// # Panics
    /// Nunca: el lock interno no puede envenenarse.
    pub async fn call_tracked<P, R>(
        &self,
        method: &str,
        params: &P,
        on_id: impl FnOnce(u64),
    ) -> Result<R, ClientError>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        on_id(id);
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

    /// Envía una notificación JSON-RPC (sin id, sin respuesta): fire-and-forget.
    /// La usa el puente para reenviar `rpc.cancel` (#72). Si la conexión ya
    /// murió, se descarta en silencio (best-effort, como el propio `rpc.cancel`).
    ///
    /// # Errors
    /// [`ClientError`] solo si los params no serializan; un canal muerto NO es
    /// error (best-effort).
    pub fn notify<P: serde::Serialize>(&self, method: &str, params: &P) -> Result<(), ClientError> {
        let notif = Notification {
            jsonrpc: JsonRpcVersion,
            method: method.to_owned(),
            params: Some(serde_json::to_value(params)?),
        };
        let frame = encode_frame(&notif)?;
        let _ = self.frames_out.send(frame); // canal muerto = no-op best-effort
        Ok(())
    }

    /// Siguiente notificación del server (`task.progress`…). `None` =
    /// conexión cerrada.
    pub async fn notification(&mut self) -> Option<Notification> {
        self.notifications.recv().await
    }

    /// Se lleva el receptor de notificaciones (para bombearlo desde una
    /// task propia mientras el `Client` — en un `Arc` — sigue sirviendo
    /// `call`). Tras esto, [`Self::notification`] devuelve `None`.
    pub fn take_notifications(&mut self) -> mpsc::UnboundedReceiver<Notification> {
        let (_dead_tx, dead_rx) = mpsc::unbounded_channel();
        std::mem::replace(&mut self.notifications, dead_rx)
    }
}

/// El código RPC que señala "el server no habla nuestra versión" —
/// distinguible para que un frontend lo explique bien.
#[must_use]
pub fn is_version_mismatch(e: &ClientError) -> bool {
    // Por CÓDIGO, jamás parseando message (B1 del protocol-guardian).
    matches!(e, ClientError::Rpc(rpc) if rpc.code == codes::VERSION_MISMATCH)
}
