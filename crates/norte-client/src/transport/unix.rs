//! Today's transport: a UNIX socket authenticated by peer credentials.
//!
//! EVERYTHING that knows there is a `UnixStream` underneath lives here. The
//! framed JSON-RPC ([`crate::rpc`]) only sees one read half and one write
//! half, so adding a Windows named pipe — a separate milestone, see ADR 0066
//! — means writing another module like this one, not copying the
//! correlation or the framing.

use std::path::Path;
use std::time::Duration;

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

/// Connects and, if nobody is listening, STARTS the daemon and retries with
/// backoff for up to ~3s.
///
/// The child is not waited on (`wait`): if the daemon dies before this
/// process does, it leaves a zombie until we exit — an accepted cost of not
/// double-forking (which would require unsafe).
///
/// # A daemon that DIES on startup
///
/// This is a different case from "is slow", and used to be read the same
/// way: the child's `stderr` went to `/dev/null`, so the one sentence that
/// explained the failure — "the journal predates `undoes_seq`", "the socket
/// is held by someone else" — was lost, and the caller waited the whole
/// 3.2s just to get a `SpawnTimeout` inviting a retry of something that will
/// never change.
///
/// Now `stderr` is captured and the child is watched with `try_wait` on
/// every round: if it died, [`ClientError::SpawnFailed`] is returned with
/// what it said, **without exhausting the backoff**.
///
/// With a daemon that DOES start, a pipe is left that nobody reads, and that
/// fills up: a daemon writing a warning to stderr with the buffer full
/// BLOCKS on the `write`, meaning the window would hang it for having
/// started it. That is why success leaves a thread draining it.
///
/// # Errors
/// I/O; [`ClientError::SpawnFailed`] if the daemon started and died; or
/// [`ClientError::SpawnTimeout`] if it is still alive and never accepts.
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
    let mut cmd = spawn();
    // The daemon is a process INDEPENDENT from the frontend that spawned it,
    // except for `stderr`: that is where it says why it could not start, and
    // dropping it leaves the caller with no explanation at all.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    // Total backoff ≈ 3.2s (documented as "up to ~3s").
    for backoff_ms in [25u64, 50, 100, 200, 400, 800, 1600] {
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        if let Ok(stream) = UnixStream::connect(socket).await {
            drain_stderr(child.stderr.take());
            return authenticated(stream).await;
        }
        // Did it die? Then waiting out the rest of the backoff changes
        // nothing, and what it wrote is the only thing that explains the
        // failure. `try_wait` does not block, and after death `stderr` is
        // closed: reading it terminates.
        if let Ok(Some(status)) = child.try_wait() {
            return Err(ClientError::SpawnFailed {
                status: status.code(),
                stderr: read_stderr(child.stderr.take()).await,
            });
        }
    }
    drain_stderr(child.stderr.take());
    Err(ClientError::SpawnTimeout)
}

/// What the daemon said before dying, trimmed and with no control bytes.
///
/// Capped to 4 KiB: it is an error message meant to be displayed, not a log,
/// and what arrives is another process's output. Read in `spawn_blocking`
/// because it is synchronous I/O (rule 2) — and it terminates, because the
/// child already died and the write end is closed.
async fn read_stderr(stderr: Option<std::process::ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let read = tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        let mut buf = Vec::new();
        let _ = std::io::Read::by_ref(&mut stderr)
            .take(4096)
            .read_to_end(&mut buf);
        buf
    })
    .await
    .unwrap_or_default();
    String::from_utf8_lossy(&read).trim().to_owned()
}

/// Leaves the pipe draining forever, discarding whatever arrives.
///
/// Without this, a daemon that starts fine and later writes to `stderr`
/// blocks as soon as it fills the pipe's buffer, because nobody in this
/// process is reading. The thread only dies when the daemon closes its end.
fn drain_stderr(stderr: Option<std::process::ChildStderr>) {
    let Some(mut stderr) = stderr else {
        return;
    };
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
    });
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
