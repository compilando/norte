//! [`SftpProvider`]: el trait [`Provider`] sobre un `SftpSession` de
//! `russh-sftp` (ADR 0013). Contención del servidor hostil incluida.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{
    ByteRange, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, Scheme, VPath,
};
use norte_vfs::{
    ByteSink, ByteStream, EntryStream, FollowLinks, NodeId, Provider, SymlinkKind, trash,
};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

/// Tamaño de chunk de lectura (256 KiB, alineado con el copy engine).
const READ_CHUNK: usize = 256 * 1024;
/// Prefijo del staging de escritura (ADR 0012, mismo convenio que local).
const PARTIAL_PREFIX: &str = ".norte-partial.";

/// Provider VFS sobre una sesión SFTP ya establecida (ADR 0013).
///
/// La conexión SSH con auth y verificación de host key llega en la fase 6;
/// aquí la sesión se INYECTA ([`SftpProvider::new`]) — así el provider se
/// testea contra un servidor sftp in-process sobre `duplex`, sin SSH.
pub struct SftpProvider {
    session: Arc<SftpSession>,
    /// Raíz remota absoluta (POSIX) bajo la que vive todo. Sin `..`.
    base: String,
    /// Contador de staging (nombre efímero único para `write`).
    seq: AtomicU64,
    /// Papelera lógica `.norte-trash/` activa (opt-in por conexión, ADR
    /// 0019). Off por defecto → no declara `TRASH` → borrado permanente.
    logical_trash: bool,
    /// Contador monótono para desempatar ids de papelera del mismo ms.
    trash_counter: AtomicU64,
}

impl SftpProvider {
    /// Provider sobre una sesión ya establecida, enraizado en `base`
    /// (path remoto absoluto POSIX, p. ej. `/home/user`). `base` se
    /// normaliza a sin barra final.
    #[must_use]
    pub fn new(session: SftpSession, base: impl Into<String>) -> Self {
        let mut base = base.into();
        while base.len() > 1 && base.ends_with('/') {
            base.pop();
        }
        Self {
            session: Arc::new(session),
            base,
            seq: AtomicU64::new(0),
            logical_trash: false,
            trash_counter: AtomicU64::new(0),
        }
    }

    /// Activa/desactiva la papelera lógica `.norte-trash/` (ADR 0019).
    /// Sin ella el provider no declara `TRASH` y `trash()` da `Unsupported`.
    #[must_use]
    pub fn with_logical_trash(mut self, enabled: bool) -> Self {
        self.logical_trash = enabled;
        self
    }

