//! The norte daemon's client SDK.
//!
//! What it takes to talk to a daemon — and NONE of what it takes to be one.
//! A frontend that only talks over a socket has no reason to drag in the
//! engine, the providers, the index or the plugin host, which is what used
//! to happen when all of this lived inside `norte-core` (ADR 0066).
//!
//! Three layers, bottom up:
//!
//! - `transport` (private): how the daemon is reached and how it is
//!   authenticated: a UNIX socket with the peer's credentials, or on
//!   Windows a named pipe with the peer's user SID (ADR 0159).
//! - [`rpc`]: the framed JSON-RPC, which does not know what it travels over.
//! - `remote`: the typed backend frontends actually use.
//!
//! This crate's boundary is its dependency list, and a test watches it:
//! `tests/dependency_boundary.rs`.
#![forbid(unsafe_code)]

pub mod remote;
pub mod rpc;
pub mod socket;
pub mod task;
mod transport;
pub mod types;

pub use remote::RemoteBackend;
pub use remote::calls::to_taxonomy;
pub use rpc::{Client, ClientError, is_version_mismatch};
#[cfg(windows)]
pub use socket::pipe_name;
pub use socket::{
    SPAWNED_DAEMON_IDLE_SECS, daemon_listening, daemon_run_argv, default_socket_path,
    process_uid_best_effort,
};
pub use task::{RemoteTask, RemoteTaskCanceller};
pub use types::{
    AI_CALL_TIMEOUT, ConnEvent, EntryStream, SyncPlanEvent, Transfer, TransferOptions,
};
