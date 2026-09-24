//! The daemon's FRAMED JSON-RPC (ADR 0011): the `initialize` handshake,
//! requests correlated by id, and a notification stream.
//!
//! Does not know what it travels over. It receives one read half and one
//! write half — who opened them and how the peer was authenticated is the
//! private `transport` module's business — so a new transport does not force
//! copying the correlation or the framing (ADR 0066).

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

/// Client errors.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Socket I/O.
    #[error("client i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The server responded with an error (protocol or application).
    #[error(transparent)]
    Rpc(#[from] RpcError),
    /// The connection died with the request in flight.
    #[error("connection closed with the request in flight")]
    ConnectionClosed,
    /// The result does not deserialize to the expected type (a server of a
    /// different version).
    #[error("malformed result: {0}")]
    BadResult(#[from] serde_json::Error),
    /// Could not start the daemon (auto-start).
    ///
    /// This is "still alive and not accepting yet", not "will never accept":
    /// the latter is [`ClientError::SpawnFailed`].
    #[error("the daemon did not start in time")]
    SpawnTimeout,
    /// The daemon started and DIED, with what it said on `stderr`.
    ///
    /// Exists separately from [`ClientError::SpawnTimeout`] because they are
    /// opposite pieces of advice: one invites waiting and the other reading.
    /// A daemon that dies opening a journal it cannot migrate is never going
    /// to start no matter how many times it is retried, and only it has the
    /// sentence that says what to do.
    #[error("the daemon could not start{}: {}",
        match .status { Some(c) => format!(" (exited with {c})"), None => String::new() },
        if .stderr.is_empty() { "it did not say why" } else { .stderr })]
    SpawnFailed {
        /// Exit code, if there was one (`None` = killed by a signal).
        status: Option<i32>,
        /// What it wrote to `stderr`, trimmed. May come back empty.
        stderr: String,
    },
    /// The socket is served by ANOTHER user: it is never talked to (a spoof
    /// in the /tmp fallback — security-reviewer finding M2).
    #[error("the socket's daemon belongs to another user")]
    ForeignDaemon,
}

/// Responses in flight, by request id.
type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value, RpcError>>>>>;

/// Authenticated connection (implicitly: same uid, or the server cuts it)
/// to the daemon.
pub struct Client {
    frames_out: mpsc::UnboundedSender<Vec<u8>>,
    notifications: mpsc::UnboundedReceiver<Notification>,
    pending: PendingMap,
    next_id: AtomicU64,
    /// Set by the reader BEFORE draining `pending`: a `call()` after closing
    /// fails instead of hanging (rust-reviewer B1).
    closed: Arc<AtomicBool>,
}

impl Client {
    /// Connects to the daemon's socket (no handshake: see
    /// [`Self::initialize`]).
    ///
    /// # Errors
    /// Connection I/O, or the socket being served by another user.
    pub async fn connect(socket: &Path) -> Result<Self, ClientError> {
        let (reader, writer) = unix::connect(socket).await?;
        Ok(Self::from_halves(reader, writer))
    }

    /// Connects, and if the socket does not exist or nobody is listening,
    /// STARTS the daemon (`spawn` produces the already-configured `Command`
    /// — frontends decide the binary and flags) and retries with backoff for
    /// up to ~3s.
    ///
    /// # Errors
    /// I/O, or [`ClientError::SpawnTimeout`] if the daemon never accepts.
    pub async fn connect_or_spawn(
        socket: &Path,
        spawn: impl FnOnce() -> std::process::Command,
    ) -> Result<Self, ClientError> {
        let (reader, writer) = unix::connect_or_spawn(socket, spawn).await?;
        Ok(Self::from_halves(reader, writer))
    }

