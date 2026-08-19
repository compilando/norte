//! Task `index.embed` (M4-IA-2, ADR 0031 A3): embeddings de los ficheros ya
//! indexados. Filtra (`denied_prefixes`, heurística de texto, tamaño) ANTES de
//! leer nada; lee prefijos acotados por el provider (regla 2); sha256 del
//! prefijo decide re-embed; batches al proveedor con reintento acotado ante
//! rate-limit. Cancelación cooperativa por fichero (regla 3).

use std::sync::Arc;

use futures::StreamExt;
use norte_proto::{Error, VPath};
use norte_vfs::Provider;
use sha2::{Digest, Sha256};

use crate::scheduler::TaskCtx;

/// Bytes de prefijo que se embeben por fichero (v1, hard-coded — spec §IA-2).
pub(crate) const EMBED_PREFIX_BYTES: u64 = 32 * 1024;
/// Tamaño de batch hacia el proveedor (v1, hard-coded).
pub(crate) const EMBED_BATCH: usize = 16;
/// Ficheros mayores se saltan (el prefijo de un binario gigante no es texto útil).
pub(crate) const EMBED_MAX_FILE_SIZE: u64 = 8 * 1024 * 1024;
/// Reintentos ante `RateLimited` antes de fallar la task (spec: jamás cuelga).
pub(crate) const EMBED_RETRY_MAX: u32 = 3;

/// Extensiones consideradas texto (heurística v1). Bytes, no strings: los
/// nombres de fichero no son UTF-8 (regla 1).
const EMBED_TEXT_EXTS: &[&[u8]] = &[
    b"txt", b"md", b"rst", b"org", b"tex", b"rs", b"py", b"js", b"ts", b"tsx", b"jsx", b"go",
    b"java", b"kt", b"rb", b"php", b"pl", b"lua", b"c", b"h", b"cpp", b"hpp", b"cc", b"hh", b"cs",
    b"sh", b"bash", b"zsh", b"fish", b"toml", b"json", b"yaml", b"yml", b"xml", b"html", b"htm",
    b"css", b"sql", b"csv", b"ini", b"cfg", b"conf", b"log",
];

/// ¿Candidato a embedding? Decide SIN leer el contenido (heurística v1 por
/// extensión; sniffing de contenido es deuda declarada en la spec). Tamaño
/// desconocido (`None`) pasa: la lectura posterior está acotada igualmente.
pub(crate) fn is_text_candidate(path: &VPath, size: Option<u64>) -> bool {
    if size.is_some_and(|s| s > EMBED_MAX_FILE_SIZE) {
        return false;
    }
    let Some(name) = path.file_name() else {
        return false;
    };
    let bytes = name.as_bytes();
    let Some(dot) = bytes.iter().rposition(|b| *b == b'.') else {
        return false;
    };
    let ext = &bytes[dot + 1..];
    if ext.is_empty() || ext.len() > 4 {
        return false;
    }
    EMBED_TEXT_EXTS.contains(&ext.to_ascii_lowercase().as_slice())
}

/// Similitud coseno. `None` si las dimensiones difieren, un vector es nulo o
/// el resultado no es finito (cinturón: un `NaN` serializado por `serde_json`
/// se vuelve `null` y envenena la respuesta entera en el cliente — el score
/// del wire es SIEMPRE finito).
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    // `>` es falso para 0.0 y para NaN: ambos casos ⇒ None.
    if denom > 0.0 {
        let s = dot / denom;
        s.is_finite().then_some(s)
    } else {
        None
    }
}

/// Recorta `k` al rango del wire `[1, INDEX_SEMANTIC_MAX_K]`
/// (`index.search_semantic`).
pub(crate) fn clamp_k(k: u32) -> usize {
    // Invariante: MAX_K=100 cabe en usize en cualquier plataforma.
    usize::try_from(k.clamp(1, norte_proto::methods::INDEX_SEMANTIC_MAX_K))
        .expect("MAX_K cabe en usize")
}

/// Mapea un [`norte_index::IndexError`] a la taxonomía del wire (mismo
/// criterio que `index_build_as`: `BUSY`/`LOCKED` de `SQLite` ⇒ retryable).
pub(crate) fn index_to_proto(e: &norte_index::IndexError) -> Error {
    tracing::warn!(error = %e, "index.embed: error del índice");
    Error::Io {
        retryable: e.is_retryable(),
    }
}

