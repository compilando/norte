//! Operaciones compuestas del core (spec §5): copy/move/delete sobre el
//! contrato `Provider`. Los providers hacen operaciones simples; AQUÍ viven
//! la recursión, las políticas de colisión y symlinks (ADR 0005), los
//! reintentos con backoff y el chequeo de cancelación por chunk (regla 3).

use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use norte_proto::SymlinkPolicy;
use norte_proto::{CollisionPolicy, ConflictKind, Entry, EntryKind, Error, Segment, VPath};
use norte_vfs::{Provider, SymlinkKind};
use tokio_util::sync::CancellationToken;

use crate::engine::TransferOptions;
use crate::observer::{Mutation, MutationObserver};
use crate::scheduler::TaskCtx;

/// Reintentos máximos ante errores transitorios (ADR 0005).
const MAX_RETRIES: u32 = 3;
/// Base del backoff exponencial: 100 ms · 2^n, determinista (sin jitter).
const BACKOFF_BASE_MS: u64 = 100;

/// ¿Merece reintento? Solo lo explícitamente transitorio; el resto de
/// errores JAMÁS se reintenta (repetir un `Conflict` no lo arregla).
fn is_transient(e: &Error) -> bool {
    matches!(
        e,
        Error::ProviderUnavailable { retryable: true } | Error::Io { retryable: true }
    )
}

/// Reintenta una operación puntual de provider ante errores transitorios:
/// hasta [`MAX_RETRIES`] reintentos con backoff exponencial, cancelable
/// DURANTE la espera (regla 3: la cancelación jamás espera al backoff).
///
/// SOLO para operaciones idempotentes (`stat`/`read`/`read_link`). Reintentar
/// mutación cuyo efecto pudo aplicarse antes del error (timeout post-commit
/// en remotos M2) duplicaría efectos o perdería la entrada del journal —
/// mapear esa ambigüedad por operación es deuda de M2 (issue vinculada).
async fn with_retry<'a, T: 'a>(
    cancel: &CancellationToken,
    mut op: impl FnMut() -> futures::future::BoxFuture<'a, Result<T, Error>>,
) -> Result<T, Error> {
    let mut attempt = 0u32;
    loop {
        match op().await {
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) => {
                let delay = std::time::Duration::from_millis(BACKOFF_BASE_MS << attempt);
                attempt += 1;
                tokio::select! {
                    () = cancel.cancelled() => return Err(Error::Cancelled),
                    () = tokio::time::sleep(delay) => {}
                }
            }
            other => return other,
        }
    }
}

/// Resultado de colocar UNA hoja (archivo o symlink) en el destino.
#[derive(Debug, PartialEq, Eq)]
enum Placed {
    /// Transferida (quizá bajo un nombre alternativo, `RenameAuto`).
    Done,
    /// Saltada por política: el destino no se tocó; en un move, el ORIGEN
    /// debe conservarse.
    Skipped,
}

/// ¿La política tolera que el dir destino ya exista (merge)?
/// `Fail`/`Ask` mantienen el comportamiento estricto de M0.
fn merge_allowed(p: CollisionPolicy) -> bool {
    !matches!(p, CollisionPolicy::Fail | CollisionPolicy::Ask)
}

/// Nombre alternativo nº `n`: sufijo ` (n)` antes de la ÚLTIMA extensión
/// (split en el último `.` que no sea el primer byte — un dotfile no tiene
/// extensión). Byte-safe: jamás decodifica el nombre.
fn rename_auto_candidate(name: &[u8], n: u32) -> Vec<u8> {
    let dot = name.iter().rposition(|&b| b == b'.').filter(|&i| i > 0);
    let (stem, ext) = match dot {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, &[][..]),
    };
    let mut out = stem.to_vec();
    out.extend_from_slice(format!(" ({n})").as_bytes());
    out.extend_from_slice(ext);
    out
}

