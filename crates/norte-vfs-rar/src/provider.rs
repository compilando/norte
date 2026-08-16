//! [`RarProvider`]: el `Provider` read-only de un `.rar`, servido por un
//! programa externo.
//!
//! No compone sobre otro provider: sostiene la RUTA LOCAL del archivo. Quién
//! puede montar un `rar` —solo sobre `file://`— lo decide el dispatch del
//! engine, que es donde se sabe qué hay al otro lado del scheme interior.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use norte_proto::{
    ByteRange, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};
use tokio_util::sync::CancellationToken;

use crate::delegate::Delegate;
use crate::index::{ArchiveIndex, InnerPath};
use crate::listing::Listing;
use crate::{LIST_TIMEOUT, RarLimits};

/// El provider de UN archivo `.rar`.
pub struct RarProvider {
    archive: PathBuf,
    delegate: Delegate,
    limits: RarLimits,
    /// Índice cacheado y su generación. Un solo archivo, un solo slot.
    cache: Mutex<Option<Arc<ArchiveIndex>>>,
    /// El índice viene puesto y NO se reconstruye: solo lo usan los tests que
    /// fijan política sin un `.rar` que la contenga.
    pinned: bool,
}

impl RarProvider {
    /// Un provider para el `.rar` que vive en `archive`, servido por
    /// `delegate`.
    #[must_use]
    pub fn new(archive: PathBuf, delegate: Delegate, limits: RarLimits) -> Self {
        Self {
            archive,
            delegate,
            limits,
            cache: Mutex::new(None),
            pinned: false,
        }
    }

    /// Un provider con el índice ya puesto: para tests que fijan política
    /// (una entrada cifrada, un nombre ambiguo) sin necesitar un `.rar` que
    /// los contenga de verdad.
    #[must_use]
    pub fn with_index_for_test(index: ArchiveIndex) -> Self {
        Self {
            archive: PathBuf::from("/dev/null"),
            delegate: Delegate::SevenZip(PathBuf::from("/nonexistent/7z")),
            limits: RarLimits::default(),
            cache: Mutex::new(Some(Arc::new(index))),
            pinned: true,
        }
    }

    /// Desmonta el path y comprueba que habla de ESTE provider.
    fn split(p: &VPath) -> Result<InnerPath, Error> {
        match p.archive_split() {
            Ok(Some(aref)) if aref.format == "rar" => {
                Ok(aref.inner.iter().map(|s| s.as_bytes().to_vec()).collect())
            }
            _ => Err(Error::InvalidPath),
        }
    }

    /// `(mtime_ms, size)` del `.rar`: la moneda de invalidación.
    ///
    /// El `metadata` es I/O bloqueante, así que va a `spawn_blocking` (regla
    /// 2). Sin mtime nada se cachea: un índice rancio enseña ficheros que ya
    /// no están.
    async fn generation(&self) -> Result<(Option<i64>, Option<u64>), Error> {
        let path = self.archive.clone();
        let meta = tokio::task::spawn_blocking(move || std::fs::metadata(&path))
            .await
            .map_err(|_| Error::Internal { panic: true })?
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Error::NotFound,
                std::io::ErrorKind::PermissionDenied => Error::PermissionDenied,
                _ => Error::Io { retryable: false },
            })?;
        if !meta.is_file() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok());
        Ok((mtime_ms, Some(meta.len())))
    }

    /// El índice, de caché o reconstruido llamando al delegado.
    async fn index(&self) -> Result<Arc<ArchiveIndex>, Error> {
        if self.pinned {
            let cache = self.cache.lock().expect("cache lock sano");
            return Ok(Arc::clone(cache.as_ref().expect("índice fijado")));
        }
        let generation = self.generation().await?;
        if generation.0.is_some() {
            let cache = self.cache.lock().expect("cache lock sano");
            if let Some(hit) = cache.as_ref().filter(|i| i.generation == generation) {
                return Ok(Arc::clone(hit));
            }
        }
        let argv = self.delegate.list_argv(&self.archive);
        let stdout = self
            .delegate
            .run_capture(&argv, LIST_TIMEOUT)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "el listado del rar falló");
                Error::from(e)
            })?;
        let Listing { entries, skipped } = match self.delegate {
            Delegate::SevenZip(_) => crate::parse_7z_slt(&stdout),
            Delegate::Unrar(_) => crate::parse_unrar_vt(&stdout),
        };
        let index = Arc::new(ArchiveIndex::build(
            entries,
            &self.limits,
            generation,
            skipped,
        ));
        if generation.0.is_some() {
            *self.cache.lock().expect("cache lock sano") = Some(Arc::clone(&index));
        }
        Ok(index)
    }
}

