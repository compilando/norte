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

/// El índice cacheado de UN contenedor. Desde #59 es SOLO el índice (el
/// locator zip es autocontenido: no se retiene ningún objeto de archive);
/// el struct conserva el nombre para minimizar churn.
#[derive(Clone)]
struct CachedContainer {
    index: Arc<ArchiveIndex>,
}

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

/// `(mtime_ms, size)` del contenedor exterior: la moneda de invalidación de
/// caché/spool en todo este módulo.
type Generation = (Option<i64>, Option<u64>);

/// Umbral de calor del spool (#95.1): en la lectura gz N.º
/// `SPOOL_HEAT_THRESHOLD` de un mismo contenedor (misma generación) se
/// construye el spool descomprimido.
const SPOOL_HEAT_THRESHOLD: u32 = 2;

/// Tope de entradas del mapa de calor. Es solo una heurística: al llenarse
/// se expulsa una entrada arbitraria.
const SPOOL_HEAT_CAP: usize = 32;

/// Centinela en el mapa de calor: contenedor NO spooleable (su descomprimido
/// supera `Limits::spool_max_bytes`) — no se reintenta el build hasta que
/// cambie de generación. `saturating_add` lo deja clavado aquí.
const SPOOL_UNSPOOLABLE: u32 = u32::MAX;

/// El spool de UN contenedor `tar+gz` caliente (#95.1): su stream gz entero
/// DESCOMPRIMIDO en un fichero temporal, para que las lecturas repetidas
/// sean seeks locales O(1) en vez de forward-decode O(offset).
struct Spool {
    /// Wire canónico del contenedor exterior (misma clave que `IndexCache`).
    key: String,
    /// Generación del contenedor al spoolar — la invalidación.
    generation: Generation,
    /// Fichero de [`tempfile::tempfile()`]: ANÓNIMO — nace ya unlinked, el
    /// SO recupera el espacio al morir el último descriptor y JAMÁS tiene
    /// pathname (cero superficie de ataque por nombre de staging). Va bajo
    /// `Mutex` porque el cursor del fd es COMPARTIDO (un `try_clone` es un
    /// dup: mismo offset) — cada chunk re-seekea a posición ABSOLUTA bajo
    /// el lock, así dos lecturas concurrentes del spool no se pisan.
    file: Arc<Mutex<std::fs::File>>,
    /// Bytes descomprimidos totales del spool.
    len: u64,
}

/// Estado compartido del spool (#95.1). Vive en un `Arc` porque los hilos
/// `spawn_blocking` que construyen/instalan el spool necesitan `'static`.
struct SpoolState {
    /// Slot ÚNICO por provider (v1): el último contenedor caliente gana —
    /// otro contenedor que se caliente REEMPLAZA al anterior.
    slot: tokio::sync::Mutex<Option<Spool>>,
    /// Calor por contenedor: nº de lecturas gz de la generación vista. La
    /// generación nueva resetea el contador (y des-marca un no-spooleable);
    /// las entradas de generaciones viejas se podan así, oportunistamente.
    heat: Mutex<HashMap<String, (Generation, u32)>>,
    /// Build en curso (clave del contenedor). Los competidores NO esperan:
    /// caen a forward-decode — solo un hilo paga el build.
    building: Mutex<Option<String>>,
}

impl SpoolState {
    fn new() -> Self {
        Self {
            slot: tokio::sync::Mutex::new(None),
            heat: Mutex::new(HashMap::new()),
            building: Mutex::new(None),
        }
    }

    /// Suma una lectura al calor de `key` y devuelve el contador resultante.
    fn bump_heat(&self, key: &str, generation: Generation) -> u32 {
        let mut heat = self.heat.lock().expect("heat lock sano");
        if heat.len() >= SPOOL_HEAT_CAP
            && !heat.contains_key(key)
            && let Some(victim) = heat.keys().next().cloned()
        {
            heat.remove(&victim);
        }
        let e = heat.entry(key.to_owned()).or_insert((generation, 0));
        if e.0 != generation {
            *e = (generation, 0);
        }
        e.1 = e.1.saturating_add(1);
        e.1
    }

