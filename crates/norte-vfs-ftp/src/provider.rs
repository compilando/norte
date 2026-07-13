//! [`FtpProvider`]: el trait [`Provider`] sobre una conexión FTP de suppaftp
//! (ADR 0014). La conexión de control es ÚNICA y estatal → `Arc<Mutex>`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{
    ByteRange, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, Scheme, VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};
use suppaftp::list::{File, ListParser};
use suppaftp::tokio::AsyncRustlsFtpStream;
use suppaftp::types::FileType;
use suppaftp::{FtpError, Status};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, OwnedMutexGuard};

/// Tamaño de chunk de lectura (256 KiB, alineado con el copy engine).
const READ_CHUNK: usize = 256 * 1024;
/// Prefijo del staging de escritura (ADR 0012, mismo convenio que local/sftp).
const PARTIAL_PREFIX: &str = ".norte-partial.";

// Rustls-capaz: el MISMO tipo transporta FTP plano (sin `into_secure`) y FTPS
// (ADR 0015 F). El connect con la política TLS vive en norte-connect; aquí
// solo cambia el parámetro de tipo del stream inyectado.
type Ftp = AsyncRustlsFtpStream;

/// Conexión de control + estado de resincronización (issue #39 M1).
///
/// Cancelar una lectura (drop del stream con el RETR en vuelo) deja la
/// respuesta de transferencia (226/426) PENDIENTE en el control; la
/// siguiente operación la leería como suya y desincronizaría la conexión.
/// El flag lo marca el read y [`lock_synced`] drena antes de operar.
struct Conn {
    stream: Ftp,
    pending_retr: bool,
}

impl Conn {
    fn new(stream: Ftp) -> Self {
        Self {
            stream,
            pending_retr: false,
        }
    }
}

impl std::ops::Deref for Conn {
    type Target = Ftp;
    fn deref(&self) -> &Ftp {
        &self.stream
    }
}

impl std::ops::DerefMut for Conn {
    fn deref_mut(&mut self) -> &mut Ftp {
        &mut self.stream
    }
}

/// Bloquea la conexión y, si un RETR cancelado dejó respuesta pendiente, la
/// drena (best-effort: si la conexión está rota, la operación que sigue
/// fallará con su propio error, que es el diagnóstico honesto).
///
/// El drenado va por `finalize_retr_stream` con un stream VACÍO: además de
/// leer la respuesta pendiente (426, o 226 si el servidor terminó antes de
/// notar el cierre), resetea el flag interno `data_connection_open` de
/// suppaftp — sin eso, todo comando de datos posterior fallaría con
/// `DataConnectionAlreadyOpen` aunque el control esté limpio.
async fn lock_synced(conn: &Arc<Mutex<Conn>>) -> OwnedMutexGuard<Conn> {
    let mut g = Arc::clone(conn).lock_owned().await;
    if g.pending_retr {
        // Err(UnexpectedResponse(426)) esperado en cancelación: la respuesta
        // queda consumida del wire igualmente.
        let _ = g.stream.finalize_retr_stream(tokio::io::empty()).await;
        g.pending_retr = false;
    }
    g
}

/// Provider VFS sobre una conexión FTP ya establecida (ADR 0014).
///
/// FTP tiene una sola conexión de control ESTATAL (CWD/TYPE/REST se acumulan):
/// se envuelve en `Arc<Mutex>` y las operaciones se serializan. La conexión
/// con auth (cleartext / FTPS) se INYECTA ([`FtpProvider::new`] /
/// [`FtpProvider::with_reader`]); el `connect` con secretos vive en
/// norte-connect (fase 6). El modo BINARIO se fija al construir (FTP
/// arranca en ASCII, que corrompe binarios).
pub struct FtpProvider {
    ftp: Arc<Mutex<Conn>>,
    /// Conexión de control DEDICADA a lecturas (issue #39 B1): el RETR de una
    /// copia FTP→FTP mismo host retiene su conexión durante todo el stream —
    /// con una sola, el APPE del lado escritura deadlockearía. `None` = una
    /// sola conexión (lecturas y escrituras se serializan; una copia
    /// mismo-host NO es segura).
    reader: Option<Arc<Mutex<Conn>>>,
    /// Raíz remota absoluta bajo la que vive todo. Sin `..`, sin barra final.
    base: String,
    /// Contador de staging (nombre efímero único para `write`).
    seq: AtomicU64,
    /// El servidor soporta MLSD/MLST (machine-readable). Si no (p. ej.
    /// pure-ftpd), se degrada a `LIST` (`ls -l`), universal pero frágil con
    /// nombres hostiles — ADR 0014 C.
    has_mlsd: bool,
}

