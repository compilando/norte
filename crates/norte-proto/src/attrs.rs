//! Provider attributes (0.30.0, ADR 0039): typed, on-demand metadata a
//! provider publishes beyond [`Entry`](crate::Entry)'s four fields — POSIX
//! mode, SFTP owner, S3 storage class, archive packed size.
//!
//! Three pieces land in this module: the attribute vocabulary ([`AttrType`],
//! [`AttrHint`]), [`AttrInfo`] (what a provider offers, discovered through
//! `fs.capabilities`), and [`AttrValue`] (the per-entry cell carried in
//! `Entry::attrs`). An id requested in `fs.list`/`fs.stat` is a plain
//! `String`, validated with [`is_valid_attr_id`].

use serde::{Deserialize, Serialize};

/// Maximum requested attribute ids per `fs.list`/`fs.stat` call. Enforced
/// server-side; more than this is `-32602`.
pub const ATTRS_MAX_REQUEST: usize = 16;
/// Maximum number of [`AttrInfo`] entries a `fs.capabilities` catalog may
/// advertise. Unlike [`ATTRS_MAX_REQUEST`], exceeding this is NOT an error: a
/// fat catalog is a buggy provider, not a broken peer, so a client TRUNCATES
/// the advertised `Vec<AttrInfo>` beyond this many entries instead of
/// rejecting the response — the same spirit as an unrequested/unknown id
/// coming back absent rather than failing the call.
pub const ATTRS_MAX_ADVERTISED: usize = 64;
/// Maximum length of an attribute id, in bytes.
pub const ATTR_ID_MAX: usize = 64;
/// Maximum length of [`AttrInfo::label`], in bytes.
pub const ATTR_LABEL_MAX: usize = 64;
/// Maximum length of an [`AttrValue::Text`] value, in bytes.
pub const ATTR_TEXT_MAX: usize = 256;
/// Maximum length of an [`AttrValue::Bytes`] value, in bytes AFTER decoding.
pub const ATTR_BYTES_MAX: usize = 256;

/// Is `id` a well-formed, NAMESPACED attribute id: at least one `.`,
/// non-empty `.`-separated segments each made of `[a-z0-9_-]`, at most
/// [`ATTR_ID_MAX`] bytes total. Namespaced by its origin (`posix.mode`,
/// `archive.packed_size`), with no central registry — a provider owns its
/// namespace (ADR 0039). A bare word with no dot (`mode`), a dot with an
/// empty segment on either side (`posix.`, `.mode`, `.`, `..`), or anything
/// outside the byte class is rejected: relaxing this rule later is
/// backward-compatible, tightening it after 0.30 ships is not.
///
/// An `Entry.attrs` key that fails this check is DROPPED from that entry by
/// [`Entry::attrs`](crate::Entry::attrs)'s own deserialisation — never a hard
/// error, mirroring the "unknown requested id comes back absent" rule (ADR
/// 0039 §5). The same rule is a REQUIREMENT ON THE RECEIVER for an advertised
/// catalog: an `AttrInfo` with a malformed id must be discarded rather than
/// requested. Nothing enforces that yet, because `FsCapabilitiesResult` does
/// not carry the catalog until the next task of this block; it becomes a
/// property of the type when that field lands.
///
/// ```
/// use norte_proto::attrs::is_valid_attr_id;
/// assert!(is_valid_attr_id("s3.storage_class"));
/// assert!(!is_valid_attr_id("S3.StorageClass"));
/// assert!(!is_valid_attr_id("mode"));
/// ```
#[must_use]
pub fn is_valid_attr_id(id: &str) -> bool {
    fn valid_segment(seg: &str) -> bool {
        !seg.is_empty()
            && seg
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
    }

    !id.is_empty()
        && id.len() <= ATTR_ID_MAX
        && id.contains('.')
        && id.split('.').all(valid_segment)
}

