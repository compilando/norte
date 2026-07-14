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
}