impl FtpProvider {
    /// Provider sobre una conexión ya logueada, enraizado en `base`. Fija el
    /// modo de transferencia BINARIO (imprescindible: ASCII corrompe binarios).
    ///
    /// # Errors
    /// Si el `TYPE I` inicial falla (conexión caída).
    pub async fn new(stream: Ftp, base: impl Into<String>) -> Result<Self, Error> {
        Self::build(stream, None, base).await
    }

    /// Como [`FtpProvider::new`], con una SEGUNDA conexión (al mismo servidor,
    /// ya logueada) dedicada a lecturas: hace segura la copia FTP→FTP dentro
    /// del mismo host (issue #39 B1) — el RETR va por `reader` y el APPE del
    /// lado escritura por la principal, sin deadlock.
    ///
    /// # Errors
    /// Si el `TYPE I` inicial falla en cualquiera de las dos conexiones.
    pub async fn with_reader(
        main: Ftp,
        reader: Ftp,
        base: impl Into<String>,
    ) -> Result<Self, Error> {
        Self::build(main, Some(reader), base).await
    }

    async fn build(
        mut stream: Ftp,
        reader: Option<Ftp>,
        base: impl Into<String>,
    ) -> Result<Self, Error> {
        let has_mlsd = setup_conn(&mut stream).await?;
        let reader = match reader {
            Some(mut r) => {
                // Misma preparación (BINARIO es imprescindible para RETR); el
                // has_mlsd manda el de la principal (mismo servidor).
                setup_conn(&mut r).await?;
                Some(Arc::new(Mutex::new(Conn::new(r))))
            }
            None => None,
        };
        let mut base = base.into();
        while base.len() > 1 && base.ends_with('/') {
            base.pop();
        }
        // `base` es config de confianza (fase 6), pero se valida como defensa en
        // profundidad: absoluta y sin CR/LF/NUL (que inyectarían un comando FTP
        // en cada operación, saltándose el filtro por-segmento).
        if !base.starts_with('/') || base.contains(['\r', '\n', '\0']) {
            return Err(Error::InvalidPath);
        }
        Ok(Self {
            ftp: Arc::new(Mutex::new(Conn::new(stream))),
            reader,
            base,
            seq: AtomicU64::new(0),
            has_mlsd,
        })
    }

    /// La raíz de este provider para un `authority` dado (`ftp://host:21/`).
    ///
    /// # Panics
    /// Nunca: el scheme es constante y válido.
    #[must_use]
    pub fn root(authority: norte_proto::Authority) -> VPath {
        VPath::root(
            Scheme::new("ftp").expect("scheme constante válido"),
            Some(authority),
        )
    }