#[async_trait]
impl Provider for RarProvider {
    fn scheme(&self) -> &'static str {
        "rar+file"
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
        let inner = Self::split(p)?;
        self.index().await?.entry_for(p, &inner)
    }

    /// Listado de un dir del árbol virtual.
    ///
    /// CONTRATO (ADR 0018 C2, y una regla más que es propia del RAR): no
    /// aparecen las entradas cuyo nombre no mapea a segmentos `VPath`
    /// (`..`, `.`, vacío, NUL, absoluto, componente `!`) NI las que el
    /// listado por líneas del delegado no puede llevar (un nombre con `\n`
    /// o `\r`). Todas se cuentan en [`Provider::list_skipped`].
    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        let inner = Self::split(p)?;
        let index = self.index().await?;
        if !inner.is_empty() {
            match index.node(&inner) {
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
        for name in index.children.get(&inner).into_iter().flatten() {
            let seg = norte_proto::Segment::new(name.clone())
                .expect("el índice solo contiene segmentos válidos");
            let child_path = p.join(seg);
            let mut child_key = inner.clone();
            child_key.push(name.clone());
            entries.push(index.entry_for(&child_path, &child_key));
        }
        Ok(futures::stream::iter(entries).boxed())
    }

    async fn list_skipped(&self, p: &VPath) -> Result<Option<u64>, Error> {
        Self::split(p)?;
        Ok(Some(self.index().await?.skipped()))
    }

    /// Lee UNA entrada haciendo que el delegado la escupa por `stdout`.
    ///
    /// Dos negativas explícitas antes de arrancar nada:
    ///
    /// - una entrada **cifrada** se lista pero no se lee: la contraseña no se
    ///   puede pedir (el hijo tiene `stdin` cerrado a propósito) y fingir que
    ///   el fichero está vacío sería peor;
    /// - un nombre que el delegado trataría como **patrón** y alcanzase a
    ///   otra entrada se rehúsa: el flujo equivocado es indistinguible del
    ///   correcto.
    ///
    /// El `range` se sirve descartando del flujo, porque una tubería no tiene
    /// seek: se pide lo mismo al delegado y se corta en cuanto hay bastante
    /// —matando al hijo—, en vez de esperar a que termine de descomprimir.
    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, Error> {
        let inner = Self::split(p)?;
        let index = self.index().await?;
        if inner.is_empty() {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let node = index.node(&inner).ok_or(Error::NotFound)?;
        if node.kind != EntryKind::File {
            return Err(Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            });
        }
        let name = inner.join(&b'/');
        if node.encrypted {
            tracing::warn!(
                name = ?String::from_utf8_lossy(&name),
                "entrada cifrada: se lista, pero leerla exigiría una contraseña que nadie puede teclear"
            );
            return Err(Error::Unsupported);
        }
        index.addressable(&name).map_err(Error::from)?;
        let argv = self.delegate.read_argv(&self.archive, &name);
        // El token es del stream: soltarlo mata al hijo (regla 3).
        let cancel = CancellationToken::new();
        let guard = cancel.clone().drop_guard();
        let stream = self
            .delegate
            .run_stream(&argv, cancel)
            .await
            .map_err(Error::from)?;
        Ok(apply_range(stream, range, guard).boxed())
    }

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

/// Recorta el flujo del delegado al `range` pedido y lo corta en cuanto está
/// servido. `guard` mata al hijo al soltarse: viaja DENTRO del stream para
/// que el corte temprano y el drop del consumidor tengan el mismo efecto.
fn apply_range(
    stream: ByteStream,
    range: Option<ByteRange>,
    guard: tokio_util::sync::DropGuard,
) -> impl futures::Stream<Item = Result<bytes::Bytes, Error>> + Send {
    let (to_skip, left) = match range {
        Some(r) => (r.offset, r.len),
        None => (0, None),
    };
    // El recorte va en el ESTADO del unfold, no capturado por el bloque async:
    // un `async move` copiaría los contadores en cada chunk y el recorte se
    // reaplicaría desde cero — el fallo lo delató `unused_assignments`.
    futures::stream::unfold(
        (Some(stream), guard, to_skip, left),
        |(stream, guard, mut to_skip, mut left)| async move {
            let mut stream = stream?;
            if left == Some(0) {
                return None; // servido: soltar el guard mata al hijo
            }
            loop {
                let chunk = match stream.next().await {
                    Some(Ok(c)) => c,
                    Some(Err(e)) => return Some((Err(e), (None, guard, to_skip, left))),
                    None => return None,
                };
                let chunk = if to_skip >= chunk.len() as u64 {
                    to_skip -= chunk.len() as u64;
                    continue;
                } else {
                    let start = usize::try_from(to_skip).unwrap_or(usize::MAX);
                    to_skip = 0;
                    chunk.slice(start..)
                };
                let chunk = match left {
                    Some(n) if (chunk.len() as u64) > n => {
                        let take = usize::try_from(n).unwrap_or(usize::MAX);
                        left = Some(0);
                        chunk.slice(..take)
                    }
                    Some(n) => {
                        left = Some(n - chunk.len() as u64);
                        chunk
                    }
                    None => chunk,
                };
                return Some((Ok(chunk), (Some(stream), guard, to_skip, left)));
            }
        },
    )
}

impl std::fmt::Debug for RarProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RarProvider")
            .field("archive", &self.archive)
            .field("delegate", &self.delegate)
            .finish_non_exhaustive()
    }
}
