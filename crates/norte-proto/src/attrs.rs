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
/// An [`AttrInfo`] whose id fails this check is DROPPED from the catalog,
/// and an `Entry.attrs` key that fails it is DROPPED from that entry —
/// never a hard error, mirroring the "unknown requested id comes back
/// absent" rule (ADR 0039 §5).
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
/// An `AttrInfo` whose `id` fails [`is_valid_attr_id`] is DROPPED from the
/// catalog, and an `Entry.attrs` key that fails it is DROPPED from that
/// entry — never a hard error, mirroring the "unknown requested id comes
/// back absent" rule (ADR 0039 §5).
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

/// The value of one attribute for one entry (ADR 0039).
///
/// Absence of a key in `Entry::attrs` means the provider does not know the
/// value — there is never a fabricated `0`, exactly as
/// [`Entry::size`](crate::Entry::size) is `None` rather than zero.
///
/// Wire: a one-key object tagged by variant — `{"uint": 33188}`,
/// `{"bytes_b64": "//4="}`. Deserialisation is hand-written (the same route
/// [`CapabilityFlags`](crate::CapabilityFlags) takes) so that a variant from a
/// newer protocol degrades to [`AttrValue::Unknown`] instead of failing the
/// whole entry: `#[serde(other)]` cannot express a catch-all on a
/// data-carrying enum.
///
/// ```
/// use norte_proto::attrs::AttrValue;
/// let v: AttrValue = serde_json::from_str(r#"{"variante_del_futuro": 1}"#).unwrap();
/// assert_eq!(v, AttrValue::Unknown);
/// let n: AttrValue = serde_json::from_str(r#"{"uint": 33188}"#).unwrap();
/// assert_eq!(n, AttrValue::Uint(33188));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AttrValue {
    /// Unsigned integer (mode word, packed size, uid).
    Uint(u64),
    /// Signed integer.
    Int(i64),
    /// UTF-8 text. THIRD-PARTY — mask before painting.
    Text(String),
    /// Raw bytes (base64 on the wire): an owner name that is not UTF-8.
    Bytes(Vec<u8>),
    /// Milliseconds since the UTC epoch; negative is valid.
    TimeMs(i64),
    /// Boolean.
    Bool(bool),
    /// A value this protocol version does not understand, or a corrupt
    /// base64 payload. Never emitted by a conforming daemon; it exists so one
    /// bad cell costs one cell, not the listing.
    Unknown,
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
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ValueVisitor;

        impl<'de> serde::de::Visitor<'de> for ValueVisitor {
            type Value = AttrValue;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a one-key attribute value object, e.g. {\"uint\": 1}")
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<AttrValue, M::Error> {
                use base64::Engine as _;

                let Some(tag) = map.next_key::<String>()? else {
                    // Objeto VACÍO: peer roto, no peer nuevo. Error duro.
                    return Err(serde::de::Error::custom("empty attribute value object"));
                };
                let value = match tag.as_str() {
                    "uint" => AttrValue::Uint(map.next_value()?),
                    "int" => AttrValue::Int(map.next_value()?),
                    "text" => AttrValue::Text(map.next_value()?),
                    "bytes_b64" => {
                        let b64: String = map.next_value()?;
                        match base64::engine::general_purpose::STANDARD.decode(&b64) {
                            Ok(bytes) => AttrValue::Bytes(bytes),
                            // Celda corrupta: degrada la CELDA, no la entrada.
                            Err(_) => AttrValue::Unknown,
                        }
                    }
                    "time_ms" => AttrValue::TimeMs(map.next_value()?),
                    "bool" => AttrValue::Bool(map.next_value()?),
                    _ => {
                        // Variante de un protocolo MÁS NUEVO (ADR 0004).
                        map.next_value::<serde::de::IgnoredAny>()?;
                        AttrValue::Unknown
                    }
                };
                // Claves de más: se ignoran (mismo criterio indulgente que
                // cualquier campo desconocido de un struct del wire).
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(value)
            }
        }

        deserializer.deserialize_map(ValueVisitor)
    }
}

// `AttrValue` serializes as a one-key tagged object, so its schema is an
// object with one optional property per variant; the serde impls are
// hand-written and cannot derive (same situation as `CapabilityFlags`).
#[cfg(feature = "schema")]
impl schemars::JsonSchema for AttrValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AttrValue".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "One-key tagged attribute value; an unknown key degrades to Unknown.",
            "properties": {
                "uint": { "type": "integer", "minimum": 0 },
                "int": { "type": "integer" },
                "text": { "type": "string" },
                "bytes_b64": { "type": "string" },
                "time_ms": { "type": "integer" },
                "bool": { "type": "boolean" },
                "unknown": { "type": "null" }
            },
            "minProperties": 1
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
    fn envelope_roto_es_error_duro() {
        // Un peer ROTO (no uno más nuevo): no hay nada que degradar.
        assert!(serde_json::from_str::<AttrValue>("42").is_err());
        assert!(serde_json::from_str::<AttrValue>("{}").is_err());
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
