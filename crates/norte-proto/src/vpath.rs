//! `VPath`: la representación de paths del VFS — bytes con forma URI.
//!
//! Los nombres de archivo NO son UTF-8 (principio 3 de la spec): cada segmento
//! guarda bytes crudos (Unix: los bytes del OS tal cual; Windows: la forma
//! WTF-8 de `OsStr::as_encoded_bytes`). UTF-8 es solo una vista para display,
//! lossy y marcada. El wire format (percent-encoding) está fijado por el
//! ADR 0001 y sus golden tests.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::wire::vpath_codec;

/// Error de validación o parseo de [`VPath`] y sus componentes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VPathError {
    /// El wire no contiene el separador `://`.
    #[error("missing scheme: expected `scheme://…`")]
    MissingScheme,
    /// El scheme no cumple `[a-z][a-z0-9+.-]*` (sin case-folding).
    #[error("invalid scheme: expected `[a-z][a-z0-9+.-]*`")]
    InvalidScheme,
    /// La authority está vacía o sale del charset (ASCII imprimible sin `/` ni `%`).
    #[error("invalid authority: expected non-empty printable ASCII without `/` or `%`")]
    InvalidAuthority,
    /// Segmento vacío (`//` doble o slash final fuera de la raíz).
    #[error("empty path segment")]
    EmptySegment,
    /// Segmento `.` o `..` (literal o vía escape): `VPath` no resuelve rutas relativas.
    #[error("dot segment (`.`/`..`) is not allowed")]
    DotSegment,
    /// Byte NUL en un segmento (literal o vía escape).
    #[error("NUL byte in segment")]
    NulByte,
    /// Byte inválido en un segmento (p. ej. `/` introducido vía `%2F`).
    #[error("invalid byte in segment (separator cannot be escaped in)")]
    InvalidByte,
    /// Escape percent malformado (`%G1`, `%4`, `%` final).
    #[error("malformed percent escape")]
    BadEscape,
}

/// Scheme de un [`VPath`] (`file`, `sftp`, `mem`…), validado a `[a-z][a-z0-9+.-]*`.
///
/// ```
/// use norte_proto::Scheme;
/// assert!(Scheme::new("file").is_ok());
/// assert!(Scheme::new("FILE").is_err()); // sin case-folding
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Scheme(String);

impl Scheme {
    /// Valida y construye un scheme.
    ///
    /// # Errors
    /// [`VPathError::InvalidScheme`] si no cumple `[a-z][a-z0-9+.-]*`.
    pub fn new(s: &str) -> Result<Self, VPathError> {
        let bytes = s.as_bytes();
        let head_ok = bytes.first().is_some_and(u8::is_ascii_lowercase);
        let tail_ok = bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'.' | b'-')
        });
        if head_ok && tail_ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(VPathError::InvalidScheme)
        }
    }

    /// El scheme como `&str` (siempre ASCII lowercase).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Authority de un [`VPath`] (host, `host:puerto`, nombre de conexión…).
///
/// Charset validado: ASCII imprimible (0x21–0x7E) sin `/` ni `%`; nunca vacía
/// (la ausencia de authority es `None`, no `""`). No hay percent-encoding en
/// la authority: viaja literal, y por eso el primer `/` tras `://` separa
/// siempre authority de path (inyectividad del wire). Hosts no-ASCII van en
/// punycode — decisión del provider, no de proto.
///
/// ```
/// use norte_proto::Authority;
/// assert!(Authority::new("host:22").is_ok());
/// assert!(Authority::new("a/b").is_err());
/// assert!(Authority::new("").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Authority(String);

impl Authority {
    /// Valida y construye una authority.
    ///
    /// # Errors
    /// [`VPathError::InvalidAuthority`] si está vacía o contiene bytes fuera
    /// de ASCII imprimible, `/` o `%`.
    pub fn new(s: &str) -> Result<Self, VPathError> {
        let ok = !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_graphic() && b != b'/' && b != b'%');
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(VPathError::InvalidAuthority)
        }
    }

    /// La authority como `&str` (siempre ASCII imprimible).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Un segmento de path: bytes crudos, jamás forzados a UTF-8.
