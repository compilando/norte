//! Codificación wire: los tipos que JSON no transporta tal cual (ADR 0001)
//! y — desde M2 — el envelope JSON-RPC 2.0 con su framing (ADR 0011).

pub(crate) mod vpath_codec;

mod envelope;
mod framing;

pub use envelope::{
    JsonRpcVersion, Message, MessageKind, Notification, Request, RequestId, Response, RpcError,
    classify, codes,
};
pub use framing::{FrameDecoder, FrameOversized, MAX_FRAME_BYTES, encode_frame};
