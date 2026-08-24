//! `Entry`: lo que el VFS sabe de un nodo (resultado de `fs.stat` / `fs.list`).

use serde::{Deserialize, Serialize};

use crate::VPath;

/// Clase de un nodo del VFS.
///
/// Los valores desconocidos (protocolo N+1) deserializan a [`EntryKind::Other`]:
/// un cliente viejo degrada, no revienta.
///
/// ```
/// use norte_proto::EntryKind;
/// let k: EntryKind = serde_json::from_str("\"kind_del_futuro\"").unwrap();
/// assert_eq!(k, EntryKind::Other);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Archivo regular.
    File,
    /// Directorio.
    Dir,
    /// Symlink (M0: jamás se sigue; se preserva o `Unsupported`).
    Symlink,
    /// Cualquier otra cosa (device, socket, fifo, o kind de protocolo futuro).
    #[serde(other)]
    Other,
}

/// Metadatos de un nodo del VFS.
///
/// Campos opcionales `None` = "el provider no lo sabe", nunca un 0 fingido.
///
/// ```
/// use norte_proto::attrs::AttrValue;
/// use norte_proto::{Entry, EntryKind, VPath};
/// let e = Entry {
///     path: VPath::parse("file:///a.txt").unwrap(),
///     kind: EntryKind::File,
///     size: Some(42),
///     mtime_ms: None,
///     attrs: [("posix.mode".to_owned(), AttrValue::Uint(0o100_644))].into(),
/// };
/// let json = serde_json::to_string(&e).unwrap();
/// assert_eq!(serde_json::from_str::<Entry>(&json).unwrap(), e);
///
/// // Sin attrs, el wire es EXACTAMENTE el de 0.29.
/// let plain = Entry { attrs: Default::default(), ..e };
/// assert!(!serde_json::to_string(&plain).unwrap().contains("attrs"));
///
/// // Una clave de atributo mal formada NO llega al mapa: se descarta al
/// // decodificar, sin error (ver [`Entry::attrs`]).
/// let wire = r#"{"path":"file:///a.txt","kind":"file","attrs":{"MODE":{"uint":1}}}"#;
/// let filtrada: Entry = serde_json::from_str(wire).unwrap();
/// assert!(filtrada.attrs.is_empty());
/// ```
///
/// OJO: `PartialEq`/`Eq`/`Hash` derivados son REPRESENTACIONALES, no de
/// IDENTIDAD: dos `Entry` del MISMO nodo difieren si una se pidió con
/// atributos y la otra no, o si se pidieron ids distintos. Para "¿es el mismo
/// nodo?" se compara [`Entry::path`]; estas derivaciones existen para
/// fixtures, tests y deduplicado de listados idénticos, no para decidir
/// identidad.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Entry {
    /// Path completo del nodo (los frontends operan con él, no con el nombre).
    pub path: VPath,
    /// Clase del nodo.
    pub kind: EntryKind,
    /// Tamaño en bytes; `None` si el provider no lo conoce (p. ej. directorios).
    #[serde(default)]
    pub size: Option<u64>,
    /// Última modificación en milisegundos desde epoch UTC; `i64` porque las
    /// fechas pre-1970 existen en FS reales. `None` = desconocida.
    #[serde(default)]
    pub mtime_ms: Option<i64>,
    /// Provider attributes (0.30.0, ADR 0039): typed metadata BEYOND the four
    /// fields above, delivered only for the ids the client requested in
    /// `fs.list`/`fs.stat`. Empty (the default, and the only possibility for a
    /// 0.29 peer) is omitted from the wire entirely, so a payload without
    /// attributes is byte-identical to 0.29's.
    ///
    /// An absent key has THREE possible causes and a caller cannot tell them
    /// apart: the provider does not know the value (never a fabricated one),
    /// the id the peer sent was malformed, or the id lost the byte-order cut
    /// of the cap below. The drop is deliberately not observable — a caller
    /// treats absence as "no value", exactly as it does for a `None`
    /// [`size`](Entry::size).
    ///
    /// # Rules applied ON DECODE
    ///
    /// Deserialisation applies both of these, and NEITHER is ever an error:
    /// one bad key costs that key, never the entry and never the page — the
    /// same reason a malformed cell degrades to
    /// [`AttrValue::Unknown`](crate::attrs::AttrValue::Unknown).
    ///
    /// 1. A key that is not a well-formed id
    ///    ([`is_valid_attr_id`](crate::attrs::is_valid_attr_id)) is DROPPED.
    ///    An attribute id becomes a configuration id and a map lookup
    ///    downstream, so `MODE` or `../etc/passwd` must not survive the wire.
    /// 2. The map is bounded at
    ///    [`ATTRS_MAX_REQUEST`](crate::attrs::ATTRS_MAX_REQUEST) entries — a
    ///    client can request no more than that, so a bigger map is a buggy or
    ///    hostile peer. The SMALLEST ids in byte order are kept, so the SET of
    ///    ids that survives does not depend on the order the peer serialised
    ///    its keys in (JSON objects are unordered, RFC 8259 §4). A key
    ///    REPEATED in the same object resolves last-wins, as it would in any
    ///    JSON parser. At most `ATTRS_MAX_REQUEST + 1` entries ever exist at
    ///    once and a dropped key's value is never MATERIALISED — the tokens are
    ///    still walked, since JSON has to be traversed to be skipped, but
    ///    nothing is built from them — so a 10 000-key object does not
    ///    materialise a 10 000-entry map first.
    ///
    /// A non-map `attrs` IS a hard error: that is serde's decision on the
    /// parent, and a peer that sends one is broken rather than newer.
    ///
    /// # The filter is one-directional
    ///
    /// Serialisation is NOT filtered, and the field is public with no
    /// constructor: an `Entry` built in-process with an invalid id or with
    /// more than the cap emits exactly what it holds, and decoding that wire
    /// gives back a DIFFERENT `Entry`. That asymmetry is deliberate — a
    /// producer's bug must stay visible at the boundary that validates it
    /// instead of being laundered by the serialiser. A producer therefore
    /// validates before emitting; daemon-side enforcement lands in block 2 of
    /// this feature.
    #[serde(
        default,
        deserialize_with = "crate::attrs::deserialize_attr_map",
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(extend(
            "maxProperties" = crate::attrs::ATTRS_MAX_REQUEST,
            "propertyNames" = serde_json::json!({
                "maxLength": crate::attrs::ATTR_ID_MAX,
                "pattern": r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$",
            })
        ))
    )]
    pub attrs: std::collections::BTreeMap<String, crate::attrs::AttrValue>,
}