    /// Siguiente valor del contador monótono de ids de papelera.
    fn next_counter(&self) -> u64 {
        self.trash_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Crea `dir` tolerando que ya exista (idempotente). Bajo concurrencia
    /// v3 puede devolver `Failure` genérico (→ `Io`) en vez de `Conflict`
    /// si otra sesión lo crea entre el `exists()` y el `create_dir`; si al
    /// final el directorio está, el resultado es benigno.
    async fn ensure_dir_idempotent(&self, dir: &VPath) -> Result<(), Error> {
        match self.mkdir(dir).await {
            Ok(())
            | Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }) => Ok(()),
            Err(e) => {
                let remote = self.remote(dir)?;
                if self.exists(&remote).await.unwrap_or(false) {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// La raíz de este provider para un `authority` dado
    /// (`sftp://host:22/`).
    ///
    /// # Panics
    /// Nunca: el scheme es constante y válido.
    #[must_use]
    pub fn root(authority: norte_proto::Authority) -> VPath {
        VPath::root(
            Scheme::new("sftp").expect("scheme constante válido"),
            Some(authority),
        )
    }

    /// Traduce un [`VPath`] al path remoto POSIX absoluto bajo la `base`.
    /// Los segmentos son BYTES; SFTP (vía russh-sftp) exige UTF-8 — un
    /// nombre no representable es [`Error::InvalidPath`] (rechazo LIMPIO,
    /// jamás lossy — regla 1, ADR 0013 D2). El provider construye el path
    /// SIEMPRE así, nunca desde un path ecoado por el servidor.
    fn remote(&self, p: &VPath) -> Result<String, Error> {
        if p.scheme() != "sftp" {
            return Err(Error::InvalidPath);
        }
        let mut out = String::from(&self.base);
        for seg in p.segments() {
            let name = std::str::from_utf8(seg).map_err(|_| Error::InvalidPath)?;
            // Un segmento jamás lleva separador ni es `.`/`..` (el VPath ya
            // lo garantiza); defensa en profundidad por si acaso.
            if name.contains('/') || name == "." || name == ".." {
                return Err(Error::InvalidPath);
            }
            if out.len() > 1 || !out.ends_with('/') {
                out.push('/');
            }
            out.push_str(name);
        }
        Ok(out)
    }

    /// Path del staging estable de resume para `p` (ADR 0012).
    fn stable_partial(&self, p: &VPath) -> Result<String, Error> {
        let parent = self.remote_parent(p)?;
        let name = p.file_name().ok_or(Error::InvalidPath)?;
        // Hash de los bytes del nombre final (SHA no hace falta aquí: el
        // servidor de test es de confianza; el nombre solo debe ser estable
        // y único por destino — se usa el mismo esquema que el efímero).
        let hash = fnv1a_128(name.as_bytes());
        Ok(format!("{parent}/{PARTIAL_PREFIX}{hash:032x}"))
    }

    /// El path remoto del DIRECTORIO padre de `p`.
    fn remote_parent(&self, p: &VPath) -> Result<String, Error> {
        let parent = p.parent().ok_or(Error::InvalidPath)?;
        self.remote(&parent)
    }

    fn session(&self) -> Arc<SftpSession> {
        Arc::clone(&self.session)
    }
}

impl std::fmt::Debug for SftpProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpProvider")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// FNV-1a de 128 bits: hash estable (no depende de la versión de Rust) para
/// nombrar staging. No es cripto — el servidor sftp de test es de confianza
/// y el hash solo necesita ser estable y único por destino.
fn fnv1a_128(bytes: &[u8]) -> u128 {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut h = OFFSET;
    for &b in bytes {
        h ^= u128::from(b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// Mapea el error de russh-sftp a la taxonomía del protocolo (spec §17.7):
/// los frontends renderizan por categoría, jamás parsean strings.
fn map_err(e: &russh_sftp::client::error::Error) -> Error {
    use russh_sftp::client::error::Error as E;
    use russh_sftp::protocol::StatusCode as S;
    match e {
        E::Status(st) => match st.status_code {
            // Eof al pedir metadatos/leer un inexistente = NotFound.
            S::NoSuchFile | S::Eof => Error::NotFound,
            S::PermissionDenied => Error::PermissionDenied,
            S::OpUnsupported => Error::Unsupported,
            // v3 devuelve `Failure` genérico para casi todo (incluido "ya
            // existe" en mkdir/rename): el caller que sepa el contexto lo
            // reinterpreta; por defecto, I/O no reintentable.
            _ => Error::Io { retryable: false },
        },
        // Fallo de transporte: el provider "no responde" — reintentable.
        E::IO(_) | E::Timeout | E::Limited(_) => Error::ProviderUnavailable { retryable: true },
        E::UnexpectedPacket | E::UnexpectedBehavior(_) => Error::Io { retryable: false },
    }
}

/// Reconstruye una [`Entry`] a partir de la `Metadata` de sftp sobre el
/// `VPath` pedido (la authority/scheme se preservan — la identidad del path
/// en el wire no cambia por pasar por el provider).
/// ¿`name` tiene la FORMA exacta de un staging sftp? (#11) Estrecho a las
/// dos formas de ESTE provider — jamás el prefijo suelto (H2):
/// - estable: prefijo + exactamente 32 hex ([`SftpProvider`] resumable)
/// - efímero: prefijo + `eph.` + dígitos (contador de `write`)
fn is_norte_partial(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PARTIAL_PREFIX) else {
        return false;
    };
    let is_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    if rest.len() == 32 && rest.bytes().all(is_hex) {
        return true;
    }
    rest.strip_prefix("eph.")
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}

fn entry_from(path: VPath, md: &russh_sftp::protocol::FileAttributes) -> Entry {
    let kind = if md.is_symlink() {
        EntryKind::Symlink
    } else if md.is_dir() {
        EntryKind::Dir
    } else if md.is_regular() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    let size = (kind == EntryKind::File).then(|| md.len());
    // mtime de sftp v3 es segundos u32 desde epoch.
    let mtime_ms = md.mtime.map(|s| i64::from(s) * 1000);
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path,
        kind,
        size,
        mtime_ms,
    }
}

#[async_trait]
impl Provider for SftpProvider {
    #[allow(clippy::unnecessary_literal_bound)]
    fn scheme(&self) -> &str {
        "sftp"
    }

    fn capabilities(&self) -> Capabilities {
        // Honestas (ADR 0013): sftp tiene symlinks y escritura en offset/
        // append (habilita el resume de ADR 0012), y se asume remoto POSIX
        // case-sensitive. NO declara: rename atómico (v3 no lo garantiza) ni
        // server-copy. TRASH solo si la conexión activó la papelera lógica
        // `.norte-trash/` (ADR 0019).
        let mut flags = CapabilityFlags::SYMLINKS
            | CapabilityFlags::APPEND
            | CapabilityFlags::RANDOM_WRITE
            | CapabilityFlags::CASE_PRESERVING
            // El remoto se asume POSIX (case-sensitive): declararla evita
            // que el engine invente colisiones de caja que un servidor
            // Linux no tiene (bytes exactos = conservador correcto).
            | CapabilityFlags::CASE_SENSITIVE;
        if self.logical_trash {
            flags |= CapabilityFlags::TRASH;
        }
        Capabilities {
            flags,
            max_path: None,
        }
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        let remote = self.remote(p)?;
        // lstat: describe el LINK, jamás lo sigue (contención de symlinks
        // trampa — ADR 0013).
        let md = self
            .session
            .symlink_metadata(remote)
            .await
            .map_err(|e| map_err(&e))?;
        Ok(entry_from(p.clone(), &md))
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        let remote = self.remote(p)?;
        let base = p.clone();
        let dir = self
            .session
            .read_dir(remote)
            .await
            .map_err(|e| map_err(&e))?;
        let mut entries: Vec<Result<Entry, Error>> = Vec::new();
        for dent in dir {
            let name = dent.file_name();
            // `.`/`..` no son hijos; un nombre con `/` es un servidor
            // hostil intentando escapar la base — se rechaza, el listado
            // NO continúa a ciegas (ADR 0013).
            if name == "." || name == ".." {
                continue;
            }
            // russh-sftp decodifica los nombres del servidor con
            // `from_utf8_lossy`: un byte no-UTF8 llega ya sustituido por U+FFFD
            // y los bytes originales se perdieron BAJO nuestra frontera. No se
            // puede garantizar identidad de bytes → rechazo LIMPIO en vez de
            // emitir un Entry corrupto (regla 1 / ADR 0013 D2). Deuda: leer los
            // bytes crudos del paquete SSH_FXP_NAME (issue #37).
            if name.contains('\u{FFFD}') || name.contains('/') {
                entries.push(Err(Error::InvalidPath));
                break;
            }
            let Ok(seg) = norte_proto::Segment::new(name.into_bytes()) else {
                entries.push(Err(Error::InvalidPath));
                break;
            };
            let child = base.join(seg);
            entries.push(Ok(entry_from(child, &dent.metadata())));
        }
        Ok(futures::stream::iter(entries).boxed())
    }

    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        let remote = self.remote(p)?;
        let session = self.session();
        // Rechaza dirs (leerlos es error, como el resto de providers).
        let md = session
            .symlink_metadata(&remote)
            .await
            .map_err(|e| map_err(&e))?;
        if md.is_dir() || md.is_symlink() {
            // Un dir no se lee; un symlink NO se sigue (coherente con el
            // invariante lstat de stat/node_id — ADR 0013). El engine recorre
            // symlinks vía read_link, jamás vía read().
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let mut file = session
            .open_with_flags(&remote, OpenFlags::READ)
            .await
            .map_err(|e| map_err(&e))?;
        if let Some(r) = range {
            file.seek(std::io::SeekFrom::Start(r.offset))
                .await
                .map_err(|_| Error::Io { retryable: false })?;
        }
        let len = range.and_then(|r| r.len);
        Ok(read_stream::sftp_read_stream(file, len))
    }

    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        // Staging efímero: nombre corto único (no deriva del nombre final,
        // que puede rozar NAME_MAX; ADR 0012). Contrato: el destino final
        // debe NO existir (create-new) — sftp v3 no tiene O_EXCL, así que
        // se comprueba con stat (ventana TOCTOU documentada).
        let final_remote = self.remote(p)?;
        self.check_final_absent(&final_remote).await?;
        let parent = self.remote_parent(p)?;
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let staging = format!("{parent}/{PARTIAL_PREFIX}eph.{seq}");
        let file = self
            .session
            .open_with_flags(
                &staging,
                // EXCLUDE (create-new atómico): si el servidor pre-plantó el
                // staging predecible como symlink fuera de base, el open FALLA
                // en vez de seguirlo y escribir en el target (contención de
                // escritura — ADR 0013 / threat model §14).
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
            )
            .await
            .map_err(|e| map_err(&e))?;
        Ok(Box::new(SftpSink {
            session: self.session(),
            file: Some(file),
            staging,
            final_remote,
        }))
    }

    async fn partial_digest(&self, p: &VPath, len: u64) -> Result<Option<[u8; 32]>, Error> {
        use sha2::{Digest, Sha256};
        use tokio::io::AsyncReadExt as _;
        // Mismo staging estable que open_resumable (#35): SHA-256 de sus
        // primeros `len` bytes.
        let staging = self.stable_partial(p)?;
        // Sin staging = sin digest (el engine degrada a Length). El servidor
        // puede señalar la ausencia de varias formas; cualquier fallo al abrir
        // el staging efímero se trata como "no hay".
        let Ok(mut file) = self
            .session
            .open_with_flags(&staging, OpenFlags::READ)
            .await
        else {
            return Ok(None);
        };
        let mut hasher = Sha256::new();
        let mut remaining = len;
        let mut buf = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let want = usize::try_from(remaining.min(buf.len() as u64)).unwrap_or(buf.len());
            let n = file
                .read(&mut buf[..want])
                .await
                .map_err(|_| Error::Io { retryable: false })?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
        if remaining > 0 {
            // Staging más corto que `len`: sin prefijo completo → Length.
            return Ok(None);
        }
        Ok(Some(hasher.finalize().into()))
    }

    async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
        let final_remote = self.remote(p)?;
        self.check_final_absent(&final_remote).await?;
        let staging = self.stable_partial(p)?;
        // Un staging PRE-EXISTENTE debe ser un fichero regular: si el servidor
        // lo pre-plantó como symlink (el nombre es determinista), reanudar en
        // APPEND escribiría en el target fuera de base. Se rechaza (no se puede
        // usar EXCLUDE: el resume reabre legítimamente un parcial). TOCTOU
        // documentada, de la misma clase que check_final_absent.
        match self.session.symlink_metadata(&staging).await {
            Ok(md) if md.file_type().is_symlink() => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            _ => {}
        }
        // Abre (o crea) el staging en APPEND: si había bytes de una copia
        // previa, se reanuda tras ellos (ADR 0012).
        let mut file = self
            .session
            .open_with_flags(
                &staging,
                OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::APPEND,
            )
            .await
            .map_err(|e| map_err(&e))?;
        let already = file.metadata().await.map_err(|e| map_err(&e))?.len();
        // El offset de escritura del cliente arranca en 0 aunque el flag sea
        // APPEND (sftp lleva el offset explícito en cada WRITE): hay que
        // posicionarlo al final para AÑADIR y no pisar lo ya escrito.
        if already > 0 {
            file.seek(std::io::SeekFrom::Start(already))
                .await
                .map_err(|_| Error::Io { retryable: false })?;
        }
        Ok((
            Box::new(SftpSink {
                session: self.session(),
                file: Some(file),
                staging,
                final_remote,
            }),
            already,
        ))
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        let remote = self.remote(p)?;
        // v3 devuelve `Failure` genérico si ya existe: se comprueba antes
        // para dar `Conflict` honesto (el engine lo distingue).
        if self.exists(&remote).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        self.session
            .create_dir(remote)
            .await
            .map_err(|e| map_err(&e))
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        let remote = self.remote(p)?;
        let md = self
            .session
            .symlink_metadata(&remote)
            .await
            .map_err(|e| map_err(&e))?;
        if md.is_dir() {
            self.session
                .remove_dir(remote)
                .await
                .map_err(|e| map_err(&e))
        } else {
            // remove_file borra archivos Y symlinks (jamás sigue el link).
            self.session
                .remove_file(remote)
                .await
                .map_err(|e| map_err(&e))
        }
    }

    /// GC de staging huérfano (#11, ADR 0012): barre los `.norte-partial.*`
    /// de `dir` cuya mtime supera `older_than`, reconocidos por su FORMA
    /// exacta (`is_norte_partial`) — un archivo real del usuario con el
    /// prefijo jamás se toca (H2). Los nombres del staging son ASCII por
    /// construcción: la decodificación lossy de russh-sftp (#37) no puede
    /// producir un falso positivo (U+FFFD no matchea la forma).
    async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        let parent = self.remote(dir)?;
        let dirents = self
            .session
            .read_dir(&parent)
            .await
            .map_err(|e| map_err(&e))?;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut removed = 0usize;
        for dent in dirents {
            let name = dent.file_name();
            if !is_norte_partial(&name) {
                continue;
            }
            // Edad por mtime (sftp v3: segundos u32); sin mtime legible se
            // deja (conservador, como el provider local).
            let old = dent
                .metadata()
                .mtime
                .is_some_and(|m| now_secs.saturating_sub(u64::from(m)) >= older_than.as_secs());
            if !old {
                continue;
            }
            let path = if parent.ends_with('/') {
                format!("{parent}{name}")
            } else {
                format!("{parent}/{name}")
            };
            // Un fallo individual cuenta como no-borrado, sin abortar.
            if self.session.remove_file(&path).await.is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        let from_r = self.remote(from)?;
        let to_r = self.remote(to)?;
        // v3 rename no garantiza no-replace: se comprueba el destino antes
        // (ventana TOCTOU documentada) para dar `Conflict`, no pisar.
        if self.exists(&to_r).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        self.session
            .rename(from_r, to_r)
            .await
            .map_err(|e| map_err(&e))
    }

    async fn trash(&self, p: &VPath) -> Result<Option<VPath>, Error> {
        if !self.logical_trash {
            return Err(Error::Unsupported);
        }
        // Víctima ausente = `NotFound` limpio (como `remove`), sin crear una
        // entrada de papelera huérfana (lstat: no sigue symlinks).
        let _ = self.stat(p).await?;

        // Reloj de pared + contador de sesión → id único y ordenable.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));

        // Primer plan: valida `p` (rechaza papelerizar la propia papelera,
        // ADR 0019) y da la raíz `.norte-trash`.
        let first = trash::plan(p, &trash::trash_id(now_ms, self.next_counter()))?;
        let trash_root = first.dir.parent().ok_or(Error::Unsupported)?;

        // `.norte-trash/` idempotente: bajo concurrencia entre sesiones el
        // `create_dir` perdedor puede dar `Failure` genérico (→ `Io`) en vez
        // de `Conflict`; si ya existe, es benigno.
        self.ensure_dir_idempotent(&trash_root).await?;

        // `.norte-trash/<id>/` fresco. `<id>` solo es único POR SESIÓN; dos
        // conexiones borrando en el mismo ms colisionan → reintenta con id
        // nuevo (el contador avanza) en vez de fallar en duro.
        let mut paths = first;
        let mut attempts = 0u32;
        loop {
            match self.mkdir(&paths.dir).await {
                Ok(()) => break,
                Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                }) if attempts < 8 => {
                    attempts += 1;
                    paths = trash::plan(p, &trash::trash_id(now_ms, self.next_counter()))?;
                }
                Err(e) => return Err(e),
            }
        }

        // Escribe `.norte-info` ANTES de mover: si el rename falla, el origen
        // queda intacto y solo hay un info huérfano (basura limpiable), nunca
        // un payload sin metadatos.
        let info = trash::info_encode(p, now_ms);
        let mut sink = self.write(&paths.info).await?;
        sink.write(Bytes::from(info)).await?;
        sink.commit().await?;

        // Mueve el árbol entero (un rename del server — ADR 0009,
        // entries_total = 1).
        self.rename(p, &paths.payload).await?;
        // Papelera LÓGICA: el payload ES la ruta recuperable → reversal_ref.
        Ok(Some(paths.payload))
    }

    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        let remote = self.remote(p)?;
        // Un no-symlink da TypeMismatch honesto (v3 devuelve Failure
        // genérico para readlink sobre un archivo normal).
        let md = self
            .session
            .symlink_metadata(&remote)
            .await
            .map_err(|e| map_err(&e))?;
        if !md.is_symlink() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        // Bytes CRUDOS del target (regla 1): el target puede ser `../../`
        // — es DATO, jamás se resuelve.
        let target = self
            .session
            .read_link(remote)
            .await
            .map_err(|e| map_err(&e))?;
        // russh-sftp decodifica lossy: un target no-UTF8 llega mutilado a
        // U+FFFD. No se pueden devolver bytes crudos fiables → rechazo limpio
        // (Hallazgo A / ADR 0013 D2), jamás un target corrupto.
        if target.contains('\u{FFFD}') {
            return Err(Error::InvalidPath);
        }
        Ok(target.into_bytes())
    }

    async fn symlink(&self, link: &VPath, target: &[u8], _kind: SymlinkKind) -> Result<(), Error> {
        let link_r = self.remote(link)?;
        // El target son bytes crudos; sftp (russh-sftp) exige UTF-8.
        let target = std::str::from_utf8(target).map_err(|_| Error::InvalidPath)?;
        if self.exists(&link_r).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        self.session
            .symlink(link_r, target)
            .await
            .map_err(|e| map_err(&e))
    }

    async fn node_id(&self, p: &VPath, _follow: FollowLinks) -> Result<Option<NodeId>, Error> {
        // SFTP no expone identidad estable (las FileAttributes no llevan
        // inodo): el engine degrada a heurística y `Follow` sobre
        // dir-symlinks responde `Unsupported` — la contención que queremos
        // (ADR 0013). Se stat-ea igual para propagar NotFound honesto.
        let remote = self.remote(p)?;
        self.session
            .symlink_metadata(remote)
            .await
            .map_err(|e| map_err(&e))?;
        Ok(None)
    }
}

