//! How the daemon is reached. The framing does not know which platform
//! transport supplied its read and write halves.

#[cfg(any(unix, windows))]
mod spawn;
#[cfg(unix)]
pub(crate) mod unix;
#[cfg(not(any(unix, windows)))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::{connect, connect_or_spawn, listening};
#[cfg(not(any(unix, windows)))]
pub(crate) use unsupported::{connect, connect_or_spawn, listening};
#[cfg(windows)]
pub(crate) use windows::{connect, connect_or_spawn, listening};
