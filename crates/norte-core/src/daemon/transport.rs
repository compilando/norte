//! The daemon's listening end, per platform: a unix socket authenticated by
//! uid (ADR 0011), or a named pipe authenticated by user SID (ADR 0159).
//!
//! Everything above this — framing, dispatch, shutdown — sees a stream it
//! can split and an owner it can admit peers against.

#[cfg(unix)]
pub(super) use unix::{Bound, Listener, Owner, Reader, Stream, admit, bind, release, split};
#[cfg(windows)]
pub(super) use windows::{Bound, Listener, Owner, Reader, Stream, admit, bind, release, split};

#[cfg(unix)]
mod unix {
    use std::path::PathBuf;

    use crate::daemon::DaemonError;

    pub(in crate::daemon) type Stream = tokio::net::UnixStream;
    pub(in crate::daemon) type Reader = tokio::net::unix::OwnedReadHalf;
    type Writer = tokio::net::unix::OwnedWriteHalf;
    /// Who may connect: the daemon's own uid.
    pub(in crate::daemon) type Owner = u32;

    /// Bound in the blocking pool, not yet registered with the runtime.
    pub(in crate::daemon) struct Bound(std::os::unix::net::UnixListener);

    pub(in crate::daemon) struct Listener(tokio::net::UnixListener);

    /// Synchronous: goes inside `spawn_blocking` (rule 2).
    pub(in crate::daemon) fn bind(
        requested: Option<PathBuf>,
    ) -> Result<(Bound, Owner, PathBuf), DaemonError> {
        let (listener, uid, path) = super::super::server::bind_socket(requested)?;
        Ok((Bound(listener), uid, path))
    }

    impl Listener {
        pub(in crate::daemon) fn new(bound: Bound) -> std::io::Result<Self> {
            bound.0.set_nonblocking(true)?;
            tokio::net::UnixListener::from_std(bound.0).map(Self)
        }

        pub(in crate::daemon) async fn accept(&mut self) -> std::io::Result<Stream> {
            self.0.accept().await.map(|(stream, _)| stream)
        }
    }

    /// Only the SAME uid gets in (spec §17.6), checked before reading a byte.
    #[allow(clippy::trivially_copy_pass_by_ref)] // one signature for both platforms
    pub(in crate::daemon) fn admit(stream: &Stream, owner: &Owner) -> Result<(), String> {
        let peer = stream
            .peer_cred()
            .map_err(|e| format!("peer_cred failed: {e}"))?;
        if super::super::server::peer_allowed(peer.uid(), *owner) {
            Ok(())
        } else {
            Err(format!("connection from uid {} rejected", peer.uid()))
        }
    }

    pub(in crate::daemon) fn split(stream: Stream) -> (Reader, Writer) {
        stream.into_split()
    }

    /// Removes the socket path. Synchronous: `spawn_blocking`.
    pub(in crate::daemon) fn release(path: PathBuf) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use tokio::net::windows::named_pipe::NamedPipeServer;

    use crate::daemon::DaemonError;

    pub(in crate::daemon) type Stream = NamedPipeServer;
    pub(in crate::daemon) type Reader = tokio::io::ReadHalf<NamedPipeServer>;
    type Writer = tokio::io::WriteHalf<NamedPipeServer>;
    /// Who may connect: the daemon's own user SID.
    pub(in crate::daemon) type Owner = norte_winpipe::UserSid;

    /// The pipe's first instance, which is what claims the name.
    pub(in crate::daemon) struct Bound {
        name: OsString,
        first: NamedPipeServer,
    }

    /// Always holds the NEXT instance, so a client never finds the name
    /// missing between two accepts.
    pub(in crate::daemon) struct Listener {
        name: OsString,
        next: NamedPipeServer,
    }

    /// The service accounts: SYSTEM, LOCAL SERVICE, NETWORK SERVICE. A user
    /// daemon is not a surface for them, as it is not for root on unix.
    const SERVICE_ACCOUNTS: [&str; 3] = ["S-1-5-18", "S-1-5-19", "S-1-5-20"];

    /// Runs inside `spawn_blocking`, which keeps the runtime's context: the
    /// instance registers with its reactor.
    pub(in crate::daemon) fn bind(
        requested: Option<PathBuf>,
    ) -> Result<(Bound, Owner, PathBuf), DaemonError> {
        let address = requested.unwrap_or_else(|| super::super::default_socket_path(None));
        let owner = norte_winpipe::current_user()?;
        if SERVICE_ACCOUNTS.contains(&owner.as_str()) {
            return Err(DaemonError::Root);
        }
        let name = norte_client::pipe_name(&address);
        // The FIRST instance fails if the name exists, whoever holds it: a
        // live daemon of ours, or a squatter, which our clients will then
        // refuse as `ForeignDaemon`. Either way this one does not start.
        let first = match norte_winpipe::create_server(&name, true) {
            Ok(pipe) => pipe,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(DaemonError::AlreadyRunning);
            }
            Err(e) => return Err(e.into()),
        };
        Ok((Bound { name, first }, owner, address))
    }

    impl Listener {
        #[allow(clippy::unnecessary_wraps)] // the unix one can fail
        pub(in crate::daemon) fn new(bound: Bound) -> std::io::Result<Self> {
            Ok(Self {
                name: bound.name,
                next: bound.first,
            })
        }

        /// Cancel-safe: dropping a pending `connect` loses no client.
        pub(in crate::daemon) async fn accept(&mut self) -> std::io::Result<Stream> {
            self.next.connect().await?;
            let fresh = norte_winpipe::create_server(&self.name, false)?;
            Ok(std::mem::replace(&mut self.next, fresh))
        }
    }

    /// Only the SAME user gets in. The DACL already keeps other users from
    /// opening the pipe; this is the second, independent check.
    pub(in crate::daemon) fn admit(stream: &Stream, owner: &Owner) -> Result<(), String> {
        let peer = norte_winpipe::client_user(stream)
            .map_err(|e| format!("peer identity unavailable: {e}"))?;
        if &peer == owner {
            Ok(())
        } else {
            Err(format!("connection from {peer} rejected"))
        }
    }

    pub(in crate::daemon) fn split(stream: Stream) -> (Reader, Writer) {
        tokio::io::split(stream)
    }

    /// A pipe is no file: it disappears with its last instance.
    #[allow(clippy::unnecessary_wraps)]
    pub(in crate::daemon) fn release(_path: PathBuf) -> std::io::Result<()> {
        Ok(())
    }
}
