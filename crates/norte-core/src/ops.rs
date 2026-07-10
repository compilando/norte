//! Operaciones compuestas del core (spec §5): copy/move/delete sobre el
//! contrato `Provider`. Los providers hacen operaciones simples; AQUÍ vive
//! la recursión, la política de colisión (stat contra el provider DESTINO)
//! y el chequeo de cancelación por chunk (regla dura 3).

use std::sync::Arc;

use futures::StreamExt;
use norte_proto::{ConflictKind, Entry, EntryKind, Error, Segment, VPath};
use norte_vfs::Provider;
use tokio_util::sync::CancellationToken;

use crate::observer::{Mutation, MutationObserver};
use crate::scheduler::TaskCtx;

/// Copia `from` → `to` (recursiva si es dir). La colisión se evalúa contra
/// el provider DESTINO (trampa del dominio: FS case-insensitive).
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
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    // Copiar un dir DENTRO de sí mismo produciría una copia anidada absurda.
    if Arc::ptr_eq(&src, &dst) && is_descendant(&to, &from) {
        return Err(Error::InvalidPath);
    }
    match dst.stat(&to).await {
        Ok(_) => {
            return Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            });
        }
        Err(Error::NotFound) => {}
        Err(e) => return Err(e),
    }
    let src_entry = src.stat(&from).await?;
    match src_entry.kind {
        EntryKind::File => {
            ctx.progress.update(|p| {
                p.bytes_total = src_entry.size;
                p.entries_total = Some(1);
                p.current = Some(from.clone());
            });
            copy_file(&*src, &*dst, &from, &to, src_entry.size, &observer, ctx).await?;
            ctx.progress.update(|p| p.entries_done = 1);
            Ok(())
        }
        EntryKind::Dir => copy_tree(&src, &dst, &from, &to, &observer, ctx).await,
        // M0: sin API de crear symlinks en el contrato Provider (deuda M1);
        // jamás se sigue el link para copiar el destino en su lugar.
        EntryKind::Symlink | EntryKind::Other => Err(Error::Unsupported),
    }
}

async fn copy_tree(
    src: &Arc<dyn Provider>,
    dst: &Arc<dyn Provider>,
    from: &VPath,
    to: &VPath,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    // Walk previo para totales de progreso (el usuario ve X/Y desde el inicio).
    let entries = walk(&**src, from, &ctx.cancel).await?;
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

    dst.mkdir(to).await?;
    observer.on_mutation(&Mutation::Created(to));
    ctx.progress.update(|p| {
        p.entries_done += 1;
        p.current = Some(to.clone());
    });

    for entry in &entries {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let target = rebase(&entry.path, from, to)?;
        ctx.progress
            .update(|p| p.current = Some(entry.path.clone()));
        match entry.kind {
            EntryKind::Dir => {
                dst.mkdir(&target).await?;
                observer.on_mutation(&Mutation::Created(&target));
            }
            EntryKind::File => {
                copy_file(
                    &**src,
                    &**dst,
                    &entry.path,
                    &target,
                    entry.size,
                    observer,
                    ctx,
                )
                .await?;
            }
            EntryKind::Symlink | EntryKind::Other => return Err(Error::Unsupported),
        }
        ctx.progress.update(|p| p.entries_done += 1);
    }
    Ok(())
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

    let mut stream = src.read(from).await?;
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
/// cross-provider = copy + delete del origen (spec §5).
///
/// Estado post-fallo del cross-provider: si el delete del origen falla a
/// mitad, el DESTINO ya está completo y el origen queda parcial — duplicado,
/// jamás pérdida de lo copiado. Limitación M0 (deuda): copy y delete hacen
/// walks separados; entradas creadas en el origen entre ambos se borran sin
/// haberse copiado. El journal M3 conducirá ambos desde un único plan.
#[tracing::instrument(skip_all, fields(from = %from.display_lossy(), to = %to.display_lossy()))]
pub(crate) async fn move_task(
    src: Arc<dyn Provider>,
    dst: Arc<dyn Provider>,
    from: VPath,
    to: VPath,
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
        // Colisión contra el provider destino ANTES del rename.
        match dst.stat(&to).await {
            Ok(_) => {
                return Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                });
            }
            Err(Error::NotFound) => {}
            Err(e) => return Err(e),
        }
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        ctx.progress.update(|p| {
            p.entries_total = Some(1);
            p.current = Some(from.clone());
        });
        src.rename(&from, &to).await?;
        observer.on_mutation(&Mutation::Renamed {
            from: &from,
            to: &to,
        });
        ctx.progress.update(|p| p.entries_done = 1);
        return Ok(());
    }
    copy_task(
        Arc::clone(&src),
        dst,
        from.clone(),
        to,
        Arc::clone(&observer),
        ctx,
    )
    .await?;
    delete_task(src, from, observer, ctx).await
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
