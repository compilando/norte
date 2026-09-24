//! Daemon JSON-RPC over UDS (ADR 0011, spec §17.6): one daemon per user,
//! never root, authenticated by `SO_PEERCRED`. Windows is deferred behind an
//! issue (the embedded mode remains the path there).
//!
//! - [`Daemon`] (server): accepts connections, authenticates, dispatches
//!   `fs.*`/`task.*` and broadcasts `task.progress` to humans and to the
//!   owner of each task (#66: an agent connection does not observe other
//!   agents' tasks).
//! - [`Client`]: frontend connection (initialize, call, notifications,
//!   `connect_or_spawn`).

pub mod approvals;
pub mod componer;
mod server;

pub use approvals::DaemonApprovalResolver;
pub use componer::componer;
pub use server::{Daemon, DaemonConfig};

// The CLIENT side has lived in `norte-client` since ADR 0066: the SDK cannot
// depend on the core, so the socket address and the framed JSON-RPC live
// there and are re-exported here so the usual consumers (CLI, MCP, e2e
// tests) keep naming them where they always named them.
pub use norte_client::{
    Client, ClientError, daemon_run_argv, default_socket_path, is_version_mismatch,
};

/// Errors of the daemon's lifecycle (server side).
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// I/O on the socket or the socket's filesystem.
    #[error("daemon i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The daemon NEVER runs as root (spec §17.6).
    #[error("the daemon does not run as root")]
    Root,
    /// The socket's directory is not safe (owner/mode/symlink).
    #[error("unsafe socket directory: {reason}")]
    InsecureDir {
        /// Which check failed.
        reason: &'static str,
    },
    /// The default dir under `/tmp/norte-<uid>` is not usable (#34.1):
    /// typically pre-occupied by another user (`squat`, a denial of
    /// availability, not of integrity: the daemon refuses to hijack it).
    /// Actionable: set `XDG_RUNTIME_DIR` (the supported path) or pass
    /// `--socket <path>` pointing at a directory the user owns.
    #[error(
        "the default socket dir ({path}) is not usable ({reason}); \
         set XDG_RUNTIME_DIR or pass --socket <path>"
    )]
    UnusableDefaultDir {
        /// The path of the fallback dir that could not be used.
        path: std::path::PathBuf,
        /// Which check failed.
        reason: &'static str,
    },
    /// A daemon is already alive and listening on the socket.
    #[error("a daemon is already listening on the socket")]
    AlreadyRunning,
}