/// Declared type of an attribute. An unknown value (protocol N+1) degrades to
/// [`AttrType::Unknown`]: an old client ignores the attribute, it never breaks.
///
/// `"unknown"` is a RESERVED wire tag: no future protocol version may name a
/// real variant that. A component that reads a value and writes it back
/// (a proxy, a cache) collapses a newer tag to `unknown` in the round trip —
/// the same accepted trade-off as [`EntryKind::Other`](crate::EntryKind::Other).
///
/// ```
/// use norte_proto::attrs::AttrType;
/// let t: AttrType = serde_json::from_str("\"tipo_del_futuro\"").unwrap();
/// assert_eq!(t, AttrType::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrType {
    /// Unsigned integer.
    Uint,
    /// Signed integer.
    Int,
    /// UTF-8 text (third-party: mask before painting).
    Text,
    /// Raw bytes, base64 on the wire (a name that is not UTF-8).
    Bytes,
    /// Milliseconds since the UTC epoch; negative is valid (pre-1970).
    TimeMs,
    /// Boolean.
    Bool,
    /// Type of a newer protocol (fallback of deserialisation). A conforming
    /// peer NEVER emits it as a declared type; a client that sees it on an
    /// [`AttrInfo`] should not request that attribute at all, since it has
    /// no way to interpret the values it would get back.
    #[serde(other)]
    Unknown,
}

/// What an attribute MEANS, so a frontend can pick a default format and
/// alignment without a hard-coded table of ids. Two `Uint`s render very
/// differently depending on this.
///
/// `"unknown"` is a RESERVED wire tag: no future protocol version may name a
/// real variant that. A component that reads a value and writes it back
/// (a proxy, a cache) collapses a newer tag to `unknown` in the round trip —
/// the same accepted trade-off as [`EntryKind::Other`](crate::EntryKind::Other).
///
/// ```
/// use norte_proto::attrs::AttrHint;
/// let h: AttrHint = serde_json::from_str("\"pista_del_futuro\"").unwrap();
/// assert_eq!(h, AttrHint::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrHint {
    /// A byte count: right-aligned, IEC/SI formatting applies.
    Size,
    /// A point in time: date formatting applies.
    Timestamp,
    /// A POSIX permission word: octal or `rwx` formatting applies.
    Mode,
    /// A user/group/owner identity: short text or a number.
    Identity,
    /// No presentation advice; render as plain text.
    Opaque,
    /// Hint of a newer protocol (fallback of deserialisation). A conforming
    /// peer NEVER emits it; a frontend that encounters it renders the value
    /// as if it were [`AttrHint::Opaque`].
    #[serde(other)]
    Unknown,
}

/// One attribute a provider offers, as advertised by `fs.capabilities`
/// ([`FsCapabilitiesResult::attrs`](crate::methods::FsCapabilitiesResult)).
///
/// An `AttrInfo` whose `id` fails [`is_valid_attr_id`] must be DISCARDED by
/// the receiver rather than requested — never a hard error, mirroring the
/// "unknown requested id comes back absent" rule (ADR 0039 §5). That is a
/// requirement on the reader for now: the catalog field does not exist until
/// the next task of this block adds `FsCapabilitiesResult::attrs`, which is
/// where the filter will live. The equivalent rule for entry keys is already
/// enforced by the type — see [`Entry::attrs`](crate::Entry::attrs).
///
/// `label` is provider text and therefore THIRD-PARTY (a WASM provider plugin
/// writes it, an SFTP server influences it): mask it exactly like a plugin's
/// column header before painting, and clamp it to [`ATTR_LABEL_MAX`].
///
/// ```
/// use norte_proto::attrs::{AttrHint, AttrInfo, AttrType};
/// let info = AttrInfo {
///     id: "posix.mode".to_owned(),
///     label: "Mode".to_owned(),
///     ty: AttrType::Uint,
///     hint: AttrHint::Mode,
/// };
/// let wire = serde_json::to_string(&info).unwrap();
/// assert!(wire.contains("\"type\":\"uint\""));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AttrInfo {
    /// Attribute id, namespaced ([`is_valid_attr_id`]).
    pub id: String,
    /// Human label. THIRD-PARTY text — mask and clamp before painting.
    ///
    /// This is a FALLBACK, not the primary source of a column header: a
    /// frontend should prefer a localized string keyed by the stable `id`
    /// (e.g. `t!("attr.posix.mode")`) and fall back to this masked provider
    /// label only for ids it does not recognize. First-party attributes
    /// still route their labels through `i18n/`.
    pub label: String,
    /// Declared type of the values of this attribute.
    #[serde(rename = "type")]
    pub ty: AttrType,
    /// Presentation hint.
    pub hint: AttrHint,
}

