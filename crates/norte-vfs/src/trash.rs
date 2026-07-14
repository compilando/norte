//! Papelera lógica `.norte-trash/` para providers sin trash nativo
//! (ADR 0019). Helpers PUROS, sin I/O: los providers construyen las rutas
//! y los metadatos con estas funciones y ejecutan la relocalización con
//! sus propios primitivos (rename en sftp, copy+delete en object).

use norte_proto::{Error, Segment, VPath};

/// Directorio raíz de la papelera lógica dentro de una conexión.
pub const TRASH_DIR: &[u8] = b".norte-trash";
/// Fichero de metadatos de restauración dentro de cada entrada.
pub const INFO_NAME: &[u8] = b".norte-info";
/// Cabecera de versión del fichero `.norte-info`.
const INFO_HEADER: &str = "norte-trash-info v1";

/// Identificador único y ordenable de una entrada de papelera:
/// `<deleted_ms>-<counter>`. `counter` es monótono por sesión para
/// desempatar borrados en el mismo milisegundo. Siempre un [`Segment`]
/// válido (solo dígitos y `-`).
#[must_use]
pub fn trash_id(deleted_ms: u64, counter: u64) -> String {
    format!("{deleted_ms}-{counter}")
}

/// Rutas absolutas de una entrada de papelera para un path a borrar.
#[derive(Debug, Clone)]
pub struct TrashPaths {
    /// El directorio de la entrada: `.norte-trash/<id>/`.
    pub dir: VPath,
    /// El payload movido: `.norte-trash/<id>/<basename-original>`.
    pub payload: VPath,
    /// Los metadatos: `.norte-trash/<id>/.norte-info`.
    pub info: VPath,
}

/// Construye las rutas de papelera para `p` bajo la raíz de su conexión.
///
/// `id` debe venir de [`trash_id`] (o cualquier [`Segment`] válido).
///
/// # Errors
/// - [`Error::Unsupported`] si `p` es la raíz del provider (sin basename):
///   la raíz de la conexión no se papeleriza.
/// - [`Error::InvalidPath`] si `id` no es un segmento válido.
pub fn plan(p: &VPath, id: &str) -> Result<TrashPaths, Error> {
    let basename = p.file_name().ok_or(Error::Unsupported)?.clone();
    // No se papeleriza la propia papelera ni nada dentro de ella: evita el
    // rename de `.norte-trash` dentro de sí mismo (POSIX EINVAL) y entradas
    // basura autorreferenciales que confundirían al restore de M3.
    if p.segments().next() == Some(TRASH_DIR) {
        return Err(Error::Unsupported);
    }
    let id_seg = Segment::new(id.as_bytes().to_vec()).map_err(|_| Error::InvalidPath)?;
    let dir = provider_root(p).join(seg_const(TRASH_DIR)).join(id_seg);
    let payload = dir.join(basename);
    let info = dir.join(seg_const(INFO_NAME));
    Ok(TrashPaths { dir, payload, info })
}

/// La raíz de la conexión de `p` (mismo scheme+authority, sin segmentos).
fn provider_root(p: &VPath) -> VPath {
    let mut r = p.clone();
    while let Some(parent) = r.parent() {
        r = parent;
    }
    r
}

/// Segmento desde bytes de una constante del módulo (`TRASH_DIR`,
/// `INFO_NAME`). Invariante: son literales válidos; un pánico aquí es un
/// bug del módulo, jamás entrada de usuario.
fn seg_const(bytes: &[u8]) -> Segment {
    Segment::new(bytes.to_vec()).expect("constante de papelera es un Segment válido")
}

/// Metadatos de restauración de una entrada de papelera.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashInfo {
    /// Ruta original, reconstruida desde su forma wire.
    pub original: VPath,
    /// Instante de borrado, ms desde epoch.
    pub deleted_ms: u64,
}

/// Serializa los metadatos de restauración a los bytes de un `.norte-info`.
/// La ruta va como [`VPath::to_wire`] (percent-encoded ASCII, lossless,
/// sin newlines ni controles ASCII) → el resultado es line-safe (siempre
/// exactamente 3 líneas, el valor de `path:` jamás contiene `\n`).
#[must_use]
pub fn info_encode(original: &VPath, deleted_ms: u64) -> Vec<u8> {
    format!(
        "{INFO_HEADER}\npath: {}\ndeleted-ms: {deleted_ms}\n",
        original.to_wire()
    )
    .into_bytes()
}

