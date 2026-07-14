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
///
/// `non_exhaustive`: como [`Error`](crate::Error) — un consumidor externo
/// no debe romper cuando una versión nueva añade una causa de rechazo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum VPathError {
    /// El wire no contiene el separador `://`.
    #[error("missing scheme: expected `scheme://…`")]
    MissingScheme,
    /// El scheme no cumple `[a-z][a-z0-9+.-]*` (sin case-folding).
    #[error("invalid scheme: expected `[a-z][a-z0-9+.-]*`")]
    InvalidScheme,
    /// La authority está vacía, sale del charset (ASCII imprimible sin `/` ni
    /// `%`) o lleva un `:` en el userinfo (password inline, #46).
    #[error(
        "invalid authority: non-empty printable ASCII without `/`/`%`, and no `:` in the userinfo"
    )]
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
    /// Direccionamiento de archivo-como-directorio malformado (ADR 0018):
    /// marcador `!` ausente en un scheme compuesto, `!` en posición
    /// prohibida al componer, o formato fuera de [`ARCHIVE_FORMATS`].
    #[error("malformed archive addressing (`!` marker / format, ADR 0018)")]
    ArchiveAddressing,
}

/// Tokens de formato de archivo-como-directorio reconocidos (ADR 0018).
///
/// Un scheme es compuesto si y solo si su prefijo hasta el primer `+` está
/// aquí; ampliar la lista es cambio de protocolo. Reserva normativa: ningún
/// provider registra schemes que empiecen por `<formato>+`.
pub const ARCHIVE_FORMATS: &[&str] = &["zip", "tar"];

