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
    /// tar.gz/tgz (ADR 0028, #55): capa gz OPACA sobre tar — índice
    /// secuencial (`entries()`, sin `Seek`) y lectura forward-decode
    /// (descarta hasta el offset con un decoder fresco por lectura).
    TarGz,
}

impl Format {
    fn token(self) -> &'static str {
        match self {
            Self::Tar => "tar",
            Self::Zip => "zip",
            Self::TarGz => "tar+gz",
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

/// Techo de lecturas forward-decode de `tar+gz` CONCURRENTES por provider
/// (FIX-2, security MAJOR, #55 review). El descarte hasta `offset` en
/// `read_entry_gz` puede pinnear un hilo de `spawn_blocking` durante
/// MINUTOS (hasta `Limits::max_decompressed_bytes` de inflate real) — N
/// lecturas profundas concurrentes son N hilos bloqueados simultáneamente
/// sobre el pool de `spawn_blocking` de tokio, que es COMPARTIDO por TODO
/// el runtime del daemon (journal, otros providers, tareas de fondo…), no
/// exclusivo de este provider: sin tope, un cliente que dispara muchas
/// lecturas profundas de un `tar+gz` grande hambrea el pool blocking entero
/// (`DoS`). El [`tokio::sync::Semaphore`] pone un tope duro: las lecturas
/// EXCEDENTES se ENCOLAN (esperan su turno), nunca se rechazan.
const GZ_READ_CONCURRENCY: usize = 4;

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
    /// Tope de concurrencia del forward-decode `tar+gz` (FIX-2, #55
    /// review): ver [`GZ_READ_CONCURRENCY`]. Sin efecto en `Tar`/`Zip`
    /// (esos `read` no pasan por este semáforo).
    gz_read_permits: Arc<tokio::sync::Semaphore>,
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
            gz_read_permits: Arc::new(tokio::sync::Semaphore::new(GZ_READ_CONCURRENCY)),
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
        // MINOR-4 (#61): sin mtime nada es cacheable — `get()` lo tiraría
        // siempre por stale (regla de `IndexCache::get`). El single-flight
        // solo aporta cuando el trabajo coalescido se REUTILIZA; aquí no hay
        // reutilización posible, así que pasar por el lock solo serializaría
        // N builds detrás de uno sin beneficio. Camino directo, en paralelo,
        // como pre-B2.
        if generation.0.is_none() {
            let container_len = outer.size.unwrap_or(0);
            return self.build_blocking(aref, generation, container_len).await;
        }

        // RAII (MAJOR-1, #61): `slot` se declara ANTES que `_build_guard` a
        // propósito — Rust suelta las locales en orden inverso de
        // declaración, así que en cualquier salida (return, `?`, panic,
        // CANCELACIÓN del future) el guard libera el mutex primero y el
        // slot se poda del map (o queda para el siguiente interesado)
        // después, sin la carrera de dos finalistas viéndose mutuamente el
        // Arc que tenía el `prune_building` manual.
        let slot = BuildingSlot::new(self, &key);
        let _build_guard = slot.shared().lock_owned().await;
        // MINOR-3 (#61): re-stat BAJO el lock. El stat de arriba solo sirve
        // al fast path (caché caliente); un waiter puede haber esperado el
        // lock tanto tiempo que su generación quedó vieja — usar la vieja
        // aquí pisaría (o fallaría en pisar) una entrada fresca que el
        // builder anterior ya puso con la generación ACTUAL.
        let outer = self.outer_stat(aref).await?;
        let generation = (outer.mtime_ms, outer.size);
        // Double-check: otro caller pudo construir mientras esperábamos.
        {
            let mut cache = self.cache.lock().expect("cache lock sano");
            if let Some(hit) = cache.get(&key, generation) {
                return Ok(hit);
            }
        }
        let container_len = outer.size.unwrap_or(0);
        let container = self.build_blocking(aref, generation, container_len).await?;
        // Sin mtime no hay validador: get() lo daría siempre por stale —
        // no gastes un slot LRU en un índice inrecuperable.
        if generation.0.is_some() {
            self.cache
                .lock()
                .expect("cache lock sano")
                .put(&key, container.clone());
        }
        Ok(container)
    }

