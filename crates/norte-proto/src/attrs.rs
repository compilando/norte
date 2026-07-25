//! Provider attributes (0.30.0, ADR 0039): typed, on-demand metadata a
//! provider publishes beyond [`Entry`](crate::Entry)'s four fields — POSIX
//! mode, SFTP owner, S3 storage class, archive packed size.
//!
//! Two pieces land in this module: the attribute vocabulary ([`AttrType`],
//! [`AttrHint`]) and [`AttrInfo`] (what a provider offers, discovered
//! through `fs.capabilities`). An id requested in `fs.list`/`fs.stat` is a
//! plain `String`, validated with [`is_valid_attr_id`]. The value type
//! carried in `Entry::attrs` — `AttrValue` — is a separate task; this block
//! is the descriptor side only.

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
/// Maximum length of an `AttrValue::Text` value, in bytes.
pub const ATTR_TEXT_MAX: usize = 256;
/// Maximum length of an `AttrValue::Bytes` value, in bytes AFTER decoding.
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