///
/// Invariantes (validadas en construcción, nunca saneadas en silencio):
/// no vacío, sin NUL, sin `/`, distinto de `.` y `..`.
///
/// ```
/// use norte_proto::Segment;
/// let s = Segment::new(vec![0xFF, 0xFE]).unwrap(); // bytes no-UTF8: válidos
/// assert_eq!(s.as_bytes(), &[0xFF, 0xFE]);
/// assert!(Segment::new(b"a/b".to_vec()).is_err());
/// ```
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Segment(Vec<u8>);

impl Segment {
    /// Valida y construye un segmento desde bytes crudos.
    ///
    /// # Errors
    /// [`VPathError::EmptySegment`], [`VPathError::NulByte`],
    /// [`VPathError::InvalidByte`] (contiene `/`) o [`VPathError::DotSegment`].
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, VPathError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(VPathError::EmptySegment);
        }
        if bytes.contains(&0x00) {
            return Err(VPathError::NulByte);
        }
        if bytes.contains(&b'/') {
            return Err(VPathError::InvalidByte);
        }
        if bytes.as_slice() == b"." || bytes.as_slice() == b".." {
            return Err(VPathError::DotSegment);
        }
        Ok(Self(bytes))
    }

    /// Los bytes crudos del segmento.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Segment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Segment({:?})", String::from_utf8_lossy(&self.0))
    }
}

/// Path del VFS: `scheme://authority/<segmentos en bytes>`.
///
/// Siempre absoluto respecto a la raíz del provider; sin `.`/`..`; el wire
/// format es un string percent-encoded (ADR 0001) y es lo que serializa serde.
///
/// ```
/// use norte_proto::VPath;
/// let p = VPath::parse("file:///home/user/doc.txt").unwrap();
/// assert_eq!(p.scheme(), "file");
/// assert_eq!(p.file_name().unwrap().as_bytes(), b"doc.txt");
/// assert_eq!(p.parent().unwrap().to_wire(), "file:///home/user");
/// ```
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VPath {
    scheme: Scheme,
    authority: Option<Authority>,
    segments: Vec<Segment>,
}

impl VPath {
    /// La raíz de un provider: `scheme://authority/` sin segmentos.
    #[must_use]
    pub fn root(scheme: Scheme, authority: Option<Authority>) -> Self {
        Self {
            scheme,
            authority,
            segments: Vec::new(),
        }
    }

    /// Parsea la forma wire. Acepta formas no canónicas (`%41` ≡ `A`,
    /// raíz sin slash final); [`Self::to_wire`] canonicaliza.
    ///
    /// # Errors
    /// Cualquier [`VPathError`]; las invariantes de segmento se validan
    /// POST-decode (un `%2F` no fabrica un separador, un `%2E%2E` no cuela
    /// un `..`).
    pub fn parse(wire: &str) -> Result<Self, VPathError> {
        let (scheme_raw, rest) = wire.split_once("://").ok_or(VPathError::MissingScheme)?;
        let scheme = Scheme::new(scheme_raw)?;

        let (authority_raw, path_raw) = match rest.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (rest, None),
        };
        let authority = if authority_raw.is_empty() {
            None
        } else {
            Some(Authority::new(authority_raw)?)
        };

