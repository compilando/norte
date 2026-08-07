//! Guest WASM (#30 stage 3c): el provider FTP COMPLETO. Proyección SÍNCRONA del
//! trait `norte_vfs::Provider` sobre `suppaftp::FtpStream` (sync) por
//! `wasi:sockets`. Port de `norte-vfs-ftp` con las MISMAS defensas
//! anti-inyección CR/LF y el mismo tratamiento MLSD/LIST. Sin TLS (FTPS=deuda:
//! aws-lc-rs no compila a wasm32-wasip2).
//!
//! Single-threaded wasm: la conexión de control vive en un `thread_local`
//! `RefCell<Option<Session>>`, no en `Arc<Mutex>`. Nombres crudos en bytes
//! (regla 1); FTP exige UTF-8 → un nombre no representable es `invalid-path`.
//!
//! LECTURA (#30 M1): la interfaz WIT `read(segs, offset, len)` es acotada, pero el
//! guest CACHEA el `DataStream` del RETR en la sesión y lo reutiliza mientras las
//! lecturas sean secuenciales (offset = fin del chunk anterior) → un solo RETR por
//! fichero, O(n). Cualquier otra op (o un offset no secuencial) drena y finaliza la
//! caché ANTES de emitir su comando de control (`flush_cached_read`), así el `226`
//! pendiente jamás se intercala. DEUDA (timeout/cancelación, ADR 0033): una lectura
//! bloqueada en el socket no la corta el epoch deadline (solo traba código guest),
//! y el hilo `spawn_blocking` del host queda retenido — mitigación futura:
//! `tokio::time::timeout` en el adapter.

use std::cell::RefCell;
use std::io::Write;

use suppaftp::list::{File, ListParser};
use suppaftp::types::FileType;
use suppaftp::{FtpError, FtpStream, Status};

wit_bindgen::generate!({
    world: "norte:provider/norte-provider",
    path: "wit",
    // `host-log`/`host-config` viven en OTRO paquete desde la partición
    // (ADR 0041 decisión 4); wit-bindgen exige decidir explícitamente qué
    // hacer con los imports de fuera del paquete del world.
    generate_all,
});

use exports::norte::provider::provider::{
    Caps, Entry, EntryKind, Guest, GuestWriter, Page, ProviderConfig, VfsError, Writer,
};

/// Cota defensiva de entradas materializadas por listado (issue #40). El bound
/// real contra OOM es upstream (suppaftp bufferiza las líneas).
const MAX_LIST_ENTRIES: usize = 1 << 20;
/// Prefijo del staging de escritura (ADR 0012, mismo convenio que local/sftp).
const PARTIAL_PREFIX: &str = ".norte-partial.";

/// Sesión FTP establecida: conexión de control + estado de la raíz remota.
struct Session {
    ftp: FtpStream,
    /// Raíz remota absoluta bajo la que vive todo. Sin `..`, sin barra final.
    base: String,
    /// El servidor soporta MLSD/MLST (machine-readable). Si no, se degrada a
    /// `LIST` (`ls -l`), universal pero frágil con nombres hostiles (ADR 0014 C).
    has_mlsd: bool,
    /// Contador de staging (nombre efímero único para `open_writer`).
    seq: u64,
    /// RETR en curso reutilizable entre lecturas secuenciales (#30 M1): evita el
    /// re-RETR por chunk (O(n²)→O(n)). `None` = sin lectura en vuelo.
    cached_read: Option<CachedRead>,
}

/// Un RETR vivo cacheado: la conexión de DATOS + el path y el siguiente offset
/// que entregará. El `reader` es independiente del control; se drena y finaliza
/// vía [`flush_cached_read`] ANTES de cualquier comando de control, para que el
/// `226` pendiente jamás se intercale.
struct CachedRead {
    remote: String,
    next_offset: u64,
    reader: Box<dyn std::io::Read>,
}