/// Lee los primeros [`EMBED_PREFIX_BYTES`] de `path` vía el provider. El
/// `len` acota en el provider; el break + truncate son el cinturón por si
/// alguno entrega de más (espejo de `handle_plugin_preview`).
///
/// **Se vuelve a mirar QUÉ es antes de leerlo (#122).** El candidato viene de
/// la fila que dejó `index.build`, y entre aquel build y este embed cabe una
/// sustitución: quien pueda escribir en el árbol indexado cambia un `.txt` por
/// un enlace a un fichero DENEGADO, y sus 32 KiB se iban al proveedor de
/// embeddings — con lo que «ni un byte de un prefijo denegado se lee» dejaba
/// de ser cierto. `Provider::stat` describe el ENLACE y jamás su destino, así
/// que exigir `File` aquí cierra la puerta; lo que queda es la ventana entre
/// este `stat` y el `read`, que es la mitigación estándar y no la ausencia de
/// una.
///
/// Lo que esto NO cubre, y hay que decirlo: un ENLACE DURO al fichero
/// denegado. Tiene `kind = File` y una ruta que el filtro no reconoce, así que
/// pasa sin carrera ninguna. Cerrarlo pide comparar inodos contra el conjunto
/// denegado, que es otra cosa.
async fn read_prefix(provider: &dyn Provider, path: &VPath) -> Result<Vec<u8>, Error> {
    if provider.stat(path).await?.kind != norte_proto::EntryKind::File {
        tracing::debug!(
            path = %crate::engine::span_path(path),
            "index.embed: el candidato ya no es un fichero regular; no se lee"
        );
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::TypeMismatch,
        });
    }
    let range = norte_proto::ByteRange {
        offset: 0,
        len: Some(EMBED_PREFIX_BYTES),
    };
    let mut stream = provider.read(path, Some(range)).await?;
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 >= EMBED_PREFIX_BYTES {
            break;
        }
    }
    let cap = usize::try_from(EMBED_PREFIX_BYTES).unwrap_or(usize::MAX);
    bytes.truncate(cap.min(bytes.len()));
    Ok(bytes)
}

/// Envía `batch` al proveedor (con reintento acotado ante rate-limit) y
/// persiste los vectores. Deja `batch` vacío al completar. La espera de
/// reintento es cancel-aware (regla 3: el retry loop es un inner loop — un
/// cancel no debe esperar hasta 30s a que venza el sleep).
async fn flush_batch(
    embedder: &dyn norte_ai::AiProvider,
    index: &norte_index::Index,
    model: &str,
    batch: &mut Vec<(i64, String, [u8; 32])>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if batch.is_empty() {
        return Ok(());
    }
    let texts: Vec<String> = batch.iter().map(|(_, t, _)| t.clone()).collect();
    let mut attempt: u32 = 0;
    let vectors = loop {
        match embedder.embed(&texts).await {
            Ok(v) => break v,
            Err(norte_ai::AiError::RateLimited { retry_after })
                if attempt + 1 < EMBED_RETRY_MAX =>
            {
                // Espera acotada: lo que pida el server (clamp 30s) o 1s.
                let secs = retry_after.unwrap_or(1).min(30);
                tracing::debug!(attempt, secs, "proveedor rate-limited; reintentando");
                tokio::select! {
                    () = ctx.cancel.cancelled() => return Err(Error::Cancelled),
                    () = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {}
                }
                attempt += 1;
            }
            Err(e) => return Err(crate::engine::ai_to_proto_error(&e)),
        }
    };
    if vectors.len() != batch.len() {
        // Proveedor mentiroso: dijo N entradas y devolvió otra cosa. Zipear a
        // ciegas asociaría vectores a ficheros equivocados — mejor fallar.
        // Mala conducta del PROVEEDOR (no un bug nuestro): taxonomía
        // ProviderUnavailable, no retryable (repetir no lo arregla).
        tracing::warn!(
            expected = batch.len(),
            got = vectors.len(),
            "index.embed: el proveedor devolvió un número de vectores inesperado"
        );
        return Err(Error::ProviderUnavailable { retryable: false });
    }
    for ((file_id, _, hash), vec) in batch.iter().zip(vectors.iter()) {
        index
            .upsert_embedding(*file_id, model, vec, hash)
            .await
            .map_err(|e| index_to_proto(&e))?;
    }
    batch.clear();
    Ok(())
}

