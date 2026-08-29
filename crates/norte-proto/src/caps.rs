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
        /// Hay papelera: `trash()` mueve a un lugar recuperable (ADR 0009).
        const TRASH = 1 << 7;
        /// El provider es solo-lectura (0.9.0, ADR 0018: archivos como
        /// directorios): TODA mutación responde `Unsupported`. La UI veta
        /// upfront y el copy engine rechaza destinos aquí sin round-trip.
        const READ_ONLY = 1 << 8;
        /// El plegado de caja de esta UBICACIÓN **expande** (0.45.0, #145,
        /// ADR 0054): ext4/f2fs con el directorio en `+F`, cuya tabla del
        /// kernel se construye de `CaseFolding.txt` con estado `C + F`, así
        /// que `straße.txt` y `strasse.txt` son UN archivo ahí.
        ///
        /// Solo tiene sentido SIN [`Self::CASE_SENSITIVE`] —un directorio que
        /// distingue caja no pliega nada— y solo lo responde
        /// `Provider::capabilities_at`: es del directorio, no del backend.
        const FULL_FOLD = 1 << 9;
        /// Una escritura bajo esta ubicación puede confinarse bajo la raíz que
        /// nombre el caller, con garantía del kernel (0.45.0, #164, ADR 0054):
        /// `Provider::open_root` devuelve un handle en vez de `Unsupported`.
        ///
        /// Lo responde `Provider::capabilities_at` y JAMÁS `capabilities()`:
        /// depende del mount, de la plataforma y del kernel en marcha. Su
        /// ausencia no impide nada —el core degrada al paseo con `lstat` y lo
        /// dice— pero significa que un symlink en un componente INTERMEDIO
        /// puede redirigir la escritura fuera de su raíz.
        const CONFINED_WRITES = 1 << 10;
        /// Los nodos de esta ubicación tienen permisos POSIX y se pueden
        /// CAMBIAR (0.60.0, #314): `fs.set_mode` funciona aquí.
        ///
        /// Lo declara quien puede hacer las dos cosas, leerlos y escribirlos.
        /// Un `.zip` no tiene nada que cambiar y un bucket de objetos no tiene
        /// modo; sin este flag, el frontend apaga el gesto con su motivo en
        /// vez de ofrecerlo para que falle con `Unsupported`.
        const POSIX_MODE = 1 << 11;
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

// `CapabilityFlags` serializes as a `A | B` string (ADR 0004), so its JSON
// Schema is a string — the bitflags serde is hand-written and cannot derive.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for CapabilityFlags {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CapabilityFlags".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Capability flags as a `NAME | NAME` string \
                            (e.g. `RENAME_ATOMIC | CASE_SENSITIVE`). Unknown \
                            well-formed names are ignored for forward-compat.",
        })
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capabilities {
    /// Flags de capacidad.
    pub flags: CapabilityFlags,
    /// Longitud máxima de path nativo en bytes; `None` = sin límite conocido.
    #[serde(default)]
    pub max_path: Option<u32>,
}