/// Longitud exacta, en caracteres, de un [`DirAnchor`] bien formado.
pub const DIR_ANCHOR_LEN: usize = 32;

/// La identidad OPACA del directorio que un listado devolvió, para que la
/// petición que escribe en él pueda decir CUÁL era (#295, 0.54.0).
///
/// # Qué problema resuelve
///
/// El core abre el directorio destino como raíz confinada (ADR 0072), así que
/// una sustitución POSTERIOR a esa apertura ya no desvía nada. Lo que no puede
/// distinguir es un enlace que **ya estaba puesto** cuando miró por primera
/// vez: desde dentro del core, `dest/sub -> /etc` recién plantado y un
/// `~/copias -> /mnt/disco/copias` legítimo son idénticos —los dos resuelven a
/// otro sitio—, y rechazar los dos rompe copiar a `/tmp` en macOS o a `/bin`
/// en un Linux con usrmerge.
///
/// Lo único que los separa es la identidad que se observó **cuando el humano
/// aprobó**: el listado que estaba mirando. Eso es esto.
///
/// # Es opaco a propósito
///
/// Dentro no hay un inodo ni un número de volumen, sino un valor derivado de
/// ellos con un secreto del daemon: dos rutas del mismo nodo dan el mismo
/// ancla, y un cliente no puede ni fabricar una ni deducir qué nodo hay
/// detrás. Un cliente lo trata como bytes: lo guarda, lo devuelve y jamás lo
/// interpreta ni lo construye.
///
/// Un ancla **no sobrevive al reinicio del daemon**, que renueva el secreto.
/// Un cliente que reconecta ha perdido su listado de todas formas y vuelve a
/// pedirlo, así que la ventana que importa —mirar, aprobar, escribir— cae
/// entera dentro de una sesión.
///
/// ```
/// use norte_proto::entry::{DIR_ANCHOR_LEN, DirAnchor};
/// let a = DirAnchor::new("0123456789abcdef0123456789abcdef".to_owned());
/// assert!(a.is_well_formed());
/// assert_eq!(a.as_str().len(), DIR_ANCHOR_LEN);
/// // Va por el wire como una cadena y nada más.
/// assert_eq!(
///     serde_json::to_string(&a).unwrap(),
///     "\"0123456789abcdef0123456789abcdef\""
/// );
///
/// // Lo que no está bien formado NO es un error de decodificación: llega,
/// // y quien compare no encontrará jamás un nodo que le case, que es la
/// // respuesta segura (un ancla que no se reconoce no autoriza nada).
/// let raro: DirAnchor = serde_json::from_str("\"../etc\"").unwrap();
/// assert!(!raro.is_well_formed());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DirAnchor(String);

impl DirAnchor {
    /// Envuelve el valor que produjo el daemon.
    ///
    /// No valida: quien lo emite sabe lo que emite, y quien lo recibe por el
    /// wire pregunta con [`DirAnchor::is_well_formed`]. Esa asimetría es la
    /// misma que [`Entry::attrs`] documenta y por el mismo motivo — el fallo
    /// de un productor tiene que seguir siendo visible.
    ///
    /// ```
    /// use norte_proto::entry::DirAnchor;
    /// assert_eq!(DirAnchor::new("ab".to_owned()).as_str(), "ab");
    /// ```
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// El valor tal cual, para guardarlo o devolverlo. Nunca para leerlo.
    ///
    /// ```
    /// use norte_proto::entry::DirAnchor;
    /// let a = DirAnchor::new("0123456789abcdef0123456789abcdef".to_owned());
    /// assert!(a.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// ¿Tiene la forma que emite un daemon: [`DIR_ANCHOR_LEN`] dígitos hex en
    /// minúscula?
    ///
    /// Un ancla mal formada no es un error del wire —no rompe la petición—,
    /// pero tampoco casa con ningún nodo, así que la operación que la traía se
    /// rechaza. Fallar cerrado es lo correcto aquí: el ancla existe para
    /// autorizar, no para dispensar.
    ///
    /// ```
    /// use norte_proto::entry::DirAnchor;
    /// assert!(DirAnchor::new("0123456789abcdef0123456789abcdef".to_owned()).is_well_formed());
    /// assert!(!DirAnchor::new("0123456789ABCDEF0123456789ABCDEF".to_owned()).is_well_formed());
    /// assert!(!DirAnchor::new("corto".to_owned()).is_well_formed());
    /// ```
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.0.len() == DIR_ANCHOR_LEN
            && self
                .0
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}
