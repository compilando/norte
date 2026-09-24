//! IN-PROCESS SFTP server for the tests (ADR 0013, C2): `russh-sftp`
//! server over a `tokio::io::duplex`, backed by a real `tempdir`.
//! Lets the FULL contract suite (and the hostile corpus) run in normal
//! CI, without Docker. An injectable HOSTILE mode covers containment.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use russh_sftp::protocol::{
    Attrs, Data, File as NameFile, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
    Version,
};
use russh_sftp::server::Handler;

/// How the test server behaves.
#[derive(Clone, Copy)]
pub enum Mode {
    /// Faithful: mirrors the tempdir as is.
    Honest,
    /// Hostile: injects into EVERY `readdir` a `../../escape` entry and a
    /// symlink pointing outside the base — to test containment.
    Hostile,
}

/// Handler backed by `base` (tempdir). In-memory handle state.
pub struct TestHandler {
    base: PathBuf,
    mode: Mode,
    files: HashMap<String, PathBuf>,
    dirs: HashMap<String, DirState>,
    next: u64,
}

struct DirState {
    path: PathBuf,
    served: bool,
}

impl TestHandler {
    fn new(base: PathBuf, mode: Mode) -> Self {
        Self {
            base,
            mode,
            files: HashMap::new(),
            dirs: HashMap::new(),
            next: 0,
        }
    }

    fn handle(&mut self) -> String {
        self.next += 1;
        format!("h{}", self.next)
    }
}