/// ¿`from` y `to` apuntan con toda probabilidad al MISMO nodo del provider?
/// Sobrescribir algo consigo mismo lo DESTRUYE (remove + read → NotFound):
/// hay que rechazarlo antes. Byte-igual siempre; en destino case-insensitive,
/// también la variante que solo difiere en caja (lowercase Unicode de std).
/// La identidad REAL ((dev,ino)/FileId) llega en M2 — hasta entonces este
/// guard es deliberadamente conservador y NO cubre pares de caja que el FS
/// pliegue de forma más ancha que `to_lowercase`.
fn same_node_likely(from: &VPath, to: &VPath, dst: &dyn Provider) -> bool {
    if from == to {
        return true;
    }
    if from.scheme() != to.scheme() || from.authority() != to.authority() {
        return false;
    }
    if dst
        .capabilities()
        .flags
        .contains(norte_proto::CapabilityFlags::CASE_SENSITIVE)
    {
        return false;
    }
    let a: Vec<&[u8]> = from.segments().collect();
    let b: Vec<&[u8]> = to.segments().collect();
    a.len() == b.len()
        && a.iter().zip(&b).all(|(x, y)| {
            x == y
                || match (std::str::from_utf8(x), std::str::from_utf8(y)) {
                    (Ok(x), Ok(y)) => x.to_lowercase() == y.to_lowercase(),
                    _ => false,
                }
        })
}

/// Resuelve la colisión de UNA hoja contra el provider DESTINO (trampa del
/// dominio: siempre contra el destino). `Ok(Some(path))` = copiar ahí;
/// `Ok(None)` = saltar por política.
///
/// Ventana TOCTOU residual documentada: entre este `stat` y el
/// remove/write posterior el destino puede cambiar. Sin pérdida silenciosa
/// (el `write()` del provider es create-new), pero el replace atómico llega
/// con `WriteOpts` en M2 (ADR 0005).
async fn resolve_collision(
    dst: &dyn Provider,
    to: &VPath,
    src_entry: &Entry,
    policy: CollisionPolicy,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Option<VPath>, Error> {
    let existing = match with_retry(&ctx.cancel, || dst.stat(to).boxed()).await {
        Err(Error::NotFound) => return Ok(Some(to.clone())),
        Ok(e) => e,
        Err(e) => return Err(e),
    };
    // Si el provider ecoa claves REALES (MemProvider), esto caza cualquier
    // plegado (caja Y normalización): el "colisionado" es el propio origen.
    if existing.path == src_entry.path {
        return Err(Error::InvalidPath);
    }
    match policy {
        // `Ask` de verdad llega con los diálogos del TUI (fase 5, ADR 0005).
        CollisionPolicy::Fail | CollisionPolicy::Ask => Err(Error::Conflict {
            conflict: ConflictKind::Exists,
        }),
        CollisionPolicy::Skip => Ok(None),
        CollisionPolicy::Overwrite => {
            overwrite_existing(dst, to, &existing, observer).await?;
            Ok(Some(to.clone()))
        }
        CollisionPolicy::Newer => match (src_entry.mtime_ms, existing.mtime_ms) {
            (Some(s), Some(d)) if s > d => {
                overwrite_existing(dst, to, &existing, observer).await?;
                Ok(Some(to.clone()))
            }
            (Some(_), Some(_)) => Ok(None),
            // Sin mtime comparable: jamás adivinar (ADR 0005).
            _ => Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }),
        },
        CollisionPolicy::RenameAuto => {
            let name = to.file_name().ok_or(Error::InvalidPath)?;
            let name = name.as_bytes().to_vec();
            for n in 1..=1000u32 {
                if ctx.cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let seg = Segment::new(rename_auto_candidate(&name, n))
                    .map_err(|_| Error::InvalidPath)?;
                let cand = to.with_file_name(seg).ok_or(Error::InvalidPath)?;
                match with_retry(&ctx.cancel, || dst.stat(&cand).boxed()).await {
                    Err(Error::NotFound) => return Ok(Some(cand)),
                    Ok(_) => {}
                    Err(e) => return Err(e),
                }
            }
            Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            })
        }
    }
}