/// Metadatos mínimos de una entrada, agnósticos del backend de parse (MLSD
/// self-parse o `ls -l` de suppaftp). Reemplaza el `File` de suppaftp en la
/// superficie de [`stat_remote`] para que el size sea u64 (no el `usize` de
/// suppaftp, techo 4 GiB en wasm32, #30 H2).
struct StatEntry {
    kind: EntryKind,
    size: Option<u64>,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Ejecuta `f` con la sesión establecida, o `provider-unavailable` si
/// `configure` no se llamó (o falló). El wasm es single-threaded: el
/// `borrow_mut` jamás se solapa.
fn with_session<T>(f: impl FnOnce(&mut Session) -> Result<T, VfsError>) -> Result<T, VfsError> {
    SESSION.with_borrow_mut(|s| match s.as_mut() {
        Some(sess) => f(sess),
        None => Err(VfsError::ProviderUnavailable),
    })
}

/// Drena y finaliza el RETR cacheado (si hay), dejando el control LIMPIO para el
/// siguiente comando. Best-effort e idempotente (`None` = no-op). Todo op que
/// emita un comando de control lo llama ANTES (invariante #30 M1: jamás un
/// comando con un `226` pendiente en el control).
///
/// COSTE (deuda, ADR 0033): el drenado va hasta EOF de la conexión de datos, así
/// que abandonar una lectura de un fichero grande hace que la SIGUIENTE op pague
/// transferir la cola no leída; y un servidor hostil que streamee sin fin cuelga
/// el hilo `spawn_blocking` del host (el epoch deadline no traba I/O de socket).
/// Igual que la deuda de timeout/cancelación; el fix limpio sería `ABOR` o
/// reconectar el control. El `8192`-scratch acota la MEMORIA, no el total.
fn flush_cached_read(s: &mut Session) {
    let Some(mut cr) = s.cached_read.take() else {
        return;
    };
    use std::io::Read;
    // Drena el resto de la conexión de datos (RETR va offset→EOF; parar sin
    // drenar desincronizaría el control), luego lee la respuesta de transferencia.
    let mut scratch = [0u8; 8192];
    loop {
        match cr.reader.read(&mut scratch) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let _ = s.ftp.finalize_retr_stream(cr.reader);
}

struct FtpProvider;

impl Guest for FtpProvider {
    fn configure(cfg: ProviderConfig) -> Result<(), VfsError> {
        // `base` es config de confianza (el host la fija), pero se valida como
        // defensa en profundidad: absoluta y sin CR/LF/NUL (que inyectarían un
        // comando FTP en cada op, saltándose el filtro por-segmento).
        let mut base = cfg.base;
        while base.len() > 1 && base.ends_with('/') {
            base.pop();
        }
        if !base.starts_with('/') || base.contains(['\r', '\n', '\0']) {
            return Err(VfsError::InvalidPath);
        }
        // El endpoint YA es `ip:puerto` numérico (el host resolvió DNS): connect
        // no resuelve hostnames (el guest no tiene DNS).
        let mut ftp =
            FtpStream::connect(cfg.endpoint.as_str()).map_err(|_| VfsError::ProviderUnavailable)?;
        ftp.login(cfg.user.as_str(), cfg.password.as_str())
            .map_err(|e| map_err(&e))?;
        let has_mlsd = setup_conn(&mut ftp).map_err(|e| map_err(&e))?;
        SESSION.set(Some(Session {
            ftp,
            base,
            has_mlsd,
            seq: 0,
            cached_read: None,
        }));
        Ok(())
    }

    fn capabilities() -> Caps {
        // Honestas (ADR 0014): remoto POSIX case-sensitive y case-preserving.
        // NO declara symlinks/trash/server-copy (el adapter los mapea a ausente
        // → Unsupported), ni resume (el adapter usa el open_resumable por
        // defecto, que no reanuda). Sin READ_ONLY (escribible).
        Caps {
            read_only: false,
            case_sensitive: true,
            case_preserving: true,
        }
    }

    fn stat(segments: Vec<Vec<u8>>) -> Result<Entry, VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            // La raíz del provider es el directorio base (no tiene padre a listar).
            if segments.is_empty() {
                return Ok(Entry {
                    name: Vec::new(),
                    kind: EntryKind::Dir,
                    size: None,
                });
            }
            let remote = remote(&s.base, &segments)?;
            match stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                Some(st) => Ok(Entry {
                    name: last_name(&segments),
                    kind: st.kind,
                    size: st.size,
                }),
                None => Err(VfsError::NotFound),
            }
        })
    }

    fn list_dir(segments: Vec<Vec<u8>>, _cursor: Option<Vec<u8>>) -> Result<Page, VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            let remote = remote(&s.base, &segments)?;
            let lines = if s.has_mlsd {
                s.ftp.mlsd(Some(remote.as_str()))
            } else {
                s.ftp.list(Some(remote.as_str()))
            }
            .map_err(|e| map_err(&e))?;
            let mut entries = Vec::new();
            // suppaftp decodifica los nombres con `from_utf8_lossy`: un byte no-UTF8
            // llega ya sustituido por U+FFFD e irrecuperable → rechazo LIMPIO (regla
            // 1, ADR 0014 D2). Un `/` o NUL inyectados por un servidor hostil buscan
            // escapar/truncar la ruta: se falla la página LOUD (encoding M2).
            let reject = |name: &str| name.contains('\u{FFFD}') || name.contains(['/', '\0']);
            for line in lines {
                // Cota defensiva de nuestra materialización (issue #40).
                if entries.len() >= MAX_LIST_ENTRIES {
                    return Err(VfsError::Io);
                }
                // Ramas separadas para que cada `name` sea dueño de su lifetime
                // (el `f.name()` de LIST toma prestado de `f`, que muere al salir).
                if s.has_mlsd {
                    // MLSD self-parse (#30 H2): (kind, size u64, nombre crudo).
                    let Some((kind, size, name)) = parse_mlsd_facts(&line) else {
                        // MLSD machine-readable: una línea ilegible es anómala.
                        return Err(VfsError::Io);
                    };
                    if name == "." || name == ".." {
                        continue;
                    }
                    if reject(name) {
                        return Err(VfsError::InvalidPath);
                    }
                    entries.push(Entry {
                        name: name.as_bytes().to_vec(),
                        kind,
                        size,
                    });
                } else {
                    // `ls -l`: líneas no parseables (cabecera `total N`) se descartan.
                    let Some(f) = parse_list_line(&line) else {
                        continue;
                    };
                    let name = f.name();
                    if name == "." || name == ".." {
                        continue;
                    }
                    if reject(name) {
                        return Err(VfsError::InvalidPath);
                    }
                    let st = stat_entry_from_file(&f);
                    entries.push(Entry {
                        name: name.as_bytes().to_vec(),
                        kind: st.kind,
                        size: st.size,
                    });
                }
            }
            Ok(Page {
                entries,
                next_cursor: None,
            })
        })
    }

    fn read(segments: Vec<Vec<u8>>, offset: u64, len: u64) -> Result<Vec<u8>, VfsError> {
        use std::io::Read;
        with_session(|s| {
            let remote = remote(&s.base, &segments)?;
            // Reusa el RETR cacheado si casa el path Y el offset secuencial (#30
            // M1). Si no casa (o no hay), finaliza el anterior y abre uno nuevo.
            let hit = s
                .cached_read
                .as_ref()
                .is_some_and(|cr| cr.remote == remote && cr.next_offset == offset);
            if !hit {
                flush_cached_read(s);
                // Rechaza dir (leerlo es error) y ausente (NotFound).
                match stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                    None => return Err(VfsError::NotFound),
                    Some(st) if st.kind == EntryKind::Dir => return Err(VfsError::Conflict),
                    Some(_) => {}
                }
                // REST offset (resume/rango): posiciona el inicio del RETR.
                if offset > 0 {
                    let off = usize::try_from(offset).map_err(|_| VfsError::Io)?;
                    s.ftp.resume_transfer(off).map_err(|e| map_err(&e))?;
                }
                let reader = s
                    .ftp
                    .retr_as_stream(remote.as_str())
                    .map_err(|e| map_err(&e))?;
                s.cached_read = Some(CachedRead {
                    remote: remote.clone(),
                    next_offset: offset,
                    reader: Box::new(reader),
                });
            }
            // Lee hasta `want` bytes del reader cacheado. Se lee DIRECTO sobre un
            // buffer del tamaño pedido (no un buf fijo + truncado): así jamás se
            // saca del socket más de lo pedido, que corromperia la siguiente
            // lectura secuencial (perdería esos bytes de su ventana).
            let want = usize::try_from(len).unwrap_or(usize::MAX);
            // Techo por si `want == u64::MAX` (el adapter pide 64 KiB; jamás pica).
            let cap = want.min(1 << 20);
            // El hit reusa la caché; el miss la instaló justo arriba — en ambos
            // casos `cached_read` es Some. El else es inalcanzable; se trata como
            // Io en vez de panicar (regla 6, sin `expect`).
            let Some(cr) = s.cached_read.as_mut() else {
                return Err(VfsError::Io);
            };
            let mut out = vec![0u8; cap];
            let mut filled = 0usize;
            let mut eof = false;
            let mut read_err = false;
            while filled < cap {
                // Un error NO puede hacer `?` (saltaría el flush → 226 pendiente,
                // rust review B1). Se marca y se finaliza fuera del bucle.
                match cr.reader.read(&mut out[filled..]) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => filled += n,
                    Err(_) => {
                        read_err = true;
                        break;
                    }
                }
            }
            out.truncate(filled);
            cr.next_offset += filled as u64;
            // EOF o error: finaliza la caché (limpio, o best-effort en error).
            if eof || read_err {
                flush_cached_read(s);
            }
            if read_err {
                return Err(VfsError::Io);
            }
            Ok(out)
        })
    }

    // ---- escritura ----

    type Writer = FtpWriter;

    fn open_writer(segments: Vec<Vec<u8>>) -> Result<Writer, VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            let final_remote = remote(&s.base, &segments)?;
            let parent_len = segments.len().saturating_sub(1);
            let parent = remote(&s.base, &segments[..parent_len])?;
            // El destino final no debe existir (create-new; la política de
            // sobrescritura es del core). Ventana TOCTOU documentada.
            if exists(&mut s.ftp, &final_remote, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            let seq = s.seq;
            s.seq += 1;
            let staging = format!("{parent}/{PARTIAL_PREFIX}eph.{seq}");
            // Crea el staging VACÍO (STOR sin datos): un write de 0 bytes tiene
            // qué renombrar y los write() posteriores solo APPE-an.
            create_empty(&mut s.ftp, &staging)?;
            Ok(Writer::new(FtpWriter {
                staging: RefCell::new(Some(staging)),
                final_remote,
            }))
        })
    }

    fn make_dir(segments: Vec<Vec<u8>>) -> Result<(), VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            let remote = remote(&s.base, &segments)?;
            if exists(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            s.ftp.mkdir(&remote).map_err(|e| map_err(&e))
        })
    }

    fn remove(segments: Vec<Vec<u8>>) -> Result<(), VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            let remote = remote(&s.base, &segments)?;
            // Saber si es dir para elegir RMD vs DELE.
            let st =
                stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)?.ok_or(VfsError::NotFound)?;
            if st.kind == EntryKind::Dir {
                s.ftp.rmdir(&remote).map_err(|e| map_err(&e))
            } else {
                s.ftp.rm(&remote).map_err(|e| map_err(&e))
            }
        })
    }

    fn rename(src: Vec<Vec<u8>>, dst: Vec<Vec<u8>>) -> Result<(), VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            let from_r = remote(&s.base, &src)?;
            let to_r = remote(&s.base, &dst)?;
            // RNFR/RNTO no garantiza no-replace: se comprueba antes (TOCTOU
            // documentada) para dar Conflict, no pisar.
            if exists(&mut s.ftp, &to_r, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            s.ftp.rename(&from_r, &to_r).map_err(|e| map_err(&e))
        })
    }
}