/// Referencia desmontada de un path de archivo-como-directorio (ADR 0018):
/// `<formato>+<scheme>://auth/<exterior>/!/<interior>`.
///
/// ```
/// use norte_proto::VPath;
/// let p = VPath::parse("zip+file:///a.zip/!/x").unwrap();
/// let r = p.archive_split().unwrap().unwrap();
/// assert_eq!(r.format, "zip");
/// assert_eq!(r.outer.to_wire(), "file:///a.zip");
/// assert_eq!(r.inner[0].as_bytes(), b"x");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRef {
    /// Formato del contenedor (token de [`ARCHIVE_FORMATS`]).
    pub format: String,
    /// Path del ARCHIVO contenedor en su provider interior.
    pub outer: VPath,
    /// Segmentos interiores relativos a la raíz del archivo.
    pub inner: Vec<Segment>,
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
    /// Rechaza un `:` en el userinfo (`user:pass@host`): una password inline
    /// en la URL acabaría en config/logs (regla 10). Defensa RAÍZ, proto 0.8.0
    /// (#46): los guards de la CLI/`norte-connect` quedan como defensa en
    /// profundidad. El `:` del `host:port` (tras `@`, o sin `@`) y el de un
    /// IPv6 con corchetes siguen siendo válidos.
    ///
    /// # Errors
    /// [`VPathError::InvalidAuthority`] si está vacía, contiene bytes fuera de
    /// ASCII imprimible / `/` / `%`, o lleva un `:` en el userinfo.
    pub fn new(s: &str) -> Result<Self, VPathError> {
        let charset_ok = !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_graphic() && b != b'/' && b != b'%');
        // userinfo = lo anterior al ÚLTIMO `@`; un `:` ahí es `user:pass`. Se
        // usa el último `@` (no el primero) para que un authority patológico
        // con varios `@` (`a@b:c@host`) no cuele un `:` en un tramo intermedio;
        // un authority legítimo tiene a lo sumo un `@` (el host no lleva `@`),
        // así que esto no rechaza nada válido. Coincide con el `rsplit_once`
        // del guard de la CLI (defensa en profundidad consistente).
        let no_inline_password = match s.rfind('@') {
            Some(at) => !s[..at].contains(':'),
            None => true,
        };
        if charset_ok && no_inline_password {
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
    /// `�` marcando bytes no decodificables, caracteres de control Y
    /// formateadores bidi/invisibles (jamás controles ni overrides RTL
    /// crudos hacia un terminal — spoofing de dirección, issue #21).
    /// Deliberadamente NO tiene forma wire (sin `://`): [`Self::parse`]
    /// sobre un display siempre falla, así que nunca reconstruye un path
    /// por accidente (usa [`Self::to_wire`]).
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
                out.push(if is_display_hazard(c) {
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

    /// Compone el path de una entrada DENTRO de un archivo (ADR 0018):
    /// `<format>+<scheme-de-outer>://<auth-de-outer>/<outer>/!/<inner>`.
    ///
    /// Solo sobre segmentos ya validados — jamás fabrica paths que
    /// [`Self::archive_split`] no pueda deshacer (roundtrip garantizado).
    /// Puramente sintáctico: un `outer` raíz (`file:///`) compone sin error
    /// y es el provider quien responde `TypeMismatch` (la raíz no es un
    /// archivo).
    ///
    /// ```
    /// use norte_proto::VPath;
    /// let outer = VPath::parse("file:///home/o/a.zip").unwrap();
    /// let root = VPath::archive_compose("zip", &outer, &[]).unwrap();
    /// assert_eq!(root.to_wire(), "zip+file:///home/o/a.zip/!");
    /// ```
    ///
    /// # Errors
    /// [`VPathError::ArchiveAddressing`] si `format` no está en
    /// [`ARCHIVE_FORMATS`], si `outer` ya es compuesto (v1 = una capa), o
    /// si `outer`/`inner` contienen un segmento `!` literal (el exterior
    /// no sería direccionable; el interior, incontrastable con el índice,
    /// que omite esos componentes). [`VPathError::InvalidScheme`] si la
    /// concatenación no forma un scheme válido (imposible con formatos de
    /// la whitelist; defensa en profundidad).
    ///
    /// # Panics
    /// Nunca en la práctica: `!` es un segmento válido por construcción.
    pub fn archive_compose(
        format: &str,
        outer: &Self,
        inner: &[Segment],
    ) -> Result<Self, VPathError> {
        if !ARCHIVE_FORMATS.contains(&format) {
            return Err(VPathError::ArchiveAddressing);
        }
        if scheme_format_prefix(outer.scheme()).is_some() {
            return Err(VPathError::ArchiveAddressing);
        }
        let marker = || Segment::new(MARKER.to_vec()).expect("`!` es segmento válido");
        if outer.segments.iter().any(|s| s.as_bytes() == MARKER)
            || inner.iter().any(|s| s.as_bytes() == MARKER)
        {
            return Err(VPathError::ArchiveAddressing);
        }
        let mut segments = outer.segments.clone();
        segments.push(marker());
        segments.extend_from_slice(inner);
        Ok(Self {
            scheme: Scheme::new(&format!("{format}+{}", outer.scheme()))?,
            authority: outer.authority.clone(),
            segments,
        })
    }

    /// Deshace [`Self::archive_compose`]: `Ok(None)` si el scheme no es
    /// compuesto (el prefijo hasta el primer `+` no es un formato de
    /// [`ARCHIVE_FORMATS`] — `s3+v2.x-y` es un scheme de provider
    /// legítimo, no un archivo). Corta en el PRIMER segmento `!`; los `!`
    /// posteriores quedan en el interior (el índice del provider jamás
    /// los contiene → `NotFound` aguas abajo). Puramente sintáctico: no
    /// valida que el exterior nombre un archivo.
    ///
    /// ```
    /// use norte_proto::VPath;
    /// let plano = VPath::parse("file:///a.zip").unwrap();
    /// assert!(plano.archive_split().unwrap().is_none());
    /// ```
    ///
    /// # Errors
    /// [`VPathError::ArchiveAddressing`] si el scheme es compuesto pero no
    /// hay marcador `!` en el path. [`VPathError::InvalidScheme`] si tras
    /// quitar el formato el scheme interior es a su vez compuesto
    /// (anidamiento, fuera de v1 — ADR 0018) o queda vacío.
    pub fn archive_split(&self) -> Result<Option<ArchiveRef>, VPathError> {
        let Some(format) = scheme_format_prefix(self.scheme.as_str()) else {
            return Ok(None);
        };
        let inner_scheme = &self.scheme.as_str()[format.len() + 1..];
        if scheme_format_prefix(inner_scheme).is_some() {
            return Err(VPathError::InvalidScheme);
        }
        let marker_pos = self
            .segments
            .iter()
            .position(|s| s.as_bytes() == MARKER)
            .ok_or(VPathError::ArchiveAddressing)?;
        Ok(Some(ArchiveRef {
            format: format.to_owned(),
            outer: Self {
                scheme: Scheme::new(inner_scheme)?,
                authority: self.authority.clone(),
                segments: self.segments[..marker_pos].to_vec(),
            },
            inner: self.segments[marker_pos + 1..].to_vec(),
        }))
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

/// `true` si `c` no debe ir crudo a un display (terminal/GUI): controles
/// C0/C1 **y** formateadores bidi/invisibles. Estos últimos (overrides RTL
/// como U+202E, marcas de dirección, zero-width, BOM) permiten spoofing
/// visual del nombre —un `.exe` que se ve como `.jpg`— sin ser `is_control`
/// (issue #21).
fn is_display_hazard(c: char) -> bool {
    // Controles C0/C1 + formateadores/overrides BIDI. NO se enmascaran
    // ZWJ/ZWNJ (U+200C/200D): son legítimos en secuencias emoji y en escrituras
    // (persa, índicas) — el vector de #21 es la dirección bidi, no la unión.
    c.is_control()
        || matches!(c,
            '\u{200E}' | '\u{200F}' | '\u{061C}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
        )
}

/// El segmento marcador de ADR 0018.
const MARKER: &[u8] = b"!";

/// El token de formato si `scheme` es compuesto (`zip+file` → `Some("zip")`);
/// `None` si el prefijo hasta el primer `+` no es un formato registrado.
fn scheme_format_prefix(scheme: &str) -> Option<&str> {
    let (prefix, _) = scheme.split_once('+')?;
    ARCHIVE_FORMATS.contains(&prefix).then_some(prefix)
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
