//! Núcleo de norte: scheduler de tasks con cancelación y progreso, copy engine,
//! y (en M0) modo embebido como biblioteca — el daemon llega en hitos posteriores.
#![forbid(unsafe_code)]

pub mod approval;
pub mod backend;
pub mod connect;
#[cfg(unix)]
pub mod daemon;
mod engine;
pub mod journal;
pub mod logging;
mod observer;
mod ops;
pub mod policy;
mod progress;
mod scheduler;
mod undo;

pub use engine::{Engine, TransferOptions};
pub use journal::{Actor, Journal, JournalEntry, Reversal, SqliteJournal};
pub use observer::{Mutation, MutationObserver};
pub use policy::{
    AllowAll, Decision, DenyReason, OpSet, PolicyConfig, PolicyGate, PolicyOp, Scope,
    ScopeRegistry, ScopedPolicy,
};
pub use progress::ProgressReporter;
pub use scheduler::{Priority, Scheduler, TaskBody, TaskCtx, TaskHandle};
pub use undo::UndoReport;