/// Writer transaccional sobre ftp (ADR 0014): cada `write` hace un `APPE` de su
/// chunk al staging; `commit` renombra staging→final; `abort` borra el staging.
/// El estado (path de staging) vive tras `RefCell` porque los métodos WIT del
/// recurso toman `&self`. La conexión se alcanza vía el `thread_local`.
struct FtpWriter {
    /// `Some` mientras no se haya publicado/descartado.
    staging: RefCell<Option<String>>,
    final_remote: String,
}

impl GuestWriter for FtpWriter {
    fn write(&self, chunk: Vec<u8>) -> Result<(), VfsError> {
        if chunk.is_empty() {
            return Ok(());
        }
        let staging = self.staging.borrow().clone().ok_or(VfsError::Io)?;
        with_session(|s| {
            flush_cached_read(s);
            let mut data = s
                .ftp
                .append_with_stream(&staging)
                .map_err(|e| map_err(&e))?;
            let res = data.write_all(&chunk);
            // Cierra la conexión de datos y lee la respuesta SIEMPRE (aunque el
            // write fallara), o el control queda desincronizado.
            let fin = s.ftp.finalize_put_stream(data);
            res.map_err(|_| VfsError::Io)?;
            fin.map_err(|e| map_err(&e))
        })
    }

    fn commit(&self) -> Result<(), VfsError> {
        let staging = self.staging.borrow_mut().take().ok_or(VfsError::Io)?;
        with_session(|s| {
            flush_cached_read(s);
            // El destino final no debe existir (create-new): comprobado al abrir;
            // la ventana hasta aquí es TOCTOU (FTP sin rename atómico).
            if exists(&mut s.ftp, &self.final_remote, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            s.ftp
                .rename(&staging, &self.final_remote)
                .map_err(|e| map_err(&e))
        })
    }

    fn abort(&self) -> Result<(), VfsError> {
        // Borra el staging (cada write lo dejó durable en el servidor).
        if let Some(staging) = self.staging.borrow_mut().take() {
            let _ = with_session(|s| {
                flush_cached_read(s);
                let _ = s.ftp.rm(&staging);
                Ok(())
            });
        }
        Ok(())
    }
}

/// Path remoto absoluto bajo `base` desde segmentos crudos. MISMAS defensas que
/// el provider nativo: UTF-8 exigido, sin `/`/`.`/`..`, sin CR/LF, ≤255 bytes.
///
/// FTP es un protocolo de LÍNEAS (comando terminado en CRLF): un nombre con
/// CR/LF inyectaría un comando FTP arbitrario (`STOR path\r\nDELE víctima`) — se
/// rechaza. Contención ESPECÍFICA de FTP.
fn remote(base: &str, segments: &[Vec<u8>]) -> Result<String, VfsError> {
    let mut out = String::from(base);
    for seg in segments {
        let name = std::str::from_utf8(seg).map_err(|_| VfsError::InvalidPath)?;
        // Un segmento vacío haría un path con `//` que aliasa al padre (encoding
        // M1); `/`/`.`/`..` escaparían la base. El `Segment` del host ya los
        // rechaza, pero el guest revalida (bytes crudos en el WIT).
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(VfsError::InvalidPath);
        }
        // CR/LF inyectarían un comando FTP; NUL trunca paths en servidores en C.
        // El `Segment` del host ya rechaza NUL, pero el guest revalida (la
        // interfaz WIT cruza bytes crudos): defensa en profundidad, misma que
        // `configure` aplica a `base` (rust review m2 / security LOW).
        if name.contains(['\r', '\n', '\0']) {
            return Err(VfsError::InvalidPath);
        }
        // NAME_MAX: la mayoría de FS rechazan >255 bytes con ENAMETOOLONG; el
        // servidor fallaría a media op con un 550 ambiguo. Se rechaza LIMPIO.
        if seg.len() > 255 {
            return Err(VfsError::InvalidPath);
        }
        if out.len() > 1 || !out.ends_with('/') {
            out.push('/');
        }
        out.push_str(name);
    }
    Ok(out)
}