/// Decodes the attribute map of an [`Entry`](crate::Entry) under the
/// receive-side rules of ADR 0039 §4/§5; the contract itself is documented on
/// [`Entry::attrs`](crate::Entry::attrs), which is what a caller reads.
pub(crate) fn deserialize_attr_map<'de, D>(
    deserializer: D,
) -> Result<std::collections::BTreeMap<String, AttrValue>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use std::collections::BTreeMap;

    struct AttrMapVisitor;

    impl<'de> serde::de::Visitor<'de> for AttrMapVisitor {
        type Value = BTreeMap<String, AttrValue>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a map of attribute id to attribute value")
        }

        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut access: M,
        ) -> Result<Self::Value, M::Error> {
            // Se conservan a lo sumo `ATTRS_MAX_REQUEST` claves — las MENORES
            // en orden de bytes — descartando los ids mal formados, y sin
            // llegar a parsear el valor de lo que se descarta (nunca hay más
            // de `ATTRS_MAX_REQUEST + 1` entradas vivas a la vez). El
            // contrato completo está en el rustdoc de `Entry::attrs`.
            let mut out: BTreeMap<String, AttrValue> = BTreeMap::new();
            while let Some(key) = access.next_key::<String>()? {
                // Id mal formado: se descarta la CLAVE, jamás la entrada.
                let keep = is_valid_attr_id(&key)
                    && (out.len() < ATTRS_MAX_REQUEST
                        // Una clave REPETIDA ya ocupa su hueco: sobrescribe
                        // (last-wins, como cualquier parser JSON) en vez de
                        // depender de si el mapa estaba lleno al llegar.
                        || out.contains_key(&key)
                        // Con el mapa lleno, una clave que ordena DESPUÉS de
                        // la peor que ya se guarda no puede entrar: ni se
                        // parsea su valor.
                        || out.last_key_value().is_some_and(|(peor, _)| &key < peor));
                if !keep {
                    access.next_value::<serde::de::IgnoredAny>()?;
                    continue;
                }
                out.insert(key, access.next_value()?);
                if out.len() > ATTRS_MAX_REQUEST {
                    out.pop_last();
                }
            }
            Ok(out)
        }
    }

    deserializer.deserialize_map(AttrMapVisitor)
}

/// Maximum length of the base64 TEXT of an `AttrValue::Bytes` payload: the
/// RFC 4648 expansion of [`ATTR_BYTES_MAX`] (4 characters per 3-byte group,
/// padded). Checked BEFORE decoding, so an oversized payload never allocates
/// the buffer it asks for.
const ATTR_BYTES_B64_MAX: usize = 4 * ATTR_BYTES_MAX.div_ceil(3);

