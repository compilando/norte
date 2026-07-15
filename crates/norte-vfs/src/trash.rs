//! Papelera lógica `.norte-trash/` para providers SIN trash nativo del OS
//! (sftp, object storage). ADR 0019 (extiende 0009).
//!
//! Un provider gana papelera lógica declarando [`crate::CapabilityFlags`]`::TRASH` y
//! delegando su [`Provider::trash`] en [`logical_trash`] — sin conocer a los
//! demás providers (regla dura, ADR 0005). El movimiento es UNIFORME sobre
//! [`Provider::rename`]: atómico donde el backend lo ofrezca (sftp), o
//! copy+delete O(n) donde no (object) — cada backend aporta su semántica ya
//! auditada.

use bytes::Bytes;
use norte_proto::{ConflictKind, Segment};

use crate::{Error, Provider, VPath};

/// Nombre del directorio raíz de la papelera lógica (ADR 0019 D1).
const TRASH_DIR: &[u8] = b".norte-trash";
/// Subdirectorio con los nodos movidos.
const FILES_DIR: &[u8] = b"files";
/// Subdirectorio con los sidecars de procedencia.
const META_DIR: &[u8] = b"meta";
/// Cota de reintentos de `<id>` ante colisión (mismo instante de borrado).
const MAX_ID_RETRIES: u32 = 4096;

/// Mueve `target` (árbol entero si es dir) a `.norte-trash/files/<id>` en la
/// raíz del propio provider, con un sidecar de procedencia en
/// `.norte-trash/meta/<id>.json` (ADR 0019). Recuperable: es la base de la
/// papelera universal de la spec para backends sin trash del OS.
///
/// `now_ms` = instante del borrado (épocas ms), inyectado por el caller para
/// que el `<id>` y el sidecar sean deterministas y testeables. El `<id>` se
/// deriva de él a ancho fijo; una colisión (mismo ms, o reintento) se resuelve
/// con sufijo `-{n}` apoyándose en el contrato anti-sobrescritura de
/// [`Provider::rename`] (jamás pisa un `<id>` ocupado).
///
/// Orden (ADR 0019 D4): el DATO se mueve PRIMERO; el sidecar es best-effort
/// después. Un dato a salvo en `files/<id>` sin sidecar sigue siendo
/// recuperable; un sidecar sin dato es inútil. Nunca al revés.
///
/// # Errors
/// - [`Error::InvalidPath`] si `target` es la raíz del provider o ya vive
///   dentro de `.norte-trash/` (no se recicla la papelera; jamás un no-op
///   silencioso que engañe al caller sobre un borrado que no ocurrió).
/// - Lo que devuelvan `mkdir`/`rename` del provider (p. ej.
///   [`Error::NotFound`] si `target` no existe).
///
/// Nota de cancelación (ADR 0019 D5): para el engine esto es UNA operación
/// (`entries_total = 1`, cancelable antes de disparar). En object el `rename`
/// de un prefijo grande es internamente copy+delete O(n) e INCANCELABLE a
/// mitad — misma naturaleza que la excepción cross-device de ADR 0009 (#26).
///
/// Un provider sin trash del OS delega aquí su [`Provider::trash`] y declara
/// [`crate::CapabilityFlags`]`::TRASH`:
///
/// ```ignore
/// async fn trash(&self, p: &VPath) -> Result<(), Error> {
///     norte_vfs::logical_trash(self, p, now_ms()).await
/// }
/// ```
#[tracing::instrument(
    skip(provider),
    fields(target = %target.display_lossy(), now_ms)
)]
pub async fn logical_trash<P>(provider: &P, target: &VPath, now_ms: u64) -> Result<(), Error>
where
    P: Provider + ?Sized,
{
    // Guardas: ni la raíz ni algo YA dentro de la papelera son reciclables.
    if target.is_root() {
        return Err(Error::InvalidPath);
    }
    if target.segments().next() == Some(TRASH_DIR) {
        return Err(Error::InvalidPath);
    }

    // El nodo debe existir ANTES de crear el esqueleto de la papelera: sin
    // esto, un trash de un path inexistente dejaría `.norte-trash/` vacío atrás
    // solo para que `rename` acabe devolviendo NotFound.
    provider.stat(target).await?;

    // Raíz del provider derivada del propio destino (mismo namespace): subir
    // por parent() hasta agotar segmentos. NO se pasa como parámetro.
    let mut root = target.clone();
    while let Some(parent) = root.parent() {
        root = parent;
    }

    let trash_dir = root.join(seg(TRASH_DIR)?);
    let files_dir = trash_dir.join(seg(FILES_DIR)?);
    let meta_dir = trash_dir.join(seg(META_DIR)?);

    // mkdir idempotente de la jerarquía (un Conflict = ya existe).
    ensure_dir(provider, &trash_dir).await?;
    ensure_dir(provider, &files_dir).await?;
    ensure_dir(provider, &meta_dir).await?;

    // Mover el DATO primero, reintentando el `<id>` ante colisión.
    let mut n: u32 = 0;
    let id = loop {
        let id = trash_id(now_ms, n);
        let dest = files_dir.join(seg(id.as_bytes())?);
        match provider.rename(target, &dest).await {
            Ok(()) => break id,
            // SOLO `Exists` significa "ese `<id>` está ocupado, prueba otro".
            // Cualquier otro conflicto (TypeMismatch, subtipo futuro) es un
            // fallo real: propagar, no girar el bucle 4096 veces en balde.
            Err(Error::Conflict {
                conflict: ConflictKind::Exists,
            }) if n < MAX_ID_RETRIES => {
                n += 1;
            }
            Err(e) => return Err(e),
        }
    };

    // Sidecar de procedencia: best-effort. El dato YA está a salvo; un fallo
    // aquí pierde la procedencia (restauración manual en M3), jamás el dato,
    // así que NO se propaga como fallo del trash.
    if let Err(e) = write_meta(provider, &meta_dir, &id, target, now_ms).await {
        tracing::warn!(
            error = %e,
            "papelera: dato movido pero sidecar de procedencia falló"
        );
    }
    Ok(())
}

