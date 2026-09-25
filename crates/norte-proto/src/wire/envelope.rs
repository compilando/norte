//! JSON-RPC 2.0 envelope (ADR 0011): request/response/notification and the
//! error object. [`Error`](crate::Error)'s taxonomy travels WHOLE in
//! `error.data` — `code`/`message` are protocol and presentation, never the
//! contract (frontends match by `data.kind`).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Error;

/// The envelope's `jsonrpc` constant. Serializes to `"2.0"` and REJECTS any
/// other value on deserialization (a peer that does not speak 2.0 is a
/// protocol error, not something to tolerate).
///
/// ```
/// use norte_proto::wire::JsonRpcVersion;
/// assert_eq!(serde_json::to_string(&JsonRpcVersion).unwrap(), r#""2.0""#);
/// assert!(serde_json::from_str::<JsonRpcVersion>(r#""1.0""#).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        if v == "2.0" {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(format!(
                "jsonrpc must be \"2.0\", got {v:?}"
            )))
        }
    }
}

/// Id of a request. The canonical emitter writes numbers (a `u64` counter);
/// strings are accepted for tolerance (JSON-RPC 2.0 allows them).
///
/// ```
/// use norte_proto::wire::RequestId;
/// let n: RequestId = serde_json::from_str("7").unwrap();
/// assert_eq!(n, RequestId::Num(7));
/// let s: RequestId = serde_json::from_str(r#""abc""#).unwrap();
/// assert_eq!(s, RequestId::Str("abc".into()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum RequestId {
    /// The canonical emitter's counter.
    Num(u64),
    /// Accepted for tolerance with other JSON-RPC clients.
    Str(String),
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Num(n) => write!(f, "{n}"),
            Self::Str(s) => f.write_str(s),
        }
    }
}

/// JSON-RPC 2.0 request (expects a response with the same `id`).
///
/// ```
/// use norte_proto::wire::{JsonRpcVersion, Request, RequestId};
/// let r = Request {
///     jsonrpc: JsonRpcVersion,
///     id: RequestId::Num(1),
///     method: "fs.stat".into(),
///     params: None,
/// };
/// let wire = serde_json::to_string(&r).unwrap();
/// assert!(wire.contains(r#""params":null"#)); // explicit null (ADR 0004)
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Correlates the response.
    pub id: RequestId,
    /// Method (`initialize`, `fs.list`, `task.cancel`…).
    pub method: String,
    /// The method's params (the concrete type lives in [`crate::methods`]).
    /// The canonical emitter writes an explicit `null` when there are no
    /// params.
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 notification (no `id`: nobody answers).
///
/// ```
/// use norte_proto::wire::{JsonRpcVersion, Notification};
/// let n = Notification {
///     jsonrpc: JsonRpcVersion,
///     method: "task.progress".into(),
///     params: None,
/// };
/// assert!(serde_json::to_string(&n).unwrap().contains("task.progress"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// The notification's method (`task.progress`…).
    pub method: String,
    /// Payload (concrete type in [`crate::methods`]).
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 response: `result` XOR `error` (validated by
/// [`Response::outcome`], not by the type — deserialization tolerance).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// The id of the request being answered; `None` (= `null` on the wire)
    /// when it could not be known (a parse error, JSON-RPC spec).
    pub id: Option<RequestId>,
    /// Result, if the request succeeded.
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    /// Error, if it failed.
    #[serde(default)]
    pub error: Option<RpcError>,
}

impl Response {
    /// Success response.
    ///
    /// ```
    /// use norte_proto::wire::{RequestId, Response};
    /// let r = Response::ok(RequestId::Num(1), serde_json::json!({"x": 1}));
    /// assert!(r.outcome().is_ok());
    /// ```
    #[must_use]
    pub fn ok(id: RequestId, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    /// Error response.
    ///
    /// ```
    /// use norte_proto::wire::{Response, RpcError, codes};
    /// let r = Response::err(None, RpcError::protocol(codes::PARSE_ERROR, "x"));
    /// assert_eq!(r.outcome().unwrap_err().code, codes::PARSE_ERROR);
    /// ```
    #[must_use]
    pub fn err(id: Option<RequestId>, error: RpcError) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id,
            result: None,
            error: Some(error),
        }
    }

    /// `result` XOR `error`, validated: both present or neither is a
    /// violation of JSON-RPC and is treated as a protocol error.
    ///
    /// # Errors
    /// [`RpcError`] with [`codes::INVALID_REQUEST`] if the response is
    /// malformed; the peer's own error as is if there is one.
    pub fn outcome(&self) -> Result<&serde_json::Value, RpcError> {
        match (&self.result, &self.error) {
            (Some(r), None) => Ok(r),
            (None, Some(e)) => Err(e.clone()),
            _ => Err(RpcError::protocol(
                codes::INVALID_REQUEST,
                "response needs exactly one of result/error",
            )),
        }
    }
}

