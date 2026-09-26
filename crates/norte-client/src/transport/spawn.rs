//! Starting the daemon a frontend did not find, whatever the transport.

use std::future::Future;
use std::time::Duration;

use crate::ClientError;

/// STARTS the daemon and retries `try_connect` with backoff for up to ~3s.
///
/// `try_connect` answers `None` while nobody listens yet, and `Some` with
/// the (authenticated) connection or its error once somebody does.
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
pub(crate) async fn spawn_then_connect<T, F, Fut>(
    spawn: impl FnOnce() -> std::process::Command,
    mut try_connect: F,
) -> Result<T, ClientError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<Result<T, ClientError>>>,
{
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
        if let Some(connected) = try_connect().await {
            drain_stderr(child.stderr.take());
            return connected;
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