    /// Traduce un [`VPath`] al path remoto absoluto bajo la `base`. Los
    /// segmentos son BYTES; FTP (vía suppaftp) exige UTF-8 — un nombre no
    /// representable es [`Error::InvalidPath`] (rechazo LIMPIO, regla 1, ADR
    /// 0014 D2). El path se construye SIEMPRE así, nunca desde uno ecoado.
    fn remote(&self, p: &VPath) -> Result<String, Error> {
        if p.scheme() != "ftp" {
            return Err(Error::InvalidPath);
        }
        let mut out = String::from(&self.base);
        for seg in p.segments() {
            let name = std::str::from_utf8(seg).map_err(|_| Error::InvalidPath)?;
            if name.contains('/') || name == "." || name == ".." {
                return Err(Error::InvalidPath);
            }
            // FTP es un protocolo de LÍNEAS: el comando termina en CRLF. Un
            // nombre con CR/LF inyectaría un comando FTP arbitrario
            // (`STOR path\r\nDELE víctima`) — se rechaza. Contención ESPECÍFICA
            // de FTP; sftp (binario, con framing) es inmune. `Segment` ya
            // rechaza NUL y `/`, pero no CR/LF (son nombres válidos en POSIX).
            if name.contains(['\r', '\n']) {
                return Err(Error::InvalidPath);
            }
            // NAME_MAX: la abrumadora mayoría de FS (ext4/xfs/apfs/ntfs)
            // rechazan nombres de >255 BYTES con ENAMETOOLONG; el servidor
            // fallaría a media operación con un `550` ambiguo (que se mapea a
            // NotFound, no a InvalidPath). Se rechaza LIMPIO upfront.
            if seg.len() > 255 {
                return Err(Error::InvalidPath);
            }
            if out.len() > 1 || !out.ends_with('/') {
                out.push('/');
            }
            out.push_str(name);
        }
        Ok(out)
    }

    /// El path remoto del DIRECTORIO padre de `p`.
    fn remote_parent(&self, p: &VPath) -> Result<String, Error> {
        let parent = p.parent().ok_or(Error::InvalidPath)?;
        self.remote(&parent)
    }

    /// Path del staging estable de resume para `p` (ADR 0012).
    fn stable_partial(&self, p: &VPath) -> Result<String, Error> {
        let parent = self.remote_parent(p)?;
        let name = p.file_name().ok_or(Error::InvalidPath)?;
        let hash = fnv1a_128(name.as_bytes());
        Ok(format!("{parent}/{PARTIAL_PREFIX}{hash:032x}"))
    }

    fn ftp(&self) -> Arc<Mutex<Conn>> {
        Arc::clone(&self.ftp)
    }
}

/// Prepara una conexión recién logueada: BINARIO (ASCII corrompe binarios),
/// detección de MLSD/MLST y `OPTS UTF8 ON` si el servidor lo anuncia (RFC
/// 2640, ADR 0014 D2; best-effort). Devuelve si hay MLSD.
async fn setup_conn(stream: &mut Ftp) -> Result<bool, Error> {
    stream
        .transfer_type(FileType::Binary)
        .await
        .map_err(|e| map_err(&e))?;
    // ¿MLSD/MLST? (listado machine-readable, robusto con nombres hostiles).
    // Muchos servidores (pure-ftpd) no lo anuncian → respaldo a LIST.
    let feats = stream.feat().await.ok();
    let has_mlsd = feats.as_ref().is_some_and(|f| {
        f.keys()
            .any(|k| k.eq_ignore_ascii_case("MLST") || k.eq_ignore_ascii_case("MLSD"))
    });
    if feats
        .as_ref()
        .is_some_and(|f| f.keys().any(|k| k.eq_ignore_ascii_case("UTF8")))
    {
        let _ = stream.opts("UTF8", Some("ON")).await;
    }
    Ok(has_mlsd)
}

impl std::fmt::Debug for FtpProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FtpProvider")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// FNV-1a de 128 bits: hash estable para nombrar el staging (no cripto).
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