impl SftpProvider {
    /// El destino FINAL no debe existir (contrato de `write`/`open_resumable`;
    /// la política de sobrescritura es del core, no del provider).
    async fn check_final_absent(&self, remote: &str) -> Result<(), Error> {
        if self.exists(remote).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        Ok(())
    }

    /// ¿Existe `remote`? (lstat; un symlink cuenta como existente.)
    async fn exists(&self, remote: &str) -> Result<bool, Error> {
        match self.session.symlink_metadata(remote).await {
            Ok(_) => Ok(true),
            Err(e) => match map_err(&e) {
                Error::NotFound => Ok(false),
                other => Err(other),
            },
        }
    }
}

/// Sink de escritura sobre sftp: los bytes van a un staging remoto;
/// `commit` renombra a final (contrato de [`ByteSink`], ADR 0012).
struct SftpSink {
    session: Arc<SftpSession>,
    file: Option<russh_sftp::client::fs::File>,
    staging: String,
    final_remote: String,
}

impl SftpSink {
    async fn remove_staging(&self) {
        let _ = self.session.remove_file(&self.staging).await;
    }
}

#[async_trait]
impl ByteSink for SftpSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        let file = self.file.as_mut().ok_or(Error::Io { retryable: false })?;
        file.write_all(&chunk)
            .await
            .map_err(|_| Error::Io { retryable: false })
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        if let Some(mut file) = self.file.take() {
            file.flush()
                .await
                .map_err(|_| Error::Io { retryable: false })?;
            file.sync_all().await.map_err(|e| map_err(&e))?;
            drop(file);
        }
        // El destino final no debe existir (create-new): comprobado en
        // write()/open_resumable; la ventana hasta aquí es TOCTOU (v3 sin
        // rename atómico) — si aparece algo, Conflict y el staging se
        // queda para el GC.
        match self.session.symlink_metadata(&self.final_remote).await {
            Ok(_) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                });
            }
            Err(e) if matches!(map_err(&e), Error::NotFound) => {}
            Err(e) => {
                return Err(map_err(&e));
            }
        }
        self.session
            .rename(&self.staging, &self.final_remote)
            .await
            .map_err(|e| map_err(&e))?;
        Ok(())
    }

    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        self.file.take();
        self.remove_staging().await;
        Ok(())
    }

    async fn keep(mut self: Box<Self>) -> Result<(), Error> {
        // Conserva el staging para un open_resumable posterior (ADR 0012):
        // durabiliza y suelta sin renombrar ni borrar.
        if let Some(file) = self.file.take() {
            let _ = file.sync_all().await;
        }
        Ok(())
    }
}