/// Quita la hoja existente del destino para reemplazarla (Overwrite/Newer).
/// Jamás pisa un DIR con una hoja: eso es `TypeMismatch`, no política.
async fn overwrite_existing(
    dst: &dyn Provider,
    to: &VPath,
    existing: &Entry,
    observer: &Arc<dyn MutationObserver>,
) -> Result<(), Error> {
    if existing.kind == EntryKind::Dir {
        return Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        });
    }
    dst.remove(to).await?;
    observer.on_mutation(&Mutation::Removed(to));
    Ok(())
}

/// Crea el dir destino, o lo ACEPTA si ya existe como dir y la política
/// permite merge (spec: copiar dir sobre dir = fusionar, política por hoja).
async fn ensure_dir(
    dst: &dyn Provider,
    to: &VPath,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    match dst.mkdir(to).await {
        Ok(()) => {
            observer.on_mutation(&Mutation::Created(to));
            Ok(())
        }
        Err(Error::Conflict { .. }) if merge_allowed(opts.on_collision) => {
            let existing = with_retry(&ctx.cancel, || dst.stat(to).boxed()).await?;
            if existing.kind == EntryKind::Dir {
                Ok(())
            } else {
                Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                })
            }
        }
        Err(e) => Err(e),
    }
}

/// Copia una hoja ARCHIVO aplicando la política de colisión y reintentos
/// a nivel de archivo completo (un fallo transitorio reinicia el archivo;
/// el resume por offset llega en M2).
async fn copy_file_leaf(
    src: &dyn Provider,
    dst: &dyn Provider,
    entry: &Entry,
    to: &VPath,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Placed, Error> {
    let Some(target) = resolve_collision(dst, to, entry, opts.on_collision, observer, ctx).await?
    else {
        return Ok(Placed::Skipped);
    };
    copy_file_retrying(src, dst, &entry.path, &target, entry.size, observer, ctx).await?;
    Ok(Placed::Done)
}

/// Copia una hoja SYMLINK según la política (ADR 0005).
async fn copy_symlink_leaf(
    src: &dyn Provider,
    dst: &dyn Provider,
    entry: &Entry,
    to: &VPath,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Placed, Error> {
    match opts.symlinks {
        SymlinkPolicy::Skip => Ok(Placed::Skipped),
        SymlinkPolicy::Preserve => {
            let target_bytes =
                with_retry(&ctx.cancel, || src.read_link(&entry.path).boxed()).await?;
            let Some(target) =
                resolve_collision(dst, to, entry, opts.on_collision, observer, ctx).await?
            else {
                return Ok(Placed::Skipped);
            };
            // Kind `File`: el único OS donde importa (Windows) no declara
            // SYMLINKS en M1 — su provider responde Unsupported antes.
            dst.symlink(&target, &target_bytes, SymlinkKind::File)
                .await?;
            observer.on_mutation(&Mutation::Created(&target));
            Ok(Placed::Done)
        }
        SymlinkPolicy::Follow => {
            // Sondea el target ANTES de cualquier acción destructiva
            // (Overwrite borra el destino): un dir-symlink debe fallar sin
            // haber tocado nada. Soltar el stream libera el fd (testeado).
            match src.read(&entry.path, None).await {
                Ok(probe) => drop(probe),
                // El link apunta a un DIRECTORIO: seguirlo exige detección
                // de ciclos (visited set) — M2 (ADR 0005).
                Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                }) => return Err(Error::Unsupported),
                Err(e) => return Err(e),
            }
            let Some(target) =
                resolve_collision(dst, to, entry, opts.on_collision, observer, ctx).await?
            else {
                return Ok(Placed::Skipped);
            };
            // Tamaño desconocido (el stat describe el LINK, no el destino).
            // El target pudo cambiar tras el sondeo: se re-mapea igual.
            match copy_file_retrying(src, dst, &entry.path, &target, None, observer, ctx).await {
                Ok(()) => Ok(Placed::Done),
                Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                }) => Err(Error::Unsupported),
                Err(e) => Err(e),
            }
        }
    }
}