    /// Negative-cache: el descomprimido de `key` supera el presupuesto —
    /// no reintentar el build mientras dure esta generación.
    fn mark_unspoolable(&self, key: &str, generation: Generation) {
        self.heat
            .lock()
            .expect("heat lock sano")
            .insert(key.to_owned(), (generation, SPOOL_UNSPOOLABLE));
    }

    /// Reclama el flag de build para `key`. `None` = otro build en curso
    /// (el caller cae a forward-decode, jamás espera).
    fn try_claim_build(self: &Arc<Self>, key: &str) -> Option<SpoolBuildClaim> {
        let mut building = self.building.lock().expect("building lock sano");
        if building.is_some() {
            return None;
        }
        *building = Some(key.to_owned());
        Some(SpoolBuildClaim {
            state: Arc::clone(self),
        })
    }
}

/// RAII del flag de build (#95.1): lo limpia en drop pase lo que pase —
/// éxito, abort, panic del hilo, o closure de `spawn_blocking` descartada
/// sin ejecutar (shutdown del runtime). Solo puede existir UNO a la vez
/// (transición `None → Some` bajo el lock), así que limpiar sin comparar
/// es correcto.
struct SpoolBuildClaim {
    state: Arc<SpoolState>,
}

impl SpoolBuildClaim {
    fn state(&self) -> &SpoolState {
        &self.state
    }
}

impl Drop for SpoolBuildClaim {
    fn drop(&mut self) {
        *self.state.building.lock().expect("building lock sano") = None;
    }
}

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
/// Caveat de anidamiento (#56): la generación del caché de una capa ANIDADA
/// es el (mtime, size) de la entrada DENTRO del archivo exterior — reemplazar
/// el contenedor exterior con entradas de metadatos idénticos puede servir un
/// índice interior rancio hasta la evicción; el CRC de lecturas completas
/// (#59) y los short-reads fail-loud son el cinturón.
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
    /// Tope de concurrencia de las lecturas con DESCARTE descomprimido:
    /// el forward-decode `tar+gz` (FIX-2, #55 review) y, desde #59, los
    /// deflate RANGED de zip (skip>0 — deflate no tiene seek). Ver
    /// [`GZ_READ_CONCURRENCY`]. `Tar` y las lecturas sin descarte no pasan
    /// por aquí.
    gz_read_permits: Arc<tokio::sync::Semaphore>,
    /// Spool de tar.gz calientes (#95.1): slot único + calor + flag de
    /// build. En `Arc` para los hilos blocking (ver [`SpoolState`]).
    spool: Arc<SpoolState>,
}

