//! [`ArchiveProvider`]: el `Provider` read-only de archivos comprimidos.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use norte_proto::{
    ArchiveRef, ByteRange, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error,
    VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};

use crate::blocking::ProviderReader;
use crate::index::{ArchiveIndex, InnerPath, Limits, Locator};

/// Formato de contenedor soportado (whitelist `ARCHIVE_FORMATS` de proto).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// tar plano (ustar/GNU/pax). Lectura passthrough (datos contiguos).
    Tar,
    /// zip stored+deflate. Lectura descomprimiendo en hilo blocking.
    Zip,
}

impl Format {
    fn token(self) -> &'static str {
        match self {
            Self::Tar => "tar",
            Self::Zip => "zip",
        }
    }
}

/// Índice + central directory del zip ya parseado (`None` en tar). Misma
/// clave y misma generación: la invalidación existente los gobierna juntos.
#[derive(Clone)]
struct CachedContainer {
    index: Arc<ArchiveIndex>,
    zip: Option<zip::ZipArchive<ProviderReader>>,
}

// `zip::ZipArchive` comparte el central directory por Arc interno; `Clone`
// con `R: Clone` es la base del cache (#61). Si una subida de `zip` lo
// rompe, que lo diga el compilador aquí y no una regresión de perf
// silenciosa.
const _: fn() = || {
    fn assert_clone<T: Clone>() {}
    let _ = assert_clone::<zip::ZipArchive<ProviderReader>>;
};

/// Caché LRU mínima de índices: clave = wire canónico del exterior. Cap fijo
/// (ADR 0018): RAII — al morir el provider muere todo.
struct IndexCache {
    map: HashMap<String, CachedContainer>,
    /// Orden de uso (el último es el más reciente).
    order: Vec<String>,
}

const CACHE_CAP: usize = 8;

impl IndexCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn touch(&mut self, key: &str) {
        self.order.retain(|k| k != key);
        self.order.push(key.to_owned());
    }

    fn get(
        &mut self,
        key: &str,
        generation: (Option<i64>, Option<u64>),
    ) -> Option<CachedContainer> {
        let hit = self.map.get(key)?;
        // mtime desconocido = SIEMPRE stale (ADR 0018): sin validador no
        // hay caché que valga.
        if hit.index.generation != generation || generation.0.is_none() {
            self.map.remove(key);
            self.order.retain(|k| k != key);
            return None;
        }
        let hit = hit.clone();
        self.touch(key);
        Some(hit)
    }

    fn put(&mut self, key: &str, container: CachedContainer) {
        if self.map.len() >= CACHE_CAP
            && !self.map.contains_key(key)
            && let Some(evict) = self.order.first().cloned()
        {
            self.map.remove(&evict);
            self.order.retain(|k| k != &evict);
        }
        self.map.insert(key.to_owned(), container);
        self.touch(key);
    }
}