/// Copia `from` → `to` (recursiva si es dir) con las políticas de `opts`.
///
/// Cancelación/fallo a mitad de ÁRBOL: cada archivo individual queda completo
/// o sin rastro (contrato del sink), pero el subárbol ya copiado PERMANECE en
/// el destino — todo lo commiteado fue observado como `Created` (el undo del
/// journal M3 lo revertirá; hasta entonces es limpieza manual).
#[tracing::instrument(skip_all, fields(from = %from.display_lossy(), to = %to.display_lossy()))]
pub(crate) async fn copy_task(
    src: Arc<dyn Provider>,
    dst: Arc<dyn Provider>,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    // Copiar un dir DENTRO de sí mismo produciría una copia anidada absurda;
    // copiar algo SOBRE SÍ MISMO con Overwrite lo destruiría (hallazgo B1).
    if Arc::ptr_eq(&src, &dst) && (is_descendant(&to, &from) || same_node_likely(&from, &to, &*dst))
    {
        return Err(Error::InvalidPath);
    }
    let src_entry = with_retry(&ctx.cancel, || src.stat(&from).boxed()).await?;
    match src_entry.kind {
        EntryKind::File => {
            ctx.progress.update(|p| {
                p.bytes_total = src_entry.size;
                p.entries_total = Some(1);
                p.current = Some(from.clone());
            });
            copy_file_leaf(&*src, &*dst, &src_entry, &to, opts, &observer, ctx).await?;
            ctx.progress.update(|p| p.entries_done = 1);
            Ok(())
        }
        EntryKind::Symlink => {
            ctx.progress.update(|p| {
                p.entries_total = Some(1);
                p.current = Some(from.clone());
            });
            copy_symlink_leaf(&*src, &*dst, &src_entry, &to, opts, &observer, ctx).await?;
            ctx.progress.update(|p| p.entries_done = 1);
            Ok(())
        }
        EntryKind::Dir => {
            let entries = walk(&*src, &from, &ctx.cancel).await?;
            copy_tree(&src, &dst, &from, &to, &entries, opts, &observer, ctx)
                .await
                .map(|_skipped| ())
        }
        EntryKind::Other => Err(Error::Unsupported),
    }
}

/// Copia el árbol `from` → `to` según un plan de entradas YA walkeado
/// (el walk es del caller: el move lo reusa para el delete — issue #9).
/// Devuelve los paths de ORIGEN saltados por política (el move no debe
/// borrarlos).
#[allow(clippy::too_many_arguments)] // función interna del módulo, no API
async fn copy_tree(
    src: &Arc<dyn Provider>,
    dst: &Arc<dyn Provider>,
    from: &VPath,
    to: &VPath,
    entries: &[Entry],
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Vec<VPath>, Error> {
    let bytes_total: u64 = entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .filter_map(|e| e.size)
        .sum();
    let total = entries.len() as u64 + 1; // +1 por la raíz
    ctx.progress.update(|p| {
        p.bytes_total = Some(bytes_total);
        p.entries_total = Some(total);
    });

    ensure_dir(&**dst, to, opts, observer, ctx).await?;
    ctx.progress.update(|p| {
        p.entries_done += 1;
        p.current = Some(to.clone());
    });

    let mut skipped: Vec<VPath> = Vec::new();
    for entry in entries {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let target = rebase(&entry.path, from, to)?;
        ctx.progress
            .update(|p| p.current = Some(entry.path.clone()));
        match entry.kind {
            EntryKind::Dir => {
                ensure_dir(&**dst, &target, opts, observer, ctx).await?;
            }
            EntryKind::File => {
                if copy_file_leaf(&**src, &**dst, entry, &target, opts, observer, ctx).await?
                    == Placed::Skipped
                {
                    // La barra debe poder llegar a 100%: lo saltado no cuenta.
                    ctx.progress.update(|p| {
                        p.bytes_total = p
                            .bytes_total
                            .map(|t| t.saturating_sub(entry.size.unwrap_or(0)));
                    });
                    skipped.push(entry.path.clone());
                }
            }
            EntryKind::Symlink => {
                if copy_symlink_leaf(&**src, &**dst, entry, &target, opts, observer, ctx).await?
                    == Placed::Skipped
                {
                    skipped.push(entry.path.clone());
                }
            }
            EntryKind::Other => return Err(Error::Unsupported),
        }
        ctx.progress.update(|p| p.entries_done += 1);
    }
    Ok(skipped)
}

/// Copia UN archivo con reintentos a nivel de archivo: un fallo transitorio
/// a mitad de stream reinicia el archivo entero (el sink ya abortó limpio) y
/// devuelve el progreso de bytes al punto de partida.
async fn copy_file_retrying(
    src: &dyn Provider,
    dst: &dyn Provider,
    from: &VPath,
    to: &VPath,
    known_size: Option<u64>,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let mut attempt = 0u32;
    loop {
        let before = ctx.progress.snapshot().bytes_done;
        match copy_file(src, dst, from, to, known_size, observer, ctx).await {
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) && !ctx.cancel.is_cancelled() => {
                ctx.progress.update(|p| p.bytes_done = before);
                let delay = std::time::Duration::from_millis(BACKOFF_BASE_MS << attempt);
                attempt += 1;
                tokio::select! {
                    () = ctx.cancel.cancelled() => return Err(Error::Cancelled),
                    () = tokio::time::sleep(delay) => {}
                }
            }
            other => return other,
        }
    }
}

