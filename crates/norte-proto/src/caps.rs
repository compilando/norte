//! `Capabilities`: lo que un provider sabe hacer (spec §5). El core elige
//! estrategia consultándolas (nunca sondeando en caliente) y los frontends
//! adaptan la UI.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

bitflags::bitflags! {
    /// Flags de capacidad de un provider (M0; el resto llega con sus hitos).
    ///
    /// Wire: string con nombres separados por ` | ` (formato de `bitflags`),
    /// también en encodings binarios (legibilidad > 4 bytes). Política de
    /// deserialización (ADR 0004):
    /// - Nombre desconocido con forma válida (`[A-Z0-9_]+`): se IGNORA. Una
    ///   capability es un anuncio; un cliente N-1 que no la conoce simplemente
    ///   no la explota — jamás revienta por un flag N+1.
    /// - Hex (`0x…`) o token malformado: ERROR. Bits sin nombre no viajan.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct CapabilityFlags: u32 {
        /// `rename()` atómico dentro del provider.
        const RENAME_ATOMIC = 1 << 0;
        /// Copia server-side (S3 CopyObject, SFTP ext, reflink/clonefile).
        const SERVER_COPY = 1 << 1;
        /// Soporta symlinks (crearlos puede requerir privilegio en Windows).
        const SYMLINKS = 1 << 2;
        /// El FS distingue mayúsculas (ext4 sí; NTFS/APFS por defecto no).
        const CASE_SENSITIVE = 1 << 3;
        /// El FS preserva la caja aunque no la distinga (NTFS/APFS).
        const CASE_PRESERVING = 1 << 4;
        /// Se puede añadir al final de un archivo existente (resume M2).
        const APPEND = 1 << 5;
        /// Se puede escribir en un offset arbitrario (verificación/parcheo).
        const RANDOM_WRITE = 1 << 6;
    }
}

impl Serialize for CapabilityFlags {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = String::new();
        bitflags::parser::to_writer(self, &mut out).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&out)
    }
}

impl<'de> Deserialize<'de> for CapabilityFlags {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FlagsVisitor;

        impl serde::de::Visitor<'_> for FlagsVisitor {
            type Value = CapabilityFlags;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a `A | B` capability flags string")
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<CapabilityFlags, E> {
                parse_flags(s).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(FlagsVisitor)
    }
}

/// Parser del wire de flags con la política del ADR 0004: nombres conocidos
/// se acumulan, nombres desconocidos bien formados se ignoran (forward-compat),
/// hex y tokens malformados son error (`bitflags::parser::from_str` retendría
/// bits desconocidos en silencio — inaceptable en el wire).
fn parse_flags(s: &str) -> Result<CapabilityFlags, &'static str> {
    let mut flags = CapabilityFlags::empty();
    if s.trim().is_empty() {
        return Ok(flags);
    }
    for token in s.split('|') {
        let token = token.trim();
        if token.is_empty() {
            return Err("empty flag between separators");
        }
        if token.starts_with("0x") || token.starts_with("0X") {
            return Err("hex flag values are not allowed on the wire");
        }
        if !token
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err("malformed flag name (expected `[A-Z0-9_]+`)");
        }
        if let Some(known) = CapabilityFlags::from_name(token) {
            flags |= known;
        }
        // Nombre bien formado pero desconocido: capability de un protocolo
        // más nuevo — se ignora, no se explota.
    }
    Ok(flags)
}

/// Capacidades declaradas por un provider.
///
/// ```
/// use norte_proto::{Capabilities, CapabilityFlags};
/// let c = Capabilities {
///     flags: CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_SENSITIVE,
///     max_path: Some(4096),
/// };
/// assert!(c.flags.contains(CapabilityFlags::RENAME_ATOMIC));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capabilities {
    /// Flags de capacidad.
    pub flags: CapabilityFlags,
    /// Longitud máxima de path nativo en bytes; `None` = sin límite conocido.
    #[serde(default)]
    pub max_path: Option<u32>,
}