/// El último segmento (nombre) de un path, o vacío para la raíz.
fn last_name(segments: &[Vec<u8>]) -> Vec<u8> {
    segments.last().cloned().unwrap_or_default()
}

/// Prepara una conexión recién logueada: BINARIO (ASCII corrompe binarios),
/// detección de MLSD/MLST y `OPTS UTF8 ON` si el servidor lo anuncia (RFC 2640,
/// ADR 0014 D2; best-effort). Devuelve si hay MLSD.
fn setup_conn(ftp: &mut FtpStream) -> Result<bool, FtpError> {
    ftp.transfer_type(FileType::Binary)?;
    let feats = ftp.feat().ok();
    let has_mlsd = feats.as_ref().is_some_and(|f| {
        f.keys()
            .any(|k| k.eq_ignore_ascii_case("MLST") || k.eq_ignore_ascii_case("MLSD"))
    });
    if feats
        .as_ref()
        .is_some_and(|f| f.keys().any(|k| k.eq_ignore_ascii_case("UTF8")))
    {
        let _ = ftp.opts("UTF8", Some("ON"));
    }
    Ok(has_mlsd)
}

/// Mapea el error de suppaftp a la taxonomía WIT `vfs-error` (spec §17.7).
fn map_err(e: &FtpError) -> VfsError {
    match e {
        FtpError::UnexpectedResponse(r) => match r.status {
            // 550 es ambiguo en FTP (no existe / sin permiso): NotFound es el
            // caso común y el que el contrato espera para paths ausentes.
            Status::FileUnavailable => VfsError::NotFound,
            Status::NotLoggedIn => VfsError::PermissionDenied,
            Status::BadFilename => VfsError::InvalidPath,
            // El flag `retryable` de la taxonomía proto no cruza la interfaz WIT
            // (enum cerrado): 450 y el resto caen a `io`.
            _ => VfsError::Io,
        },
        // `SecureError` está gateado por la feature TLS (deshabilitada: FTPS es
        // deuda) — no existe en esta compilación.
        FtpError::ConnectionError(_) => VfsError::ProviderUnavailable,
        FtpError::InvalidAddress(_) => VfsError::InvalidPath,
        FtpError::BadResponse | FtpError::DataConnectionAlreadyOpen => VfsError::Io,
    }
}