/// Parsea el contenido de un `.norte-info` y **ancla** la ruta original a
/// la conexión de la papelera.
///
/// `expected_root` es la raíz del provider donde vive esta papelera (mismo
/// scheme+authority que la conexión). El guard es de SEGURIDAD: un
/// `.norte-info` en un share/bucket compartido es atacante-controlable; sin
/// anclar, un restore (M3) escribiría el payload en OTRA conexión/host
/// (`path: sftp://otro-host/.ssh/authorized_keys`) — deputy confundido. El
/// traversal (`.`/`..`/`%2F`/NUL) ya lo bloquea [`VPath::parse`]; aquí se
/// cierra el vector scheme/authority. La política de sobrescritura de un
/// fichero existente DENTRO de la misma conexión es decisión del restore
/// (confirmación reforzada), no de este parser.
///
/// # Errors
/// [`Error::InvalidPath`] si el contenido no es UTF-8, le falta la
/// cabecera o un campo, sobran líneas, la ruta wire no parsea, el
/// timestamp no es un `u64`, o la ruta original no pertenece a
/// `expected_root` (distinto scheme o authority).
pub fn info_decode(bytes: &[u8], expected_root: &VPath) -> Result<TrashInfo, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::InvalidPath)?;
    let mut lines = text.lines();
    if lines.next() != Some(INFO_HEADER) {
        return Err(Error::InvalidPath);
    }
    let wire = lines
        .next()
        .and_then(|l| l.strip_prefix("path: "))
        .ok_or(Error::InvalidPath)?;
    let ms = lines
        .next()
        .and_then(|l| l.strip_prefix("deleted-ms: "))
        .ok_or(Error::InvalidPath)?;
    // Estricto: nada tras el 3.er campo. Rechaza `.norte-info` semi-corruptos
    // o con líneas inyectadas de más.
    if lines.next().is_some() {
        return Err(Error::InvalidPath);
    }
    let original = VPath::parse(wire).map_err(|_| Error::InvalidPath)?;
    // Guard confused-deputy: la ruta restaurada DEBE pertenecer a la MISMA
    // conexión que la papelera.
    if original.scheme() != expected_root.scheme()
        || original.authority() != expected_root.authority()
    {
        return Err(Error::InvalidPath);
    }
    let deleted_ms = ms.parse::<u64>().map_err(|_| Error::InvalidPath)?;
    Ok(TrashInfo {
        original,
        deleted_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_id_is_sortable_and_valid_segment() {
        assert_eq!(trash_id(1_726_000_000_123, 0), "1726000000123-0");
        // Ordena lexicográficamente igual que numéricamente para mismo ancho.
        assert!(trash_id(1_726_000_000_123, 0) < trash_id(1_726_000_000_124, 0));
        // Siempre construye un Segment válido (sin `/`, sin NUL, no `.`/`..`).
        assert!(Segment::new(trash_id(1, 2).into_bytes()).is_ok());
    }

    #[test]
    fn plan_builds_entry_under_provider_root() {
        let p = VPath::parse("sftp://host/deep/nested/victim.txt").unwrap();
        let paths = plan(&p, "1726000000123-0").unwrap();
        assert_eq!(
            paths.dir.to_wire(),
            "sftp://host/.norte-trash/1726000000123-0"
        );
        assert_eq!(
            paths.payload.to_wire(),
            "sftp://host/.norte-trash/1726000000123-0/victim.txt"
        );
        assert_eq!(
            paths.info.to_wire(),
            "sftp://host/.norte-trash/1726000000123-0/.norte-info"
        );
    }

    #[test]
    fn plan_preserves_hostile_basename() {
        // Segmento final no-UTF8 (0xFF 0xFE): el payload conserva sus bytes.
        let p = VPath::parse("sftp://host/dir/%FF%FE").unwrap();
        let paths = plan(&p, "1-0").unwrap();
        assert_eq!(
            paths.payload.to_wire(),
            "sftp://host/.norte-trash/1-0/%FF%FE"
        );
    }

    #[test]
    fn plan_refuses_provider_root() {
        let root = VPath::parse("sftp://host/").unwrap();
        assert!(matches!(plan(&root, "1-0"), Err(Error::Unsupported)));
    }

    #[test]
    fn plan_rejects_bad_id() {
        let p = VPath::parse("sftp://host/x").unwrap();
        // Un id con `/` no es un Segment válido.
        assert!(matches!(plan(&p, "bad/id"), Err(Error::InvalidPath)));
    }

    #[test]
    fn plan_refuses_trashing_the_trash_itself() {
        // La papelera misma no se papeleriza (auto-referencia).
        let dir = VPath::parse("sftp://host/.norte-trash").unwrap();
        assert!(matches!(plan(&dir, "1-0"), Err(Error::Unsupported)));
        // Ni nada que ya viva dentro de ella (re-trashear una entrada).
        let inside = VPath::parse("sftp://host/.norte-trash/2-0/x").unwrap();
        assert!(matches!(plan(&inside, "3-0"), Err(Error::Unsupported)));
        // Pero un fichero `.norte-trash` ANIDADO (no en la raíz) sí se puede.
        let nested = VPath::parse("sftp://host/dir/.norte-trash").unwrap();
        assert!(plan(&nested, "4-0").is_ok());
    }

    #[test]
    fn seg_const_uses_valid_constants() {
        // Ata la invariante del `expect()` de `seg_const` (rule 6): las
        // constantes del módulo son siempre segmentos válidos.
        assert!(Segment::new(TRASH_DIR.to_vec()).is_ok());
        assert!(Segment::new(INFO_NAME.to_vec()).is_ok());
    }

    #[test]
    fn info_roundtrips_hostile_path() {
        // Ruta con byte no-UTF8 (0xFF) Y un byte de control newline (0x0A)
        // dentro de un segmento: to_wire los escapa a %FF/%0A → line-safe.
        let root = VPath::parse("sftp://host/").unwrap();
        let p = VPath::parse("sftp://host/a/%FF/x%0Ay").unwrap();
        let bytes = info_encode(&p, 1_726_000_000_123);

        // Line-safe: exactamente 3 líneas, ningún newline dentro del valor.
        let text = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(text.lines().count(), 3);

        let info = info_decode(&bytes, &root).unwrap();
        assert_eq!(info.original, p);
        assert_eq!(info.deleted_ms, 1_726_000_000_123);
    }

    #[test]
    fn info_decode_rejects_corrupt() {
        let root = VPath::parse("sftp://host/").unwrap();
        assert!(matches!(
            info_decode(b"garbage", &root),
            Err(Error::InvalidPath)
        ));
        assert!(matches!(
            info_decode(b"norte-trash-info v1\npath: sftp://host/x\n", &root),
            Err(Error::InvalidPath) // falta deleted-ms
        ));
        assert!(matches!(
            info_decode(
                b"norte-trash-info v1\npath: not-a-wire-path\ndeleted-ms: 5\n",
                &root
            ),
            Err(Error::InvalidPath) // wire no parsea
        ));
        assert!(matches!(
            info_decode(
                b"norte-trash-info v1\npath: sftp://host/x\ndeleted-ms: NaN\n",
                &root
            ),
            Err(Error::InvalidPath) // ms no numérico
        ));
    }

    #[test]
    fn info_decode_rejects_trailing_lines() {
        // Estricto: una 4.ª línea inyectada invalida el fichero.
        let root = VPath::parse("sftp://host/").unwrap();
        assert!(matches!(
            info_decode(
                b"norte-trash-info v1\npath: sftp://host/x\ndeleted-ms: 5\ninyectado\n",
                &root
            ),
            Err(Error::InvalidPath)
        ));
    }

    #[test]
    fn info_decode_anchors_to_connection() {
        // Guard confused-deputy: un .norte-info envenenado que apunta a OTRA
        // conexión (distinto scheme o authority) se rechaza.
        let root = VPath::parse("sftp://host/").unwrap();

        // Distinto scheme (papelera sftp → ruta file://).
        assert!(matches!(
            info_decode(
                b"norte-trash-info v1\npath: file:///etc/passwd\ndeleted-ms: 0\n",
                &root
            ),
            Err(Error::InvalidPath)
        ));
        // Distinta authority (otro host).
        assert!(matches!(
            info_decode(
                b"norte-trash-info v1\npath: sftp://evil/home/victima/.ssh/authorized_keys\ndeleted-ms: 0\n",
                &root
            ),
            Err(Error::InvalidPath)
        ));
        // Misma conexión (mismo scheme+authority): aceptado, ruta profunda ok.
        let ok = info_decode(
            b"norte-trash-info v1\npath: sftp://host/deep/nested/file.txt\ndeleted-ms: 7\n",
            &root,
        )
        .unwrap();
        assert_eq!(
            ok.original,
            VPath::parse("sftp://host/deep/nested/file.txt").unwrap()
        );
        assert_eq!(ok.deleted_ms, 7);
    }
}
