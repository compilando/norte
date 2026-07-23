//! Walk de indexación (M4, ADR 0034): recorre un subárbol vía
//! [`Provider::list`] y colecciona [`IndexEntry`] para
//! [`norte_index::Index::build`]. Modelo BFS de [`crate::search::run_walk`]:
//! confine al root (defensa en profundidad), chequeo de cancelación (regla 3),
//! y NO desciende symlinks (candidato de nombre, no se sigue → sin ciclos).

use std::collections::VecDeque;
use std::sync::Arc;

use futures::StreamExt;
use norte_index::IndexEntry;
use norte_proto::{EntryKind, Error, VPath};
use norte_vfs::Provider;

use crate::scheduler::TaskCtx;

/// Tope de entradas materializadas (anti-DoS; misma clase que las cotas de
/// `list`/`search`). Un árbol mayor falla `LimitExceeded` en vez de agotar RAM.
const MAX_INDEX_ENTRIES: usize = 5_000_000;

/// Recorre `root` y devuelve todas las entradas (dirs incluidos) como
/// [`IndexEntry`]. Cancelable: un token disparado corta con [`Error::Cancelled`]
/// (el `build` posterior no corre, así que el índice previo queda intacto).
pub(crate) async fn walk_for_index(
    provider: Arc<dyn Provider>,
    root: VPath,
    ctx: &TaskCtx,
) -> Result<Vec<IndexEntry>, Error> {
    let confine = root.clone();
    let mut queue: VecDeque<VPath> = VecDeque::new();
    queue.push_back(root);
    let mut out: Vec<IndexEntry> = Vec::new();

    while let Some(dir) = queue.pop_front() {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // Directorio ilegible: se salta (cuenta como examinado) y sigue.
        let Ok(mut stream) = provider.list(&dir).await else {
            ctx.progress.update(|p| p.entries_done += 1);
            continue;
        };
        while let Some(item) = stream.next().await {
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let Ok(entry) = item else {
                ctx.progress.update(|p| p.entries_done += 1);
                continue;
            };
            // Cinturón-y-tirantes: una entrada fuera del root se ignora POR
            // COMPLETO (el scope es invariante del core, no de la corrección del
            // provider) — mismo criterio que `search::run_walk`.
            if !crate::policy::is_under(&confine, &entry.path) {
                ctx.progress.update(|p| p.entries_done += 1);
                continue;
            }
            ctx.progress.update(|p| {
                p.entries_done += 1;
                p.current = Some(entry.path.clone());
            });
            // Descenso: dirs sí; symlinks NO (no se siguen → sin ciclos).
            if entry.kind == EntryKind::Dir {
                queue.push_back(entry.path.clone());
            }
            out.push(IndexEntry {
                path: entry.path,
                kind: entry.kind,
                size: entry.size,
                mtime_ms: entry.mtime_ms,
            });
            if out.len() > MAX_INDEX_ENTRIES {
                return Err(Error::LimitExceeded {
                    limit: Error::LIMIT_ENTRIES.to_owned(),
                });
            }
        }
    }
    Ok(out)
}