/// Parsea una línea de `LIST` (`ls -l` POSIX, con respaldo DOS). `None` para
/// líneas no parseables (cabeceras `total N`): se descartan.
fn parse_list_line(line: &str) -> Option<File> {
    ListParser::parse_posix(line)
        .ok()
        .or_else(|| ListParser::parse_dos(line).ok())
}

/// Parsea una línea MLSD/MLST (RFC 3659: `[facts] SP pathname`). Devuelve
/// `(kind, size, raw_name)`: `kind` del fact `type`, `size` como u64 (`None` si
/// el fact `size` falta o no es numérico — un dir lo omite), `raw_name` tras el
/// PRIMER espacio (crudo; el caller aplica el rechazo U+FFFD/`/`/NUL). Reemplaza
/// a `ListParser::parse_mlsd`/`parse_mlst` de suppaftp, que parsea el size a
/// `usize` (techo 4 GiB en wasm32) y truncaba el nombre en `;` (#30 H2). `None`
/// si la línea no tiene la forma `facts SP name` (sin espacio, o nombre vacío).
fn parse_mlsd_facts(line: &str) -> Option<(EntryKind, Option<u64>, &str)> {
    let (facts, name) = line.split_once(' ')?;
    if name.is_empty() {
        return None;
    }
    let mut kind = EntryKind::File; // default si falta `type`
    let mut size = None;
    for fact in facts.split(';') {
        let Some((key, value)) = fact.split_once('=') else {
            continue;
        };
        if key.eq_ignore_ascii_case("type") {
            kind = match value.to_ascii_lowercase().as_str() {
                "dir" | "cdir" | "pdir" => EntryKind::Dir,
                "file" => EntryKind::File,
                "link" => EntryKind::Symlink,
                _ => EntryKind::Other,
            };
        } else if key.eq_ignore_ascii_case("size") {
            size = value.parse::<u64>().ok();
        }
    }
    Some((kind, size, name))
}