/// Translates a `std::io` errno into an sftp `StatusCode`.
fn code(e: &std::io::Error) -> StatusCode {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => StatusCode::NoSuchFile,
        K::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

impl Handler for TestHandler {
    type Error = StatusCode;

    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _ext: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        // The client canonicalizes on connect; we return the path as is
        // (paths are already absolute POSIX under the base).
        let p = if path.is_empty() || path == "." {
            "/".to_string()
        } else {
            path
        };
        Ok(Name {
            id,
            files: vec![NameFile::new(p, FileAttributes::default())],
        })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let path = self.resolve(&filename)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(pflags.contains(OpenFlags::READ));
        opts.write(pflags.contains(OpenFlags::WRITE));
        opts.append(pflags.contains(OpenFlags::APPEND));
        if pflags.contains(OpenFlags::EXCLUDE) {
            // SSH_FXF_EXCL = create-new: fails if the path already exists
            // (including a symlink) and does NOT follow it. Real OpenSSH
            // honors it natively; the test server must replicate it to test
            // H1 containment.
            opts.create_new(true);
        } else {
            opts.create(pflags.contains(OpenFlags::CREATE));
            opts.truncate(pflags.contains(OpenFlags::TRUNCATE));
        }
        opts.open(&path).map_err(|e| code(&e))?;
        let h = self.handle();
        self.files.insert(h.clone(), path);
        Ok(Handle { id, handle: h })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.files.remove(&handle);
        self.dirs.remove(&handle);
        Ok(ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let path = self.files.get(&handle).ok_or(StatusCode::Failure)?.clone();
        let mut f = std::fs::File::open(&path).map_err(|e| code(&e))?;
        f.seek(SeekFrom::Start(offset)).map_err(|e| code(&e))?;
        let mut buf = vec![0u8; len as usize];
        let n = f.read(&mut buf).map_err(|e| code(&e))?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let path = self.files.get(&handle).ok_or(StatusCode::Failure)?.clone();
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|e| code(&e))?;
        f.seek(SeekFrom::Start(offset)).map_err(|e| code(&e))?;
        f.write_all(&data).map_err(|e| code(&e))?;
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let p = self.resolve(&path)?;
        let md = std::fs::symlink_metadata(&p).map_err(|e| code(&e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&md),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let p = self.resolve(&path)?;
        let md = std::fs::metadata(&p).map_err(|e| code(&e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&md),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let path = self.files.get(&handle).ok_or(StatusCode::Failure)?.clone();
        let md = std::fs::metadata(&path).map_err(|e| code(&e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&md),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        _path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        // The provider does not set metadata; we accept it so as not to
        // break flows.
        Ok(ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        _handle: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let p = self.resolve(&path)?;
        let md = std::fs::metadata(&p).map_err(|e| code(&e))?;
        if !md.is_dir() {
            return Err(StatusCode::Failure);
        }
        let h = self.handle();
        self.dirs.insert(
            h.clone(),
            DirState {
                path: p,
                served: false,
            },
        );
        Ok(Handle { id, handle: h })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let st = self.dirs.get_mut(&handle).ok_or(StatusCode::Failure)?;
        if st.served {
            // Second call: end of the listing.
            return Err(StatusCode::Eof);
        }
        st.served = true;
        let dir = st.path.clone();
        let hostile = matches!(self.mode, Mode::Hostile);
        let mut files: Vec<NameFile> = Vec::new();
        for dent in std::fs::read_dir(&dir).map_err(|e| code(&e))? {
            let dent = dent.map_err(|e| code(&e))?;
            let md = dent.metadata().map_err(|e| code(&e))?;
            let name = dent.file_name().to_string_lossy().into_owned();
            files.push(NameFile::new(name, FileAttributes::from(&md)));
        }
        if hostile {
            // A hostile server tries to get the client to escape the base.
            let mut attrs = FileAttributes::default();
            attrs.set_dir(true);
            files.push(NameFile::new("../../escape", attrs));
        }
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let p = self.resolve(&filename)?;
        std::fs::remove_file(&p).map_err(|e| code(&e))?;
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let p = self.resolve(&path)?;
        std::fs::create_dir(&p).map_err(|e| code(&e))?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        let p = self.resolve(&path)?;
        std::fs::remove_dir(&p).map_err(|e| code(&e))?;
        Ok(ok(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        let o = self.resolve(&oldpath)?;
        let n = self.resolve(&newpath)?;
        std::fs::rename(&o, &n).map_err(|e| code(&e))?;
        Ok(ok(id))
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let p = self.resolve(&path)?;
        let target = std::fs::read_link(&p).map_err(|e| code(&e))?;
        Ok(Name {
            id,
            files: vec![NameFile::new(
                target.to_string_lossy().into_owned(),
                FileAttributes::default(),
            )],
        })
    }

    async fn symlink(
        &mut self,
        id: u32,
        linkpath: String,
        targetpath: String,
    ) -> Result<Status, Self::Error> {
        let link = self.resolve(&linkpath)?;
        // The target is raw data: created as is (may be relative/broken).
        std::os::unix::fs::symlink(&targetpath, &link).map_err(|e| code(&e))?;
        Ok(ok(id))
    }
}

impl TestHandler {
    /// Translates a remote path (`/sub/f`) into a tempdir path, CONTAINED:
    /// never leaves the base (a client trying `../` stays inside). The
    /// honest server does not need this; it is the server's own safety net.
    fn resolve(&self, remote: &str) -> Result<PathBuf, StatusCode> {
        let mut p = self.base.clone();
        for comp in remote.split('/').filter(|c| !c.is_empty() && *c != ".") {
            if comp == ".." {
                // The test server does NOT let it escape (self-defense); a
                // compliant client never sends `..`.
                return Err(StatusCode::PermissionDenied);
            }
            p.push(comp);
        }
        Ok(p)
    }
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en".into(),
    }
}

/// Starts an in-process sftp server backed by `base` and returns a client
/// `SftpSession` connected over a `duplex` (no SSH).
pub async fn connect(base: &Path, mode: Mode) -> russh_sftp::client::SftpSession {
    let (client_end, server_end) = tokio::io::duplex(64 * 1024);
    russh_sftp::server::run(server_end, TestHandler::new(base.to_path_buf(), mode)).await;
    russh_sftp::client::SftpSession::new(client_end)
        .await
        .expect("in-process sftp handshake")
}