/// Productor del stream de lectura, aislado para no arrastrar genéricos al
/// método del trait.
mod read_stream {
    use super::{ByteStream, Bytes, Error, READ_CHUNK, StreamExt};
    use tokio::io::AsyncReadExt;

    /// Stream de chunks desde un `File` sftp abierto (ya posicionado en el
    /// offset). `len` acota los bytes a entregar (`None` = hasta EOF).
    pub(super) fn sftp_read_stream(
        file: russh_sftp::client::fs::File,
        len: Option<u64>,
    ) -> ByteStream {
        let s = futures::stream::unfold(
            (file, len, false),
            |(mut file, mut remaining, done)| async move {
                if done {
                    return None;
                }
                let want = match remaining {
                    Some(0) => return None,
                    Some(n) => usize::try_from(n.min(READ_CHUNK as u64)).unwrap_or(READ_CHUNK),
                    None => READ_CHUNK,
                };
                let mut buf = vec![0u8; want];
                match file.read(&mut buf).await {
                    Ok(0) => None,
                    Ok(n) => {
                        buf.truncate(n);
                        if let Some(rem) = &mut remaining {
                            *rem -= n as u64;
                        }
                        Some((Ok(Bytes::from(buf)), (file, remaining, false)))
                    }
                    Err(_) => Some((Err(Error::Io { retryable: false }), (file, remaining, true))),
                }
            },
        );
        s.boxed()
    }
}
