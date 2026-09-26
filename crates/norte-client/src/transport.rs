//! How the daemon is reached. The framing does not know which platform
//! transport supplied its read and write halves.

#[cfg(unix)]
pub(crate) mod unix;
#[cfg(not(unix))]
mod unsupported;

#[cfg(unix)]
pub(crate) use unix::{connect, connect_or_spawn};
#[cfg(not(unix))]
pub(crate) use unsupported::{connect, connect_or_spawn};
