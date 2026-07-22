//! Adapter [`PluginProvider`] (#30 stage 2b, ADR 0032): expone un
//! guest-provider WASM como un [`norte_vfs::Provider`] normal. REENSAMBLA los
//! streams del trait a partir de las llamadas ACOTADAS del guest — `list`
//! paginando hasta agotar el cursor, `read` leyendo por rango hasta EOF. Las
//! mutaciones se DELEGAN al guest: `write` proyecta el [`ByteSink`]
//! transaccional sobre el `writer` resource del guest (staging → commit/abort),
//! y `mkdir`/`remove`/`rename` llaman a sus funciones. Un guest read-only
//! responde [`Error::Unsupported`] en todas y el adapter lo propaga; `trash`/
//! `symlink` no están en la interfaz WIT → Unsupported directo.
//!
//! Cada llamada al guest es SÍNCRONA (wasmtime) y serializada por un `Mutex`;
//! se ejecuta en `spawn_blocking` para no bloquear el executor async (regla 2).
//! El `PluginProvider` mantiene vivo el [`PluginRuntime`] (su ticker de época
//! gobierna el deadline de CPU del guest).

use std::path::Path;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::stream::{self, StreamExt};
use norte_plugin_host::{
    Capabilities as HostCaps, PluginRuntime, ProviderInstance, RuntimeError, provider_iface,
};
use norte_vfs::proto::{
    ByteRange, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, Segment, VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider, SymlinkKind};

/// Un provider VFS respaldado por un plugin WASM que exporta la interfaz WIT
/// `provider` (#30). Ver el módulo.
pub struct PluginProvider {
    /// Mantiene vivo el ticker de época (deadline de CPU del guest).
    _runtime: PluginRuntime,
    /// La instancia del guest; el `Mutex` serializa sus llamadas síncronas.
    inst: Arc<Mutex<ProviderInstance>>,
    /// Scheme que sirve este provider (p. ej. `mem`, `ftp`).
    scheme: String,
    /// Capabilities cacheadas al construir (`Provider::capabilities` es sync).
    caps: Capabilities,
}

impl PluginProvider {
    /// Instancia el guest `wasm` bajo `host_caps` y cachea sus capabilities.
    ///
    /// # Errors
    /// [`RuntimeError`] si el artefacto no instancia o la llamada a
    /// `capabilities` atrapa.
    pub fn new(
        runtime: PluginRuntime,
        wasm: &Path,
        host_caps: HostCaps,
        scheme: impl Into<String>,
    ) -> Result<Self, RuntimeError> {
        let mut inst = runtime.instantiate_provider(wasm, host_caps)?;
        let guest = inst.capabilities()?;
        let flags = if guest.read_only {
            CapabilityFlags::READ_ONLY
        } else {
            CapabilityFlags::empty()
        };
        Ok(Self {
            _runtime: runtime,
            inst: Arc::new(Mutex::new(inst)),
            scheme: scheme.into(),
            caps: Capabilities {
                flags,
                max_path: None,
            },
        })
    }

    /// Los segmentos crudos de `p` (el path que entiende el guest). El root del
    /// provider es scheme-only, así que los segmentos del `VPath` son el path.
    /// La authority (si la hubiera) NO se proyecta — el root es scheme-only por
    /// contrato; se afirma en debug.
    fn segments(p: &VPath) -> Vec<Vec<u8>> {
        debug_assert!(
            p.authority().is_none(),
            "PluginProvider asume un root scheme-only (sin authority)"
        );
        p.segments().map(<[u8]>::to_vec).collect()
    }

    /// Ejecuta una llamada al guest en `spawn_blocking` (regla 2), serializada
    /// por el `Mutex`. Un `JoinError` (panic del hilo bloqueante) se mapea a
    /// `Internal`.
    async fn call<T, F>(&self, f: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&mut ProviderInstance) -> Result<T, Error> + Send + 'static,
    {
        let inst = Arc::clone(&self.inst);
        tokio::task::spawn_blocking(move || {
            let mut guard = inst.lock().map_err(|_| Error::Internal { panic: true })?;
            f(&mut guard)
        })
        .await
        .map_err(|_| Error::Internal { panic: true })?
    }
}

/// Traduce el error lógico del guest a la taxonomía del protocolo. `other` y
/// `other` cae a `Internal` no-panic (categoría gruesa, sin inventar detalle);
/// `conflict`/`no-space` (alcanzables en el camino de escritura) se mapean fiel.
fn map_vfs_error(e: provider_iface::VfsError) -> Error {
    use provider_iface::VfsError as V;
    match e {
        V::NotFound => Error::NotFound,
        V::PermissionDenied => Error::PermissionDenied,
        V::Unsupported => Error::Unsupported,
        V::InvalidPath => Error::InvalidPath,
        V::Io => Error::Io { retryable: false },
        V::Corrupt => Error::Corrupt,
        V::CursorExpired => Error::CursorExpired,
        V::ProviderUnavailable => Error::ProviderUnavailable { retryable: false },
        V::Loop => Error::Loop,
        V::Conflict => Error::Conflict {
            conflict: ConflictKind::Unknown,
        },
        V::NoSpace => Error::NoSpace,
        V::Other => Error::Internal { panic: false },
    }
}