    /// Construye el índice (y, en zip, el `ZipArchive` cacheable) en un hilo
    /// `spawn_blocking`. Sin caché ni single-flight propios: lo comparten el
    /// camino con lock de `index_for` y el atajo MINOR-4 de generación
    /// desconocida.
    async fn build_blocking(
        &self,
        aref: &ArchiveRef,
        generation: (Option<i64>, Option<u64>),
        container_len: u64,
    ) -> Result<CachedContainer, Error> {
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
            }
            Format::TarGz => {
                // Sin `container_len` como cota del locator (ADR 0028): ese
                // tamaño es el COMPRIMIDO y no acota nada del stream
                // descomprimido — el truncamiento se detecta en el read.
                crate::targz_format::build_index_gz(reader, generation, &limits, &cancel)
                    .map(|idx| (idx, None))
            }
        })
        .await;
        guard.disarm();
        let (index, zip) = joined
            .map_err(|e| {
                if e.is_panic() {
                    Error::Internal { panic: true }
                } else {
                    // Runtime en shutdown: cancelación, no bug (spec §17.7).
                    Error::Cancelled
                }
            })
            .and_then(|r| r)?;
        Ok(CachedContainer {
            index: Arc::new(index),
            zip,
        })
    }

    fn inner_key(aref: &ArchiveRef) -> InnerPath {
        aref.inner.iter().map(|s| s.as_bytes().to_vec()).collect()
    }
}

/// Guard RAII del single-flight (MAJOR-1, #61): registra (o reutiliza) el
/// lock de la clave al crearse y, al morir (éxito, error o CANCELACIÓN en
/// cualquier `await` — el drop de Rust corre igual), suelta su Arc y borra
/// la entrada del map si queda como único dueño. Sin poda manual por
/// call-site y sin la carrera de dos finalistas que se veían mutuamente el
/// Arc (ambos contaban 3 y nadie borraba).
struct BuildingSlot<'a> {
    provider: &'a ArchiveProvider,
    key: String,
    lock: Option<Arc<tokio::sync::Mutex<()>>>,
}

