//! Núcleo de norte: scheduler de tasks con cancelación y progreso, copy engine,
//! y (en M0) modo embebido como biblioteca — el daemon llega en hitos posteriores.
#![forbid(unsafe_code)]

pub mod ai;
pub mod approval;
pub mod archive_config;
pub mod audit;
pub mod backend;
pub mod connect;
#[cfg(unix)]
pub mod daemon;
mod engine;
pub mod journal;
pub mod logging;
mod observer;
mod ops;
pub mod plugin_provider;
pub mod plugins;
pub mod policy;
mod progress;
mod scheduler;
pub mod search;
mod sessions;
mod undo;

pub use engine::{Engine, TransferOptions};
pub use journal::{Actor, ChainStatus, Journal, JournalEntry, Reversal, SqliteJournal};
/// Límites anti-bomba de los providers archive (#95.2): re-export para que
/// los frontends configuren [`Engine::set_archive_limits`] sin depender de
/// `norte-vfs-archive` (regla 7: hablan con el core).
pub use norte_vfs_archive::Limits as ArchiveLimits;
pub use observer::{Mutation, MutationObserver};
pub use plugins::{PluginRegistry, PluginRunError};
pub use policy::{
    AllowAll, Decision, DenyReason, OpSet, PolicyConfig, PolicyGate, PolicyOp, Scope,
    ScopeRegistry, ScopedPolicy,
};
pub use progress::ProgressReporter;
pub use scheduler::{Priority, Scheduler, TaskBody, TaskCtx, TaskHandle};
pub use undo::UndoReport;