    /// Assembles the client over an ALREADY open and authenticated
    /// transport.
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
                        continue; // corrupt frame from the server: ignored
                    };
                    match msg {
                        Message::Response(resp) => {
                            let Some(RequestId::Num(id)) = resp.id else {
                                continue;
                            };
                            let waiter = pending_reader
                                .lock()
                                .expect("pending lock is sound")
                                .remove(&id);
                            if let Some(tx) = waiter {
                                let outcome = resp.outcome().cloned();
                                let _ = tx.send(outcome);
                            }
                        }
                        Message::Notification(n) => {
                            let _ = notif_tx.send(n);
                        }
                        // The server does not send us requests in M2.
                        Message::Request(_) => {}
                    }
                }
            }
            // Dead connection: the flag first (a new call() no longer
            // registers), then waking up those who were waiting. That order
            // closes the insert/clear race (rust-reviewer B1).
            closed_reader.store(true, Ordering::SeqCst);
            pending_reader
                .lock()
                .expect("pending lock is sound")
                .clear();
        });

        Self {
            frames_out,
            notifications,
            pending,
            next_id: AtomicU64::new(1),
            closed,
        }
    }

    /// Mandatory handshake (ADR 0011). HUMAN connection: with no
    /// `agent_session`, the daemon binds it to `Actor::User` (no agent
    /// gate). For an agent connection, [`Self::initialize_as_agent`] —
    /// already linkable: it stopped being `pub(crate)` when the MCP bridge
    /// started negotiating through it.
    ///
    /// # Errors
    /// [`ClientError::Rpc`] if the core rejects the version or encoding.
    pub async fn initialize(
        &mut self,
        client_info: methods::ClientInfo,
    ) -> Result<methods::InitializeResult, ClientError> {
        self.handshake(client_info, None).await
    }

    /// Handshake declaring `agent_session`: the daemon binds the connection
    /// to `Actor::Agent { session }` and everything that passes through it
    /// falls under the agent gate (per-session scopes, journal with that
    /// actor).
    ///
    /// `pub` because BOTH sides of the same agent session use it:
    /// `backend::remote::RemoteBackend::connect_as_agent` (the arm that
    /// drains notifications) and the MCP bridge
    /// (`norte_mcp::bridge::Bridge::connect`, its tools connection). That
    /// they negotiate through here and not each with its own literal is the
    /// point: a widened `encodings` or a new capability has to reach both of
    /// them or neither.
    ///
    /// # Errors
    /// Those of [`Self::initialize`], plus a session rejected by the daemon
    /// (charset `[A-Za-z0-9._-]`, 1..=64).
    pub async fn initialize_as_agent(
        &mut self,
        client_info: methods::ClientInfo,
        agent_session: String,
    ) -> Result<methods::InitializeResult, ClientError> {
        self.handshake(client_info, Some(agent_session)).await
    }

    /// The handshake, in ONE single place: two copies of these params is how
    /// they drift apart (one gains a new field and the other does not).
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

    /// Request with a typed response.
    ///
    /// # Errors
    /// [`ClientError`]: I/O, an RPC error from the server, a dead connection
    /// or a result that does not deserialize.
    ///
    /// # Panics
    /// Never: the internal lock cannot be poisoned (nobody panics while
    /// holding it).
    pub async fn call<P, R>(&self, method: &str, params: &P) -> Result<R, ClientError>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        self.call_tracked(method, params, |_| {}).await
    }

    /// Like [`Self::call`], but invokes `on_id` with the assigned JSON-RPC id
    /// BEFORE waiting for the response — so the caller can correlate it
    /// (e.g. sending an `rpc.cancel` for that request while it is still in
    /// flight, #72). `on_id` runs BEFORE serializing/sending: on the error
    /// paths (connection already closed, dead channel) the reported id did
    /// NOT reach the wire, so it does not correspond to any request in
    /// flight — an `rpc.cancel` for that id is a harmless no-op on the
    /// daemon.
    ///
    /// # Errors
    /// The same as [`Self::call`].
    ///
    /// # Panics
    /// Never: the internal lock cannot be poisoned.
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
            .expect("pending lock is sound")
            .insert(id, tx);
        // Order matters: insert and THEN check the flag — if the reader
        // closed in between, its clear() already swept us or we do it here.
        if self.closed.load(Ordering::SeqCst) {
            self.pending
                .lock()
                .expect("pending lock is sound")
                .remove(&id);
            return Err(ClientError::ConnectionClosed);
        }
        let frame = encode_frame(&req)?;
        if self.frames_out.send(frame).is_err() {
            self.pending
                .lock()
                .expect("pending lock is sound")
                .remove(&id);
            return Err(ClientError::ConnectionClosed);
        }
        let value = rx.await.map_err(|_| ClientError::ConnectionClosed)??;
        Ok(serde_json::from_value(value)?)
    }

    /// Sends a JSON-RPC notification (no id, no response): fire-and-forget.
    /// Used by the bridge to relay `rpc.cancel` (#72). If the connection
    /// already died, it is silently dropped (best-effort, like `rpc.cancel`
    /// itself).
    ///
    /// # Errors
    /// [`ClientError`] only if the params do not serialize; a dead channel is
    /// NOT an error (best-effort).
    pub fn notify<P: serde::Serialize>(&self, method: &str, params: &P) -> Result<(), ClientError> {
        let notif = Notification {
            jsonrpc: JsonRpcVersion,
            method: method.to_owned(),
            params: Some(serde_json::to_value(params)?),
        };
        let frame = encode_frame(&notif)?;
        let _ = self.frames_out.send(frame); // dead channel = benign no-op
        Ok(())
    }

    /// The server's next notification (`task.progress`…). `None` = the
    /// connection is closed.
    pub async fn notification(&mut self) -> Option<Notification> {
        self.notifications.recv().await
    }

    /// Takes the notification receiver (to pump it from its own task while
    /// the `Client` — in an `Arc` — keeps serving `call`). After this,
    /// [`Self::notification`] returns `None`.
    pub fn take_notifications(&mut self) -> mpsc::UnboundedReceiver<Notification> {
        let (_dead_tx, dead_rx) = mpsc::unbounded_channel();
        std::mem::replace(&mut self.notifications, dead_rx)
    }
}

/// The RPC code that signals "the server does not speak our version" —
/// distinguishable so a frontend can explain it properly.
#[must_use]
pub fn is_version_mismatch(e: &ClientError) -> bool {
    // By CODE, never by parsing the message (protocol-guardian B1).
    matches!(e, ClientError::Rpc(rpc) if rpc.code == codes::VERSION_MISMATCH)
}