/// Copia UN archivo: `copy_native` si el provider (el mismo a ambos lados)
/// declara `SERVER_COPY`; si no, streaming con cancelación por chunk y
/// limpieza garantizada vía `abort` del sink.
async fn copy_file(
    src: &dyn Provider,
    dst: &dyn Provider,
    from: &VPath,
    to: &VPath,
    known_size: Option<u64>,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if std::ptr::eq(
        std::ptr::from_ref(src).cast::<()>(),
        std::ptr::from_ref(dst).cast::<()>(),
    ) && src
        .capabilities()
        .flags
        .contains(norte_proto::CapabilityFlags::SERVER_COPY)
        && let Some(res) = src.copy_native(from, to).await
    {
        res?;
        // El tamaño ya lo dio el stat del origen: cero round-trips extra.
        let size = known_size.unwrap_or(0);
        ctx.progress.update(|p| p.bytes_done += size);
        observer.on_mutation(&Mutation::Created(to));
        return Ok(());
    }

    let mut stream = src.read(from, None).await?;
    let mut sink = dst.write(to).await?;
    while let Some(item) = stream.next().await {
        // Cancelación por chunk: destino limpio o `.norte-partial`, jamás
        // un archivo a medias sin marcar.
        if ctx.cancel.is_cancelled() {
            abort_traced(sink, to).await;
            return Err(Error::Cancelled);
        }
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                abort_traced(sink, to).await;
                return Err(e);
            }
        };
        let n = chunk.len() as u64;
        if let Err(e) = sink.write(chunk).await {
            abort_traced(sink, to).await;
            return Err(e);
        }
        ctx.progress.update(|p| p.bytes_done += n);
    }
    if ctx.cancel.is_cancelled() {
        abort_traced(sink, to).await;
        return Err(Error::Cancelled);
    }
    sink.commit().await?;
    observer.on_mutation(&Mutation::Created(to));
    Ok(())
}

/// Move: rename si origen y destino viven en el MISMO provider (0 bytes);
/// si el provider no puede (`Unsupported`: EXDEV entre montajes, remoto sin
/// rename) o es cross-provider, copy + delete del origen (spec §5).
///
/// El rename se INTENTA primero (no-replace atómico del provider) y la
/// política de colisión se aplica sobre su `Conflict` — así el case-rename
/// en FS insensitive jamás se confunde con una colisión real (el provider
/// lo resuelve por identidad) y `Overwrite` jamás borra el propio origen.
///
/// El copy y el delete se conducen desde UN plan (walk único, issue #9): el
/// delete borra EXACTAMENTE lo copiado, en post-order. Lo saltado por
/// política Y lo aparecido tras el walk sobreviven en el origen — jamás
/// pérdida silenciosa.
#[tracing::instrument(skip_all, fields(from = %from.display_lossy(), to = %to.display_lossy()))]
pub(crate) async fn move_task(
    src: Arc<dyn Provider>,
    dst: Arc<dyn Provider>,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if Arc::ptr_eq(&src, &dst) {
        if is_descendant(&to, &from) {
            return Err(Error::InvalidPath);
        }
        ctx.progress.update(|p| {
            p.entries_total = Some(1);
            p.current = Some(from.clone());
        });
        match rename_with_policy(&*src, &from, &to, opts, &observer, ctx).await {
            Ok(RenameOutcome::Renamed | RenameOutcome::SkippedByPolicy) => {
                ctx.progress.update(|p| p.entries_done = 1);
                return Ok(());
            }
            // El provider no sabe renombrar ESTO (EXDEV entre montajes es el
            // caso típico): degradar a copy+delete, como cross-provider.
            Err(Error::Unsupported) => {}
            Err(e) => return Err(e),
        }
    }
    move_by_copy(src, dst, from, to, opts, observer, ctx).await
}

