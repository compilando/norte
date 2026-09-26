//! Where the daemon is, and who it belongs to.
//!
//! The socket's address is needed by BOTH ends: the client to connect and
//! the server to bind. It lives here, in the crate both can see, because two
//! definitions of the same security-relevant path is exactly how they drift
//! apart (`norte-core` re-exports it from `daemon`).

use std::path::PathBuf;

/// Default socket path: `$XDG_RUNTIME_DIR/norte/daemon.sock` (the runtime
/// dir is already 0700 per user); with no `XDG_RUNTIME_DIR`,
/// `/tmp/norte-<uid>/daemon.sock` — the server creates and VERIFIES the dir:
/// owner = the process's uid, mode 0700, never a symlink.
///
/// Seconds with no clients or tasks after which a daemon STARTED BY A
/// FRONTEND shuts itself down.
///
/// A daemon a person launches by hand with `norte daemon run` lives as long
/// as its `--idle-timeout` (five minutes): someone asked for it on its own.
/// One a window or an `ntc --daemon` started because none existed lives FOR
/// that client, and staying around five minutes after the last one leaves is
/// a process nobody sees and nobody asked for. Two seconds is how long a
/// reconnect or a handoff takes: a client returning within that window finds
/// the same daemon; one that does not starts another (~half a second). Since
/// it counts clients, closing a window with an `ntc --daemon` open shuts
/// nothing down.
pub const SPAWNED_DAEMON_IDLE_SECS: u64 = 2;

/// The argv a frontend uses to start the daemon it did not find: `norte
/// daemon run --socket <socket> --idle-timeout 2`.
///
/// ONE single definition, here, in the crate the window, the terminal and
/// the CLI all see: until now each one assembled its own, and a decision
/// written four times — how long what you started lives — is exactly the
/// kind that drifts with nothing turning red.
///
/// ```
/// use std::path::Path;
/// let argv = norte_client::daemon_run_argv("norte", Path::new("/run/u/1/norte/daemon.sock"));
/// let flat: Vec<String> = argv.iter().map(|a| a.to_string_lossy().into_owned()).collect();
/// assert_eq!(
///     flat,
///     ["norte", "daemon", "run", "--socket", "/run/u/1/norte/daemon.sock", "--idle-timeout", "2"]
/// );
/// ```
#[must_use]
pub fn daemon_run_argv(
    program: impl Into<std::ffi::OsString>,
    socket: &std::path::Path,
) -> Vec<std::ffi::OsString> {
    vec![
        program.into(),
        "daemon".into(),
        "run".into(),
        "--socket".into(),
        socket.as_os_str().to_owned(),
        "--idle-timeout".into(),
        SPAWNED_DAEMON_IDLE_SECS.to_string().into(),
    ]
}

/// `uid_hint` is only used for the /tmp fallback (the server derives it from
/// its own socket; clients, from the dir they find).
#[must_use]
#[cfg(unix)]
pub fn default_socket_path(uid_hint: Option<u32>) -> PathBuf {
    socket_path_from(
        std::env::var_os("XDG_RUNTIME_DIR"),
        uid_hint.unwrap_or_else(process_uid_best_effort),
    )
}

#[must_use]
#[cfg(not(unix))]
/// Reserved address for the forthcoming Windows named-pipe transport.
///
/// Kept under the user's local application-data directory so command-line
/// overrides and diagnostics remain path-shaped on every platform.
pub fn default_socket_path(_uid_hint: Option<u32>) -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("norte")
        .join("daemon.pipe")
}

/// `true` if a daemon answers at `socket` right now, without authenticating
/// it or speaking JSON-RPC.
///
/// Synchronous and cheap enough to run under a lock: it is a presence probe
/// ("is someone on the other end?"), not a connection. A stale socket file
/// with nobody behind it answers `false`.
///
/// ```
/// let dir = tempfile::tempdir().expect("tempdir");
/// assert!(!norte_client::daemon_listening(&dir.path().join("daemon.sock")));
/// ```
#[must_use]
pub fn daemon_listening(socket: &std::path::Path) -> bool {
    crate::transport::listening(socket)
}

/// [`default_socket_path`]'s pure logic (testable without touching the
/// global environment — which in edition 2024 requires `unsafe`, forbidden
/// here).
#[cfg(unix)]
fn socket_path_from(xdg: Option<std::ffi::OsString>, uid: u32) -> PathBuf {
    if let Some(runtime) = xdg.filter(|v| !v.is_empty()) {
        return PathBuf::from(runtime).join("norte").join("daemon.sock");
    }
    PathBuf::from(format!("/tmp/norte-{uid}")).join("daemon.sock")
}

/// The process's uid with NO unsafe (rule 5): the owner of a temp file we
/// just created IS our euid. Only used to NAME the /tmp dir and to compare
/// against the socket's peer; the real security comes from the server's
/// checks on owner and mode.
#[must_use]
#[cfg(unix)]
pub fn process_uid_best_effort() -> u32 {
    use std::os::unix::fs::MetadataExt;
    let probe = std::env::temp_dir().join(format!(
        ".norte-uid-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos())
    ));
    let uid = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|f| f.metadata())
        .map(|m| m.uid());
    let _ = std::fs::remove_file(&probe);
    // Fallback impossible in practice (temp_dir not writable): 0 will make
    // bind()'s anti-root check refuse, fail-safe.
    uid.unwrap_or(0)
}

/// Windows transports authenticate with the process token rather than a
/// numeric Unix uid. Kept for API compatibility with callers that only use
/// the value as a best-effort naming hint.
#[must_use]
#[cfg(not(unix))]
pub const fn process_uid_best_effort() -> u32 {
    0
}

#[cfg(all(test, unix))]
mod tests {
    use super::{process_uid_best_effort, socket_path_from};

    #[test]
    fn socket_path_uses_xdg_if_present() {
        let p = socket_path_from(Some("/run/user/4242".into()), 1000);
        assert_eq!(p, std::path::Path::new("/run/user/4242/norte/daemon.sock"));
    }

    #[test]
    fn socket_path_falls_back_to_tmp_without_xdg() {
        assert_eq!(
            socket_path_from(None, 1000),
            std::path::Path::new("/tmp/norte-1000/daemon.sock")
        );
        // Empty XDG = same as absent.
        assert_eq!(
            socket_path_from(Some(String::new().into()), 7),
            std::path::Path::new("/tmp/norte-7/daemon.sock")
        );
    }

    #[test]
    fn uid_best_effort_is_our_euid() {
        use std::os::unix::fs::MetadataExt;
        // The owner of a file we just created IS our euid.
        let probe = std::env::temp_dir().join(format!(".norte-uid-test-{}", std::process::id()));
        let f = std::fs::File::create(&probe).expect("create probe");
        let expected = f.metadata().expect("metadata").uid();
        drop(f);
        let _ = std::fs::remove_file(&probe);
        assert_eq!(process_uid_best_effort(), expected);
    }
}