/// Fallo del runtime del guest. Solo un TRAP es panic-clase (el guest crasheó);
/// un rechazo controlado (tope de retorno, deadline, instanciación) es un fallo
/// interno NO-panic.
fn map_runtime_error(e: &RuntimeError) -> Error {
    Error::Internal {
        panic: matches!(e, RuntimeError::Trap(_)),
    }
}

/// Tope de entradas que el adapter reensambla de un `list` antes de fallar
/// fail-loud — un guest hostil no cuelga el host paginando sin fin.
const MAX_LIST_ENTRIES: usize = 1_000_000;

fn map_kind(k: provider_iface::EntryKind) -> EntryKind {
    use provider_iface::EntryKind as K;
    match k {
        K::File => EntryKind::File,
        K::Dir => EntryKind::Dir,
        K::Symlink => EntryKind::Symlink,
        K::Other => EntryKind::Other,
    }
}

#[async_trait::async_trait]
impl Provider for PluginProvider {
    fn scheme(&self) -> &str {
        &self.scheme
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        let segs = Self::segments(p);
        let path = p.clone();
        self.call(move |g| {
            let e = g
                .stat(&segs)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error)?;
            Ok(Entry {
                path,
                kind: map_kind(e.kind),
                size: e.size,
                mtime_ms: None,
            })
        })
        .await
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        let segs = Self::segments(p);
        let dir = p.clone();
        // Reensamblado EAGER: se agotan todas las páginas del cursor y se
        // devuelve un stream sobre el Vec (stage 2 de-risk; lazy = optimización
        // posterior). El árbol de un contrato es pequeño.
        let entries: Vec<Entry> = self
            .call(move |g| {
                let mut out = Vec::new();
                let mut cursor: Option<Vec<u8>> = None;
                loop {
                    let page = g
                        .list_dir(&segs, cursor.as_deref())
                        .map_err(|e| map_runtime_error(&e))?
                        .map_err(map_vfs_error)?;
                    for e in page.entries {
                        // El modelo de segmentos del WIT es MÁS permisivo que
                        // `VPath`: un nombre con `/` (o NUL, `.`/`..`) es válido
                        // como bytes pero NO como `Segment`. Se OMITE con warn
                        // — no tiene ruta representable donde vivir (mismo
                        // criterio que los providers archive, #93). Deuda
                        // stage-2b: contarlo y exponerlo por `list_skipped`.
                        let name = e.name;
                        let Ok(seg) = Segment::new(name.clone()) else {
                            tracing::warn!(
                                name = ?String::from_utf8_lossy(&name),
                                "nombre del provider no representable como VPath: omitido"
                            );
                            continue;
                        };
                        out.push(Entry {
                            path: dir.join(seg),
                            kind: map_kind(e.kind),
                            size: e.size,
                            mtime_ms: None,
                        });
                        // Cota anti-DoS: un guest hostil no reensambla sin fin.
                        if out.len() > MAX_LIST_ENTRIES {
                            return Err(Error::LimitExceeded {
                                limit: Error::LIMIT_ENTRIES.to_owned(),
                            });
                        }
                    }
                    match page.next_cursor {
                        Some(c) => cursor = Some(c),
                        None => break,
                    }
                }
                Ok(out)
            })
            .await?;
        Ok(stream::iter(entries.into_iter().map(Ok)).boxed())
    }

    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        let segs = Self::segments(p);
        let (offset, limit) = match range {
            None => (0u64, None),
            Some(r) => (r.offset, r.len),
        };
        // Reensamblado LAZY: cada poll lee UN chunk acotado del guest en
        // spawn_blocking — jamás se bufferiza el fichero entero (regla 2 + cota
        // de memoria: un guest hostil no infla la RAM del host, cada llamada
        // está acotada por `want` y por el deadline de época). El error de
        // apertura (fichero inexistente, dir) llega como el primer item del
        // stream. Estado: (instancia, segmentos, offset, bytes ya leídos).
        let inst = Arc::clone(&self.inst);
        let stream = stream::try_unfold(
            (inst, segs, offset, 0u64),
            move |(inst, segs, off, done)| async move {
                const CHUNK: u64 = 64 * 1024;
                let want = match limit {
                    Some(l) => {
                        let remaining = l.saturating_sub(done);
                        if remaining == 0 {
                            return Ok(None);
                        }
                        remaining.min(CHUNK)
                    }
                    None => CHUNK,
                };
                let inst2 = Arc::clone(&inst);
                let segs2 = segs.clone();
                let chunk: Vec<u8> = tokio::task::spawn_blocking(move || {
                    let mut g = inst2.lock().map_err(|_| Error::Internal { panic: true })?;
                    g.read(&segs2, off, want)
                        .map_err(|e| map_runtime_error(&e))?
                        .map_err(map_vfs_error)
                })
                .await
                .map_err(|_| Error::Internal { panic: true })??;
                if chunk.is_empty() {
                    return Ok(None); // EOF
                }
                // Un guest hostil que devuelve MÁS que `want` se recorta: el
                // rango pedido manda (contrato de `ByteRange`).
                let mut chunk = chunk;
                if chunk.len() as u64 > want {
                    chunk.truncate(usize::try_from(want).unwrap_or(usize::MAX));
                }
                let n = chunk.len() as u64;
                Ok(Some((
                    Bytes::from(chunk),
                    (inst, segs, off.saturating_add(n), done + n),
                )))
            },
        );
        Ok(stream.boxed())
    }

    // ---- mutaciones: se DELEGAN al guest (#30 stage 2b-write). Un guest
    // read-only responde Unsupported en cada una y el adapter lo propaga; uno
    // escribible hace el trabajo. ----

    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        let segs = Self::segments(p);
        // Abre el writer transaccional del guest (staging propio; el path final
        // no existe hasta commit — contrato de ByteSink).
        let handle = self
            .call(move |g| {
                g.open_writer(&segs)
                    .map_err(|e| map_runtime_error(&e))?
                    .map_err(map_vfs_error)
            })
            .await?;
        Ok(Box::new(PluginByteSink {
            inst: Arc::clone(&self.inst),
            writer: Some(handle),
        }))
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        let segs = Self::segments(p);
        self.call(move |g| {
            g.make_dir(&segs)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error)
        })
        .await
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        let segs = Self::segments(p);
        self.call(move |g| {
            g.remove(&segs)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error)
        })
        .await
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        let src = Self::segments(from);
        let dst = Self::segments(to);
        self.call(move |g| {
            g.rename(&src, &dst)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error)
        })
        .await
    }

    // `trash`/`symlink` no están en la interfaz WIT `provider` (stage 2): un
    // guest no los ofrece → Unsupported directo.
    async fn trash(&self, _p: &VPath) -> Result<Option<VPath>, Error> {
        Err(Error::Unsupported)
    }

    async fn symlink(
        &self,
        _link: &VPath,
        _target: &[u8],
        _kind: SymlinkKind,
    ) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// `ByteSink` (#30 stage 2b-write) respaldado por un `writer` resource del
