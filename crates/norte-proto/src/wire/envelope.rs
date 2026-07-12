//! Envelope JSON-RPC 2.0 (ADR 0011): request/response/notification y el
//! objeto de error. La taxonomía de [`Error`](crate::Error) viaja ÍNTEGRA en
//! `error.data` — `code`/`message` son protocolo y presentación, jamás el
//! contrato (los frontends hacen match por `data.kind`).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Error;

/// La constante `jsonrpc` del envelope. Serializa a `"2.0"` y RECHAZA
/// cualquier otro valor al deserializar (un peer que no habla 2.0 es un
/// error de protocolo, no algo que tolerar).
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
                "jsonrpc debe ser \"2.0\", llegó {v:?}"
            )))
        }
    }
}

/// Id de una request. El emisor canónico escribe números (contador u64);
/// se aceptan strings por tolerancia (JSON-RPC 2.0 los permite).
///
/// ```
/// use norte_proto::wire::RequestId;
/// let n: RequestId = serde_json::from_str("7").unwrap();
/// assert_eq!(n, RequestId::Num(7));
/// let s: RequestId = serde_json::from_str(r#""abc""#).unwrap();
/// assert_eq!(s, RequestId::Str("abc".into()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// Contador del emisor canónico.
    Num(u64),
    /// Aceptado por tolerancia con otros clientes JSON-RPC.
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

/// Request JSON-RPC 2.0 (espera respuesta con el mismo `id`).
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
/// assert!(wire.contains(r#""params":null"#)); // null explícito (ADR 0004)
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Siempre `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Correlaciona la respuesta.
    pub id: RequestId,
    /// Método (`initialize`, `fs.list`, `task.cancel`…).
    pub method: String,
    /// Params del método (el tipo concreto vive en [`crate::methods`]).
    /// El emisor canónico escribe `null` explícito si no hay params.
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// Notificación JSON-RPC 2.0 (sin `id`: nadie responde).
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
    /// Siempre `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Método de la notificación (`task.progress`…).
    pub method: String,
    /// Payload (tipo concreto en [`crate::methods`]).
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// Response JSON-RPC 2.0: `result` XOR `error` (validado por
/// [`Response::outcome`], no por el tipo — tolerancia de deserialización).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Siempre `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// El id de la request respondida; `None` (= `null` en el wire) cuando
    /// no pudo conocerse (error de parse, spec JSON-RPC).
    pub id: Option<RequestId>,
    /// Resultado, si la request tuvo éxito.
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    /// Error, si falló.
    #[serde(default)]
    pub error: Option<RpcError>,
}

impl Response {
    /// Respuesta de éxito.
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

    /// Respuesta de error.
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

    /// `result` XOR `error`, validado: ambos presentes o ninguno es una
    /// violación de JSON-RPC y se trata como error de protocolo.
    ///
    /// # Errors
    /// [`RpcError`] con [`codes::INVALID_REQUEST`] si la respuesta está
    /// malformada; el error del peer tal cual si lo hay.
    pub fn outcome(&self) -> Result<&serde_json::Value, RpcError> {
        match (&self.result, &self.error) {
            (Some(r), None) => Ok(r),
            (None, Some(e)) => Err(e.clone()),
            _ => Err(RpcError::protocol(
                codes::INVALID_REQUEST,
                "response necesita exactamente uno de result/error",
            )),
        }
    }
}

/// Objeto de error JSON-RPC. La taxonomía completa viaja en `data`
/// (ADR 0011): `code` distingue protocolo de aplicación, `message` es
/// `Display` del error (inglés estable) — JAMÁS se parsea.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("rpc error {code}: {message}")]
pub struct RpcError {
    /// Código JSON-RPC (ver [`codes`]).
    pub code: i64,
    /// Detalle humano; presentación, nunca contrato.
    pub message: String,
    /// La taxonomía de la spec §17.7 — el contrato real para frontends.
    /// `None` en errores DE PROTOCOLO (parse, método desconocido…).
    #[serde(default)]
    pub data: Option<Error>,
}

impl RpcError {
    /// Error de protocolo (sin taxonomía: no hay operación de FS detrás).
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
    /// Error de APLICACIÓN: código único [`codes::APP_ERROR`] y la
    /// taxonomía íntegra en `data`.
    fn from(e: Error) -> Self {
        Self {
            code: codes::APP_ERROR,
            message: e.to_string(),
            data: Some(e),
        }
    }
}

/// Códigos JSON-RPC (ADR 0011): estándar para protocolo, `-32000` único
/// para aplicación (la categoría viaja en `data`, no en el código).
pub mod codes {
    /// JSON inválido en el frame.
    pub const PARSE_ERROR: i64 = -32700;
    /// Envelope malformado (no es request/notification válida).
    pub const INVALID_REQUEST: i64 = -32600;
    /// Método desconocido.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Params que no deserializan al tipo del método.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Error interno del servidor RPC (no confundir con
    /// [`Error::Internal`](crate::Error::Internal), que viaja como app).
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Error de APLICACIÓN: la taxonomía completa está en `data`.
    pub const APP_ERROR: i64 = -32000;
    /// `initialize` rechazado por versión de protocolo incompatible — LA
    /// señal que un cliente debe distinguir programáticamente (upgrade
    /// dance, ADR 0011). Jamás se detecta parseando `message`.
    pub const VERSION_MISMATCH: i64 = -32001;
    /// Se llamó a un método antes de `initialize` (ADR 0011).
    pub const NOT_INITIALIZED: i64 = -32002;
    /// El server rechaza trabajo nuevo por límite de recursos (tasks o
    /// conexiones): reintentable más tarde.
    pub const OVERLOADED: i64 = -32003;
}

/// Clase estructural de un mensaje entrante, decidida por PRESENCIA de
/// claves sobre el `Value` ya parseado — para servidores: permite
/// distinguir "JSON roto" (-32700) de "envelope inválido" (-32600) y
/// evita que una request con `id` de tipo ilegal se pierda en silencio
/// como notification (JSON-RPC exige responder).
///
/// ```
/// use norte_proto::wire::{classify, MessageKind};
/// let v: serde_json::Value = serde_json::json!({"jsonrpc":"2.0","id":-1,"method":"m"});
/// assert_eq!(classify(&v), MessageKind::Request); // id ilegal ≠ notification
/// assert_eq!(classify(&serde_json::json!({"foo":1})), MessageKind::Invalid);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// Tiene `method` e `id`: espera respuesta (aunque el `id` luego no
    /// parsee — eso es `INVALID_REQUEST`, no silencio).
    Request,
    /// Tiene `method` sin `id`.
    Notification,
    /// Sin `method` pero con `id`/`result`/`error`.
    Response,
    /// Nada de lo anterior: envelope inválido.
    Invalid,
}

/// Ver [`MessageKind`].
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

/// Un mensaje entrante, clasificado estructuralmente: `method`+`id` =
/// request; `method` sin `id` = notification; sin `method` = response.
/// El ORDEN de las variantes es el orden de prueba de `untagged` y es
/// significativo (una request también encajaría como notification).
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
    /// Espera respuesta.
    Request(Request),
    /// No espera respuesta.
    Notification(Notification),
    /// Respuesta a una request nuestra.
    Response(Response),
}