/// The value of one attribute for one entry (ADR 0039).
///
/// Absence of a key in `Entry::attrs` means the provider does not know the
/// value — there is never a fabricated `0`, exactly as
/// [`Entry::size`](crate::Entry::size) is `None` rather than zero.
///
/// Wire: a one-key object tagged by variant — `{"uint": 33188}`,
/// `{"bytes_b64": "//4="}`. Deserialisation is hand-written (the same route
/// [`CapabilityFlags`](crate::CapabilityFlags) takes) because
/// `#[serde(other)]` cannot express a catch-all on a data-carrying enum.
///
/// # Every malformed value degrades; NOTHING here is a hard error
///
/// A value is one cell of one entry of a page, so a bad cell must cost that
/// cell and nothing more (ADR 0039 §3). Deserialisation therefore yields
/// [`AttrValue::Unknown`], never an error, for all of:
///
/// - a tag this protocol version does not know (a newer peer, ADR 0004);
/// - an object carrying TWO OR MORE known tags (ambiguous — key order in JSON
///   is not significant, so "the first one wins" would make the result depend
///   on the whim of a relay that round-trips through a sorted map);
/// - a payload of the wrong JSON type (`{"uint": "33188"}`);
/// - a non-object, a `null`, or an empty object;
/// - undecodable base64;
/// - a payload OVER the caps — [`ATTR_TEXT_MAX`] bytes of `Text`, or
///   [`ATTR_BYTES_MAX`] bytes of `Bytes` after decoding. The caps are
///   enforced here, at decode; the daemon additionally enforces them on emit.
///
/// The containing message still fails when the JSON itself is unparseable or
/// `attrs` is not an object — that is serde's job on the parent type.
///
/// `"unknown"` is a RESERVED wire tag: no future protocol version may name a
/// real variant that. A component that reads a value and writes it back
/// (a proxy, a cache) collapses a newer tag to `unknown` in the round trip —
/// the same accepted trade-off as [`EntryKind::Other`](crate::EntryKind::Other).
/// The loss is strictly worse here than for [`AttrType`]/[`AttrHint`]: those
/// lose a TAG, this loses the tag AND its payload. A relay that means to be
/// transparent must forward the raw JSON of the cell rather than a
/// re-serialised `AttrValue`.
///
/// ```
/// use norte_proto::attrs::AttrValue;
/// let v: AttrValue = serde_json::from_str(r#"{"variante_del_futuro": 1}"#).unwrap();
/// assert_eq!(v, AttrValue::Unknown);
/// let n: AttrValue = serde_json::from_str(r#"{"uint": 33188}"#).unwrap();
/// assert_eq!(n, AttrValue::Uint(33188));
/// // Un payload del tipo JSON equivocado degrada la CELDA, no la entrada.
/// let malo: AttrValue = serde_json::from_str(r#"{"uint": "33188"}"#).unwrap();
/// assert_eq!(malo, AttrValue::Unknown);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AttrValue {
    /// Unsigned integer (mode word, packed size, uid).
    Uint(u64),
    /// Signed integer.
    Int(i64),
    /// UTF-8 text. THIRD-PARTY — mask before painting. Over
    /// [`ATTR_TEXT_MAX`] bytes degrades to [`AttrValue::Unknown`].
    Text(String),
    /// Raw bytes (base64 on the wire): an owner name that is not UTF-8.
    ///
    /// EMITTED as RFC 4648 §4 — the standard alphabet (`+`, `/`) with
    /// padding required. On READ the unpadded and URL-safe (`-`, `_`) forms
    /// are accepted too, tried in that order: a producer using Go's
    /// `RawStdEncoding` would otherwise lose every `Bytes` cell silently, and
    /// widening what is accepted is backward-compatible. More than
    /// [`ATTR_BYTES_MAX`] bytes AFTER decoding degrades to
    /// [`AttrValue::Unknown`].
    Bytes(Vec<u8>),
    /// Milliseconds since the UTC epoch; negative is valid.
    TimeMs(i64),
    /// Boolean.
    Bool(bool),
    /// A value this protocol version does not understand, or one that is
    /// malformed in any of the ways listed on the type. Never emitted by a
    /// conforming daemon; it exists so one bad cell costs one cell, not the
    /// listing.
    Unknown,
}

impl AttrValue {
    /// Wire tags this protocol version recognises, in variant order.
    const TAGS: [&'static str; 7] = [
        "uint",
        "int",
        "text",
        "bytes_b64",
        "time_ms",
        "bool",
        "unknown",
    ];