/// guest: `write` añade un chunk, `commit`/`abort` publican o descartan y
/// liberan el handle (salvo si el guest atrapa — el trap envenena la instancia,
/// que se descarta). Soltar el sink sin commit/abort dispara un `abort`+drop
/// best-effort en [`Drop`] (contrato de `ByteSink`; síncrono con `try_lock`).
struct PluginByteSink {
    inst: Arc<Mutex<ProviderInstance>>,
    /// `Some` mientras el handle no se haya liberado; `commit`/`abort`/`Drop` lo
    /// toman.
    writer: Option<norte_plugin_host::WriterHandle>,
}

impl Drop for PluginByteSink {
    fn drop(&mut self) {
        // Best-effort (contrato de `ByteSink`): soltar sin commit/abort limpia
        // el staging del guest. Síncrono (las llamadas al guest lo son) con
        // `try_lock` — jamás bloquea: en Drop no hay ninguna op en vuelo sobre
        // este sink, y si por lo que fuera el mutex estuviera tomado, se cede el
        // handle a la tabla de recursos del store hasta que el provider muera.
        if let Some(w) = self.writer.take()
            && let Ok(mut g) = self.inst.try_lock()
        {
            let _ = g.writer_abort(w);
            let _ = g.writer_drop(w);
        }
    }
}

impl PluginByteSink {
    /// Ejecuta una op sobre el writer en `spawn_blocking` (regla 2).
    async fn call<F>(inst: &Arc<Mutex<ProviderInstance>>, f: F) -> Result<(), Error>
    where
        F: FnOnce(&mut ProviderInstance) -> Result<(), Error> + Send + 'static,
    {
        let inst = Arc::clone(inst);
        tokio::task::spawn_blocking(move || {
            let mut guard = inst.lock().map_err(|_| Error::Internal { panic: true })?;
            f(&mut guard)
        })
        .await
        .map_err(|_| Error::Internal { panic: true })?
    }
}

#[async_trait::async_trait]
impl ByteSink for PluginByteSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        let Some(w) = self.writer else {
            return Err(Error::Internal { panic: false }); // usado tras consumir
        };
        Self::call(&self.inst, move |g| {
            g.writer_write(w, &chunk)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error)
        })
        .await
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        let Some(w) = self.writer.take() else {
            return Err(Error::Internal { panic: false });
        };
        Self::call(&self.inst, move |g| {
            let r = g
                .writer_commit(w)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error);
            // El handle se libera tras un commit lógico (OK o VfsError); si el
            // guest ATRAPÓ, `?` ya salió y la instancia envenenada se descarta.
            let _ = g.writer_drop(w);
            r
        })
        .await
    }

    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        let Some(w) = self.writer.take() else {
            return Err(Error::Internal { panic: false });
        };
        Self::call(&self.inst, move |g| {
            let r = g
                .writer_abort(w)
                .map_err(|e| map_runtime_error(&e))?
                .map_err(map_vfs_error);
            let _ = g.writer_drop(w);
            r
        })
        .await
    }
}