/// Provider read-only que sirve el contenido de archivos comprimidos que
/// viven en OTRO provider (composición, ADR 0018 B2). Un instance sirve UN
/// scheme compuesto (`tar+file`, `tar+sftp`…) sobre UN provider interior.
pub struct ArchiveProvider {
    scheme: String,
    format: Format,
    inner: Arc<dyn Provider>,
    limits: Limits,
    cache: Mutex<IndexCache>,
    /// Single-flight de construcción de índice (#61): un builder por clave;
    /// los concurrentes esperan el lock y releen la caché. El map se poda
    /// cuando el último interesado suelta su Arc.
    building: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl ArchiveProvider {
    /// Provider con los límites por defecto. `scheme` es el compuesto
    /// completo (`tar+file`); debe empezar por el token del formato.
    ///
    /// # Panics
    /// Si `scheme` no empieza por `<formato>+` — error de wiring, no de
    /// datos (el core compone el scheme desde el mismo token).
    #[must_use]
    pub fn new(inner: Arc<dyn Provider>, format: Format, scheme: impl Into<String>) -> Self {
        Self::with_limits(inner, format, scheme, Limits::default())
    }

    /// Como [`Self::new`] con límites propios (tests de bomba; config futura).
    ///
    /// # Panics
    /// Ver [`Self::new`].
    #[must_use]
    pub fn with_limits(
        inner: Arc<dyn Provider>,
        format: Format,
        scheme: impl Into<String>,
        limits: Limits,
    ) -> Self {
        let scheme = scheme.into();
        assert!(
            scheme.starts_with(&format!("{}+", format.token())),
            "scheme compuesto `{scheme}` no corresponde al formato {format:?}"
        );
        Self {
            scheme,
            format,
            inner,
            limits,
            cache: Mutex::new(IndexCache::new()),
            building: Mutex::new(HashMap::new()),
        }
    }

    /// Valida el path contra este provider y lo desmonta (ADR 0018).
    fn split(&self, p: &VPath) -> Result<ArchiveRef, Error> {
        if p.scheme() != self.scheme {
            return Err(Error::InvalidPath);
        }
        match p.archive_split() {
            Ok(Some(aref)) if aref.format == self.format.token() => Ok(aref),
            _ => Err(Error::InvalidPath),
        }
    }

    /// stat del contenedor en el interior: debe ser un archivo.
    async fn outer_stat(&self, aref: &ArchiveRef) -> Result<Entry, Error> {
        let e = self.inner.stat(&aref.outer).await?;
        if e.kind != EntryKind::File {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        Ok(e)
    }

    /// El índice del contenedor, de caché o reconstruido (`spawn_blocking`).
    async fn index_for(&self, aref: &ArchiveRef) -> Result<CachedContainer, Error> {
        let outer = self.outer_stat(aref).await?;
        let generation = (outer.mtime_ms, outer.size);
        let key = aref.outer.to_wire();
        {
            let mut cache = self.cache.lock().expect("cache lock sano");
            if let Some(hit) = cache.get(&key, generation) {
                return Ok(hit);
            }
        }
        let build_lock = {
            let mut building = self.building.lock().expect("building lock sano");
            Arc::clone(
                building
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        let build_guard = build_lock.lock().await;
        // Double-check: otro caller pudo construir mientras esperábamos.
        {
            let mut cache = self.cache.lock().expect("cache lock sano");
            if let Some(hit) = cache.get(&key, generation) {
                drop(build_guard);
                self.prune_building(&key, &build_lock);
                return Ok(hit);
            }
        }
        let container_len = outer.size.unwrap_or(0);
        let reader = ProviderReader::new(
            tokio::runtime::Handle::current(),
            Arc::clone(&self.inner),
            aref.outer.clone(),
            container_len,
        );
        let limits = self.limits;
        let format = self.format;
        // Regla 3: si este future muere (caller cancela), el guard arma el
        // flag y el hilo blocking corta en la siguiente entrada del loop.
        let cancel = Arc::new(AtomicBool::new(false));
        let mut guard = CancelOnDrop::new(Arc::clone(&cancel));
        let joined = tokio::task::spawn_blocking(move || match format {
            Format::Tar => {
                crate::tar_format::build_index(reader, container_len, generation, &limits, &cancel)
                    .map(|idx| (idx, None))
            }
            Format::Zip => {
                crate::zip_format::build_index(reader, container_len, generation, &limits, &cancel)
                    .map(|(idx, archive)| (idx, Some(archive)))
            }
        })
        .await;
        guard.disarm();
        let (index, zip) = match joined
            .map_err(|e| {
                if e.is_panic() {
                    Error::Internal { panic: true }
                } else {
                    // Runtime en shutdown: cancelación, no bug (spec §17.7).
                    Error::Cancelled
                }
            })
            .and_then(|r| r)
        {
            Ok(v) => v,
            Err(e) => {
                drop(build_guard);
                self.prune_building(&key, &build_lock);
                return Err(e);
            }
        };
        let container = CachedContainer {
            index: Arc::new(index),
            zip,
        };
        // Sin mtime no hay validador: get() lo daría siempre por stale —
        // no gastes un slot LRU en un índice inrecuperable.
        if generation.0.is_some() {
            self.cache
                .lock()
                .expect("cache lock sano")
                .put(&key, container.clone());
        }
        drop(build_guard);
        self.prune_building(&key, &build_lock);
        Ok(container)
    }

    /// Poda la entrada de `building` si nadie más la retiene (2 = el map + el
    /// caller). Si un caller muere cancelado con el guard tomado, la entrada
    /// sobrevive hasta la siguiente poda — acotado por claves activas, no fuga.
    fn prune_building(&self, key: &str, lock: &Arc<tokio::sync::Mutex<()>>) {
        let mut building = self.building.lock().expect("building lock sano");
        if Arc::strong_count(lock) == 2 {
            building.remove(key);
        }
    }

    fn inner_key(aref: &ArchiveRef) -> InnerPath {
        aref.inner.iter().map(|s| s.as_bytes().to_vec()).collect()
    }
}

/// Arma un `AtomicBool` si el dueño muere sin desarmarlo: la señal de
/// cancelación hacia el hilo blocking del indexado (regla 3).
struct CancelOnDrop {
    flag: Arc<AtomicBool>,
    armed: bool,
}

impl CancelOnDrop {
    fn new(flag: Arc<AtomicBool>) -> Self {
        Self { flag, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(true, Ordering::Relaxed);
        }
    }
}

#[async_trait]
impl Provider for ArchiveProvider {
    fn scheme(&self) -> &str {
        &self.scheme
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            flags: CapabilityFlags::READ_ONLY
                | CapabilityFlags::CASE_SENSITIVE
                | CapabilityFlags::CASE_PRESERVING,
            max_path: None,
        }
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        let aref = self.split(p)?;
        let cached = self.index_for(&aref).await?;
        cached.index.entry_for(p, &Self::inner_key(&aref))
    }

    /// Listado de un dir del árbol virtual.
    ///
    /// CONTRATO (ADR 0018 C2): las entradas del contenedor cuyo nombre no
    /// mapea a segmentos `VPath` válidos (`..`, `.`, vacío, NUL, absoluto,
    /// componente `!`) NO aparecen — se omiten al indexar con
    /// `tracing::warn!` y cuentan en el `skipped` del índice. Duplicados:
    /// última gana; conflicto file-vs-dir: gana dir (un file en posición de
    /// ancestro asciende a dir).
    ///
    /// Caveats zip conocidos (auditoría 8e; upstream, con issue): el crate
    /// `zip` colapsa nombres crudos distintos que decodifican igual (H1 —
    /// detectado y contado en `skipped` para no-zip64) y un extra field
    /// Info-ZIP 0x7075 válido SUSTITUYE el nombre del header (H3).
    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        let aref = self.split(p)?;
        let cached = self.index_for(&aref).await?;
        let index = &cached.index;
        let key = Self::inner_key(&aref);
        if !key.is_empty() {
            match index.nodes.get(&key) {
                None => return Err(Error::NotFound),
                Some(n) if n.kind != EntryKind::Dir => {
                    return Err(Error::Conflict {
                        conflict: ConflictKind::TypeMismatch,
                    });
                }
                Some(_) => {}
            }
        }
        let mut entries = Vec::new();
        for name in index.children.get(&key).into_iter().flatten() {
            let seg = norte_proto::Segment::new(name.clone())
                .expect("el índice solo contiene segmentos válidos");
            let child_path = p.join(seg);
            let mut child_key = key.clone();
            child_key.push(name.clone());
            entries.push(index.entry_for(&child_path, &child_key));
        }
        Ok(futures::stream::iter(entries).boxed())
    }

    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        let aref = self.split(p)?;
        let cached = self.index_for(&aref).await?;
        let index = &cached.index;
        let key = Self::inner_key(&aref);
        if key.is_empty() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let node = index.nodes.get(&key).ok_or(Error::NotFound)?;
        if node.kind != EntryKind::File {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let Some(locator) = &node.locator else {
            // Listable pero no legible (método no soportado, cifrado…).
            return Err(Error::Unsupported);
        };
        // El range pedido se recorta al tramo de la ENTRADA (semántica pread).
        let entry_size = node.size.unwrap_or(0);
        let req_off = range.map_or(0, |r| r.offset);
        if req_off >= entry_size {
            return Ok(futures::stream::empty().boxed());
        }
        let disponible = entry_size - req_off;
        let req_len = range
            .and_then(|r| r.len)
            .map_or(disponible, |l| l.min(disponible));
        if req_len == 0 {
            return Ok(futures::stream::empty().boxed());
        }
        match *locator {
            Locator::Tar { offset, .. } => {
                // Passthrough: datos contiguos sin comprimir.
                self.inner
                    .read(
                        &aref.outer,
                        Some(ByteRange {
                            offset: offset + req_off,
                            len: Some(req_len),
                        }),
                    )
                    .await
            }
            Locator::Zip { index: entry_index } => {
                // Descompresión en hilo blocking → canal acotado → stream.
                // Drop del stream = el send falla = el hilo termina (regla 3).
                let cached_zip = cached.zip.clone();
                let reader = ProviderReader::new(
                    tokio::runtime::Handle::current(),
                    Arc::clone(&self.inner),
                    aref.outer.clone(),
                    // El tamaño de la MISMA generación que el índice: vista
                    // coherente aunque el contenedor cambie por debajo.
                    cached.index.generation.1.unwrap_or(0),
                );
                let (tx, mut rx) = tokio::sync::mpsc::channel(4);
                // El JoinHandle se suelta a propósito: la vida del hilo la
                // gobierna el canal, no el caller (huérfano acotado a 4
                // chunks de 64 KiB tras el drop).
                drop(tokio::task::spawn_blocking(move || {
                    let archive = match cached_zip {
                        // CD ya parseado (#61): clon barato (Arc interno),
                        // cero re-parse del central directory.
                        Some(a) => a,
                        None => match crate::zip_format::open_archive(reader, &tx) {
                            Some(a) => a,
                            None => return,
                        },
                    };
                    crate::zip_format::read_entry(archive, entry_index, req_off, req_len, &tx);
                }));
                Ok(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed())
            }
        }
    }

    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        let aref = self.split(p)?;
        let cached = self.index_for(&aref).await?;
        let index = &cached.index;
        let key = Self::inner_key(&aref);
        if key.is_empty() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let node = index.nodes.get(&key).ok_or(Error::NotFound)?;
        match (&node.kind, &node.link_target) {
            (EntryKind::Symlink, Some(target)) => Ok(target.clone()),
            (EntryKind::Symlink, None) => Err(Error::Corrupt),
            _ => Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            }),
        }
    }

    // ---------- mutaciones: READ_ONLY (ADR 0018 E2) ----------

    async fn write(&self, _p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        Err(Error::Unsupported)
    }

    async fn mkdir(&self, _p: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }

    async fn remove(&self, _p: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }

    async fn rename(&self, _from: &VPath, _to: &VPath) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}