/// Mapea el error de suppaftp a la taxonomía del protocolo (spec §17.7).
fn map_err(e: &FtpError) -> Error {
    match e {
        FtpError::UnexpectedResponse(r) => match r.status {
            // 550 es ambiguo en FTP (no existe / sin permiso): NotFound es el
            // caso común y el que el contrato espera para paths ausentes.
            Status::FileUnavailable => Error::NotFound,
            Status::NotLoggedIn => Error::PermissionDenied,
            Status::BadFilename => Error::InvalidPath,
            // 450 = fichero ocupado/acción no tomada: reintentable.
            Status::RequestFileActionIgnored => Error::Io { retryable: true },
            _ => Error::Io { retryable: false },
        },
        // Fallo de transporte (TCP, o TLS caído a MITAD de sesión — la
        // negociación es del connect, fase 6d): el provider "no responde",
        // reintentable.
        FtpError::ConnectionError(_) | FtpError::SecureError(_) => {
            Error::ProviderUnavailable { retryable: true }
        }
        FtpError::InvalidAddress(_) => Error::InvalidPath,
        FtpError::BadResponse | FtpError::DataConnectionAlreadyOpen => {
            Error::Io { retryable: false }
        }
    }
}

/// Parsea una línea de `LIST` (`ls -l` POSIX, con respaldo DOS). `None` para
/// líneas no parseables (cabeceras `total N`, líneas raras): se descartan.
fn parse_list_line(line: &str) -> Option<File> {
    ListParser::parse_posix(line)
        .ok()
        .or_else(|| ListParser::parse_dos(line).ok())
}

fn entry_from_file(path: VPath, f: &File) -> Entry {
    let kind = if f.is_symlink() {
        EntryKind::Symlink
    } else if f.is_directory() {
        EntryKind::Dir
    } else if f.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    let size = (kind == EntryKind::File).then(|| f.size() as u64);
    let mtime_ms = f
        .modified()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok());
    Entry {
        path,
        kind,
        size,
        mtime_ms,
    }
}

/// `stat` de `remote`. Con MLSD: `MLST` directo (robusto). Sin MLSD: `LIST`
/// del DIRECTORIO padre + búsqueda por nombre (universal — pure-ftpd; ADR 0014
/// C). `None` = no existe.
async fn stat_remote(
    guard: &mut Ftp,
    remote: &str,
    has_mlsd: bool,
    base: &str,
) -> Result<Option<File>, Error> {
    if has_mlsd {
        return match guard.mlst(Some(remote)).await {
            Ok(line) => {
                let f =
                    ListParser::parse_mlst(&line).map_err(|_| Error::Io { retryable: false })?;
                Ok(Some(f))
            }
            Err(e) => match map_err(&e) {
                Error::NotFound => Ok(None),
                other => Err(other),
            },
        };
    }
    // Contención (security): la rama LIST lista el DIRECTORIO PADRE. Para la
    // RAÍZ del provider (`remote == base`) el padre estaría FUERA de la base —
    // jamás se lista por encima de la base. La raíz es un dir degenerado como
    // "hijo": None (fail-safe; sus ops son degeneradas de todas formas).
    if remote == base {
        return Ok(None);
    }
    let (parent, child) = match remote.rfind('/') {
        Some(0) => ("/", &remote[1..]),
        Some(i) => (&remote[..i], &remote[i + 1..]),
        // `remote` siempre es absoluto (arranca en la base); sin `/` no es un
        // hijo consultable.
        None => return Ok(None),
    };
    if child.is_empty() {
        return Ok(None);
    }
    let lines = match guard.list(Some(parent)).await {
        Ok(l) => l,
        Err(e) => {
            return match map_err(&e) {
                Error::NotFound => Ok(None),
                other => Err(other),
            };
        }
    };
    for line in lines {
        let Some(f) = parse_list_line(&line) else {
            continue;
        };
        // El nombre del servidor viene lossy: un no-UTF8 (U+FFFD) jamás casa un
        // `child` UTF-8 del cliente de forma fiable → se salta (fail-loud), no
        // se compara (evita un falso match con metadatos equivocados).
        let n = f.name();
        if n.contains('\u{FFFD}') {
            continue;
        }
        if n == child {
            return Ok(Some(f));
        }
    }
    Ok(None)
}

