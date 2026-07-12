//! Servidor SFTP IN-PROCESS para los tests (ADR 0013, C2): `russh-sftp`
//! server sobre un `tokio::io::duplex`, respaldado por un `tempdir` real.
//! Permite correr la suite contractual COMPLETA (y el corpus hostil) en CI
//! normal, sin Docker. Un modo HOSTIL inyectable cubre la contención.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use russh_sftp::protocol::{
    Attrs, Data, File as NameFile, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
    Version,
};
use russh_sftp::server::Handler;

/// Cómo se comporta el servidor de test.
#[derive(Clone, Copy)]
pub enum Mode {
    /// Fiel: refleja el tempdir tal cual.
    Honest,
    /// Hostil: inyecta en TODO `readdir` una entrada `../../escape` y un
    /// symlink que apunta fuera de la base — para probar la contención.
    Hostile,
}

/// Handler respaldado por `base` (tempdir). Estado de handles en memoria.
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

/// Traduce un errno de `std::io` a un `StatusCode` de sftp.
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
        // El cliente canonicaliza al conectar; devolvemos el path tal cual
        // (los paths ya son absolutos POSIX bajo la base).
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
            // SSH_FXF_EXCL = create-new: falla si el path ya existe (incluido un
            // symlink) y NO lo sigue. OpenSSH real lo honra nativamente; el
            // servidor de test debe replicarlo para probar la contención H1.
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
        // El provider no fija metadatos; aceptamos para no romper flujos.
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
            // Segunda llamada: fin del listado.
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
            // Un servidor hostil intenta que el cliente escape la base.
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
        // El target es dato crudo: se crea tal cual (puede ser relativo/roto).
        std::os::unix::fs::symlink(&targetpath, &link).map_err(|e| code(&e))?;
        Ok(ok(id))
    }
}

impl TestHandler {
    /// Traduce un path remoto (`/sub/f`) a un path del tempdir, CONTENIDO:
    /// jamás sale de la base (un cliente que intente `../` se queda dentro).
    /// El servidor honesto no lo necesita; es la red del propio servidor.
    fn resolve(&self, remote: &str) -> Result<PathBuf, StatusCode> {
        let mut p = self.base.clone();
        for comp in remote.split('/').filter(|c| !c.is_empty() && *c != ".") {
            if comp == ".." {
                // El servidor de test NO deja escapar (defensa propia); el
                // cliente conforme jamás manda `..`.
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

/// Arranca un servidor sftp in-process respaldado por `base` y devuelve una
/// `SftpSession` de cliente conectada por un `duplex` (sin SSH).
pub async fn connect(base: &Path, mode: Mode) -> russh_sftp::client::SftpSession {
    let (client_end, server_end) = tokio::io::duplex(64 * 1024);
    russh_sftp::server::run(server_end, TestHandler::new(base.to_path_buf(), mode)).await;
    russh_sftp::client::SftpSession::new(client_end)
        .await
        .expect("handshake sftp in-process")
}
