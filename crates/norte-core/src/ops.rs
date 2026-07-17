//! Operaciones compuestas del core (spec §5): copy/move/delete sobre el
//! contrato `Provider`. Los providers hacen operaciones simples; AQUÍ viven
//! la recursión, las políticas de colisión y symlinks (ADR 0005), los
//! reintentos con backoff y el chequeo de cancelación por chunk (regla 3).

use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use norte_proto::SymlinkPolicy;
use norte_proto::{
    CollisionPolicy, ConflictKind, DeleteMode, Entry, EntryKind, Error, Segment, VPath,
    VerifyPolicy,
};
use norte_vfs::{Provider, SymlinkKind};
use tokio_util::sync::CancellationToken;

use norte_vfs::{FollowLinks, NodeId};

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

/// Espera el backoff del intento `attempt`, cancelable DURANTE la espera
/// (regla 3: la cancelación jamás espera al backoff).
async fn backoff_or_cancel(cancel: &CancellationToken, attempt: u32) -> Result<(), Error> {
    let delay = std::time::Duration::from_millis(BACKOFF_BASE_MS << attempt);
    tokio::select! {
        () = cancel.cancelled() => Err(Error::Cancelled),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

/// Reintenta una operación puntual IDEMPOTENTE (`stat`/`read`/`read_link`/
/// `node_id`) ante errores transitorios: hasta [`MAX_RETRIES`] reintentos
/// con backoff exponencial cancelable.
///
/// Las MUTACIONES no pasan por aquí: tras un fallo transitorio su efecto
/// pudo haberse aplicado (timeout post-commit en remotos) y reintentarlas a
/// ciegas duplicaría efectos o mentiría al journal — usan los wrappers
/// `*_retrying` con desambiguación por operación (issue #17). Deuda
/// restante con issue: el COMMIT del write y el evento `Created` del mkdir
/// ambiguo (#32); `trash` no se reintenta (una op del OS, #32).
async fn with_retry<'a, T: 'a>(
    cancel: &CancellationToken,
    mut op: impl FnMut() -> futures::future::BoxFuture<'a, Result<T, Error>>,
) -> Result<T, Error> {
    let mut attempt = 0u32;
    loop {
        match op().await {
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) => {
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// `remove` con reintentos y desambiguación (issue #17): tras un fallo
/// transitorio el efecto pudo aplicarse — `NotFound` en el reintento
/// significa "ya no está", que ES el estado que el remove perseguía (lo
/// borrase nuestra primera aplicación o no, el journal registra un único
/// `Removed` verdadero).
async fn remove_retrying(
    p: &dyn Provider,
    path: &VPath,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut attempt = 0u32;
    let mut ambiguous = false;
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match p.remove(path).await {
            Ok(()) => return Ok(()),
            Err(Error::NotFound) if ambiguous => return Ok(()),
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) && !cancel.is_cancelled() => {
                ambiguous = true;
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// `mkdir` con reintentos (issue #17). La ambigüedad post-efecto NO se
/// resuelve aquí: un `Conflict` tras fallo transitorio puede ser nuestro
/// dir fantasma O uno preexistente — indistinguibles sin pre-stat. Se
/// devuelve `Conflict` y la política del caller decide: merge lo absorbe
/// ([`ensure_dir`]); `Fail`/`Ask` fallan EN SEGURO. Deuda journal
/// documentada: si el dir era nuestro no habrá evento `Created` — el undo
/// de M3 dejará un dir vacío de más, jamás pérdida (issue #32).
async fn mkdir_retrying(
    p: &dyn Provider,
    path: &VPath,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut attempt = 0u32;
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match p.mkdir(path).await {
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) && !cancel.is_cancelled() => {
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// `symlink` con reintentos y desambiguación (issue #17): un `Conflict`
/// tras fallo transitorio se verifica leyendo el link — si su target son
/// EXACTAMENTE nuestros bytes, es nuestra primera aplicación y cuenta como
/// éxito (un único `Created` para el journal). Target distinto = colisión
/// real.
///
/// Límites documentados: (a) un provider que CANONICALICE el target al
/// releerlo (Windows reconstruye desde el reparse buffer; SFTP exóticos)
/// daría falso negativo → `Conflict` fail-safe con el efecto aplicado y
/// un `Created` perdido para el journal (misma deuda que mkdir, #32);
/// (b) el kind no se verifica — un link preexistente con el MISMO target
/// y otro kind pasaría por nuestro (requiere transitorio + preexistencia
/// exacta; el target manda, jamás hay pérdida).
async fn symlink_retrying(
    dst: &dyn Provider,
    link: &VPath,
    target: &[u8],
    kind: SymlinkKind,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut attempt = 0u32;
    let mut ambiguous = false;
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match dst.symlink(link, target, kind).await {
            Ok(()) => return Ok(()),
            Err(e @ Error::Conflict { .. }) if ambiguous => {
                return match with_retry(cancel, || dst.read_link(link).boxed()).await {
                    Ok(bytes) if bytes == target => Ok(()),
                    Err(Error::Cancelled) => Err(Error::Cancelled),
                    _ => Err(e),
                };
            }
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) && !cancel.is_cancelled() => {
                ambiguous = true;
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// `rename` con reintentos y desambiguación (issue #17): tras un fallo
/// transitorio, un `NotFound`/`Conflict` del reintento se verifica por
/// IDENTIDAD (`from_id`, capturada por el caller ANTES del primer intento):
/// destino = nodo original Y origen ausente ⇒ el rename se aplicó. Sin
/// identidad no se adivina: surge el error transitorio original (fail-safe;
/// el usuario reintenta contra el estado real).
async fn rename_retrying(
    p: &dyn Provider,
    from: &VPath,
    to: &VPath,
    from_id: Option<NodeId>,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    let mut attempt = 0u32;
    let mut last_transient: Option<Error> = None;
    loop {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let err = match p.rename(from, to).await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        match err {
            Error::NotFound | Error::Conflict { .. } if last_transient.is_some() => {
                return match rename_applied(p, from, to, from_id, cancel).await? {
                    Some(true) => Ok(()),
                    // Verificado: NO se aplicó — el error es genuino (la
                    // política de colisión del caller sigue funcionando).
                    Some(false) => Err(err),
                    // Inverificable: el transitorio original es la verdad.
                    None => Err(last_transient.take().unwrap_or(err)),
                };
            }
            e if attempt < MAX_RETRIES && is_transient(&e) && !cancel.is_cancelled() => {
                last_transient = Some(e);
                backoff_or_cancel(cancel, attempt).await?;
                attempt += 1;
            }
            e => return Err(e),
        }
    }
}

/// ¿Se aplicó el rename de verdad? `Some(true)` = el destino ES el nodo
/// original y el origen ya no existe. `Some(false)` = verificado que NO
/// (destino es OTRO nodo con el origen aún vivo, o el "destino" es un
/// hardlink del origen — id igual pero origen presente: eso no es un
/// rename aplicado). `None` = inverificable (sin identidad).
async fn rename_applied(
    p: &dyn Provider,
    from: &VPath,
    to: &VPath,
    from_id: Option<NodeId>,
    cancel: &CancellationToken,
) -> Result<Option<bool>, Error> {
    let Some(expected) = from_id else {
        return Ok(None);
    };
    let to_id = match with_retry(cancel, || p.node_id(to, FollowLinks::No).boxed()).await {
        Ok(Some(id)) => id,
        // Sin identidad del destino (o destino ausente): inverificable.
        Ok(None) | Err(Error::NotFound) => return Ok(None),
        Err(e) => return Err(e),
    };
    let from_gone = match with_retry(cancel, || p.stat(from).boxed()).await {
        Err(Error::NotFound) => true,
        Ok(_) => false,
        Err(e) => return Err(e),
    };
    match (to_id == expected, from_gone) {
        (true, true) => Ok(Some(true)),
        // Origen vivo: no hubo rename — o el destino es OTRO nodo
        // (conflicto real) o es un HARDLINK del origen (id igual, pero un
        // rename aplicado habría hecho desaparecer el dirent de origen).
        (_, false) => Ok(Some(false)),
        // Origen desaparecido y destino ajeno: estado irreconocible.
        (false, true) => Ok(None),
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

/// ¿`from` y `to` apuntan al MISMO nodo del provider? Sobrescribir algo
/// consigo mismo lo DESTRUYE (remove + read → NotFound): hay que
/// rechazarlo antes.
///
/// Con identidad real ([`Provider::node_id`], issue #16) el veredicto es
/// DEFINITIVO en ambos sentidos: ids iguales = mismo nodo (aunque el FS
/// pliegue caja/normalización más ancho que cualquier heurística); ids
/// distintos = nodos distintos (aunque los nombres solo difieran en caja —
/// el caso NTFS case-sensitive bajo WSL, que la heurística bloqueaba mal).
/// Sin identidad (`Ok(None)`), degrada a la heurística conservadora de M1.
/// Un error real de `node_id` aborta (fail-safe: ante la duda, nada
/// destructivo).
///
/// `follow_src`: bajo `SymlinkPolicy::Follow` lo que se copia es el
/// TARGET del origen — copiar `ln → f` con `ln` apuntando a `f` es
/// sobrescribir `f` consigo mismo (el remove del Overwrite lo destruiría
/// antes de leerlo a través del link): la identidad del origen se compara
/// RESUELTA (hallazgo del encoding-auditor, fase 1 M2).
async fn same_node(
    from: &VPath,
    to: &VPath,
    dst: &dyn Provider,
    follow_src: FollowLinks,
    cancel: &CancellationToken,
) -> Result<bool, Error> {
    if from == to {
        return Ok(true);
    }
    if from.scheme() != to.scheme() || from.authority() != to.authority() {
        return Ok(false);
    }
    let from_id = match with_retry(cancel, || dst.node_id(from, follow_src).boxed()).await {
        Ok(id) => id,
        // Sin origen (o link roto): no hay autodestrucción posible; su
        // stat/read posterior dará el error honesto.
        Err(Error::NotFound) => return Ok(false),
        Err(e) => return Err(e),
    };
    if let Some(a) = from_id {
        match with_retry(cancel, || dst.node_id(to, FollowLinks::No).boxed()).await {
            Ok(Some(b)) => return Ok(a == b),
            // Destino libre: nada que destruir.
            Err(Error::NotFound) => return Ok(false),
            // Identidad a medias (volumen mixto): cae a la heurística.
            Ok(None) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(same_node_heuristic(from, to, dst))
}

/// El modo de identidad del ORIGEN según la política de symlinks: bajo
/// `Follow` se copia el target, así que la identidad relevante es la
/// resuelta.
fn follow_links_for(opts: TransferOptions) -> FollowLinks {
    if opts.symlinks == SymlinkPolicy::Follow {
        FollowLinks::Yes
    } else {
        FollowLinks::No
    }
}

/// Heurística conservadora de M1 para providers sin identidad: byte-igual
/// siempre; en destino case-insensitive, también la variante que solo
/// difiere en caja (lowercase Unicode de std). NO cubre pliegues más
/// anchos del FS — por eso la identidad real tiene prioridad.
fn same_node_heuristic(from: &VPath, to: &VPath, dst: &dyn Provider) -> bool {
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
            overwrite_existing(dst, to, &existing, observer, ctx).await?;
            Ok(Some(to.clone()))
        }
        CollisionPolicy::Newer => match (src_entry.mtime_ms, existing.mtime_ms) {
            (Some(s), Some(d)) if s > d => {
                overwrite_existing(dst, to, &existing, observer, ctx).await?;
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
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if existing.kind == EntryKind::Dir {
        return Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        });
    }
    remove_retrying(dst, to, &ctx.cancel).await?;
    observer
        .on_mutation(&Mutation::Removed(to), &ctx.actor)
        .await?;
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
    match mkdir_retrying(dst, to, &ctx.cancel).await {
        Ok(()) => {
            observer
                .on_mutation(&Mutation::Created(to), &ctx.actor)
                .await?;
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
    copy_file_retrying(
        src,
        dst,
        &entry.path,
        &target,
        entry.size,
        opts,
        observer,
        ctx,
    )
    .await?;
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
            // `Unknown` (issue #18): el kind lo resuelve el provider DESTINO
            // best-effort contra su propio árbol; unix lo ignora gratis.
            symlink_retrying(
                dst,
                &target,
                &target_bytes,
                SymlinkKind::Unknown,
                &ctx.cancel,
            )
            .await?;
            observer
                .on_mutation(&Mutation::Created(&target), &ctx.actor)
                .await?;
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
            match copy_file_retrying(src, dst, &entry.path, &target, None, opts, observer, ctx)
                .await
            {
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
    // Bajo Follow, el "sí mismo" es el TARGET resuelto del origen.
    if Arc::ptr_eq(&src, &dst)
        && (is_descendant(&to, &from)
            || same_node(&from, &to, &*dst, follow_links_for(opts), &ctx.cancel).await?)
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
            // Dir-symlink raíz con Follow: se copia el ÁRBOL del target
            // como dir real (issue #19), no como hoja.
            if opts.symlinks == SymlinkPolicy::Follow
                && probe_symlink_target(&*src, &from, &ctx.cancel).await? == TargetKind::Dir
            {
                let plan = walk_following(&*src, &from, true, &ctx.cancel).await?;
                return copy_tree(&src, &dst, &from, &to, &plan, opts, &observer, ctx)
                    .await
                    .map(|_skipped| ());
            }
            ctx.progress.update(|p| {
                p.entries_total = Some(1);
                p.current = Some(from.clone());
            });
            copy_symlink_leaf(&*src, &*dst, &src_entry, &to, opts, &observer, ctx).await?;
            ctx.progress.update(|p| p.entries_done = 1);
            Ok(())
        }
        EntryKind::Dir => {
            let plan = plan_for(&*src, &from, opts, &ctx.cancel).await?;
            copy_tree(&src, &dst, &from, &to, &plan, opts, &observer, ctx)
                .await
                .map(|_skipped| ())
        }
        EntryKind::Other => Err(Error::Unsupported),
    }
}

/// Copia el árbol `from` → `to` según un plan YA walkeado (el walk es del
/// caller: el move lo reusa para el delete — issue #9). La copia ignora la
/// provenance (un dir sintético de un link expandido se crea como dir
/// real, issue #19); la provenance manda en el DELETE del move. Devuelve
/// los paths de ORIGEN saltados por política (el move no debe borrarlos).
#[allow(clippy::too_many_arguments)] // función interna del módulo, no API
async fn copy_tree(
    src: &Arc<dyn Provider>,
    dst: &Arc<dyn Provider>,
    from: &VPath,
    to: &VPath,
    plan: &[PlanEntry],
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<Vec<VPath>, Error> {
    let bytes_total: u64 = plan
        .iter()
        .filter(|pe| pe.entry.kind == EntryKind::File)
        .filter_map(|pe| pe.entry.size)
        .sum();
    let total = plan.len() as u64 + 1; // +1 por la raíz
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
    for pe in plan {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let entry = &pe.entry;
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

/// Copia UN archivo con reintentos a nivel de archivo. Con `resume=Off` un
/// fallo transitorio reinicia el archivo entero (el sink abortó limpio) y
/// devuelve el progreso al punto de partida. Con `resume=On` el parcial
/// SOBREVIVE (`keep`) y el reintento continúa desde donde iba
/// (`open_resumable`) — el `before` que se restaura es la base del archivo,
/// no cero, y `copy_file` recompone `base + already` en cada intento.
#[allow(clippy::too_many_arguments)] // función interna del módulo, no API
async fn copy_file_retrying(
    src: &dyn Provider,
    dst: &dyn Provider,
    from: &VPath,
    to: &VPath,
    known_size: Option<u64>,
    opts: TransferOptions,
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let base = ctx.progress.snapshot().bytes_done;
    let mut attempt = 0u32;
    loop {
        match copy_file(src, dst, from, to, known_size, base, opts, observer, ctx).await {
            Err(e) if attempt < MAX_RETRIES && is_transient(&e) && !ctx.cancel.is_cancelled() => {
                // Base del archivo: `copy_file` recompone `base + already`
                // (con resume, `already` crece; sin resume, vuelve a 0).
                ctx.progress.update(|p| p.bytes_done = base);
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

/// SHA-256 de los primeros `len` bytes del ORIGEN (#35, `VerifyPolicy::Hash`):
/// se compara con el digest del staging del destino para decidir si el
/// parcial sigue siendo válido. Lee `origen[0..len]` por stream (el mismo
/// coste que Length ahorra: Hash re-LEE el prefijo del origen, pero no lo
/// re-ESCRIBE).
///
/// `Ok(Some(d))` = digest del prefijo; `Ok(None)` = el origen es MÁS CORTO
/// que `len` (no hay prefijo que casar → el caller descarta). Un error REAL
/// de lectura (transitorio, permisos) se PROPAGA con `Err` — jamás se
/// confunde con "origen corto", que destruiría el parcial (M1 del reviewer).
/// Chequea cancelación por chunk (regla 3, M2 del reviewer): un parcial de
/// GiB no bloquea la Task.
async fn hash_source_prefix(
    src: &dyn Provider,
    from: &VPath,
    len: u64,
    ctx: &TaskCtx,
) -> Result<Option<[u8; 32]>, Error> {
    use sha2::{Digest, Sha256};
    let range = norte_proto::ByteRange {
        offset: 0,
        len: Some(len),
    };
    let mut stream = src.read(from, Some(range)).await?;
    let mut hasher = Sha256::new();
    let mut seen: u64 = 0;
    while let Some(item) = stream.next().await {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let chunk = item?;
        // El origen podría entregar de más si ignora el `len`: recorta al
        // prefijo exacto para que el digest cubra SOLO `origen[..len]`.
        let take = usize::try_from(len - seen)
            .unwrap_or(chunk.len())
            .min(chunk.len());
        hasher.update(&chunk[..take]);
        seen += take as u64;
        if seen >= len {
            break;
        }
    }
    if seen < len {
        return Ok(None); // origen más corto que el parcial
    }
    Ok(Some(hasher.finalize().into()))
}

/// ¿Descartar el parcial reanudable y empezar de cero? Decide según
/// `VerifyPolicy` (#35): Length compara tamaños; Hash compara el digest del
/// prefijo del origen con el del staging (si el provider lo expone, si no
/// degrada a Length).
#[allow(clippy::too_many_arguments)]
async fn should_discard_partial(
    src: &dyn Provider,
    dst: &dyn Provider,
    from: &VPath,
    to: &VPath,
    already: u64,
    known_size: Option<u64>,
    verify: VerifyPolicy,
    ctx: &TaskCtx,
) -> Result<bool, Error> {
    // Un parcial más largo que el origen nunca cuadra (ambas políticas), y
    // ahorra hashear: el origen cambió/encogió.
    if known_size.is_some_and(|size| already > size) {
        return Ok(true);
    }
    if already == 0 || verify == VerifyPolicy::Length {
        return Ok(false);
    }
    // Hash: sin digest del staging el provider no permite verificar → degrada
    // a Length (el check de tamaño de arriba ya se aplicó).
    let Some(partial_dig) = dst.partial_digest(to, already).await? else {
        return Ok(false);
    };
    // `None` = origen más corto que el parcial → descartar. Un error REAL se
    // propaga (`?`): jamás se traga como "descartar" (M1 del reviewer).
    match hash_source_prefix(src, from, already, ctx).await? {
        Some(src_dig) => Ok(src_dig != partial_dig),
        None => Ok(true),
    }
}

/// Copia UN archivo: `copy_native` si el provider (el mismo a ambos lados)
/// declara `SERVER_COPY`; si no, streaming con cancelación por chunk.
///
/// `base` = `bytes_done` ANTES de este archivo (para recomponer el progreso
/// al reanudar). Con resume: abre `open_resumable`, descarta el parcial si
/// no cuadra con el origen (`verify`, #35), lee el origen desde `already`, y
/// en cancelación/fallo CONSERVA el parcial (`keep`) en vez de abortar.
#[allow(clippy::too_many_arguments)] // función interna del módulo, no API
async fn copy_file(
    src: &dyn Provider,
    dst: &dyn Provider,
    from: &VPath,
    to: &VPath,
    known_size: Option<u64>,
    base: u64,
    opts: TransferOptions,
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
    {
        // Regla 3 (#51): copy_native es UN await potencialmente de minutos
        // (multipart copy S3, opendal lo trocea solo) — se racea contra la
        // cancelación. `biased` con el copy PRIMERO: una completación ya
        // observada se journaliza SIEMPRE aunque el token también esté
        // cancelado (regla 4); la cancelación no pierde latencia (el select
        // pollea ambas ramas en cada wakeup). Dropear el future a medias
        // jamás publica un objeto A MEDIAS (CopyObject es atómico; un
        // multipart incompleto no publica), pero quedan dos ambigüedades
        // documentadas (contrato en el rustdoc de `Provider::copy_native`):
        // - partes huérfanas facturables en S3: opendal solo aborta el
        //   multipart en su camino de error, no en drop — mismo caso que el
        //   Drop del sink de escritura (ADR 0016 E: lifecycle rule del bucket
        //   para AbortIncompleteMultipartUpload);
        // - si el server completa la copia DESPUÉS del drop, el destino queda
        //   con el objeto ÍNTEGRO sin entrada de journal (ambigüedad
        //   post-efecto, misma familia que #32) — nunca un parcial sin marcar.
        let native = tokio::select! {
            biased;
            res = src.copy_native(from, to) => res,
            () = ctx.cancel.cancelled() => return Err(Error::Cancelled),
        };
        if let Some(res) = native {
            res?;
            // El tamaño ya lo dio el stat del origen: cero round-trips extra.
            let size = known_size.unwrap_or(0);
            ctx.progress.update(|p| p.bytes_done = base + size);
            observer
                .on_mutation(&Mutation::Created(to), &ctx.actor)
                .await?;
            return Ok(());
        }
        // `None`: el provider declinó pese al cap — cae al streaming.
    }

    // Resume AGNÓSTICO del provider (ADR 0012 A2): `open_resumable` con su
    // default seguro `(write, 0)` degrada limpio en un provider sin
    // reanudación real; no se gatea por capability (S3 reanuda por
    // multipart, no por APPEND — M1 del rust-reviewer).
    let resume = opts.resume == norte_proto::ResumePolicy::On;
    // Abre el sink: reanudable (con offset ya durable) o fresco.
    let (mut sink, already) = if resume {
        let (sink, already) = dst.open_resumable(to).await?;
        // ¿Descartar el parcial y empezar de cero? El origen pudo cambiar
        // bajo los pies entre invocaciones (ADR 0012, #35):
        //   - Length: un parcial más largo que el origen no cuadra.
        //   - Hash: el prefijo `origen[..already]` no casa byte-a-byte con el
        //     del parcial. Si el provider no expone digest del staging,
        //     DEGRADA a Length (documentado en el trait).
        let discard =
            should_discard_partial(src, dst, from, to, already, known_size, opts.verify, ctx)
                .await?;
        if discard {
            // Propagar el fallo de abort (M2 del rust-reviewer): tragarlo y
            // seguir dejaría bytes obsoletos y el destino saldría corrupto.
            sink.abort().await?;
            let (fresh, fresh_already) = dst.open_resumable(to).await?;
            if fresh_already != 0 {
                // El staging sigue ahí tras el abort: no se puede reanudar
                // limpio — fallar en vez de publicar algo dudoso.
                return Err(Error::Io { retryable: false });
            }
            (fresh, 0)
        } else {
            (sink, already)
        }
    } else {
        (dst.write(to).await?, 0)
    };

    // El tramo ya presente cuenta como hecho de inmediato (la barra no
    // retrocede al reanudar).
    ctx.progress.update(|p| p.bytes_done = base + already);

    let range = (already > 0).then_some(norte_proto::ByteRange {
        offset: already,
        len: None,
    });
    let mut stream = match src.read(from, range).await {
        Ok(s) => s,
        Err(e) => {
            release(sink, to, resume).await;
            return Err(e);
        }
    };
    let mut written = base + already;
    while let Some(item) = stream.next().await {
        // Cancelación por chunk: destino limpio, o `.norte-partial`
        // reanudable (resume), jamás un archivo a medias sin marcar.
        if ctx.cancel.is_cancelled() {
            release(sink, to, resume).await;
            return Err(Error::Cancelled);
        }
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                release(sink, to, resume).await;
                return Err(e);
            }
        };
        let n = chunk.len() as u64;
        if let Err(e) = sink.write(chunk).await {
            release(sink, to, resume).await;
            return Err(e);
        }
        written += n;
        ctx.progress.update(|p| p.bytes_done = written);
    }
    if ctx.cancel.is_cancelled() {
        release(sink, to, resume).await;
        return Err(Error::Cancelled);
    }
    sink.commit().await?;
    observer
        .on_mutation(&Mutation::Created(to), &ctx.actor)
        .await?;
    Ok(())
}

/// Suelta el sink al interrumpir: `keep` (conserva el `.norte-partial`
/// reanudable) si hay resume, `abort` (destino limpio) si no.
async fn release(sink: Box<dyn norte_vfs::ByteSink>, to: &VPath, resume: bool) {
    let res = if resume {
        sink.keep().await
    } else {
        sink.abort().await
    };
    if let Err(e) = res {
        tracing::warn!(
            path = %to.display_lossy(),
            error = %e,
            "soltar el sink falló; posible staging huérfano"
        );
    }
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
    // Identidad del origen ANTES del primer intento: es lo único que puede
    // desambiguar un rename cuyo efecto se aplicó tras un timeout (#17).
    // Best-effort puro — la identidad solo VERIFICA: cualquier error aquí
    // degrada a None (el rename sigue funcionando como en M1 y dará su
    // propio error si el problema es real).
    let from_id = with_retry(&ctx.cancel, || src.node_id(from, FollowLinks::No).boxed())
        .await
        .ok()
        .flatten();
    let first = rename_retrying(src, from, to, from_id, &ctx.cancel).await;
    let conflict = match first {
        Ok(()) => {
            observer
                .on_mutation(&Mutation::Renamed { from, to }, &ctx.actor)
                .await?;
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
            overwrite_existing(src, to, &existing, observer, ctx).await?;
            rename_retrying(src, from, to, from_id, &ctx.cancel).await?;
            observer
                .on_mutation(&Mutation::Renamed { from, to }, &ctx.actor)
                .await?;
            Ok(RenameOutcome::Renamed)
        }
        CollisionPolicy::Newer => {
            let src_e = with_retry(&ctx.cancel, || src.stat(from).boxed()).await?;
            let existing = with_retry(&ctx.cancel, || src.stat(to).boxed()).await?;
            match (src_e.mtime_ms, existing.mtime_ms) {
                (Some(s), Some(d)) if s > d => {
                    check_overwrite_kinds(&src_e, &existing)?;
                    overwrite_existing(src, to, &existing, observer, ctx).await?;
                    rename_retrying(src, from, to, from_id, &ctx.cancel).await?;
                    observer
                        .on_mutation(&Mutation::Renamed { from, to }, &ctx.actor)
                        .await?;
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
                match rename_retrying(src, from, &cand, from_id, &ctx.cancel).await {
                    Ok(()) => {
                        observer
                            .on_mutation(&Mutation::Renamed { from, to: &cand }, &ctx.actor)
                            .await?;
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
    if Arc::ptr_eq(&src, &dst)
        && (is_descendant(&to, &from)
            || same_node(&from, &to, &*dst, follow_links_for(opts), &ctx.cancel).await?)
    {
        return Err(Error::InvalidPath);
    }
    let src_entry = with_retry(&ctx.cancel, || src.stat(&from).boxed()).await?;
    // ¿Se mueve como ÁRBOL? Un dir siempre; un dir-symlink raíz solo bajo
    // Follow (issue #19): su contenido se expande en el destino y en el
    // origen se borra EL LINK.
    let tree_plan = match src_entry.kind {
        EntryKind::Dir => Some(plan_for(&*src, &from, opts, &ctx.cancel).await?),
        EntryKind::Symlink
            if opts.symlinks == SymlinkPolicy::Follow
                && probe_symlink_target(&*src, &from, &ctx.cancel).await? == TargetKind::Dir =>
        {
            Some(walk_following(&*src, &from, true, &ctx.cancel).await?)
        }
        EntryKind::File | EntryKind::Symlink => None,
        EntryKind::Other => return Err(Error::Unsupported),
    };
    match tree_plan {
        None => {
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
            remove_retrying(&*src, &from, &ctx.cancel).await?;
            observer
                .on_mutation(&Mutation::Removed(&from), &ctx.actor)
                .await?;
            ctx.progress.update(|p| p.entries_done = 2);
            Ok(())
        }
        Some(plan) => {
            let skipped = copy_tree(&src, &dst, &from, &to, &plan, opts, &observer, ctx).await?;
            // Fase delete: el total crece con los pasos de borrado (la barra
            // sigue monótona; copy_tree ya contó los suyos).
            ctx.progress.update(|p| {
                p.entries_total = p.entries_total.map(|t| t + plan.len() as u64 + 1);
            });
            // Borra EXACTAMENTE lo copiado, en post-order. Lo saltado (y sus
            // ancestros) y lo aparecido tras el walk sobreviven: ese remove
            // ni se intenta (skip) o falla con Conflict (aparecido). La
            // provenance manda (issue #19): lo visto A TRAVÉS de un link es
            // del TARGET y jamás se borra; del link expandido se borra EL
            // LINK. Esquina DAG documentada: una hoja alcanzable por dos
            // caminos, saltada en uno y movida por el otro, termina solo en
            // el destino (sin pérdida: el contenido vive allí).
            for pe in plan.iter().rev() {
                if ctx.cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let keep = match pe.provenance {
                    Provenance::ViaLink => true,
                    Provenance::LinkRoot => {
                        skipped.iter().any(|s| is_descendant(s, &pe.entry.path))
                    }
                    Provenance::Real => {
                        skipped.contains(&pe.entry.path)
                            || (pe.entry.kind == EntryKind::Dir
                                && skipped.iter().any(|s| is_descendant(s, &pe.entry.path)))
                    }
                };
                if keep {
                    ctx.progress.update(|p| p.entries_done += 1);
                    continue;
                }
                ctx.progress
                    .update(|p| p.current = Some(pe.entry.path.clone()));
                remove_retrying(&*src, &pe.entry.path, &ctx.cancel).await?;
                observer
                    .on_mutation(&Mutation::Removed(&pe.entry.path), &ctx.actor)
                    .await?;
                ctx.progress.update(|p| p.entries_done += 1);
            }
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if skipped.is_empty() {
                // Raíz: para un dir, el dir ya vacío; para un dir-symlink
                // raíz bajo Follow, EL LINK (remove jamás sigue links).
                remove_retrying(&*src, &from, &ctx.cancel).await?;
                observer
                    .on_mutation(&Mutation::Removed(&from), &ctx.actor)
                    .await?;
            }
            ctx.progress.update(|p| p.entries_done += 1);
            Ok(())
        }
    }
}

/// Delete: `Trash` = UNA operación del provider sobre la raíz (el OS se
/// lleva el árbol entero — cancelable ANTES de disparar, no a mitad);
/// `Permanent` = recursivo post-order (los hijos caen antes que su padre;
/// cancelar a mitad deja el resto del árbol intacto, la raíz cae la
/// última). ADR 0009.
#[tracing::instrument(skip_all, fields(path = %path.display_lossy(), ?mode))]
pub(crate) async fn delete_task(
    provider: Arc<dyn Provider>,
    path: VPath,
    mode: DeleteMode,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if mode == DeleteMode::Trash {
        ctx.progress.update(|p| {
            p.entries_total = Some(1);
            p.current = Some(path.clone());
        });
        let dest = provider.trash(&path).await?;
        observer
            .on_mutation(
                &Mutation::Trashed {
                    path: &path,
                    dest: dest.as_ref(),
                },
                &ctx.actor,
            )
            .await?;
        ctx.progress.update(|p| p.entries_done = 1);
        return Ok(());
    }
    let entry = with_retry(&ctx.cancel, || provider.stat(&path).boxed()).await?;
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
            remove_retrying(&*provider, &e.path, &ctx.cancel).await?;
            observer
                .on_mutation(&Mutation::Removed(&e.path), &ctx.actor)
                .await?;
            ctx.progress.update(|p| p.entries_done += 1);
        }
    } else {
        ctx.progress.update(|p| p.entries_total = Some(1));
    }
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    remove_retrying(&*provider, &path, &ctx.cancel).await?;
    observer
        .on_mutation(&Mutation::Removed(&path), &ctx.actor)
        .await?;
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

/// Entrada del plan de copia: la provenance decide cómo la trata el
/// DELETE de un move (la copia la ignora — issue #19).
#[derive(Debug)]
struct PlanEntry {
    entry: Entry,
    provenance: Provenance,
}

/// Origen de una entrada del plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provenance {
    /// Dirent real del árbol origen: se borra en un move.
    Real,
    /// Dir-symlink REAL expandido por Follow: en un move se borra EL LINK
    /// (un solo remove), jamás su contenido.
    LinkRoot,
    /// Visto A TRAVÉS de un link expandido: pertenece al TARGET del link;
    /// un move jamás lo borra (el `LinkRoot` se lleva el link).
    ViaLink,
}

/// ¿A qué apunta un symlink?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    /// Archivo — o roto: la hoja Follow dará su error honesto al copiar.
    File,
    /// Directorio.
    Dir,
}

/// Sondea el tipo del target de un symlink SIN abrir el nodo: `list()`
/// valida con metadata que sigue el link (Ok = dir; `TypeMismatch` =
/// archivo u otro; `NotFound` = roto — la hoja Follow dará su error
/// honesto al copiar). Jamás `read()`: abrir un symlink→FIFO bloquearía
/// el hilo blocking sin respetar la cancelación (hallazgo M3 del
/// rust-reviewer). Soltar el stream cancela el listado.
async fn probe_symlink_target(
    provider: &dyn Provider,
    p: &VPath,
    cancel: &CancellationToken,
) -> Result<TargetKind, Error> {
    match with_retry(cancel, || provider.list(p).boxed()).await {
        Ok(probe) => {
            drop(probe);
            Ok(TargetKind::Dir)
        }
        // TypeMismatch = archivo/otro; NotFound = roto → en ambos casos la
        // hoja Follow decide (y dará su error honesto si aplica).
        Err(
            Error::Conflict {
                conflict: ConflictKind::TypeMismatch,
            }
            | Error::NotFound,
        ) => Ok(TargetKind::File),
        Err(e) => Err(e),
    }
}

/// Plan de un árbol: walk plano (todo `Real`) salvo bajo Follow, donde los
/// dir-symlinks se expanden con detección de ciclos (issue #19).
async fn plan_for(
    provider: &dyn Provider,
    root: &VPath,
    opts: TransferOptions,
    cancel: &CancellationToken,
) -> Result<Vec<PlanEntry>, Error> {
    if opts.symlinks == SymlinkPolicy::Follow {
        walk_following(provider, root, false, cancel).await
    } else {
        Ok(walk(provider, root, cancel)
            .await?
            .into_iter()
            .map(|entry| PlanEntry {
                entry,
                provenance: Provenance::Real,
            })
            .collect())
    }
}

/// Un directorio pendiente del walk con Follow: su path, la cadena de
/// identidades de sus ancestros (ciclos, spec §17.9) y si se llegó a él a
/// través de un link expandido.
struct DirFrame {
    dir: VPath,
    ancestors: Vec<NodeId>,
    via_link: bool,
}

/// Walk con expansión de dir-symlinks (`SymlinkPolicy::Follow`, issue
/// #19): cada symlink se sondea; los que apuntan a dir se convierten en
/// dirs sintéticos y se desciende A TRAVÉS del link. Un link cuyo target
/// resuelto ya está en la cadena de ancestros es un CICLO → [`Error::Loop`].
/// Expandir exige identidad ([`Provider::node_id`]): sin ella,
/// `Unsupported` — exactamente el comportamiento M1 (los árboles sin
/// dir-symlinks no la necesitan y siguen funcionando).
///
/// `root_is_link` = la raíz misma es un dir-symlink a expandir (todo el
/// contenido queda `ViaLink` y el move borra solo el link raíz).
async fn walk_following(
    provider: &dyn Provider,
    root: &VPath,
    root_is_link: bool,
    cancel: &CancellationToken,
) -> Result<Vec<PlanEntry>, Error> {
    // La identidad de la raíz abre la cadena de ancestros. Para una raíz
    // link es OBLIGATORIA (expandir sin visited set sería ruleta rusa);
    // para un dir normal, best-effort (sin ids solo fallará si aparece un
    // dir-symlink que expandir).
    let root_id =
        match with_retry(cancel, || provider.node_id(root, FollowLinks::Yes).boxed()).await? {
            Some(id) => Some(id),
            None if root_is_link => return Err(Error::Unsupported),
            None => None,
        };
    let mut out: Vec<PlanEntry> = Vec::new();
    let mut pending = vec![DirFrame {
        dir: root.clone(),
        ancestors: root_id.into_iter().collect(),
        via_link: root_is_link,
    }];
    while let Some(frame) = pending.pop() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut stream = provider.list(&frame.dir).await?;
        while let Some(item) = stream.next().await {
            // Inner loop de verdad (regla 3), como en walk().
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let entry = item?;
            let provenance = if frame.via_link {
                Provenance::ViaLink
            } else {
                Provenance::Real
            };
            match entry.kind {
                EntryKind::Dir => {
                    let id = with_retry(cancel, || {
                        provider.node_id(&entry.path, FollowLinks::Yes).boxed()
                    })
                    .await?;
                    let mut ancestors = frame.ancestors.clone();
                    ancestors.extend(id);
                    pending.push(DirFrame {
                        dir: entry.path.clone(),
                        ancestors,
                        via_link: frame.via_link,
                    });
                    out.push(PlanEntry { entry, provenance });
                }
                EntryKind::Symlink => {
                    match probe_symlink_target(provider, &entry.path, cancel).await? {
                        TargetKind::File => out.push(PlanEntry { entry, provenance }),
                        TargetKind::Dir => {
                            let Some(id) = with_retry(cancel, || {
                                provider.node_id(&entry.path, FollowLinks::Yes).boxed()
                            })
                            .await?
                            else {
                                return Err(Error::Unsupported);
                            };
                            if frame.ancestors.contains(&id) {
                                // Ciclo: seguirlo copiaría infinito. Categoría
                                // propia desde 0.4.0 (#31, ADR 0011).
                                return Err(Error::Loop);
                            }
                            let mut ancestors = frame.ancestors.clone();
                            ancestors.push(id);
                            pending.push(DirFrame {
                                dir: entry.path.clone(),
                                ancestors,
                                via_link: true,
                            });
                            // Dir SINTÉTICO: la copia crea un dir real en
                            // el destino; el mtime del link se conserva
                            // como referencia.
                            out.push(PlanEntry {
                                entry: Entry {
                                    path: entry.path,
                                    kind: EntryKind::Dir,
                                    size: None,
                                    mtime_ms: entry.mtime_ms,
                                },
                                provenance: if frame.via_link {
                                    Provenance::ViaLink
                                } else {
                                    Provenance::LinkRoot
                                },
                            });
                        }
                    }
                }
                EntryKind::File | EntryKind::Other => out.push(PlanEntry { entry, provenance }),
            }
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

    /// Rama NEGATIVA de la desambiguación de symlink (encoding-auditor,
    /// fixture 3): tras un transitorio, el Conflict con un link AJENO
    /// (target distinto) sigue siendo Conflict y el link ajeno queda
    /// intacto. Y la rama positiva desambigua igual con bytes no-UTF8.
    #[tokio::test]
    async fn symlink_retrying_no_adopta_links_ajenos_y_desambigua_bytes_crudos() {
        use futures::StreamExt as _;
        use norte_proto::Error;
        use norte_testkit::MemProvider;
        use norte_vfs::{Provider, SymlinkKind};
        use tokio_util::sync::CancellationToken;

        let mem = MemProvider::new();
        let root = MemProvider::root();
        let seg = |b: &[u8]| norte_proto::Segment::new(b.to_vec()).expect("segmento válido");
        let cancel = CancellationToken::new();

        // Negativa: link preexistente con OTRO target + transitorio previo.
        let ajeno = root.join(seg(b"ajeno"));
        mem.symlink(&ajeno, b"otro", SymlinkKind::File)
            .await
            .expect("symlink previo");
        mem.faults().unavailable_for_next(1);
        let res =
            super::symlink_retrying(&mem, &ajeno, b"nuestro", SymlinkKind::File, &cancel).await;
        assert!(
            matches!(res, Err(Error::Conflict { .. })),
            "un link ajeno jamás se adopta: {res:?}"
        );
        assert_eq!(
            mem.read_link(&ajeno).await.expect("intacto"),
            b"otro",
            "el link ajeno no se toca"
        );

        // Positiva con bytes CRUDOS no-UTF8 (regla 1: comparación por bytes).
        let crudo = root.join(seg(b"crudo"));
        mem.faults().ambiguous_mutations(1);
        super::symlink_retrying(&mem, &crudo, b"caf\xE9", SymlinkKind::File, &cancel)
            .await
            .expect("efecto aplicado + verificado por bytes = ok");
        assert_eq!(mem.read_link(&crudo).await.expect("existe"), b"caf\xE9");
        // El stream de list sigue vivo tras todo esto (sanidad).
        drop(mem.list(&root).await.expect("list ok").next().await);
    }

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