/// ¿Existe `remote`? (vía `stat_remote`.)
async fn exists(guard: &mut Ftp, remote: &str, has_mlsd: bool, base: &str) -> Result<bool, Error> {
    Ok(stat_remote(guard, remote, has_mlsd, base).await?.is_some())
}

/// Crea `remote` como fichero VACÍO (STOR sin datos): base para APPE-ar.
///
/// FTP no tiene `O_EXCL`: si el staging predecible ya existía, `STOR` lo trunca
/// (no falla como el `EXCLUDE` de sftp). Riesgo bajo — el nombre lleva `seq`/hash,
/// FTP no tiene symlinks (no hay escape de base) y el destino final sí se
/// comprueba. Divergencia consciente del endurecimiento de sftp (threat model).
async fn create_empty(guard: &mut Ftp, remote: &str) -> Result<(), Error> {
    let data = guard
        .put_with_stream(remote)
        .await
        .map_err(|e| map_err(&e))?;
    guard
        .finalize_put_stream(data)
        .await
        .map_err(|e| map_err(&e))
}

#[async_trait]
impl Provider for FtpProvider {
    #[allow(clippy::unnecessary_literal_bound)]
    fn scheme(&self) -> &str {
        "ftp"
    }

    fn capabilities(&self) -> Capabilities {
        // Honestas (ADR 0014): APPE + REST habilitan el resume (ADR 0012); se
        // asume remoto POSIX case-sensitive. NO declara: SYMLINKS (FTP base no
        // los crea), RANDOM_WRITE (REST+STOR no fiable entre servidores),
        // rename atómico, papelera ni server-copy.
        Capabilities {
            flags: CapabilityFlags::APPEND
                | CapabilityFlags::CASE_PRESERVING
                | CapabilityFlags::CASE_SENSITIVE,
            max_path: None,
        }
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        let remote = self.remote(p)?;
        // La raíz del provider es el directorio base (no tiene padre que listar).
        if p.parent().is_none() {
            return Ok(Entry {
                path: p.clone(),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            });
        }
        let mut guard = lock_synced(&self.ftp).await;
        let found = stat_remote(&mut guard, &remote, self.has_mlsd, &self.base).await?;
        drop(guard);
        // El path del Entry es el del cliente, no el nombre ecoado.
        match found {
            Some(f) => Ok(entry_from_file(p.clone(), &f)),
            None => Err(Error::NotFound),
        }
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        let remote = self.remote(p)?;
        let base = p.clone();
        let has_mlsd = self.has_mlsd;
        let mut guard = lock_synced(&self.ftp).await;
        let lines = if has_mlsd {
            guard.mlsd(Some(&remote)).await
        } else {
            guard.list(Some(&remote)).await
        }
        .map_err(|e| map_err(&e))?;
        drop(guard);
        let mut entries: Vec<Result<Entry, Error>> = Vec::new();
        for line in lines {
            let parsed = if has_mlsd {
                ListParser::parse_mlsd(&line).ok()
            } else {
                parse_list_line(&line)
            };
            let f = match parsed {
                Some(f) => f,
                // MLSD es machine-readable: una línea ilegible es anómala y
                // corta el listado (no a ciegas).
                None if has_mlsd => {
                    entries.push(Err(Error::Io { retryable: false }));
                    break;
                }
                // `ls -l`: líneas no parseables (cabecera `total N`, formatos
                // raros) se descartan, no cortan.
                None => continue,
            };
            // Nombre: en MLSD se saca CRUDO de la línea (RFC 3659 `facts SP
            // pathname`); el extractor de suppaftp lo trunca en `;` y strippea
            // el espacio inicial (`split(';').last().trim_start()`). En LIST,
            // `f.name()` (parse ls -l).
            let name = if has_mlsd {
                let Some((_, n)) = line.split_once(' ') else {
                    entries.push(Err(Error::Io { retryable: false }));
                    break;
                };
                n
            } else {
                f.name()
            };
            if name == "." || name == ".." {
                continue;
            }
            // suppaftp decodifica los nombres con `from_utf8_lossy`: un byte
            // no-UTF8 llega ya sustituido por U+FFFD y no se puede recuperar →
            // rechazo LIMPIO en vez de un Entry corrupto (regla 1, ADR 0014
            // D2, issue #37). Un `/` inyectado busca escapar la base.
            if name.contains('\u{FFFD}') || name.contains('/') {
                entries.push(Err(Error::InvalidPath));
                break;
            }
            let Ok(seg) = norte_proto::Segment::new(name.as_bytes().to_vec()) else {
                entries.push(Err(Error::InvalidPath));
                break;
            };
            let child = base.join(seg);
            entries.push(Ok(entry_from_file(child, &f)));
        }
        Ok(futures::stream::iter(entries).boxed())
    }

