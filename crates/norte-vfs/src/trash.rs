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
}