    /// Interpret an already-parsed JSON value as a cell, degrading to
    /// [`AttrValue::Unknown`] rather than failing (see the type docs).
    ///
    /// Every known tag is looked up by name, so the result does NOT depend on
    /// the order the peer serialised its keys in — JSON objects are unordered
    /// (RFC 8259 §4) and a relay that round-trips through a sorted map would
    /// otherwise change the meaning of the very same document.
    fn from_wire(raw: &serde_json::Value) -> Self {
        use base64::Engine as _;

        let serde_json::Value::Object(map) = raw else {
            return AttrValue::Unknown;
        };

        let mut found: Option<(&str, &serde_json::Value)> = None;
        for tag in Self::TAGS {
            if let Some(payload) = map.get(tag) {
                if found.is_some() {
                    // Dos etiquetas conocidas: ambiguo, y un peer conforme
                    // jamás lo emite. Degradar mantiene el resultado
                    // independiente del orden de claves.
                    return AttrValue::Unknown;
                }
                found = Some((tag, payload));
            }
        }
        // Claves no reconocidas de más: se ignoran (mismo criterio indulgente
        // que cualquier campo desconocido de un struct del wire).
        let Some((tag, payload)) = found else {
            return AttrValue::Unknown;
        };

        match tag {
            "uint" => payload.as_u64().map_or(AttrValue::Unknown, AttrValue::Uint),
            "int" => payload.as_i64().map_or(AttrValue::Unknown, AttrValue::Int),
            "text" => match payload.as_str() {
                Some(s) if s.len() <= ATTR_TEXT_MAX => AttrValue::Text(s.to_owned()),
                _ => AttrValue::Unknown,
            },
            "bytes_b64" => {
                let Some(b64) = payload.as_str() else {
                    return AttrValue::Unknown;
                };
                // Tope ANTES de decodificar: nunca se reserva el buffer que
                // un payload gigante pide.
                if b64.len() > ATTR_BYTES_B64_MAX {
                    return AttrValue::Unknown;
                }
                let engines = [
                    &base64::engine::general_purpose::STANDARD,
                    &base64::engine::general_purpose::STANDARD_NO_PAD,
                    &base64::engine::general_purpose::URL_SAFE,
                    &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                ];
                engines
                    .iter()
                    .find_map(|engine| engine.decode(b64).ok())
                    .filter(|bytes| bytes.len() <= ATTR_BYTES_MAX)
                    .map_or(AttrValue::Unknown, AttrValue::Bytes)
            }
            "time_ms" => payload
                .as_i64()
                .map_or(AttrValue::Unknown, AttrValue::TimeMs),
            "bool" => payload
                .as_bool()
                .map_or(AttrValue::Unknown, AttrValue::Bool),
            // `"unknown"` es RESERVADA: cualquier payload vuelve a Unknown.
            _ => AttrValue::Unknown,
        }
    }
}

impl Serialize for AttrValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        use serde::ser::SerializeMap as _;

        let mut map = serializer.serialize_map(Some(1))?;
        match self {
            AttrValue::Uint(v) => map.serialize_entry("uint", v)?,
            AttrValue::Int(v) => map.serialize_entry("int", v)?,
            AttrValue::Text(v) => map.serialize_entry("text", v)?,
            AttrValue::Bytes(v) => map.serialize_entry(
                "bytes_b64",
                &base64::engine::general_purpose::STANDARD.encode(v),
            )?,
            AttrValue::TimeMs(v) => map.serialize_entry("time_ms", v)?,
            AttrValue::Bool(v) => map.serialize_entry("bool", v)?,
            AttrValue::Unknown => map.serialize_entry("unknown", &Option::<()>::None)?,
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for AttrValue {
    /// Parses through [`serde_json::Value`] on purpose: JSON is the protocol's
    /// only encoding (ADR 0011), and going through a self-describing value
    /// turns "the payload has the wrong type" into a MATCH ARM instead of a
    /// deserialiser error — which is what lets a bad cell degrade rather than
    /// sink the entry that carries it. It is also the entry point for a
    /// non-map input (`42`, `null`, `"texto"`): those arrive as values here
    /// and degrade, where `deserialize_map` would have raised.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(deserializer)?;
        Ok(AttrValue::from_wire(&raw))
    }
}