/// Overwrite/Newer jamás cruzan tipos (ADR 0005): dir sobre hoja o
/// viceversa es `TypeMismatch`; dir sobre dir degrada a copy+delete
/// (merge) devolviendo `Unsupported` al caller del rename.
fn check_overwrite_kinds(src_e: &Entry, existing: &Entry) -> Result<(), Error> {
    let src_dir = src_e.kind == EntryKind::Dir;
    let dst_dir = existing.kind == EntryKind::Dir;
    if src_dir && dst_dir {
        // Merge de dirs: que lo haga el camino copy+delete.
        return Err(Error::Unsupported);
    }
    if src_dir != dst_dir {
        return Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        });
    }
    Ok(())
}

enum RenameOutcome {
    Renamed,
    SkippedByPolicy,
}

/// Rename same-provider aplicando la política de colisión sobre el
/// `Conflict` del rename no-replace del provider.
async fn rename_with_policy(
    src: &dyn Provider,
    from: &VPath,
    to: &VPath,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<RenameOutcome, Error> {
    let first = src.rename(from, to).await;
    let conflict = match first {
        Ok(()) => {
            observer.on_mutation(&Mutation::Renamed { from, to });
            return Ok(RenameOutcome::Renamed);
        }
        Err(e @ Error::Conflict { .. }) => e,
        Err(e) => return Err(e),
    };
    match opts.on_collision {
        CollisionPolicy::Fail | CollisionPolicy::Ask => Err(conflict),
        CollisionPolicy::Skip => Ok(RenameOutcome::SkippedByPolicy),
        CollisionPolicy::Overwrite => {
            let src_e = with_retry(&ctx.cancel, || src.stat(from).boxed()).await?;
            let existing = with_retry(&ctx.cancel, || src.stat(to).boxed()).await?;
            check_overwrite_kinds(&src_e, &existing)?;
            overwrite_existing(src, to, &existing, observer).await?;
            src.rename(from, to).await?;
            observer.on_mutation(&Mutation::Renamed { from, to });
            Ok(RenameOutcome::Renamed)
        }
        CollisionPolicy::Newer => {
            let src_e = with_retry(&ctx.cancel, || src.stat(from).boxed()).await?;
            let existing = with_retry(&ctx.cancel, || src.stat(to).boxed()).await?;
            match (src_e.mtime_ms, existing.mtime_ms) {
                (Some(s), Some(d)) if s > d => {
                    check_overwrite_kinds(&src_e, &existing)?;
                    overwrite_existing(src, to, &existing, observer).await?;
                    src.rename(from, to).await?;
                    observer.on_mutation(&Mutation::Renamed { from, to });
                    Ok(RenameOutcome::Renamed)
                }
                (Some(_), Some(_)) => Ok(RenameOutcome::SkippedByPolicy),
                _ => Err(conflict),
            }
        }
        CollisionPolicy::RenameAuto => {
            let name = to.file_name().ok_or(Error::InvalidPath)?;
            let name = name.as_bytes().to_vec();
            for n in 1..=1000u32 {
                if ctx.cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let seg = Segment::new(rename_auto_candidate(&name, n))
                    .map_err(|_| Error::InvalidPath)?;
                let cand = to.with_file_name(seg).ok_or(Error::InvalidPath)?;
                match src.rename(from, &cand).await {
                    Ok(()) => {
                        observer.on_mutation(&Mutation::Renamed { from, to: &cand });
                        return Ok(RenameOutcome::Renamed);
                    }
                    Err(Error::Conflict { .. }) => {}
                    Err(e) => return Err(e),
                }
            }
            Err(conflict)
        }
    }
}

/// Move por copy + delete con plan único: el walk de la copia ES la lista
/// del delete. Lo saltado por política queda en el origen (junto con sus
/// dirs ancestros).
async fn move_by_copy(
    src: Arc<dyn Provider>,
    dst: Arc<dyn Provider>,
    from: VPath,
    to: VPath,
    opts: TransferOptions,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if Arc::ptr_eq(&src, &dst) && (is_descendant(&to, &from) || same_node_likely(&from, &to, &*dst))
    {
        return Err(Error::InvalidPath);
    }
    let src_entry = with_retry(&ctx.cancel, || src.stat(&from).boxed()).await?;
    match src_entry.kind {
        EntryKind::File | EntryKind::Symlink => {
            ctx.progress.update(|p| {
                p.bytes_total = src_entry.size;
                // 2 pasos: copiar + borrar el origen.
                p.entries_total = Some(2);
                p.current = Some(from.clone());
            });
            let placed = if src_entry.kind == EntryKind::File {
                copy_file_leaf(&*src, &*dst, &src_entry, &to, opts, &observer, ctx).await?
            } else {
                copy_symlink_leaf(&*src, &*dst, &src_entry, &to, opts, &observer, ctx).await?
            };
            ctx.progress.update(|p| p.entries_done = 1);
            if placed == Placed::Skipped {
                // No copiado ⇒ no se borra: el origen se conserva.
                ctx.progress.update(|p| p.entries_done = 2);
                return Ok(());
            }
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            src.remove(&from).await?;
            observer.on_mutation(&Mutation::Removed(&from));
            ctx.progress.update(|p| p.entries_done = 2);
            Ok(())
        }
        EntryKind::Dir => {
            let entries = walk(&*src, &from, &ctx.cancel).await?;
            let skipped = copy_tree(&src, &dst, &from, &to, &entries, opts, &observer, ctx).await?;
            // Fase delete: el total crece con los pasos de borrado (la barra
            // sigue monótona; copy_tree ya contó los suyos).
            ctx.progress.update(|p| {
                p.entries_total = p.entries_total.map(|t| t + entries.len() as u64 + 1);
            });
            // Borra EXACTAMENTE lo copiado, en post-order. Lo saltado (y sus
            // ancestros) y lo aparecido tras el walk sobreviven: ese remove
            // ni se intenta (skip) o falla con Conflict (aparecido).
            for e in entries.iter().rev() {
                if ctx.cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let keep = skipped.contains(&e.path)
                    || (e.kind == EntryKind::Dir
                        && skipped.iter().any(|s| is_descendant(s, &e.path)));
                if keep {
                    ctx.progress.update(|p| p.entries_done += 1);
                    continue;
                }
                ctx.progress.update(|p| p.current = Some(e.path.clone()));
                src.remove(&e.path).await?;
                observer.on_mutation(&Mutation::Removed(&e.path));
                ctx.progress.update(|p| p.entries_done += 1);
            }
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if skipped.is_empty() {
                src.remove(&from).await?;
                observer.on_mutation(&Mutation::Removed(&from));
            }
            ctx.progress.update(|p| p.entries_done += 1);
            Ok(())
        }
        EntryKind::Other => Err(Error::Unsupported),
    }
}

/// Delete recursivo post-order (los hijos caen antes que su padre).
/// Cancelar a mitad deja el resto del árbol intacto (la raíz cae la última).
#[tracing::instrument(skip_all, fields(path = %path.display_lossy()))]
pub(crate) async fn delete_task(
    provider: Arc<dyn Provider>,
    path: VPath,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let entry = provider.stat(&path).await?;
    if entry.kind == EntryKind::Dir {
        let entries = walk(&*provider, &path, &ctx.cancel).await?;
        ctx.progress
            .update(|p| p.entries_total = Some(entries.len() as u64 + 1));
        // El walk emite cada padre antes que sus hijos: recorrerlo al revés
        // ES el post-order (todo dir llega vacío a su remove).
        for e in entries.iter().rev() {
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            ctx.progress.update(|p| p.current = Some(e.path.clone()));
            provider.remove(&e.path).await?;
            observer.on_mutation(&Mutation::Removed(&e.path));
            ctx.progress.update(|p| p.entries_done += 1);
        }
    } else {
        ctx.progress.update(|p| p.entries_total = Some(1));
    }
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    provider.remove(&path).await?;
    observer.on_mutation(&Mutation::Removed(&path));
    ctx.progress.update(|p| p.entries_done += 1);
    Ok(())
}

/// Recorre el árbol bajo `root` (sin incluirlo). Garantía de orden: todo
/// directorio aparece ANTES que cualquiera de sus descendientes.
async fn walk(
    provider: &dyn Provider,
    root: &VPath,
    cancel: &CancellationToken,
) -> Result<Vec<Entry>, Error> {
    let mut out = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut stream = provider.list(&dir).await?;
        while let Some(item) = stream.next().await {
            // Inner loop de verdad (regla 3): un dir de 10^6 entradas o un
            // provider lento no pueden retrasar la cancelación al pop.
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let entry = item?;
            if entry.kind == EntryKind::Dir {
                pending.push(entry.path.clone());
            }
            out.push(entry);
        }
    }
    Ok(out)
}