        let mut segments = Vec::new();
        if let Some(path) = path_raw
            && !path.is_empty()
        {
            for raw in path.split('/') {
                if raw.is_empty() {
                    return Err(VPathError::EmptySegment);
                }
                segments.push(Segment::new(vpath_codec::decode_segment(raw)?)?);
            }
        }
        Ok(Self {
            scheme,
            authority,
            segments,
        })
    }

    /// La forma wire canónica (la que viaja por el protocolo y serializa serde).
    #[must_use]
    pub fn to_wire(&self) -> String {
        let mut out = String::with_capacity(16 + self.segments.len() * 12);
        self.write_prefix(&mut out);
        let mut first = true;
        for seg in &self.segments {
            if !first {
                out.push('/');
            }
            first = false;
            vpath_codec::encode_segment(seg.as_bytes(), &mut out);
        }
        out
    }

    /// Vista para humanos, forma `⟨scheme authority⟩/seg/…`: UTF-8 lossy con
    /// `�` marcando bytes no decodificables Y caracteres de control (jamás
    /// controles crudos hacia un terminal). Deliberadamente NO tiene forma
    /// wire (sin `://`): [`Self::parse`] sobre un display siempre falla, así
    /// que nunca reconstruye un path por accidente (usa [`Self::to_wire`]).
    #[must_use]
    pub fn display_lossy(&self) -> String {
        let mut out = String::from("⟨");
        out.push_str(self.scheme.as_str());
        if let Some(a) = &self.authority {
            out.push(' ');
            out.push_str(a.as_str());
        }
        out.push_str("⟩/");
        let mut first = true;
        for seg in &self.segments {
            if !first {
                out.push('/');
            }
            first = false;
            for c in String::from_utf8_lossy(seg.as_bytes()).chars() {
                out.push(if c.is_control() {
                    char::REPLACEMENT_CHARACTER
                } else {
                    c
                });
            }
        }
        out
    }

    /// El scheme del provider.
    #[must_use]
    pub fn scheme(&self) -> &str {
        self.scheme.as_str()
    }

    /// La authority (host, conexión…), si la hay.
    #[must_use]
    pub fn authority(&self) -> Option<&str> {
        self.authority.as_ref().map(Authority::as_str)
    }

    /// Iterador sobre los bytes crudos de cada segmento.
    pub fn segments(&self) -> impl Iterator<Item = &[u8]> {
        self.segments.iter().map(Segment::as_bytes)
    }

    /// `true` si es la raíz del provider (sin segmentos).
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.segments.is_empty()
    }

    /// Path hijo con `segment` añadido al final.
    #[must_use]
    pub fn join(&self, segment: Segment) -> Self {
        let mut child = self.clone();
        child.segments.push(segment);
        child
    }

    /// El path padre; `None` en la raíz.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        if self.segments.is_empty() {
            return None;
        }
        let mut p = self.clone();
        p.segments.pop();
        Some(p)
    }

    /// El último segmento; `None` en la raíz.
    #[must_use]
    pub fn file_name(&self) -> Option<&Segment> {
        self.segments.last()
    }

    /// Reemplaza el último segmento (p. ej. para derivar `x` → `x.norte-partial`);
    /// `None` en la raíz.
    #[must_use]
    pub fn with_file_name(&self, segment: Segment) -> Option<Self> {
        if self.segments.is_empty() {
            return None;
        }
        let mut p = self.clone();
        *p.segments.last_mut()? = segment;
        Some(p)
    }

    fn write_prefix(&self, out: &mut String) {
        out.push_str(self.scheme.as_str());
        out.push_str("://");
        if let Some(a) = &self.authority {
            out.push_str(a.as_str());
        }
        out.push('/');
    }
}

impl fmt::Debug for VPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VPath({})", self.to_wire())
    }
}

/// `Display` es la forma wire (lossless), pensada para logs y errores.
/// Para vista humana usa [`VPath::display_lossy`].
impl fmt::Display for VPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_wire())
    }
}

impl Serialize for VPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_wire())
    }
}

impl<'de> Deserialize<'de> for VPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WireVisitor;

        impl Visitor<'_> for WireVisitor {
            type Value = VPath;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a VPath wire string (`scheme://authority/segments`)")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<VPath, E> {
                VPath::parse(v).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(WireVisitor)
    }
}