impl<'a> BuildingSlot<'a> {
    fn new(provider: &'a ArchiveProvider, key: &str) -> Self {
        let lock = {
            let mut building = provider.building.lock().expect("building lock sano");
            Arc::clone(
                building
                    .entry(key.to_owned())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        Self {
            provider,
            key: key.to_owned(),
            lock: Some(lock),
        }
    }

    /// Arc del lock para `lock_owned` (el guard retiene su PROPIO Arc y se
    /// suelta antes que el slot — orden de declaración en `index_for`).
    fn shared(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(self.lock.as_ref().expect("slot vivo hasta el drop"))
    }
}

impl Drop for BuildingSlot<'_> {
    fn drop(&mut self) {
        let mut building = self.provider.building.lock().expect("building lock sano");
        drop(self.lock.take()); // suelta NUESTRO Arc antes de contar
        if let Some(l) = building.get(&self.key)
            && Arc::strong_count(l) == 1
        {
            building.remove(&self.key);
        }
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
                // MINOR-5 (#61): el `ProviderReader` solo hace falta en el
                // camino frío (sin CD cacheado) — construirlo aquí evita el
                // Arc::clone/VPath::clone cuando el archive cacheado ya
                // resuelve la lectura entera.
                let handle = tokio::runtime::Handle::current();
                let inner = Arc::clone(&self.inner);
                let outer_path = aref.outer.clone();
                // El tamaño de la MISMA generación que el índice: vista
                // coherente aunque el contenedor cambie por debajo.
                let outer_len = cached.index.generation.1.unwrap_or(0);
                let (tx, mut rx) = tokio::sync::mpsc::channel(4);
                // El JoinHandle se suelta a propósito: la vida del hilo la
                // gobierna el canal, no el caller (huérfano acotado a 4
                // chunks de 64 KiB tras el drop).
                drop(tokio::task::spawn_blocking(move || {
                    // CD ya parseado (#61): clon barato (Arc interno), cero
                    // re-parse del central directory. Si no, camino frío.
                    let archive = if let Some(a) = cached_zip {
                        a
                    } else {
                        let reader = ProviderReader::new(handle, inner, outer_path, outer_len);
                        match crate::zip_format::open_archive(reader, &tx) {
                            Some(a) => a,
                            None => return,
                        }
                    };
                    crate::zip_format::read_entry(archive, entry_index, req_off, req_len, &tx);
                }));
                Ok(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed())
            }
            Locator::Gz { offset, .. } => {
                // Forward-decode: gz no es seekable — descarta hasta el
                // offset y sirve el tramo. O(descomprimido-hasta-offset) por
                // read, documentado (ADR 0028); spool/restart-points = issue
                // de deuda. Drop del stream = el send falla = el hilo
                // termina (regla 3).
                //
                // FIX-2 (security MAJOR, #55 review): el descarte puede
                // pinnear el hilo blocking minutos — acota la concurrencia
                // agregada con el semáforo (ver `GZ_READ_CONCURRENCY`).
                // Las lecturas EXCEDENTES se ENCOLAN aquí (await), nunca se
                // rechazan.
                let permit = Arc::clone(&self.gz_read_permits)
                    .acquire_owned()
                    .await
                    .map_err(|_| {
                        // El semáforo nunca se `close()`a en la vida de este
                        // provider (no hay ningún caller que lo cierre) —
                        // inalcanzable en la práctica; fail-safe explícito
                        // en vez de un `expect` que podría panicar si algo
                        // cambia en el futuro.
                        Error::Cancelled
                    })?;
                let reader = ProviderReader::new(
                    tokio::runtime::Handle::current(),
                    Arc::clone(&self.inner),
                    aref.outer.clone(),
                    cached.index.generation.1.unwrap_or(0),
                );
                let (tx, mut rx) = tokio::sync::mpsc::channel(4);
                let skip = offset + req_off;
                drop(tokio::task::spawn_blocking(move || {
                    let _permit = permit; // se libera cuando el hilo termina
                    crate::targz_format::read_entry_gz(reader, skip, req_len, &tx);
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

#[cfg(test)]
mod tests {
    //! Inline (no `tests/`): necesita acceso al campo privado `building`
    //! para verificar que el RAII de MAJOR-1 (#61) no deja huérfanos.
    use std::time::Duration;

    use bytes::Bytes;
    use norte_proto::Segment;
    use norte_testkit::{MemProvider, ZipSmith};

    use super::*;

    async fn seed_zip(bytes: &[u8]) -> (Arc<ArchiveProvider>, VPath, Arc<MemProvider>) {
        let mem = Arc::new(MemProvider::new());
        let path = MemProvider::root().join(Segment::new(b"f.zip".to_vec()).expect("seg"));
        let mut sink = mem.write(&path).await.expect("write");
        sink.write(Bytes::copy_from_slice(bytes))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
        let root = VPath::archive_compose("zip", &path, &[]).expect("compose");
        let provider = Arc::new(ArchiveProvider::with_limits(
            Arc::clone(&mem) as Arc<dyn Provider>,
            Format::Zip,
            "zip+mem",
            Limits::default(),
        ));
        (provider, root, mem)
    }

    /// MINOR-6 (#61): el primer builder se cancela (abort de su task)
    /// mientras un segundo caller ya está encolado detrás del mismo lock de
    /// `building` — el `BuildingSlot` RAII debe soltar el lock en el drop
    /// de la cancelación (sin poda manual de por medio) para que el
    /// esperador tome la posta y complete su PROPIO build limpiamente, y
    /// `building` debe quedar vacío al final (sin huérfanos, MAJOR-1).
    #[tokio::test(flavor = "multi_thread")]
    async fn builder_cancelado_no_deja_huerfano_y_el_esperador_completa() {
        let bytes = ZipSmith::new().file(b"a.txt", b"hola").build();
        let (provider, root, mem) = seed_zip(&bytes).await;
        // Cada operación del Mem (incluido el `stat` del contenedor y cada
        // bloque leído) tarda; da margen de sobra para abortar al primero
        // mientras sigue dentro del build (regla: sin sleeps a ciegas, solo
        // se usa para dar tiempo real al hilo bloqueante entre nuestro poll
        // y el abort).
        mem.faults()
            .set_latency_per_op(Some(Duration::from_millis(40)));

        let p1 = Arc::clone(&provider);
        let root1 = root.clone();
        let first = tokio::spawn(async move {
            let _ = p1.list(&root1).await;
        });

        // Espera (sin sleep a ciegas: poll cooperativo) a que el primer
        // builder haya REGISTRADO su slot — confirma que está dentro del
        // camino con lock antes de abortarlo.
        loop {
            if !provider
                .building
                .lock()
                .expect("building lock sano")
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }

        let p2 = Arc::clone(&provider);
        let root2 = root.clone();
        let second = tokio::spawn(async move { p2.list(&root2).await });

        // Deja que el segundo llegue a encolarse detrás del mismo lock
        // antes de cortar al primero a mitad de build.
        tokio::time::sleep(Duration::from_millis(5)).await;
        first.abort();
        let _ = first.await; // drena el abort: el drop de su stack ya corrió

        let result = second.await.expect("join del esperador");
        match result {
            Ok(mut stream) => {
                let entries: Vec<_> = stream.by_ref().collect().await;
                assert!(
                    entries.iter().all(Result::is_ok),
                    "el esperador lista el contenido completo tras la \
                     cancelación del primero"
                );
            }
            Err(e) => panic!(
                "el esperador debe completar su propio build limpiamente \
                 tras la cancelación del primero, falló con {e:?}"
            ),
        }

        assert!(
            provider
                .building
                .lock()
                .expect("building lock sano")
                .is_empty(),
            "sin huérfanos en `building` tras cancelar el primer builder \
             (MAJOR-1: RAII sin poda manual)"
        );
    }

    /// MAJOR-1 (#61), reproducción directa del leak: si TODOS los
    /// interesados de una clave se cancelan (nadie sobrevive para hacer la
    /// poda "de éxito"), el `prune_building` manual del pre-fix nunca corre
    /// para esa entrada — huérfano permanente. El RAII no depende de que
    /// alguien "gane": cada `BuildingSlot` se poda en SU PROPIO drop,
    /// pase lo que pase. Verificado contra el código pre-fix (ver informe):
    /// con la poda manual esto deja `building` con 1 entrada; con el RAII,
    /// vacío siempre.
    #[tokio::test(flavor = "multi_thread")]
    async fn todos_los_interesados_cancelados_no_deja_huerfano() {
        for _ in 0..20 {
            let bytes = ZipSmith::new().file(b"a.txt", b"hola").build();
            let (provider, root, mem) = seed_zip(&bytes).await;
            mem.faults()
                .set_latency_per_op(Some(Duration::from_millis(10)));

            let p1 = Arc::clone(&provider);
            let root1 = root.clone();
            let first = tokio::spawn(async move {
                let _ = p1.list(&root1).await;
            });
            // Espera cooperativa (sin sleep a ciegas) a que el builder
            // registre su slot antes de sumarle esperadores detrás.
            loop {
                if !provider
                    .building
                    .lock()
                    .expect("building lock sano")
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }

            let mut waiters = Vec::new();
            for _ in 0..8 {
                let p = Arc::clone(&provider);
                let r = root.clone();
                waiters.push(tokio::spawn(async move { p.list(&r).await }));
            }
            // Deja que los 8 se encolen detrás del mismo lock antes de
            // cortar a TODOS a mitad de vuelo (el escenario "cliente se
            // desconectó" bajo carga — sin superviviente que pode al final).
            tokio::time::sleep(Duration::from_millis(2)).await;
            first.abort();
            let _ = first.await;
            for w in &waiters {
                w.abort();
            }
            for w in waiters {
                let _ = w.await;
            }

            assert!(
                provider
                    .building
                    .lock()
                    .expect("building lock sano")
                    .is_empty(),
                "huérfano en `building` cuando TODOS los interesados se \
                 cancelan (MAJOR-1)"
            );
        }
    }
}