    /// Lectura por RETR. Retiene SU conexión de control durante todo el
    /// stream (FTP no multiplexa): va por la conexión de LECTURA dedicada si
    /// existe (issue #39 B1 — así una copia mismo-host no deadlockea con el
    /// APPE de la principal). Cancelar a mitad marca la conexión para
    /// resincronizar en el siguiente uso (#39 M1). Un `range` acotado DRENA
    /// el resto del fichero hasta EOF (RETR va offset→EOF; ABOR
    /// desincronizaría) — el viewer no debe hacer reads acotados sobre FTP de
    /// ficheros enormes (issue #40).
    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        let remote = self.remote(p)?;
        let conn = self.reader.as_ref().unwrap_or(&self.ftp);
        let mut guard = lock_synced(conn).await;
        // Rechaza dirs (leerlos es error) y ausentes (NotFound): stat vía LIST
        // del padre, con la MISMA conexión bloqueada.
        match stat_remote(&mut guard, &remote, self.has_mlsd, &self.base).await? {
            None => return Err(Error::NotFound),
            Some(f) if f.is_directory() => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            Some(_) => {}
        }
        // REST offset (resume/rango): posiciona el inicio del RETR.
        if let Some(r) = range
            && r.offset > 0
        {
            let off = usize::try_from(r.offset).map_err(|_| Error::Io { retryable: false })?;
            guard.resume_transfer(off).await.map_err(|e| map_err(&e))?;
        }
        let data = guard
            .retr_as_stream(&remote)
            .await
            .map_err(|e| map_err(&e))?;
        // RETR en vuelo: si el stream se suelta sin finalizar (cancelación),
        // el flag queda puesto y el siguiente lock resincroniza (#39 M1).
        guard.pending_retr = true;
        let len = range.and_then(|r| r.len);
        Ok(read_stream::ftp_read_stream(guard, Box::new(data), len))
    }

    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        let final_remote = self.remote(p)?;
        let parent = self.remote_parent(p)?;
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let staging = format!("{parent}/{PARTIAL_PREFIX}eph.{seq}");
        let mut guard = lock_synced(&self.ftp).await;
        // El destino final no debe existir (create-new; la política de
        // sobrescritura es del core). Ventana TOCTOU documentada.
        if exists(&mut guard, &final_remote, self.has_mlsd, &self.base).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        // Crea el staging VACÍO (STOR sin datos): así un write de 0 bytes tiene
        // qué renombrar y los write() posteriores solo APPE-an. El lock se
        // suelta al volver — el sink NO lo retiene (la conexión de control es
        // única: retenerla bloquearía un stat concurrente, deadlock).
        create_empty(&mut guard, &staging).await?;
        drop(guard);
        Ok(Box::new(FtpSink {
            ftp: self.ftp(),
            staging,
            final_remote,
            has_mlsd: self.has_mlsd,
            base: self.base.clone(),
        }))
    }

    async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
        let final_remote = self.remote(p)?;
        let staging = self.stable_partial(p)?;
        let mut guard = lock_synced(&self.ftp).await;
        if exists(&mut guard, &final_remote, self.has_mlsd, &self.base).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        // Bytes ya presentes de un intento previo; si no existe el staging, se
        // crea vacío (para que los APPE de write() tengan base).
        // SIZE del parcial: NotFound = no hay parcial (créalo vacío); otro
        // error se PROPAGA (no truncar el parcial ni enmascarar un fallo real).
        let already = match guard.size(&staging).await {
            Ok(n) => n as u64,
            Err(e) => match map_err(&e) {
                Error::NotFound => {
                    create_empty(&mut guard, &staging).await?;
                    0
                }
                other => return Err(other),
            },
        };
        drop(guard);
        Ok((
            Box::new(FtpSink {
                ftp: self.ftp(),
                staging,
                final_remote,
                has_mlsd: self.has_mlsd,
                base: self.base.clone(),
            }),
            already,
        ))
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        let remote = self.remote(p)?;
        let mut guard = lock_synced(&self.ftp).await;
        if exists(&mut guard, &remote, self.has_mlsd, &self.base).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        guard.mkdir(&remote).await.map_err(|e| map_err(&e))
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        let remote = self.remote(p)?;
        let mut guard = lock_synced(&self.ftp).await;
        // Saber si es dir para elegir RMD vs DELE (stat vía LIST del padre).
        let f = stat_remote(&mut guard, &remote, self.has_mlsd, &self.base)
            .await?
            .ok_or(Error::NotFound)?;
        if f.is_directory() {
            guard.rmdir(&remote).await.map_err(|e| map_err(&e))
        } else {
            guard.rm(&remote).await.map_err(|e| map_err(&e))
        }
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        let from_r = self.remote(from)?;
        let to_r = self.remote(to)?;
        let mut guard = lock_synced(&self.ftp).await;
        // RNFR/RNTO no garantiza no-replace: se comprueba antes (TOCTOU
        // documentada) para dar `Conflict`, no pisar.
        if exists(&mut guard, &to_r, self.has_mlsd, &self.base).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        guard.rename(&from_r, &to_r).await.map_err(|e| map_err(&e))
    }
}