/// Envuelve un stream passthrough con un contador ENTREGADO-vs-PROMETIDO
/// (#97): el índice prometió `expected` bytes — si el stream interior
/// termina antes (contenedor truncado/mutado bajo nuestros pies, semántica
/// pread sin error) o entrega de más (provider interior mentiroso), el
/// consumidor recibe `Error::Corrupt`, jamás datos cortos o de sobra en
/// silencio. Un `Err` del interior se propaga verbatim y corta el stream.
/// `container` = display YA REDACTADO del contenedor (para las trazas).
fn expect_exact(inner: ByteStream, expected: u64, container: String) -> ByteStream {
    // Estado: el stream interior va en Option — en los estados terminales se
    // SUELTA al instante (m1 del review: un interior remoto puede pinnear
    // buffers/slot de conexión hasta que el caller dropee el wrapper).
    futures::stream::unfold(
        (Some(inner), 0u64, container),
        move |(stream, got, container)| async move {
            let mut stream = stream?;
            match stream.next().await {
                Some(Ok(chunk)) => {
                    let got = got + chunk.len() as u64;
                    if got > expected {
                        tracing::warn!(
                            got,
                            expected,
                            %container,
                            "tar passthrough entrega bytes DE MÁS"
                        );
                        return Some((Err(Error::Corrupt), (None, got, container)));
                    }
                    Some((Ok(chunk), (Some(stream), got, container)))
                }
                Some(Err(e)) => Some((Err(e), (None, got, container))),
                None => {
                    if got < expected {
                        tracing::warn!(
                            got,
                            expected,
                            %container,
                            "tar passthrough corto: contenedor truncado/mutado bajo el read"
                        );
                        return Some((Err(Error::Corrupt), (None, got, container)));
                    }
                    None
                }
            }
        },
    )
    // M1 del review: Unfold PANICA si se pollea tras Ready(None) — fused,
    // como el resto de ByteStreams de este crate (poll_fn/iter/empty).
    .fuse()
    .boxed()
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
            spool: Arc::new(SpoolState::new()),
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

    /// Construye el índice en un hilo `spawn_blocking`. Sin caché ni
    /// single-flight propios: lo comparten el camino con lock de
    /// `index_for` y el atajo MINOR-4 de generación desconocida.
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
        let joined = crate::blocking::spawn_blocking(move || match format {
            Format::Tar => {
                crate::tar_format::build_index(reader, container_len, generation, &limits, &cancel)
            }
            Format::Zip => {
                crate::zip_format::build_index(reader, container_len, generation, &limits, &cancel)
            }
            Format::TarGz => {
                // Sin `container_len` como cota del locator (ADR 0028): ese
                // tamaño es el COMPRIMIDO y no acota nada del stream
                // descomprimido — el truncamiento se detecta en el read.
                crate::targz_format::build_index_gz(reader, generation, &limits, &cancel)
            }
        })
        .await;
        guard.disarm();
        let index = joined
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
        })
    }

    fn inner_key(aref: &ArchiveRef) -> InnerPath {
        aref.inner.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    /// Lectura zip (#59): descompresión en hilo blocking → canal acotado →
    /// stream; lector FRESCO por lectura, locator autocontenido — sin
    /// archive retenido ni re-parse del CD. Drop del stream = el send falla
    /// (fase de entrega) o el chequeo de canal cerrado corta el DESCARTE
    /// (rust MAJOR-1 del review #59) = el hilo termina (regla 3).
    async fn read_zip(
        &self,
        aref: &ArchiveRef,
        plan: crate::zip_format::ReadPlan,
    ) -> Result<norte_vfs::ByteStream, Error> {
        let handle = tokio::runtime::Handle::current();
        let inner = Arc::clone(&self.inner);
        let outer_path = aref.outer.clone();
        let outer_len = plan.container_len;
        // rust MAJOR-1 (#59 review): un deflate RANGED descarta O(skip)
        // descomprimiendo en el hilo blocking (deflate no tiene seek) —
        // misma inanición del pool que el forward-decode gz (#55 FIX-2):
        // comparte su semáforo. stored y lecturas sin descarte no lo pagan.
        let permit = if plan.method == 8 && plan.skip > 0 {
            Some(
                Arc::clone(&self.gz_read_permits)
                    .acquire_owned()
                    .await
                    .map_err(|_| Error::Cancelled)?,
            )
        } else {
            None
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        // El JoinHandle se suelta a propósito: la vida del hilo la gobierna
        // el canal, no el caller — el huérfano tras un drop está acotado por
        // el canal (4 chunks) en la fase de entrega Y por el chequeo de
        // canal cerrado en la fase de descarte.
        drop(crate::blocking::spawn_blocking(move || {
            let _permit = permit; // se libera cuando el hilo termina
            let reader = ProviderReader::new(handle, inner, outer_path, outer_len);
            crate::zip_format::read_entry(reader, &plan, &tx);
        }));
        Ok(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed())
    }

    /// Lectura tar.gz (#95.1). Tres caminos:
    ///
    /// 1. **Spool hit** (contenedor caliente, misma generación): sirve el
    ///    tramo con seeks locales del tempfile — sin descompresión, SIN
    ///    semáforo (no pinnea nada).
    /// 2. **Lectura que cruza el umbral de calor**: construye el spool
    ///    DENTRO del mismo hilo blocking (bajo el permit gz que la lectura
    ///    ya paga) y sirve el tramo desde él. Los competidores durante el
    ///    build caen al camino 3, jamás esperan.
    /// 3. **Forward-decode** (frío, presupuesto 0, sin mtime, no-spooleable
    ///    o build ajeno en curso): el camino de siempre — descarta hasta el
    ///    offset con un decoder fresco (ADR 0028), bajo el semáforo FIX-2.
    ///
    /// Drop del stream = el send falla / `is_closed` corta = el hilo
    /// termina (regla 3), en los tres caminos.
    async fn read_gz(
        &self,
        aref: &ArchiveRef,
        cached: &CachedContainer,
        offset: u64,
        req_off: u64,
        req_len: u64,
    ) -> Result<norte_vfs::ByteStream, Error> {
        let key = aref.outer.to_wire();
        let generation = cached.index.generation;
        // Posición ABSOLUTA del tramo en el stream descomprimido.
        let start = offset + req_off;

        // Camino 1: spool vigente. mtime desconocido = JAMÁS spool (sin
        // validador no hay caché que valga — mismo criterio que IndexCache).
        if generation.0.is_some() {
            let mut slot = self.spool.slot.lock().await;
            if let Some(s) = slot.as_ref()
                && s.key == key
            {
                if s.generation == generation {
                    let file = Arc::clone(&s.file);
                    let spool_len = s.len;
                    drop(slot);
                    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
                    drop(crate::blocking::spawn_blocking(move || {
                        serve_from_spool(&file, spool_len, start, req_len, &tx);
                    }));
                    return Ok(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed());
                }
                // Mismo contenedor, generación vieja: ya no sirve a nadie —
                // suéltalo ya (libera el disco) y que el calor arranque de
                // cero para la generación nueva.
                tracing::debug!(
                    container = %aref.outer.display_lossy(),
                    "spool descartado: el contenedor cambió de generación"
                );
                *slot = None;
            }
        }

        // Calor + decisión de build (camino 2 vs 3).
        let claim = if generation.0.is_some() && self.limits.spool_max_bytes > 0 {
            let n = self.spool.bump_heat(&key, generation);
            if n >= SPOOL_HEAT_THRESHOLD && n != SPOOL_UNSPOOLABLE {
                self.spool.try_claim_build(&key)
            } else {
                None
            }
        } else {
            None
        };

        // FIX-2 (security MAJOR, #55 review): descarte/descompresión pueden
        // pinnear el hilo blocking minutos — acota la concurrencia agregada
        // con el semáforo (ver `GZ_READ_CONCURRENCY`). Las lecturas
        // EXCEDENTES se ENCOLAN aquí (await), nunca se rechazan. El build
        // del spool corre bajo el MISMO permit de la lectura que lo dispara.
        let permit = Arc::clone(&self.gz_read_permits)
            .acquire_owned()
            .await
            .map_err(|_| {
                // El semáforo nunca se `close()`a en la vida de este
                // provider (no hay ningún caller que lo cierre) —
                // inalcanzable en la práctica; fail-safe explícito en vez
                // de un `expect` que podría panicar si algo cambia.
                Error::Cancelled
            })?;
        let reader = ProviderReader::new(
            tokio::runtime::Handle::current(),
            Arc::clone(&self.inner),
            aref.outer.clone(),
            generation.1.unwrap_or(0),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        if let Some(claim) = claim {
            let job = SpoolBuildJob {
                claim,
                key,
                generation,
                budget: self.limits.spool_max_bytes,
                start,
                len: req_len,
                container: aref.outer.display_lossy(),
            };
            drop(crate::blocking::spawn_blocking(move || {
                let _permit = permit; // se libera cuando el hilo termina
                build_spool_and_serve(job, reader, &tx);
            }));
        } else {
            drop(crate::blocking::spawn_blocking(move || {
                let _permit = permit; // se libera cuando el hilo termina
                crate::targz_format::read_entry_gz(reader, start, req_len, &tx);
            }));
        }
        Ok(futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed())
    }
}

/// Parámetros del build+serve del spool (#95.1), de una pieza para el hilo
/// blocking.
struct SpoolBuildJob {
    claim: SpoolBuildClaim,
    key: String,
    generation: Generation,
    budget: u64,
    /// Posición absoluta del tramo pedido en el stream descomprimido.
    start: u64,
    /// Longitud del tramo pedido.
    len: u64,
    /// Display YA redactado del contenedor (solo trazas).
    container: String,
}

/// Construye el spool (forward-decode COMPLETO del contenedor al tempfile
/// anónimo) y, si sale bien, lo instala como slot único del provider y
/// sirve el tramo pedido desde él. Cualquier abort degrada con honestidad:
/// receptor muerto = nada que servir; sobre-presupuesto = negative-cache +
/// forward-decode; fallo de build = forward-decode (si el contenedor está
/// roto de verdad, la relectura fallará con el error correcto por el camino
/// de siempre). El flag de build lo suelta el drop de `job.claim` en TODOS
/// los caminos (RAII).
fn build_spool_and_serve(
    job: SpoolBuildJob,
    reader: ProviderReader,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) {
    use crate::targz_format::{SpoolAbort, read_entry_gz, spool_gz};
    let mut file = match tempfile::tempfile() {
        Ok(f) => f,
        Err(e) => {
            // Sin tempfile no hay spool; la lectura sigue por forward-decode.
            tracing::warn!(
                error = %e,
                container = %job.container,
                "sin tempfile para el spool tar.gz; forward-decode"
            );
            drop(job.claim);
            return read_entry_gz(reader, job.start, job.len, tx);
        }
    };
    // Reader FRESCO para el build (`Clone` resetea posición y caché de
    // bloque); el original queda para el fallback si el build aborta.
    let probe = || tx.is_closed();
    match spool_gz(reader.clone(), job.budget, &probe, &mut file) {
        Ok(len) => {
            let file = Arc::new(Mutex::new(file));
            let spool = Spool {
                key: job.key,
                generation: job.generation,
                file: Arc::clone(&file),
                len,
            };
            // blocking_lock: estamos en un hilo de `spawn_blocking`, jamás
            // dentro del runtime async (donde panicaría).
            *job.claim.state().slot.blocking_lock() = Some(spool);
            tracing::debug!(
                bytes = len,
                container = %job.container,
                "spool tar.gz construido e instalado"
            );
            drop(job.claim);
            serve_from_spool(&file, len, job.start, job.len, tx);
        }
        Err(SpoolAbort::Cancelled) => {
            // Receptor muerto: descarta el parcial sin ruido (regla 3). El
            // drop del claim libera el flag; el drop del tempfile, el disco.
            tracing::debug!(
                container = %job.container,
                "build del spool tar.gz cancelado (receptor muerto)"
            );
        }
        Err(SpoolAbort::OverBudget) => {
            job.claim.state().mark_unspoolable(&job.key, job.generation);
            tracing::warn!(
                budget = job.budget,
                container = %job.container,
                "descomprimido supera spool_max_bytes: contenedor no spooleable"
            );
            drop(job.claim);
            read_entry_gz(reader, job.start, job.len, tx);
        }
        Err(SpoolAbort::Io(e)) => {
            tracing::warn!(
                error = %e,
                container = %job.container,
                "build del spool tar.gz falló; forward-decode"
            );
            drop(job.claim);
            read_entry_gz(reader, job.start, job.len, tx);
        }
    }
}

/// Sirve `[start, start+len)` del fichero de spool en chunks de 64 KiB por
/// el canal acotado. Cada chunk re-seekea a posición ABSOLUTA bajo el lock
/// del fichero (el cursor del fd es compartido — ver [`Spool::file`]).
/// Short read = `Corrupt` fail-loud: el índice prometió bytes que el spool
/// no tiene (contenedor y spool no cuadran) — jamás datos cortos en
/// silencio. El caller ya recortó el tramo contra el tamaño de la ENTRADA
/// (semántica pread), igual que en `read_entry_gz`.
fn serve_from_spool(
    file: &Mutex<std::fs::File>,
    spool_len: u64,
    start: u64,
    len: u64,
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, Error>>,
) {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let send_err = |e: Error| {
        // Mejor esfuerzo: si el receptor murió, no hay a quién contárselo.
        let _ = tx.blocking_send(Err(e));
    };
    if start.checked_add(len).is_none_or(|end| end > spool_len) {
        tracing::warn!(start, len, spool_len, "tramo pedido fuera del spool");
        return send_err(Error::Corrupt);
    }
    let mut buf = vec![0u8; 64 * 1024];
    let mut pos = start;
    let mut remaining = len;
    while remaining > 0 {
        if tx.is_closed() {
            tracing::debug!("lectura de spool cancelada (receptor muerto)");
            return;
        }
        let want = buf
            .len()
            .min(usize::try_from(remaining).unwrap_or(buf.len()));
        let got = {
            let mut f = file.lock().expect("spool file lock sano");
            f.seek(SeekFrom::Start(pos))
                .and_then(|_| f.read(&mut buf[..want]))
        };
        match got {
            // EOF antes de servir el tramo prometido: short read del spool.
            Ok(0) => return send_err(Error::Corrupt),
            Ok(n) => {
                pos += n as u64;
                remaining -= n as u64;
                if tx
                    .blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                    .is_err()
                {
                    tracing::debug!("lectura de spool cancelada (receptor muerto)");
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "IO del fichero de spool");
                return send_err(Error::Io { retryable: true });
            }
        }
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

/// Catálogo de attrs del formato zip (#108 bloque 2): todo sale del CD ya
/// indexado — cero I/O por consulta.
fn catalogo_zip() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        vec![
            AttrInfo {
                id: "archive.method".to_owned(),
                label: "Method".to_owned(),
                ty: AttrType::Text,
                hint: AttrHint::Opaque,
            },
            AttrInfo {
                id: "archive.packed_size".to_owned(),
                label: "Packed".to_owned(),
                ty: AttrType::Uint,
                hint: AttrHint::Size,
            },
            AttrInfo {
                id: "archive.crc32".to_owned(),
                label: "CRC-32".to_owned(),
                ty: AttrType::Uint,
                hint: AttrHint::Opaque,
            },
        ]
    });
    &CAT
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
        self.stat_with(p, &norte_vfs::ListOptions::default()).await
    }

    async fn stat_with(&self, p: &VPath, opt: &norte_vfs::ListOptions) -> Result<Entry, Error> {
        let aref = self.split(p)?;
        let cached = self.index_for(&aref).await?;
        cached
            .index
            .entry_for(p, &Self::inner_key(&aref), &opt.attrs)
    }

    /// Catálogo por FORMATO (#108 bloque 2): zip retiene method/crc/packed
    /// en su CD; tar/tar.gz no tienen method ni CRC por entrada (y el packed
    /// por entrada de un stream gz sólido no significa nada) → vacío.
    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        match self.format {
            Format::Zip => catalogo_zip(),
            Format::Tar | Format::TarGz => &[],
        }
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
    /// Desde #59 el CD de zip se parsea con parser PROPIO: nombres crudos
    /// distintos que decodifican igual NO colapsan (H1 cerrado) y el extra
    /// Info-ZIP 0x7075 se ignora por diseño — jamás sustituye el nombre ni
    /// mata el archivo (H3 cerrado).
    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.list_with(p, &norte_vfs::ListOptions::default()).await
    }

    async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
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
            entries.push(index.entry_for(&child_path, &child_key, &opt.attrs));
        }
        Ok(futures::stream::iter(entries).boxed())
    }

    /// Total de omitidas del índice del CONTENEDOR de `p` (#93): las que
    /// cuenta el `skipped` del índice interno (nombres hostiles/límites
    /// por-entrada, contrato de [`Self::list`]). Reutiliza el índice
    /// cacheado — tras un `list` es una consulta barata.
    async fn list_skipped(&self, p: &VPath) -> Result<Option<u64>, Error> {
        let aref = self.split(p)?;
        let cached = self.index_for(&aref).await?;
        Ok(Some(cached.index.skipped))
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
                // Passthrough: datos contiguos sin comprimir. Con contador
                // entregado-vs-prometido (#97): un contenedor truncado/mutado
                // BAJO el read termina corto con semántica pread y sin error
                // — la clase exacta de silencio que zip (#95.4) y targz
                // (FIX-1 #55) ya fail-loudean.
                let inner = self
                    .inner
                    .read(
                        &aref.outer,
                        Some(ByteRange {
                            offset: offset + req_off,
                            len: Some(req_len),
                        }),
                    )
                    .await?;
                Ok(expect_exact(inner, req_len, aref.outer.display_lossy()))
            }
            Locator::Zip {
                header_offset,
                method,
                crc32,
                comp_size,
                uncomp_size,
            } => {
                self.read_zip(
                    &aref,
                    crate::zip_format::ReadPlan {
                        header_offset,
                        method,
                        crc32,
                        comp_size,
                        uncomp_size,
                        // El tamaño de la MISMA generación que el índice:
                        // vista coherente aunque el contenedor cambie.
                        container_len: cached.index.generation.1.unwrap_or(0),
                        skip: req_off,
                        take: req_len,
                    },
                )
                .await
            }
            Locator::Gz { offset, .. } => {
                self.read_gz(&aref, &cached, offset, req_off, req_len).await
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