/// Reubica `path` (descendiente de `from`) bajo `to`, segmento a segmento.
fn rebase(path: &VPath, from: &VPath, to: &VPath) -> Result<VPath, Error> {
    let prefix_len = from.segments().count();
    let mut target = to.clone();
    for seg in path.segments().skip(prefix_len) {
        // Invariante: los segmentos vienen de un VPath ya validado.
        let seg = Segment::new(seg.to_vec()).map_err(|_| Error::Internal { panic: false })?;
        target = target.join(seg);
    }
    Ok(target)
}

/// Aborta un sink dejando traza si la limpieza falla (posible
/// `.norte-partial` huérfano que el journal M3 barrerá).
async fn abort_traced(sink: Box<dyn norte_vfs::ByteSink>, to: &VPath) {
    if let Err(e) = sink.abort().await {
        tracing::warn!(
            path = %to.display_lossy(),
            error = %e,
            "abort del sink falló; posible staging huérfano"
        );
    }
}

/// `true` si `child` es descendiente PROPIO de `ancestor` (mismo scheme y
/// authority, prefijo estricto de segmentos).
fn is_descendant(child: &VPath, ancestor: &VPath) -> bool {
    if child.scheme() != ancestor.scheme() || child.authority() != ancestor.authority() {
        return false;
    }
    let a: Vec<&[u8]> = ancestor.segments().collect();
    let c: Vec<&[u8]> = child.segments().collect();
    c.len() > a.len() && c[..a.len()] == a[..]
}

#[cfg(test)]
mod tests {
    use super::rename_auto_candidate;

    #[test]
    fn rename_auto_respeta_la_extension() {
        assert_eq!(rename_auto_candidate(b"a.txt", 1), b"a (1).txt");
        assert_eq!(rename_auto_candidate(b"a.txt", 12), b"a (12).txt");
        assert_eq!(rename_auto_candidate(b"sin-ext", 1), b"sin-ext (1)");
        // Dotfile: el punto inicial NO es extensión.
        assert_eq!(rename_auto_candidate(b".bashrc", 1), b".bashrc (1)");
        // Solo la ÚLTIMA extensión (limitación documentada: tar.gz se parte).
        assert_eq!(
            rename_auto_candidate(b"archivo.tar.gz", 1),
            b"archivo.tar (1).gz"
        );
        // Byte-safe con nombres no-UTF8.
        assert_eq!(
            rename_auto_candidate(&[0xE9, b'.', b'd'], 2),
            &[0xE9, b' ', b'(', b'2', b')', b'.', b'd'][..]
        );
    }
}