/// Sink de escritura sobre ftp (ADR 0014). NO retiene la conexión: cada
/// `write` hace un `APPE` de su chunk al staging (lock breve por chunk),
/// liberando la conexión de control ÚNICA entre chunks — así un `stat`
/// concurrente no se bloquea (el contrato lo exige: stat del final ANTES del
/// commit). El staging queda visible en el servidor; el destino final no
/// existe hasta el `rename` del commit.
struct FtpSink {
    ftp: Arc<Mutex<Conn>>,
    staging: String,
    final_remote: String,
    has_mlsd: bool,
    base: String,
}

#[async_trait]
impl ByteSink for FtpSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        if chunk.is_empty() {
            return Ok(());
        }
        let mut guard = lock_synced(&self.ftp).await;
        let mut data = guard
            .append_with_stream(&self.staging)
            .await
            .map_err(|e| map_err(&e))?;
        let res = data.write_all(&chunk).await;
        // Cierra la conexión de datos y lee la respuesta SIEMPRE (aunque el
        // write fallara), o el control queda desincronizado para el siguiente.
        let fin = guard.finalize_put_stream(data).await;
        res.map_err(|_| Error::Io { retryable: false })?;
        fin.map_err(|e| map_err(&e))
    }

    async fn commit(self: Box<Self>) -> Result<(), Error> {
        let mut guard = lock_synced(&self.ftp).await;
        // El destino final no debe existir (create-new): comprobado al abrir;
        // la ventana hasta aquí es TOCTOU (FTP sin rename atómico) — si aparece
        // algo, Conflict y el staging se queda para el GC.
        if exists(&mut guard, &self.final_remote, self.has_mlsd, &self.base).await? {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        guard
            .rename(&self.staging, &self.final_remote)
            .await
            .map_err(|e| map_err(&e))
    }

    async fn abort(self: Box<Self>) -> Result<(), Error> {
        // Borra el staging (cada write lo dejó durable en el servidor).
        let mut guard = lock_synced(&self.ftp).await;
        let _ = guard.rm(&self.staging).await;
        Ok(())
    }

    async fn keep(self: Box<Self>) -> Result<(), Error> {
        // El staging ya está durable en el servidor (cada write lo APPE-ó): se
        // conserva para un open_resumable posterior (ADR 0012); no se renombra
        // ni se borra. No hay nada pendiente que cerrar.
        Ok(())
    }
}

