//! Wire encoding: the types JSON does not transport as is (ADR 0001) and —
//! since M2 — the JSON-RPC 2.0 envelope with its framing (ADR 0011).

pub(crate) mod vpath_codec;

mod envelope;
mod framing;

pub use envelope::{
    JsonRpcVersion, Message, MessageKind, Notification, Request, RequestId, Response, RpcError,
    classify, codes,
};
pub use framing::{FrameDecoder, FrameOversized, MAX_FRAME_BYTES, encode_frame};
