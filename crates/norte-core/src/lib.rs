//! Núcleo de norte: scheduler de tasks con cancelación y progreso, copy engine,
//! y (en M0) modo embebido como biblioteca — el daemon llega en hitos posteriores.
#![forbid(unsafe_code)]

mod progress;
mod scheduler;

pub use progress::ProgressReporter;
pub use scheduler::{Priority, Scheduler, TaskBody, TaskCtx, TaskHandle};
