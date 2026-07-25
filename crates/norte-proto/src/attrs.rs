//! Provider attributes (0.30.0, ADR 0039): typed, on-demand metadata a
//! provider publishes beyond [`Entry`](crate::Entry)'s four fields — POSIX
//! mode, SFTP owner, S3 storage class, archive packed size.
//!
//! Three pieces: [`AttrInfo`] (what a provider offers, discovered through
//! `fs.capabilities`), an id requested in `fs.list`/`fs.stat`, and
//! [`AttrValue`] (the cell itself, carried in `Entry::attrs`).

use serde::{Deserialize, Serialize};

/// Maximum requested attribute ids per `fs.list`/`fs.stat` call. Enforced
/// server-side; more than this is `-32602`.
pub const ATTRS_MAX_REQUEST: usize = 16;
/// Maximum length of an attribute id, in bytes.
pub const ATTR_ID_MAX: usize = 64;
/// Maximum length of [`AttrInfo::label`], in bytes.
pub const ATTR_LABEL_MAX: usize = 64;
/// Maximum length of an [`AttrValue::Text`] value, in bytes.
pub const ATTR_TEXT_MAX: usize = 256;
/// Maximum length of an [`AttrValue::Bytes`] value, in bytes AFTER decoding.
pub const ATTR_BYTES_MAX: usize = 256;

/// Is `id` a well-formed attribute id: `[a-z0-9._-]`, non-empty, at most
/// [`ATTR_ID_MAX`] bytes. Namespaced by its origin (`posix.mode`), with no
/// central registry — a provider owns its namespace (ADR 0039).
///
/// ```
/// use norte_proto::attrs::is_valid_attr_id;
/// assert!(is_valid_attr_id("s3.storage_class"));
/// assert!(!is_valid_attr_id("S3.StorageClass"));
/// ```
#[must_use]
pub fn is_valid_attr_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= ATTR_ID_MAX
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Declared type of an attribute. An unknown value (protocol N+1) degrades to
/// [`AttrType::Unknown`]: an old client ignores the attribute, it never breaks.
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
    /// Type of a newer protocol.
    #[serde(other)]
    Unknown,
}

/// What an attribute MEANS, so a frontend can pick a default format and
/// alignment without a hard-coded table of ids. Two `Uint`s render very
/// differently depending on this.
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
    /// Hint of a newer protocol.
    #[serde(other)]
    Unknown,
}

/// One attribute a provider offers, as advertised by `fs.capabilities`
/// ([`FsCapabilitiesResult::attrs`](crate::methods::FsCapabilitiesResult)).
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
    pub label: String,
    /// Declared type of the values of this attribute.
    #[serde(rename = "type")]
    pub ty: AttrType,
    /// Presentation hint.
    pub hint: AttrHint,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_validos_y_rechazados() {
        for ok in ["posix.mode", "s3.storage_class", "archive.packed-size", "a"] {
            assert!(is_valid_attr_id(ok), "{ok} debería valer");
        }
        for bad in [
            "",
            "Posix.mode",
            "posix mode",
            "posix/mode",
            "ñ",
            &"a".repeat(65),
        ] {
            assert!(!is_valid_attr_id(bad), "{bad:?} debería rechazarse");
        }
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