/// Cuerpo de la task `index.embed`: embebe los ficheros ya indexados de
/// `root`. El filtrado (denied → heurística de texto) ocurre ANTES de leer
/// ningún byte; el hash del prefijo decide si hay que re-embeber. Cancelable
/// entre ficheros con [`Error::Cancelled`] — los batches ya persistidos se
/// quedan (el índice es coherente en todo momento; el siguiente run los salta
/// por hash).
pub(crate) async fn embed_for_index(
    provider: Arc<dyn Provider>,
    embedder: norte_ai::SharedAiProvider,
    index: Arc<norte_index::Index>,
    root: VPath,
    model: String,
    denied: Vec<VPath>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let candidates = index
        .files_for_embed(&root)
        .await
        .map_err(|e| index_to_proto(&e))?;
    if candidates.is_empty() {
        // Defensa en profundidad: el engine ya pre-chequea esto en la
        // respuesta (NotFound sin build previo); aquí cubre la carrera con
        // un build concurrente que vació el root.
        return Err(Error::NotFound);
    }
    // Filtro ANTES de leer nada: primero denied_prefixes (ni un byte de un
    // prefijo denegado se lee ni sale — spec §9), luego la heurística de
    // texto/tamaño.
    let work: Vec<norte_index::EmbedCandidate> = candidates
        .into_iter()
        .filter(|c| !denied.iter().any(|d| crate::policy::is_under(d, &c.path)))
        .filter(|c| is_text_candidate(&c.path, c.size))
        .collect();
    let known = index
        .embedding_hashes(&root, &model)
        .await
        .map_err(|e| index_to_proto(&e))?;
    ctx.progress
        .update(|p| p.entries_total = Some(work.len() as u64));

    let mut batch: Vec<(i64, String, [u8; 32])> = Vec::new();
    for cand in work {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        ctx.progress.update(|p| {
            p.entries_done += 1;
            p.current = Some(cand.path.clone());
        });
        // Fichero ilegible: pudo morir entre el build y el embed — se salta
        // (cuenta como examinado), no tumba la task. Al log (path redactado
        // como los spans), jamás en silencio.
        let bytes = match read_prefix(provider.as_ref(), &cand.path).await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    path = %crate::engine::span_path(&cand.path),
                    "index.embed: prefijo ilegible; se salta"
                );
                continue;
            }
        };
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        if known
            .get(&cand.file_id)
            .is_some_and(|h| h.as_slice() == hash)
        {
            continue; // sin cambios para este modelo: nada que re-embeber.
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        batch.push((cand.file_id, text, hash));
        if batch.len() >= EMBED_BATCH {
            flush_batch(embedder.as_ref(), &index, &model, &mut batch, ctx).await?;
        }
    }
    // Chequeo también antes del flush final: un cancel llegado en la última
    // vuelta no debe disparar un batch más hacia el proveedor.
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    flush_batch(embedder.as_ref(), &index, &model, &mut batch, ctx).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire válido")
    }

    #[test]
    fn text_heuristic_by_extension() {
        // Extensión de texto conocida.
        assert!(is_text_candidate(&vp("mem:///a.txt"), Some(10)));
        // Case-insensitive.
        assert!(is_text_candidate(&vp("mem:///B.RS"), Some(10)));
        // Binario conocido → no.
        assert!(!is_text_candidate(&vp("mem:///c.bin"), Some(10)));
        // Sin punto → no.
        assert!(!is_text_candidate(&vp("mem:///noext"), Some(10)));
        // Sobre el tope de tamaño → no, aunque la extensión sea texto.
        assert!(!is_text_candidate(
            &vp("mem:///big.txt"),
            Some(EMBED_MAX_FILE_SIZE + 1)
        ));
        // Tamaño desconocido pasa (la lectura está acotada igualmente).
        assert!(is_text_candidate(&vp("mem:///a.md"), None));
        // Punto final (extensión vacía) → no.
        assert!(!is_text_candidate(&vp("mem:///raro."), Some(10)));
    }

    #[test]
    fn cosine_golden_order() {
        let q = [1.0f32, 0.0];
        assert!((cosine(&q, &[1.0, 0.0]).unwrap() - 1.0).abs() < 1e-6);
        assert!(cosine(&q, &[0.0, 1.0]).unwrap().abs() < 1e-6);
        assert!((cosine(&q, &[-1.0, 0.0]).unwrap() + 1.0).abs() < 1e-6);
        let mid = cosine(&q, &[1.0, 1.0]).unwrap();
        assert!(mid > 0.0 && mid < 1.0);
        // dim mismatch y vector nulo ⇒ None (se ignora, no rompe)
        assert!(cosine(&q, &[1.0]).is_none());
        assert!(cosine(&q, &[0.0, 0.0]).is_none());
        // no finito ⇒ None (cinturón: jamás un score NaN en el wire)
        assert!(cosine(&q, &[f32::NAN, 0.0]).is_none());
    }

    #[test]
    fn clamp_k_pins_wire_bounds() {
        assert_eq!(clamp_k(0), 1, "k=0 se recorta a 1");
        assert_eq!(clamp_k(1000), 100, "tope superior = INDEX_SEMANTIC_MAX_K");
        assert_eq!(clamp_k(50), 50, "dentro del rango pasa tal cual");
    }
}
