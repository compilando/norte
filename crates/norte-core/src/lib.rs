//! norte's core: a task scheduler with cancellation and progress, a copy
//! engine, and (in M0) an embedded mode as a library — the daemon arrives in
//! later milestones.
#![forbid(unsafe_code)]

pub mod ai;
pub mod anchor;
pub mod approval;
pub mod archive_config;
pub mod audit;
pub mod backend;
/// `spawn_blocking` that preserves the span (ADR 0127).
mod blocking;
/// The `fs.compare` Task: batches coalesced over the `norte-compare` engine.
mod compare;
pub mod connect;
#[cfg(unix)]
pub mod daemon;
pub mod embedded;
mod engine;
pub mod ftp_plugin;
mod hashing;
pub mod hooks;
mod index_build;
mod index_embed;
pub mod journal;
pub mod team;
/// The `tracing` setup has lived in `norte-config` since #255: the graphical
/// window needs it just the same and cannot depend on the engine (ADR 0066).
/// It's re-exported here because this used to be its home and the CLI calls
/// it this way.
pub use norte_config::logging;
mod observer;
mod ops;
pub mod organize;
mod pack;
pub mod plugin_provider;
pub mod plugins;
pub mod policy;
mod progress;
pub mod rename;
mod scheduler;
pub mod search;
mod sessions;
pub mod sync;
pub mod ui_session;
mod undo;
pub mod volumes;

pub use engine::{
    BATCH_REPORTS_MAX, Engine, RENAME_BATCH_MAX_LISTING, SYNC_REPORTS_MAX, TransferOptions,
    UNDO_REPORTS_MAX,
};
pub use journal::{
    Actor, ChainStatus, JOURNAL_FORMAT, Journal, JournalEntry, JournalFormat, NewEntry, Reversal,
    SqliteJournal,
};
pub use norte_index::Index;
/// Is this a valid plugin id? Re-exported from `norte-plugin-host` so a
/// frontend can REJECT at its entry point an id that arrived over the wire,
/// without depending on the plugin runtime (rule 7: frontends talk to the
/// core, and this crate already gives them [`PluginRegistry`] the same way).
pub use norte_plugin_host::is_valid_plugin_id;
/// Anti-bomb limits for archive providers (#95.2): re-exported so frontends
/// can configure [`Engine::set_archive_limits`] without depending on
/// `norte-vfs-archive` (rule 7: they talk to the core).
pub use norte_vfs_archive::Limits as ArchiveLimits;
pub use observer::{Mutation, MutationObserver};
pub use ops::OnExists;
pub use plugins::{PluginRegistry, PluginRunError};
pub use policy::{
    AllowAll, Decision, DenyReason, OpSet, PolicyConfig, PolicyGate, PolicyOp, Scope,
    ScopeRegistry, ScopedPolicy, scope_key,
};
pub use progress::ProgressReporter;
pub use scheduler::{Lane, PauseGate, Priority, Scheduler, TaskBody, TaskCtx, TaskHandle};
pub use undo::{OP_ORGANIZED, UNDO_MAX_UNREVERTED_PATHS, UndoReport};