/// Nombre de una línea `ls -l` de forma TOLERANTE, SÓLO para la salvaguarda
/// anti-overwrite (nunca como nombre real): `perms links owner group size mon day
/// time name` → el nombre es todo tras el 8º campo separado por whitespace.
/// `None` si la línea tiene <9 campos (p. ej. una cabecera `total N`). Los
/// nombres con espacio inicial se pierden (límite conocido de `ls -l`): eso deja
/// UN hueco en la salvaguarda anti-overwrite — un fichero ≥4 GiB con nombre de
/// espacio inicial en un servidor SIN MLSD no casaría `child` y podría
/// sobrescribirse. Intersección de 3 precondiciones raras + inherente a `ls -l`
/// (suppaftp lo pierde igual); el fix real es MLSD (que sí preserva el espacio).
fn ls_l_name(line: &str) -> Option<&str> {
    // Salta 8 campos (cada uno = token + su whitespace siguiente).
    let mut rest = line;
    for _ in 0..8 {
        let trimmed = rest.trim_start();
        let end = trimmed.find(char::is_whitespace)?;
        rest = &trimmed[end..];
    }
    let name = rest.trim_start();
    if name.is_empty() { None } else { Some(name) }
}

/// `StatEntry` desde un `File` de suppaftp (rama LIST; el size sale del `usize`
/// de suppaftp — límite 4 GiB aceptado para servidores SIN MLSD).
fn stat_entry_from_file(f: &File) -> StatEntry {
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
    StatEntry { kind, size }
}

