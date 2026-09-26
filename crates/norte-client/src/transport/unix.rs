//! Today's transport: a UNIX socket authenticated by peer credentials.
//!
//! EVERYTHING that knows there is a `UnixStream` underneath lives here. The
//! framed JSON-RPC ([`crate::rpc`]) only sees one read half and one write
//! half, so adding a Windows named pipe — a separate milestone, see ADR 0066
//! — means writing another module like this one, not copying the
//! correlation or the framing.

use std::path::Path;

use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::ClientError;

/// Connects to the socket and checks it is served by OUR uid.
///
/// The client also authenticates the server (symmetry with spec §17.6): in
/// the `/tmp` fallback, a directory pre-created by another user could serve
/// an impostor daemon (security-reviewer finding M2).
///
/// # Errors
/// Connection I/O, or [`ClientError::ForeignDaemon`] if the peer is not
/// ours.
pub(crate) async fn connect(socket: &Path) -> Result<(OwnedReadHalf, OwnedWriteHalf), ClientError> {
    let stream = UnixStream::connect(socket).await?;
    authenticated(stream).await
}

/// Synchronous presence probe: a `connect` to a local unix socket resolves
/// on the spot, and an orphaned socket answers `ECONNREFUSED`.
pub(crate) fn listening(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

/// Connects and, if nobody is listening, STARTS the daemon
/// ([`super::spawn::spawn_then_connect`]).
///
/// # Errors
/// I/O, or what starting the daemon can fail with.
pub(crate) async fn connect_or_spawn(
    socket: &Path,
    spawn: impl FnOnce() -> std::process::Command,
) -> Result<(OwnedReadHalf, OwnedWriteHalf), ClientError> {
    match UnixStream::connect(socket).await {
        Ok(stream) => return authenticated(stream).await,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) => {}
        Err(e) => return Err(e.into()),
    }
    super::spawn::spawn_then_connect(spawn, || async {
        match UnixStream::connect(socket).await {
            Ok(stream) => Some(authenticated(stream).await),
            Err(_) => None,
        }
    })
    .await
}

async fn authenticated(stream: UnixStream) -> Result<(OwnedReadHalf, OwnedWriteHalf), ClientError> {
    let peer = stream.peer_cred()?;
    // Our own uid with no unsafe (rule 5), in spawn_blocking (rule 2).
    let my_uid = tokio::task::spawn_blocking(crate::socket::process_uid_best_effort)
        .await
        .map_err(|e| ClientError::Io(std::io::Error::other(e)))?;
    if peer.uid() != my_uid {
        return Err(ClientError::ForeignDaemon);
    }
    Ok(stream.into_split())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn nonexistent_socket() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("norte-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("nobody-here.sock")
    }

    /// **A daemon that starts and DIES returns what it said**, not the
    /// `SpawnTimeout` that invites waiting (#300).
    ///
    /// The sentence on `stderr` is the only thing that explains why it will
    /// never start — a journal that cannot be migrated is the real case —
    /// and it used to be thrown away to `/dev/null`.
    #[tokio::test]
    async fn a_daemon_that_dies_returns_what_it_said() {
        let socket = nonexistent_socket();
        let e = connect_or_spawn(&socket, || {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", "echo 'the journal cannot be migrated' >&2; exit 3"]);
            cmd
        })
        .await
        .expect_err("the daemon died");

        match e {
            ClientError::SpawnFailed { status, stderr } => {
                assert_eq!(status, Some(3), "the exit code is kept");
                assert!(
                    stderr.contains("the journal cannot be migrated"),
                    "what it said has to arrive whole: {stderr:?}"
                );
            }
            other => panic!("expected SpawnFailed, got {other:?}"),
        }
    }

    /// And the backoff is not exhausted waiting for it: ~3.2s of waiting on
    /// something that already died is 3.2s of blank window for nothing.
    #[tokio::test]
    async fn dying_does_not_exhaust_the_backoff() {
        let socket = nonexistent_socket();
        let before = std::time::Instant::now();
        let _ = connect_or_spawn(&socket, || {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", "exit 1"]);
            cmd
        })
        .await;
        assert!(
            before.elapsed() < Duration::from_secs(2),
            "the whole backoff was waited out: {:?}",
            before.elapsed()
        );
    }

    /// A daemon that starts and says NOTHING on `stderr` is still a failure
    /// with its code: the error exists even with no sentence to show.
    #[tokio::test]
    async fn dying_silently_is_also_a_failure() {
        let socket = nonexistent_socket();
        let e = connect_or_spawn(&socket, || {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", "exit 9"]);
            cmd
        })
        .await
        .expect_err("died");
        assert!(
            matches!(
                e,
                ClientError::SpawnFailed {
                    status: Some(9),
                    ref stderr
                } if stderr.is_empty()
            ),
            "{e:?}"
        );
    }

    /// A process that starts, does NOT die, and does not listen either
    /// exhausts the backoff and gives `SpawnTimeout`: that IS "not yet", and
    /// the advice to retry is the right one. This is the whole distinction
    /// #300 makes, in one assertion.
    #[tokio::test]
    async fn still_alive_and_not_listening_is_still_a_timeout() {
        let socket = nonexistent_socket();
        let e = connect_or_spawn(&socket, || {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", "sleep 30"]);
            cmd
        })
        .await
        .expect_err("nobody is listening");
        assert!(matches!(e, ClientError::SpawnTimeout), "{e:?}");
    }
}
