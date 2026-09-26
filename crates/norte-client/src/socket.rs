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
/// The daemon's ADDRESS off unix: `%LOCALAPPDATA%\norte\daemon.pipe`.
///
/// Path-shaped so `--socket` and diagnostics look the same on every
/// platform; the pipe it names is [`pipe_name`]. Per user because
/// `LOCALAPPDATA` is.
pub fn default_socket_path(_uid_hint: Option<u32>) -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("norte")
        .join("daemon.pipe")
}

/// The named pipe an address names (ADR 0159): an address already under
/// `\\.\pipe\` is used as is; any other becomes `\\.\pipe\norte-<hash>` of
/// its bytes. Both ends call this, so they always agree.
///
/// The hash only names; the pipe's DACL and the peer check are what keep
/// another user out.
///
/// ```
/// # #[cfg(windows)] {
/// use std::path::Path;
/// let a = norte_client::pipe_name(Path::new(r"C:\Users\a\AppData\Local\norte\daemon.pipe"));
/// assert!(a.to_string_lossy().starts_with(r"\\.\pipe\norte-"));
/// assert_eq!(norte_client::pipe_name(Path::new(r"\\.\pipe\mine")), r"\\.\pipe\mine");
/// # }
/// ```
#[must_use]
#[cfg(windows)]
pub fn pipe_name(address: &std::path::Path) -> std::ffi::OsString {
    pipe_name_from(address.as_os_str().as_encoded_bytes()).into()
}

/// [`pipe_name`]'s pure logic, over the address's bytes.
#[cfg_attr(not(windows), allow(dead_code))]
fn pipe_name_from(address: &[u8]) -> String {
    const PREFIX: &[u8] = br"\\.\pipe\";
    // Passed through only as ONE plain name: Win32 normalises `\\.\` paths,
    // so `\\.\pipe\..\UNC\host\x` would reach a remote pipe, or a file.
    if let Some(rest) = address
        .get(..PREFIX.len())
        .filter(|p| p.eq_ignore_ascii_case(PREFIX))
        .map(|_| &address[PREFIX.len()..])
        && !rest.is_empty()
        && !rest.iter().any(|b| matches!(b, b'\\' | b'/'))
        && !rest.windows(2).any(|w| w == b"..")
    {
        return String::from_utf8_lossy(address).into_owned();
    }
    // FNV-1a, 64 bits: stable across Rust versions, unlike `DefaultHasher`.
    let hash = address.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!(r"\\.\pipe\norte-{hash:016x}")
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

#[cfg(test)]
mod pipe_tests {
    use super::pipe_name_from;

    #[test]
    fn an_address_names_one_stable_pipe() {
        let a = pipe_name_from(br"C:\Users\a\AppData\Local\norte\daemon.pipe");
        assert_eq!(
            a,
            pipe_name_from(br"C:\Users\a\AppData\Local\norte\daemon.pipe")
        );
        assert_ne!(
            a,
            pipe_name_from(br"C:\Users\b\AppData\Local\norte\daemon.pipe")
        );
        assert!(a.starts_with(r"\\.\pipe\norte-") && a.len() == r"\\.\pipe\norte-".len() + 16);
        assert_eq!(pipe_name_from(br"\\.\PIPE\mine"), r"\\.\PIPE\mine");
        for escaping in [
            &br"\\.\pipe\..\UNC\host\share\x"[..],
            br"\\.\pipe\a\b",
            br"\\.\pipe\a/b",
            br"\\.\pipe\..",
            br"\\.\pipe\",
        ] {
            assert!(
                pipe_name_from(escaping).starts_with(r"\\.\pipe\norte-"),
                "{escaping:?} must be hashed, not passed through"
            );
        }
    }
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