/// Productor del stream de lectura, aislado para no arrastrar genéricos al
/// método del trait. Sostiene el `OwnedMutexGuard` de SU conexión mientras
/// dura la transferencia de datos; en cada finalización limpia (EOF, drenado
/// de rango, error) limpia el `pending_retr` — si el stream se SUELTA sin
/// llegar aquí (cancelación), el flag queda puesto y el siguiente
/// [`lock_synced`] resincroniza la conexión (#39 M1).
mod read_stream {
    use super::{
        AsyncRead, AsyncReadExt, ByteStream, Bytes, Conn, Error, OwnedMutexGuard, READ_CHUNK,
        StreamExt,
    };

    type Reader = Box<dyn AsyncRead + Send + Unpin>;

    struct State {
        guard: OwnedMutexGuard<Conn>,
        reader: Reader,
        /// Bytes que aún se deben entregar (`None` = hasta EOF).
        remaining: Option<u64>,
    }

    /// Stream de chunks desde una conexión de datos RETR abierta (ya
    /// posicionada por REST). `len` acota los bytes a entregar.
    pub(super) fn ftp_read_stream(
        guard: OwnedMutexGuard<Conn>,
        reader: Reader,
        len: Option<u64>,
    ) -> ByteStream {
        let init = Some(State {
            guard,
            reader,
            remaining: len,
        });
        futures::stream::unfold(init, |state| async move {
            let mut st = state?;
            // Rango acotado ya entregado: FTP no sabe parar un RETR a media
            // (RETR va de offset a EOF). ABOR desincroniza la conexión de
            // control, así que en vez de abortar se DRENA el resto de la
            // conexión de datos y se finaliza limpio (la siguiente operación
            // encuentra la conexión sana). Coste: transferir la cola no pedida
            // — aceptable, los rangos acotados son de trozos pequeños.
            if st.remaining == Some(0) {
                drain_and_finalize(&mut st.guard, st.reader).await;
                return None;
            }
            let want = match st.remaining {
                Some(n) => usize::try_from(n.min(READ_CHUNK as u64)).unwrap_or(READ_CHUNK),
                None => READ_CHUNK,
            };
            let mut buf = vec![0u8; want];
            match st.reader.read(&mut buf).await {
                // EOF natural: cierra la conexión de datos y lee la respuesta.
                Ok(0) => {
                    let _ = st.guard.stream.finalize_retr_stream(st.reader).await;
                    st.guard.pending_retr = false;
                    None
                }
                Ok(n) => {
                    buf.truncate(n);
                    if let Some(rem) = &mut st.remaining {
                        *rem -= n as u64;
                    }
                    Some((Ok(Bytes::from(buf)), Some(st)))
                }
                Err(_) => {
                    // Conexión de datos rota: finaliza best-effort y corta.
                    let _ = st.guard.stream.finalize_retr_stream(st.reader).await;
                    st.guard.pending_retr = false;
                    Some((Err(Error::Io { retryable: false }), None))
                }
            }
        })
        .boxed()
    }

    /// Lee y descarta el resto de la conexión de datos hasta EOF, luego
    /// finaliza (deja la conexión de control limpia para la siguiente op).
    async fn drain_and_finalize(guard: &mut Conn, mut reader: Reader) {
        let mut scratch = vec![0u8; 8192];
        loop {
            match reader.read(&mut scratch).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = guard.stream.finalize_retr_stream(reader).await;
        guard.pending_retr = false;
    }
}