/// JSON-RPC error object. The full taxonomy travels in `data` (ADR 0011):
/// `code` distinguishes protocol from application, `message` is the error's
/// `Display` (stable English) — NEVER parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("rpc error {code}: {message}")]
pub struct RpcError {
    /// JSON-RPC code (see [`codes`]).
    pub code: i64,
    /// Human detail; presentation, never contract.
    pub message: String,
    /// The taxonomy from spec §17.7 — the real contract for frontends.
    /// `None` on PROTOCOL errors (parse, unknown method…).
    #[serde(default)]
    pub data: Option<Error>,
}

impl RpcError {
    /// Protocol error (no taxonomy: there is no FS operation behind it).
    ///
    /// ```
    /// use norte_proto::wire::{RpcError, codes};
    /// let e = RpcError::protocol(codes::METHOD_NOT_FOUND, "no such method");
    /// assert!(e.data.is_none());
    /// ```
    #[must_use]
    pub fn protocol(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

impl From<Error> for RpcError {
    /// APPLICATION error: a single [`codes::APP_ERROR`] code and the whole
    /// taxonomy in `data`.
    fn from(e: Error) -> Self {
        Self {
            code: codes::APP_ERROR,
            message: e.to_string(),
            data: Some(e),
        }
    }
}

/// JSON-RPC codes (ADR 0011): standard for protocol, a single `-32000` for
/// application (the category travels in `data`, not in the code).
pub mod codes {
    /// Invalid JSON in the frame.
    pub const PARSE_ERROR: i64 = -32700;
    /// Malformed envelope (not a valid request/notification).
    pub const INVALID_REQUEST: i64 = -32600;
    /// Unknown method.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Params that do not deserialize into the method's type.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal error of the RPC server (not to be confused with
    /// [`Error::Internal`](crate::Error::Internal), which travels as an app
    /// error).
    pub const INTERNAL_ERROR: i64 = -32603;
    /// APPLICATION error: the full taxonomy is in `data`.
    pub const APP_ERROR: i64 = -32000;
    /// `initialize` rejected for an incompatible protocol version — THE
    /// signal a client must distinguish programmatically (the upgrade dance,
    /// ADR 0011). Never detected by parsing `message`.
    pub const VERSION_MISMATCH: i64 = -32001;
    /// A method was called before `initialize` (ADR 0011).
    pub const NOT_INITIALIZED: i64 = -32002;
    /// The server rejects new work due to a resource limit (tasks or
    /// connections): retryable later.
    pub const OVERLOADED: i64 = -32003;
}

/// Structural class of an incoming message, decided by key PRESENCE on the
/// already-parsed `Value` — for servers: it lets "broken JSON" (-32700) be
/// told apart from "invalid envelope" (-32600) and keeps a request with an
/// illegal-typed `id` from silently being lost as a notification (JSON-RPC
/// requires answering).
///
/// ```
/// use norte_proto::wire::{classify, MessageKind};
/// let v: serde_json::Value = serde_json::json!({"jsonrpc":"2.0","id":-1,"method":"m"});
/// assert_eq!(classify(&v), MessageKind::Request); // an illegal id ≠ notification
/// assert_eq!(classify(&serde_json::json!({"foo":1})), MessageKind::Invalid);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// Has `method` and `id`: expects a response (even if the `id` later
    /// fails to parse — that is `INVALID_REQUEST`, not silence).
    Request,
    /// Has `method` without `id`.
    Notification,
    /// No `method` but has `id`/`result`/`error`.
    Response,
    /// None of the above: invalid envelope.
    Invalid,
}

/// See [`MessageKind`].
#[must_use]
pub fn classify(v: &serde_json::Value) -> MessageKind {
    let Some(obj) = v.as_object() else {
        return MessageKind::Invalid;
    };
    match (obj.contains_key("method"), obj.contains_key("id")) {
        (true, true) => MessageKind::Request,
        (true, false) => MessageKind::Notification,
        (false, true) => MessageKind::Response,
        (false, false) => {
            if obj.contains_key("result") || obj.contains_key("error") {
                MessageKind::Response
            } else {
                MessageKind::Invalid
            }
        }
    }
}

/// An incoming message, classified structurally: `method`+`id` = request;
/// `method` without `id` = notification; no `method` = response. The ORDER of
/// the variants is `untagged`'s trial order and is significant (a request
/// would also fit as a notification).
///
/// ```
/// use norte_proto::wire::Message;
/// let m: Message = serde_json::from_str(
///     r#"{"jsonrpc":"2.0","id":1,"method":"fs.list","params":null}"#,
/// ).unwrap();
/// assert!(matches!(m, Message::Request(_)));
/// let n: Message = serde_json::from_str(
///     r#"{"jsonrpc":"2.0","method":"task.progress","params":null}"#,
/// ).unwrap();
/// assert!(matches!(n, Message::Notification(_)));
/// let r: Message = serde_json::from_str(
///     r#"{"jsonrpc":"2.0","id":1,"result":{},"error":null}"#,
/// ).unwrap();
/// assert!(matches!(r, Message::Response(_)));
/// ```
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Message {
    /// Expects a response.
    Request(Request),
    /// Does not expect a response.
    Notification(Notification),
    /// Response to one of our requests.
    Response(Response),
}
