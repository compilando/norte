//! Platforms with neither unix sockets nor named pipes: every higher layer
//! compiles, and asking for a daemon fails honestly.

use std::path::Path;

use tokio::io::{ReadHalf, WriteHalf};

use crate::ClientError;

type Reader = ReadHalf<tokio::io::DuplexStream>;
type Writer = WriteHalf<tokio::io::DuplexStream>;

fn unavailable() -> ClientError {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the daemon transport is not implemented on this platform",
    )
    .into()
}

pub(crate) const fn listening(_socket: &Path) -> bool {
    false
}

pub(crate) async fn connect(_socket: &Path) -> Result<(Reader, Writer), ClientError> {
    Err(unavailable())
}

pub(crate) async fn connect_or_spawn(
    _socket: &Path,
    _spawn: impl FnOnce() -> std::process::Command,
) -> Result<(Reader, Writer), ClientError> {
    Err(unavailable())
}