/// `stat` de `remote`: con MLSD `MLST` directo (self-parse, size u64); sin MLSD,
/// `LIST` del DIRECTORIO padre + búsqueda por nombre (universal — pure-ftpd; ADR
/// 0014 C). `None` = no existe.
fn stat_remote(
    ftp: &mut FtpStream,
    remote: &str,
    has_mlsd: bool,
    base: &str,
) -> Result<Option<StatEntry>, VfsError> {
    if has_mlsd {
        return match ftp.mlst(Some(remote)) {
            Ok(line) => match parse_mlsd_facts(&line) {
                Some((kind, size, _name)) => Ok(Some(StatEntry { kind, size })),
                None => Err(VfsError::Io), // MLST ilegible = anómalo
            },
            Err(e) => match map_err(&e) {
                VfsError::NotFound => Ok(None),
                other => Err(other),
            },
        };
    }
    // Contención (security): la rama LIST lista el DIRECTORIO PADRE. Para la RAÍZ
    // del provider (`remote == base`) el padre estaría FUERA de la base — jamás
    // se lista por encima. La raíz es un dir degenerado: None (fail-safe).
    if remote == base {
        return Ok(None);
    }
    let (parent, child) = match remote.rfind('/') {
        Some(0) => ("/", &remote[1..]),
        Some(i) => (&remote[..i], &remote[i + 1..]),
        None => return Ok(None),
    };
    if child.is_empty() {
        return Ok(None);
    }
    let lines = match ftp.list(Some(parent)) {
        Ok(l) => l,
        Err(e) => {
            return match map_err(&e) {
                VfsError::NotFound => Ok(None),
                other => Err(other),
            };
        }
    };
    for line in lines {
        match parse_list_line(&line) {
            Some(f) => {
                // El nombre del servidor viene lossy: un no-UTF8 (U+FFFD) jamás
                // casa un `child` UTF-8 de forma fiable → se salta, no se compara.
                let n = f.name();
                if n.contains('\u{FFFD}') {
                    continue;
                }
                if n == child {
                    return Ok(Some(stat_entry_from_file(&f)));
                }
            }
            // Salvaguarda anti-overwrite (#30 H2): una línea `ls -l` que NO parsea
            // (p. ej. size ≥ 4 GiB rompe el parse `usize` de suppaftp) pero cuyo
            // nombre casa `child` NO se descarta en silencio — se falla LOUD, así
            // `exists()` no dice "no existe" y write/rename/mkdir no sobrescriben
            // un fichero invisible. Cabeceras `total N` (ls_l_name=None) siguen
            // descartándose.
            None => {
                if let Some(n) = ls_l_name(&line) {
                    if !n.contains('\u{FFFD}') && n == child {
                        return Err(VfsError::Io);
                    }
                }
            }
        }
    }
    Ok(None)
}

