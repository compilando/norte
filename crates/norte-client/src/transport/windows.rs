//! The Windows transport: a named pipe whose DACL admits only this user,
//! served by a process that must be this user too (ADR 0159).
//!
//! The pipe's own security comes from the server (`norte_winpipe`); this
//! end makes the check unix makes with the socket owner's uid: a pipe name
//! squatted by another user, or by a sandboxed process of this one, answers
//! [`ClientError::ForeignDaemon`].

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::time::Duration;

use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

use crate::ClientError;

type Reader = ReadHalf<NamedPipeClient>;
type Writer = WriteHalf<NamedPipeClient>;

/// Every instance is taken: the server is between two `accept`s.
const ERROR_PIPE_BUSY: i32 = 231;

/// `SECURITY_IDENTIFICATION`: whoever serves the pipe may learn who we are
/// but cannot ACT as us. Without it a pipe opens at impersonation level, and
/// a squatter holding the name gets this user's token (ADR 0159). Spelled
/// out on every open, tokio's default included, so no default can undo it.
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

/// Synchronous presence probe, usable outside a runtime: opening the pipe
/// with std succeeds, or fails as BUSY, exactly when a server exists.
pub(crate) fn listening(socket: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt as _;
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(crate::socket::pipe_name(socket))
    {
        Ok(_) => true,
        Err(e) => e.raw_os_error() == Some(ERROR_PIPE_BUSY),
    }
}

pub(crate) async fn connect(socket: &Path) -> Result<(Reader, Writer), ClientError> {
    let pipe = open(&crate::socket::pipe_name(socket)).await?;
    authenticated(&pipe)?;
    Ok(tokio::io::split(pipe))
}

/// Connects and, if no daemon exists, STARTS one
/// ([`super::spawn::spawn_then_connect`]).
pub(crate) async fn connect_or_spawn(
    socket: &Path,
    spawn: impl FnOnce() -> std::process::Command,
) -> Result<(Reader, Writer), ClientError> {
    let name = crate::socket::pipe_name(socket);
    match open(&name).await {
        Ok(pipe) => {
            authenticated(&pipe)?;
            return Ok(tokio::io::split(pipe));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let name: OsString = name;
    super::spawn::spawn_then_connect(spawn, || async {
        let pipe = open(&name).await.ok()?;
        Some(authenticated(&pipe).map(|()| tokio::io::split(pipe)))
    })
    .await
}

/// Opens the pipe, waiting out BUSY for up to a second: the server creates
/// the next instance right after handing one to a client.
async fn open(name: &OsStr) -> std::io::Result<NamedPipeClient> {
    let mut options = ClientOptions::new();
    options.security_qos_flags(SECURITY_IDENTIFICATION);
    for _ in 0..50 {
        match options.open(name) {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
    options.open(name)
}

/// The pipe must be this user's daemon's: `SO_PEERCRED`'s role on unix,
/// read from the pipe object itself (owner and integrity label).
fn authenticated(pipe: &NamedPipeClient) -> Result<(), ClientError> {
    if !norte_winpipe::server_is_ours(pipe)? {
        return Err(ClientError::ForeignDaemon);
    }
    Ok(())
}