/// `<id>` determinista: hex de ancho fijo del instante + sufijo de reintento.
fn trash_id(now_ms: u64, retry: u32) -> String {
    if retry == 0 {
        format!("{now_ms:016x}")
    } else {
        format!("{now_ms:016x}-{retry}")
    }
}

/// Construye un [`Segment`] desde bytes de nombre constantes/generados. Un
/// nombre ilegal (jamás, dado el origen controlado) degrada a
/// [`Error::InvalidPath`] en vez de `panic`.
fn seg(bytes: &[u8]) -> Result<Segment, Error> {
    Segment::new(bytes.to_vec()).map_err(|_| Error::InvalidPath)
}

/// `mkdir` que trata «ya existe» como éxito (la papelera es idempotente).
/// SOLO `Exists`: si `.norte-trash` existe como ARCHIVO (`TypeMismatch`), es un
/// error real —lo surface, no lo enmascara con un rename confuso después.
async fn ensure_dir<P>(provider: &P, dir: &VPath) -> Result<(), Error>
where
    P: Provider + ?Sized,
{
    match provider.mkdir(dir).await {
        Ok(())
        | Err(Error::Conflict {
            conflict: ConflictKind::Exists,
        }) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Escribe `meta/<id>.json` con la procedencia (ADR 0019 D2). El wire del
/// origen (que NO es ASCII: un nombre UTF-8 válido viaja con sus bytes crudos,
/// p. ej. `é` = `C3 A9`) se guarda HEX-encoded: el hex sí es ASCII puro, se
/// embebe en JSON sin escapes ni dependencia de base64, y revierte
/// byte-a-byte (hex-decode → `from_utf8` infalible → `VPath::parse`).
async fn write_meta<P>(
    provider: &P,
    meta_dir: &VPath,
    id: &str,
    target: &VPath,
    now_ms: u64,
) -> Result<(), Error>
where
    P: Provider + ?Sized,
{
    let name = format!("{id}.json");
    let meta_path = meta_dir.join(seg(name.as_bytes())?);
    let body = format!(
        r#"{{"v":1,"orig_hex":"{}","deleted_at_ms":{now_ms}}}"#,
        hex(target.to_wire().as_bytes())
    );
    let mut sink = provider.write(&meta_path).await?;
    sink.write(Bytes::from(body.into_bytes())).await?;
    sink.commit().await
}

/// Hex en minúsculas de un slice de bytes (sin dependencia externa).
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // Infalible sobre un String; el `_ =` documenta que no puede fallar.
        let _ = write!(out, "{b:02x}");
    }
    out
}