/// ¿Existe `remote`? (vía `stat_remote`.)
fn exists(ftp: &mut FtpStream, remote: &str, has_mlsd: bool, base: &str) -> Result<bool, VfsError> {
    Ok(stat_remote(ftp, remote, has_mlsd, base)?.is_some())
}

/// Crea `remote` como fichero VACÍO (STOR sin datos): base para APPE-ar.
fn create_empty(ftp: &mut FtpStream, remote: &str) -> Result<(), VfsError> {
    let data = ftp.put_with_stream(remote).map_err(|e| map_err(&e))?;
    ftp.finalize_put_stream(data).map_err(|e| map_err(&e))
}

export!(FtpProvider);

#[cfg(test)]
mod parse_tests {
    use super::{EntryKind, ls_l_name, parse_mlsd_facts};

    #[test]
    fn mlsd_facts_size_u64_beyond_4gib() {
        // 5 GiB = 5368709120 > u32::MAX: suppaftp lo rompía; aquí es u64 exacto.
        let line = "type=file;size=5368709120;modify=20200101000000; big.bin";
        let (kind, size, name) = parse_mlsd_facts(line).expect("parsea");
        assert_eq!(kind, EntryKind::File);
        assert_eq!(size, Some(5_368_709_120));
        assert_eq!(name, "big.bin");
    }

    #[test]
    fn mlsd_facts_dir_without_size() {
        let (kind, size, name) =
            parse_mlsd_facts("type=dir;modify=20200101000000; sub").expect("dir");
        assert_eq!(kind, EntryKind::Dir);
        assert_eq!(size, None);
        assert_eq!(name, "sub");
    }

    #[test]
    fn mlsd_facts_name_with_semicolon_survives() {
        // El nombre va tras el PRIMER espacio: un `;` en el nombre NO lo trunca.
        let (_, _, name) = parse_mlsd_facts("type=file;size=1; a;b.txt").expect("parsea");
        assert_eq!(name, "a;b.txt");
    }

    #[test]
    fn mlsd_facts_missing_type_defaults_file_and_unknown_is_other() {
        assert_eq!(parse_mlsd_facts("size=1; f").unwrap().0, EntryKind::File);
        assert_eq!(parse_mlsd_facts("type=cdir; .").unwrap().0, EntryKind::Dir);
        assert_eq!(
            parse_mlsd_facts("type=slink; x").unwrap().0,
            EntryKind::Other
        );
    }

    #[test]
    fn mlsd_facts_rejects_malformed() {
        assert!(parse_mlsd_facts("no-space-no-name").is_none());
        assert!(parse_mlsd_facts("type=file;size=1; ").is_none()); // nombre vacío
    }

    #[test]
    fn ls_l_name_extracts_after_eight_fields() {
        let n = ls_l_name("-rw-r--r-- 1 owner group 5368709120 Jan 12 10:00 big.bin");
        assert_eq!(n, Some("big.bin"));
        let n2 = ls_l_name("-rw-r--r-- 1 o g 5 Jan 12 10:00 con espacios.txt");
        assert_eq!(n2, Some("con espacios.txt"));
    }

    #[test]
    fn ls_l_name_rejects_header_and_short() {
        assert_eq!(ls_l_name("total 8"), None);
        assert_eq!(ls_l_name(""), None);
    }
}