// `AttrValue` EMITS exactly one key (`maxProperties`), tagged by variant; the
// serde impls are hand-written and cannot derive (same situation as
// `CapabilityFlags`), so this schema is hand-written to match them. What it
// ACCEPTS is wider — `additionalProperties` stays open, which is the
// forward-compat door an unknown tag walks through (ADR 0004).
#[cfg(feature = "schema")]
impl schemars::JsonSchema for AttrValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AttrValue".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "One-key tagged attribute value; an unknown, ambiguous, \
                            ill-typed or over-cap key degrades to Unknown.",
            "properties": {
                "uint": { "type": "integer", "format": "uint64", "minimum": 0 },
                "int": { "type": "integer", "format": "int64" },
                "text": { "type": "string", "maxLength": ATTR_TEXT_MAX },
                "bytes_b64": {
                    "type": "string",
                    "contentEncoding": "base64",
                    "maxLength": ATTR_BYTES_B64_MAX
                },
                "time_ms": { "type": "integer", "format": "int64" },
                "bool": { "type": "boolean" },
                "unknown": { "type": "null" }
            },
            "minProperties": 1,
            "maxProperties": 1
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_validos_y_rechazados() {
        for ok in ["posix.mode", "s3.storage_class", "archive.packed-size"] {
            assert!(is_valid_attr_id(ok), "{ok} debería valer");
        }

        // Exactamente ATTR_ID_MAX bytes vale (frontera inclusive).
        let exactamente_max = format!("a.{}", "b".repeat(ATTR_ID_MAX - 2));
        assert_eq!(exactamente_max.len(), ATTR_ID_MAX);
        assert!(
            is_valid_attr_id(&exactamente_max),
            "exactamente ATTR_ID_MAX bytes debería valer"
        );

        for bad in [
            "",
            "a",
            "mode",
            "Posix.mode",
            "posix mode",
            "posix/mode",
            "ñ",
            "posix.",
            ".mode",
            ".",
            "..",
        ] {
            assert!(!is_valid_attr_id(bad), "{bad:?} debería rechazarse");
        }

        // ATTR_ID_MAX + 1 bytes se rechaza (frontera exclusiva: pinea `<=`, no `<`).
        let uno_mas_que_max = format!("a.{}", "b".repeat(ATTR_ID_MAX - 1));
        assert_eq!(uno_mas_que_max.len(), ATTR_ID_MAX + 1);
        assert!(
            !is_valid_attr_id(&uno_mas_que_max),
            "ATTR_ID_MAX + 1 bytes debería rechazarse"
        );
    }

    #[test]
    fn attr_type_desconocido_degrada() {
        let t: AttrType = serde_json::from_str("\"tipo_del_futuro\"").unwrap();
        assert_eq!(t, AttrType::Unknown);
    }

    #[test]
    fn attr_hint_desconocido_degrada() {
        let h: AttrHint = serde_json::from_str("\"pista_del_futuro\"").unwrap();
        assert_eq!(h, AttrHint::Unknown);
    }

    fn round_trip(v: &AttrValue) -> AttrValue {
        let wire = serde_json::to_string(v).unwrap();
        serde_json::from_str(&wire).unwrap()
    }

    #[test]
    fn cada_variante_round_trip() {
        for v in [
            AttrValue::Uint(33188),
            AttrValue::Int(-7),
            AttrValue::Text("STANDARD_IA".to_owned()),
            AttrValue::Bytes(vec![0xFF, 0xFE, b'a']),
            AttrValue::TimeMs(-86_400_000),
            AttrValue::Bool(true),
        ] {
            assert_eq!(round_trip(&v), v);
        }
    }

    #[test]
    fn wire_de_bytes_es_base64() {
        let wire = serde_json::to_string(&AttrValue::Bytes(vec![0xFF, 0xFE])).unwrap();
        assert_eq!(wire, r#"{"bytes_b64":"//4="}"#);
    }

    #[test]
    fn variante_del_futuro_degrada_a_unknown() {
        let v: AttrValue = serde_json::from_str(r#"{"quaternion":[1,2,3,4]}"#).unwrap();
        assert_eq!(v, AttrValue::Unknown);
    }

    #[test]
    fn base64_corrupto_degrada_a_unknown() {
        let v: AttrValue = serde_json::from_str(r#"{"bytes_b64":"no es base64 !!"}"#).unwrap();
        assert_eq!(v, AttrValue::Unknown);
    }

    #[test]
    fn envelope_roto_degrada_a_unknown() {
        // Ni un envelope roto es error DE VALOR: una celda mala cuesta una
        // celda. Solo el JSON ilegible (o un `attrs` que no es objeto) rompe,
        // y eso lo decide serde en el tipo padre.
        for roto in ["42", "{}", "null", r#""texto""#] {
            assert_eq!(
                serde_json::from_str::<AttrValue>(roto).unwrap(),
                AttrValue::Unknown,
                "{roto} debería degradar"
            );
        }
    }

    #[test]
    fn la_etiqueta_no_depende_del_orden_de_claves() {
        // RFC 8259: un objeto JSON NO está ordenado. Un relay que pase por un
        // mapa ordenado no puede cambiar el significado del documento.
        for wire in [r#"{"uint":1,"aaa":0}"#, r#"{"aaa":0,"uint":1}"#] {
            assert_eq!(
                serde_json::from_str::<AttrValue>(wire).unwrap(),
                AttrValue::Uint(1),
                "{wire} debería dar Uint(1)"
            );
        }
    }

    #[test]
    fn dos_etiquetas_conocidas_son_ambiguas_y_degradan() {
        for wire in [r#"{"uint":1,"bool":true}"#, r#"{"bool":true,"uint":1}"#] {
            assert_eq!(
                serde_json::from_str::<AttrValue>(wire).unwrap(),
                AttrValue::Unknown,
                "{wire} es ambiguo: debería degradar en cualquier orden"
            );
        }
    }

    #[test]
    fn payload_del_tipo_equivocado_degrada() {
        for wire in [
            r#"{"uint":"33188"}"#,
            r#"{"uint":-1}"#,
            r#"{"int":true}"#,
            r#"{"text":42}"#,
            r#"{"bytes_b64":["//4="]}"#,
            r#"{"time_ms":"ayer"}"#,
            r#"{"bool":1}"#,
            r#"{"uint":null}"#,
        ] {
            assert_eq!(
                serde_json::from_str::<AttrValue>(wire).unwrap(),
                AttrValue::Unknown,
                "{wire} debería degradar"
            );
        }
    }

    #[test]
    fn texto_sobre_el_tope_degrada() {
        let justo = "a".repeat(ATTR_TEXT_MAX);
        let v: AttrValue = serde_json::from_value(serde_json::json!({ "text": justo })).unwrap();
        assert_eq!(v, AttrValue::Text(justo), "el tope exacto vale");

        let pasado = "a".repeat(ATTR_TEXT_MAX + 1);
        let v: AttrValue = serde_json::from_value(serde_json::json!({ "text": pasado })).unwrap();
        assert_eq!(v, AttrValue::Unknown, "un byte de más degrada");
    }

    #[test]
    fn bytes_sobre_el_tope_degradan() {
        use base64::Engine as _;

        let justo = base64::engine::general_purpose::STANDARD.encode(vec![0xFFu8; ATTR_BYTES_MAX]);
        assert!(justo.len() <= ATTR_BYTES_B64_MAX);
        let v: AttrValue =
            serde_json::from_value(serde_json::json!({ "bytes_b64": justo })).unwrap();
        assert_eq!(v, AttrValue::Bytes(vec![0xFF; ATTR_BYTES_MAX]));

        let pasado =
            base64::engine::general_purpose::STANDARD.encode(vec![0xFFu8; ATTR_BYTES_MAX + 1]);
        let v: AttrValue =
            serde_json::from_value(serde_json::json!({ "bytes_b64": pasado })).unwrap();
        assert_eq!(v, AttrValue::Unknown, "un byte decodificado de más degrada");

        // Un payload ENORME se rechaza por longitud del texto, sin reservar
        // el buffer que pide.
        let bomba = "A".repeat(1_000_000);
        let v: AttrValue =
            serde_json::from_value(serde_json::json!({ "bytes_b64": bomba })).unwrap();
        assert_eq!(v, AttrValue::Unknown);
    }

    #[test]
    fn acepta_los_cuatro_dialectos_base64() {
        // 0xFB 0xFF -> "+/8=" en el alfabeto estándar, "-_8" sin padding y
        // URL-safe: un productor Go con RawStdEncoding no puede perder la celda.
        let esperado = AttrValue::Bytes(vec![0xFB, 0xFF]);
        for b64 in ["+/8=", "+/8", "-_8=", "-_8"] {
            let v: AttrValue =
                serde_json::from_value(serde_json::json!({ "bytes_b64": b64 })).unwrap();
            assert_eq!(v, esperado, "{b64} debería decodificar");
        }
    }

    #[test]
    fn celda_mala_no_arrastra_a_sus_hermanas() {
        // El invariante de todo el tipo: una celda rota cuesta UNA celda.
        let json = r#"{
            "a": {"uint": 1},
            "b": {"uint": "33188"},
            "c": {"quaternion": [0]},
            "d": {"uint": 1, "bool": true},
            "e": {"bytes_b64": "no es base64 !!"},
            "f": {"bool": false}
        }"#;
        let map: std::collections::BTreeMap<String, AttrValue> =
            serde_json::from_str(json).unwrap();
        assert_eq!(map["a"], AttrValue::Uint(1));
        assert_eq!(map["b"], AttrValue::Unknown);
        assert_eq!(map["c"], AttrValue::Unknown);
        assert_eq!(map["d"], AttrValue::Unknown);
        assert_eq!(map["e"], AttrValue::Unknown);
        assert_eq!(map["f"], AttrValue::Bool(false));
    }

    #[test]
    fn unknown_serializa_y_vuelve_a_unknown() {
        assert_eq!(
            serde_json::to_string(&AttrValue::Unknown).unwrap(),
            r#"{"unknown":null}"#
        );
        assert_eq!(round_trip(&AttrValue::Unknown), AttrValue::Unknown);
    }

    #[test]
    fn una_entrada_con_una_celda_futura_sobrevive_entera() {
        // El punto de la degradación: la ENTRADA no se pierde por una celda.
        let json = r#"{"a":{"uint":1},"b":{"quaternion":[0]},"c":{"bool":false}}"#;
        let map: std::collections::BTreeMap<String, AttrValue> =
            serde_json::from_str(json).unwrap();
        assert_eq!(map["a"], AttrValue::Uint(1));
        assert_eq!(map["b"], AttrValue::Unknown);
        assert_eq!(map["c"], AttrValue::Bool(false));
    }

    #[test]
    fn attr_info_round_trip() {
        let info = AttrInfo {
            id: "posix.mode".to_owned(),
            label: "Mode".to_owned(),
            ty: AttrType::Uint,
            hint: AttrHint::Mode,
        };
        let wire = serde_json::to_string(&info).unwrap();
        assert!(
            wire.contains("\"type\":\"uint\""),
            "el campo se llama `type` en el wire: {wire}"
        );
        assert_eq!(serde_json::from_str::<AttrInfo>(&wire).unwrap(), info);
    }
}
