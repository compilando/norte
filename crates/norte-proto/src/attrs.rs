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
/// Maximum number of advertised descriptors a decoder EXAMINES before it stops
/// looking at all: [`ATTRS_MAX_ADVERTISED`] times four, so a catalog padded
/// with rejects still has room to deliver a full useful one.
///
/// [`ATTRS_MAX_ADVERTISED`] alone bounds only what is KEPT. A peer that sends
/// four million descriptors whose ids are all malformed would fill nothing and
/// therefore be parsed in full — the decoder would do unbounded work for a
/// result of zero. A peer that sends 256 unusable descriptors is buggy; one
/// that sends four million is hostile, and the difference is not worth
/// modelling: past this many examined elements the rest is drained without
/// being materialised, whatever it contains.
pub const ATTRS_MAX_CATALOG_SCAN: usize = ATTRS_MAX_ADVERTISED * 4;
/// Maximum length of an attribute id, in bytes.
pub const ATTR_ID_MAX: usize = 64;
/// Maximum length of [`AttrInfo::label`], in bytes. Enforced at decode by
/// [`sanitize_catalog`], which CLAMPS an over-long label (on a char boundary)
/// rather than dropping the descriptor: the id is what a client acts on, and a
/// fat label is a cosmetic bug, not a reason to lose the attribute.
pub const ATTR_LABEL_MAX: usize = 64;
/// Maximum length of an [`AttrValue::Text`] value, in bytes.
pub const ATTR_TEXT_MAX: usize = 256;
/// Maximum length of an [`AttrValue::Bytes`] value, in bytes AFTER decoding.
pub const ATTR_BYTES_MAX: usize = 256;

/// Is `id` a well-formed, NAMESPACED attribute id: at least one `.`, every
/// `.`-separated segment starting with an ASCII LETTER and continuing in
/// `[a-z0-9_-]`, at most [`ATTR_ID_MAX`] bytes total — the ECMA-262
/// translation the published schema carries is
/// `^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$`. Namespaced by its origin
/// (`posix.mode`, `archive.packed_size`), with no central registry — a
/// provider owns its namespace (ADR 0039). A bare word with no dot (`mode`), a
/// dot with an empty segment on either side (`posix.`, `.mode`, `.`, `..`), or
/// anything outside the byte class is rejected: relaxing this rule later is
/// backward-compatible, tightening it after 0.30 ships is not.
///
/// The leading-letter rule is what rejects the shapes that would be read as
/// something OTHER than an id downstream: `-x.y` is argv-shaped where block 2
/// will take `--attrs <id>`, and `0.0` is float-shaped in a configuration file
/// or a JSON document that keys columns by id. Neither is worth supporting, and
/// 0.30 is the last version where excluding them costs nothing.
///
/// Both RECEIVE-side fields enforce this by the TYPE, never leaving it to a
/// caller, and neither is ever a hard error — the same spirit as "an unknown
/// requested id comes back absent" (ADR 0039 §5):
///
/// - an `Entry.attrs` key that fails this check is DROPPED from that entry by
///   [`Entry::attrs`](crate::Entry::attrs)'s own deserialisation;
/// - an [`AttrInfo`] whose `id` fails it is DROPPED from the advertised
///   catalog by
///   [`FsCapabilitiesResult::attrs`](crate::methods::FsCapabilitiesResult)'s,
///   so a malformed id ACROSS THE WIRE can never be discovered and therefore
///   never requested.
///
/// "Across the wire" is literal, and it matters: the filter lives at the
/// deserialisation boundary, which an EMBEDDED backend (the default TUI/CLI
/// configuration, no daemon in between) never crosses. A catalog obtained
/// in-process — from a provider or, in block 2, from a WASM provider plugin,
/// which the threat model treats as untrusted — must be passed through
/// [`sanitize_catalog`] explicitly. That is the same function the
/// deserialiser calls, so there is exactly one implementation of the rules.
///
/// A REQUEST (`FsListParams::attrs`, `FsStatParams::attrs`) is the exception,
/// deliberately: it is data being sent, not received, so it does not filter —
/// a malformed id there survives decoding and is the daemon's `-32602`.
///
/// ```
/// use norte_proto::attrs::is_valid_attr_id;
/// assert!(is_valid_attr_id("s3.storage_class"));
/// assert!(!is_valid_attr_id("S3.StorageClass"));
/// assert!(!is_valid_attr_id("mode"));
/// // Every segment starts with a LETTER: neither argv-shaped nor float-shaped.
/// assert!(!is_valid_attr_id("-x.y"));
/// assert!(!is_valid_attr_id("0.0"));
/// ```
#[must_use]
pub fn is_valid_attr_id(id: &str) -> bool {
    fn valid_segment(seg: &str) -> bool {
        seg.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
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
    /// Raw bytes, base64 on the wire (a name that is not UTF-8). This is the
    /// one pair whose names differ across the two surfaces: the declared type
    /// is `"bytes"`, while the values it describes are tagged `"bytes_b64"`
    /// ([`AttrValue::Bytes`]) — the value tag names the ENCODING that carries
    /// them, the type names what they ARE.
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
/// An `AttrInfo` whose `id` fails [`is_valid_attr_id`], or that REPEATS an id
/// already advertised, is DISCARDED by [`sanitize_catalog`] — which
/// [`FsCapabilitiesResult::attrs`](crate::methods::FsCapabilitiesResult) runs
/// at decode, and which an embedded backend must run itself. Never a hard
/// error, mirroring the "unknown requested id comes back absent" rule (ADR
/// 0039 §5), and never a duty left to the reader. The catalog is bounded at
/// [`ATTRS_MAX_ADVERTISED`] descriptors there too. The equivalent rule for
/// entry keys is enforced the same way — see
/// [`Entry::attrs`](crate::Entry::attrs).
///
/// `label` is provider text and therefore THIRD-PARTY (a WASM provider plugin
/// writes it, an SFTP server influences it): [`sanitize_catalog`] clamps it to
/// [`ATTR_LABEL_MAX`] bytes, and a frontend still masks it exactly like a
/// plugin's column header before painting.
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
    /// Attribute id, namespaced ([`is_valid_attr_id`]). A descriptor whose id
    /// does not satisfy that check does not survive [`sanitize_catalog`].
    #[cfg_attr(
        feature = "schema",
        schemars(extend(
            "maxLength" = ATTR_ID_MAX,
            "pattern" = r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$"
        ))
    )]
    pub id: String,
    /// Human label. THIRD-PARTY text — masked before painting, and CLAMPED to
    /// [`ATTR_LABEL_MAX`] bytes by [`sanitize_catalog`] at decode.
    ///
    /// This is a FALLBACK, not the primary source of a column header: a
    /// frontend should prefer a localized string keyed by the stable `id`
    /// (e.g. `t!("attr.posix.mode")`) and fall back to this masked provider
    /// label only for ids it does not recognize. First-party attributes
    /// still route their labels through `i18n/`.
    ///
    /// The cap is in BYTES here and the schema's `maxLength` counts CODE POINTS
    /// (JSON Schema 2020-12 §6.3.1): a 60-character CJK label is schema-legal
    /// and still gets clamped to ~21 characters. See ADR 0039 §5 — the
    /// divergence is accepted and pinned by a test, not a bug to "fix" by
    /// counting characters in the code.
    #[cfg_attr(feature = "schema", schemars(extend("maxLength" = ATTR_LABEL_MAX)))]
    pub label: String,
    /// Declared type of the values of this attribute — ADVISORY, and never
    /// authoritative over a cell.
    ///
    /// Nothing stops a `{"type":"uint"}` descriptor from being paired with a
    /// `{"text": …}` cell: both decode cleanly, and only the rule below keeps
    /// two conforming frontends from rendering the same bytes differently
    /// (ADR 0039 §1).
    ///
    /// - At RENDER time the [`AttrValue`]'s own tag decides. A cell whose tag
    ///   contradicts this field is rendered as its actual variant and is NEVER
    ///   coerced into the declared one.
    /// - This field is for column-CONFIGURATION time: it picks the formatter
    ///   and the sort key for a column before any value has arrived. A column
    ///   configured from a `Uint` descriptor that then receives `Text` cells
    ///   sorts them as the text they are.
    /// - Agreement between the two is a PRODUCER obligation, enforced by block
    ///   2's provider conformance suite. A consumer cannot check it: holding
    ///   one cell, it has no way to know whether the catalog or the provider is
    ///   the one that is wrong.
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
            // At most `ATTRS_MAX_REQUEST` keys are kept — the SMALLEST in
            // byte order — dropping malformed ids, and without MATERIALIZING
            // the value of what is dropped: `IgnoredAny` walks its tokens
            // (JSON has to be traversed to be skipped) but builds nothing, so
            // there are never more than `ATTRS_MAX_REQUEST + 1` entries alive
            // at once. The full contract is in `Entry::attrs`'s rustdoc.
            let mut out: BTreeMap<String, AttrValue> = BTreeMap::new();
            while let Some(key) = access.next_key::<String>()? {
                // Malformed id: the KEY is dropped, never the entry.
                let keep = is_valid_attr_id(&key)
                    && (out.len() < ATTRS_MAX_REQUEST
                        // A REPEATED key already occupies its slot: it
                        // overwrites (last-wins, like any JSON parser)
                        // instead of depending on whether the map was full
                        // when it arrived.
                        || out.contains_key(&key)
                        // With the map full, a key that sorts AFTER the
                        // worst one already kept cannot get in: its value is
                        // not even parsed.
                        || out.last_key_value().is_some_and(|(worst, _)| &key < worst));
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

/// Applies the receive-side catalog rules of ADR 0039 §4/§5 to a list of
/// advertised descriptors, in wire order, and never fails: a bad descriptor
/// costs itself, never the catalog and never the `fs.capabilities` call.
///
/// 1. A descriptor whose `id` fails [`is_valid_attr_id`] is DROPPED — an
///    advertised id becomes a requested id, a configuration id and a map
///    lookup downstream.
/// 2. A REPEATED id is dropped, FIRST WINS. A catalog is an ordered list the
///    provider ranked, so "first" is meaningful — the opposite choice from
///    [`Entry::attrs`](crate::Entry::attrs), where keys arrive in a JSON
///    object, which RFC 8259 §4 leaves unordered, so there is no "first" to
///    prefer and a repeat resolves last-wins like any JSON parser. Keeping
///    both copies would be worse than either: a consumer folding the catalog
///    into a map would render an attribute one way and a consumer using
///    `find()` another, from the very same bytes.
/// 3. An over-long `label` is CLAMPED to [`ATTR_LABEL_MAX`] bytes on a CHAR
///    BOUNDARY (never mid-UTF-8, which would produce a `String` that is not
///    valid text). The descriptor survives: a fat label is cosmetic, the id is
///    what a client acts on.
/// 4. The result is truncated to [`ATTRS_MAX_ADVERTISED`] descriptors, keeping
///    the FIRST ones in wire order. Rules 1 and 2 run BEFORE a slot is taken,
///    so rejects and duplicates never eat the budget a legitimate later
///    attribute needs.
///
/// Prefer [`AttrCatalog::new`], which is this function plus the guarantee that
/// the result cannot be un-sanitised afterwards; that is what
/// [`FsCapabilitiesResult::attrs`](crate::methods::FsCapabilitiesResult) holds,
/// on the wire path and the embedded path alike. This function stays public
/// because block 2 assembles and reorders plain `Vec<AttrInfo>` before wrapping
/// one, but a catalog that reaches a frontend should be an `AttrCatalog` — a
/// rule enforced by a type is a rule nobody has to remember.
///
/// ```
/// use norte_proto::attrs::{AttrHint, AttrInfo, AttrType, sanitize_catalog};
/// let info = |id: &str, ty| AttrInfo {
///     id: id.to_owned(),
///     label: "L".to_owned(),
///     ty,
///     hint: AttrHint::Opaque,
/// };
/// let clean = sanitize_catalog(vec![
///     info("MODE", AttrType::Uint),        // malformed id: out
///     info("posix.mode", AttrType::Uint),  // the FIRST one wins
///     info("posix.mode", AttrType::Text),  // repeated id: out
/// ]);
/// assert_eq!(clean.len(), 1);
/// assert_eq!(clean[0].ty, AttrType::Uint);
/// ```
#[must_use]
pub fn sanitize_catalog(catalog: Vec<AttrInfo>) -> Vec<AttrInfo> {
    let mut out: Vec<AttrInfo> = Vec::new();
    for mut info in catalog {
        if out.len() >= ATTRS_MAX_ADVERTISED {
            break;
        }
        // Invalid or repeated id: dropped BEFORE it takes a slot. The linear
        // search walks at most `ATTRS_MAX_ADVERTISED` entries, so a separate
        // set does not pay for itself.
        if !is_valid_attr_id(&info.id) || out.iter().any(|seen| seen.id == info.id) {
            continue;
        }
        clamp_label(&mut info.label);
        out.push(info);
    }
    out
}

/// An advertised attribute catalog that CANNOT hold anything
/// [`sanitize_catalog`] would have removed: the only way to build one runs the
/// sanitiser, and the field is private, so the rules of ADR 0039 §4/§5 hold by
/// construction rather than by anyone remembering to call a function.
///
/// That distinction is the whole reason the type exists. The filter used to
/// live only on the deserialisation boundary, which the DEFAULT embedded
/// TUI/CLI configuration never crosses — no daemon in between — so in block 2 a
/// catalog coming from a WASM provider plugin (untrusted by the threat model)
/// would have reached a frontend with unvalidated ids and unclamped labels
/// unless a human remembered one call. `PluginColumnInfo` (ADR 0037), which
/// validates no id and caps no length, is the precedent that says the call
/// would eventually be forgotten.
///
/// On the wire it is a plain ARRAY of [`AttrInfo`] — serialisation is
/// transparent and deserialisation goes through the same sanitiser, so nothing
/// about the 0.30 wire shape changes.
///
/// ```
/// use norte_proto::attrs::{AttrCatalog, AttrHint, AttrInfo, AttrType};
/// let info = |id: &str| AttrInfo {
///     id: id.to_owned(),
///     label: "L".to_owned(),
///     ty: AttrType::Uint,
///     hint: AttrHint::Opaque,
/// };
/// // A malformed id does not reach the catalog even building it in-process.
/// let catalog = AttrCatalog::new(vec![info("MODE"), info("posix.mode")]);
/// assert_eq!(catalog.len(), 1);
/// assert_eq!(catalog[0].id, "posix.mode");
/// // And the wire is still the plain array.
/// let wire = serde_json::to_string(&catalog).unwrap();
/// assert!(wire.starts_with('['), "{wire}");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttrCatalog(Vec<AttrInfo>);

impl AttrCatalog {
    /// Sanitises `catalog` ([`sanitize_catalog`]) and keeps the result. This is
    /// the ONLY constructor: there is no way to obtain an `AttrCatalog` whose
    /// contents did not go through the rules.
    #[must_use]
    pub fn new(catalog: Vec<AttrInfo>) -> Self {
        Self(sanitize_catalog(catalog))
    }

    /// Does this provider advertise no attribute at all? (An empty catalog is
    /// omitted from the wire, which is what keeps a 0.29 payload byte-identical.)
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::ops::Deref for AttrCatalog {
    type Target = [AttrInfo];

    fn deref(&self) -> &[AttrInfo] {
        &self.0
    }
}

impl From<Vec<AttrInfo>> for AttrCatalog {
    fn from(catalog: Vec<AttrInfo>) -> Self {
        Self::new(catalog)
    }
}

impl IntoIterator for AttrCatalog {
    type Item = AttrInfo;
    type IntoIter = std::vec::IntoIter<AttrInfo>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a AttrCatalog {
    type Item = &'a AttrInfo;
    type IntoIter = std::slice::Iter<'a, AttrInfo>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl Serialize for AttrCatalog {
    /// TRANSPARENT: the wire carries the array, never a wrapper object.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AttrCatalog {
    /// Through the same sanitiser as [`AttrCatalog::new`], and additionally
    /// bounded: elements past [`ATTRS_MAX_CATALOG_SCAN`] are drained without
    /// being materialised, so a catalog padded with rejects costs bounded work
    /// rather than unbounded work for an empty result.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(deserialize_attr_catalog(deserializer)?))
    }
}

/// Clamps `label` to [`ATTR_LABEL_MAX`] bytes on a CHARACTER BOUNDARY: cutting
/// mid UTF-8 sequence is not an option (`String::truncate` would panic, and
/// the loose byte would not be text).
fn clamp_label(label: &mut String) {
    if label.len() <= ATTR_LABEL_MAX {
        return;
    }
    let mut cut = ATTR_LABEL_MAX;
    while cut > 0 && !label.is_char_boundary(cut) {
        cut -= 1;
    }
    label.truncate(cut);
}

/// Decodes the advertised attribute catalog of a
/// [`FsCapabilitiesResult`](crate::methods::FsCapabilitiesResult) under the
/// receive-side rules of ADR 0039 §4/§5 — by calling [`sanitize_catalog`], so
/// the wire path and the embedded path share ONE implementation. The contract
/// itself is documented on
/// [`FsCapabilitiesResult::attrs`](crate::methods::FsCapabilitiesResult),
/// which is what a caller reads.
pub(crate) fn deserialize_attr_catalog<'de, D>(deserializer: D) -> Result<Vec<AttrInfo>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct AttrCatalogVisitor;

    impl<'de> serde::de::Visitor<'de> for AttrCatalogVisitor {
        type Value = Vec<AttrInfo>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a list of advertised attribute descriptors")
        }

        fn visit_seq<S: serde::de::SeqAccess<'de>>(
            self,
            mut access: S,
        ) -> Result<Self::Value, S::Error> {
            // At most `ATTRS_MAX_CATALOG_SCAN` descriptors are EXAMINED and
            // the rest is drained without materializing it: bounding only
            // what is KEPT would leave a peer sending four million invalid
            // ids parsed in full for an empty result. What is examined is
            // already bounded by the frame's size, so there is no
            // amplification. `size_hint` comes from the peer: the reservation
            // is bounded the same way, so a seq that claims to carry a
            // million does not reserve a million.
            let fits = access.size_hint().unwrap_or(0).min(ATTRS_MAX_CATALOG_SCAN);
            let mut raw: Vec<AttrInfo> = Vec::with_capacity(fits);
            while raw.len() < ATTRS_MAX_CATALOG_SCAN {
                let Some(info) = access.next_element::<AttrInfo>()? else {
                    return Ok(sanitize_catalog(raw));
                };
                raw.push(info);
            }
            while access.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(sanitize_catalog(raw))
        }
    }

    deserializer.deserialize_seq(AttrCatalogVisitor)
}

/// Bounds the MEMORY of a requested-id list at decode. This is NOT validation
/// and deliberately not a filter: the contract of
/// [`FsListParams::attrs`](crate::methods::FsListParams) is that a malformed
/// or over-cap request reaches the daemon so it can answer `-32602`.
///
/// A 16 MiB frame of `["a","a",…]` is ~4 million elements, and each one costs
/// a `String` header of its own — roughly 15× the bytes that arrived, decided
/// before any daemon-side check can run. So the first
/// [`ATTRS_MAX_REQUEST`] + 1 elements are kept VERBATIM and the rest is
/// drained without being materialised. The `+ 1` is the point: over-cap stays
/// OBSERVABLE as `attrs.len() > ATTRS_MAX_REQUEST`, so the daemon still sees a
/// violation to reject rather than a list silently trimmed into legality.
pub(crate) fn deserialize_attr_request<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct AttrRequestVisitor;

    impl<'de> serde::de::Visitor<'de> for AttrRequestVisitor {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a list of requested attribute ids")
        }

        fn visit_seq<S: serde::de::SeqAccess<'de>>(
            self,
            mut access: S,
        ) -> Result<Self::Value, S::Error> {
            const WITNESS: usize = ATTRS_MAX_REQUEST + 1;
            let fits = access.size_hint().unwrap_or(0).min(WITNESS);
            let mut out: Vec<String> = Vec::with_capacity(fits);
            while out.len() < WITNESS {
                let Some(id) = access.next_element::<String>()? else {
                    return Ok(out);
                };
                // Not validated and not deduplicated: exactly as it arrived.
                out.push(id);
            }
            while access.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(out)
        }
    }

    deserializer.deserialize_seq(AttrRequestVisitor)
}

/// Maximum length of the base64 TEXT of an `AttrValue::Bytes` payload: the
/// RFC 4648 expansion of [`ATTR_BYTES_MAX`] (4 characters per 3-byte group,
/// padded). Checked BEFORE decoding, so an oversized payload never allocates
/// the buffer it asks for.
const ATTR_BYTES_B64_MAX: usize = 4 * ATTR_BYTES_MAX.div_ceil(3);

/// Base64-encodes `bytes` the way this crate EMITS: RFC 4648 §4, the
/// standard alphabet (`+`, `/`) with padding required. Shared by
/// [`AttrValue::Bytes`] and `crate::methods::Volume::label`'s wire form —
/// both are "arbitrary platform bytes with no encoding contract, base64 on
/// the wire" and there is no reason for a second copy of the encode call.
pub(crate) fn encode_bytes_b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decodes `s` trying, in order, the standard, unpadded-standard, URL-safe
/// and unpadded-URL-safe dialects — `None` if none of them parse it. Widening
/// what is ACCEPTED past what this crate emits is backward-compatible: a
/// producer using Go's `RawStdEncoding`, for instance, would otherwise lose
/// the payload silently. Shared by [`AttrValue::Bytes`] and
/// `crate::methods::Volume::label`; see [`encode_bytes_b64`].
pub(crate) fn decode_bytes_b64_lenient(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let engines = [
        &base64::engine::general_purpose::STANDARD,
        &base64::engine::general_purpose::STANDARD_NO_PAD,
        &base64::engine::general_purpose::URL_SAFE,
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ];
    engines.iter().find_map(|engine| engine.decode(s).ok())
}

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
/// // A payload of the wrong JSON type degrades the CELL, not the entry.
/// let bad: AttrValue = serde_json::from_str(r#"{"uint": "33188"}"#).unwrap();
/// assert_eq!(bad, AttrValue::Unknown);
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
    /// THIRD-PARTY, and the variant most likely to be painted wrong, because an
    /// author holding a `Vec<u8>` reaches for `String::from_utf8_lossy`. Render
    /// it through `norte_frontend::display_name(&bytes)` — the same
    /// lossy-with-badge path that paints a non-UTF-8 FILENAME, so the user is
    /// told the rendering is lossy instead of silently shown `�`. The bytes
    /// themselves are never mutated (hard rule 1): only the rendering is lossy,
    /// and the value that round-trips back onto the wire is the original.
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

/// The tag of a cell, resolved from its KEY before the payload is read — which
/// is the whole point: knowing the tag first lets the payload be decoded
/// STRAIGHT into the scalar that tag needs, with no intermediate representation
/// of it ever existing.
///
/// This is the ONE table of wire tags: both directions go through
/// [`AttrTag::wire`], so a variant cannot be serialised under a name the reader
/// does not know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttrTag {
    Uint,
    Int,
    Text,
    BytesB64,
    TimeMs,
    Bool,
    /// The RESERVED `"unknown"` tag: a known key whose payload is always
    /// [`AttrValue::Unknown`], whatever it carries.
    Reserved,
}

impl AttrTag {
    /// Every tag this protocol version recognises, in variant order.
    const ALL: [Self; 7] = [
        Self::Uint,
        Self::Int,
        Self::Text,
        Self::BytesB64,
        Self::TimeMs,
        Self::Bool,
        Self::Reserved,
    ];

    /// Wire name of the tag.
    const fn wire(self) -> &'static str {
        match self {
            Self::Uint => "uint",
            Self::Int => "int",
            Self::Text => "text",
            Self::BytesB64 => "bytes_b64",
            Self::TimeMs => "time_ms",
            Self::Bool => "bool",
            Self::Reserved => "unknown",
        }
    }

    /// `None` = a key this protocol version does not know.
    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tag| tag.wire() == key)
    }
}

/// A map key read WITHOUT allocating: only the tag matters, and a key that is
/// not a tag is not worth a `String` (a hostile peer can send megabyte-long
/// ones).
struct TagKey(Option<AttrTag>);

impl<'de> Deserialize<'de> for TagKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;

        impl serde::de::Visitor<'_> for KeyVisitor {
            type Value = TagKey;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an attribute value tag")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<TagKey, E> {
                Ok(TagKey(AttrTag::from_key(v)))
            }
        }

        deserializer.deserialize_str(KeyVisitor)
    }
}

/// The ONE visitor behind [`AttrValue`]'s deserialisation, in two roles:
///
/// - `expected: None` — the cell ENVELOPE. A map is scanned for its tag; any
///   other JSON shape (`42`, `null`, `"text"`, `[…]`) degrades.
/// - `expected: Some(tag)` — the PAYLOAD under an already-known tag. Each
///   `visit_*` decodes straight into the variant that tag needs and returns
///   [`AttrValue::Unknown`] for everything else, which is how "the payload has
///   the wrong JSON type" stays a match arm instead of a deserialiser error.
///
/// No method of this visitor can fail on the VALUE side, which is what makes
/// ADR 0039 §3 true rather than aspirational.
struct CellVisitor {
    expected: Option<AttrTag>,
}

impl CellVisitor {
    /// The payload had a shape this tag cannot use (or there is no tag yet).
    const UNKNOWN: AttrValue = AttrValue::Unknown;
}

impl<'de> serde::de::DeserializeSeed<'de> for CellVisitor {
    type Value = AttrValue;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<AttrValue, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for CellVisitor {
    type Value = AttrValue;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.expected {
            None => f.write_str("a one-key tagged attribute value"),
            Some(_) => f.write_str("the payload of an attribute value"),
        }
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<AttrValue, E> {
        Ok(match self.expected {
            Some(AttrTag::Uint) => AttrValue::Uint(v),
            Some(AttrTag::Int) => i64::try_from(v).map_or(Self::UNKNOWN, AttrValue::Int),
            Some(AttrTag::TimeMs) => i64::try_from(v).map_or(Self::UNKNOWN, AttrValue::TimeMs),
            _ => Self::UNKNOWN,
        })
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<AttrValue, E> {
        Ok(match self.expected {
            // A non-negative integer can arrive through either visit
            // depending on the deserialiser: treated the same on purpose.
            Some(AttrTag::Uint) => u64::try_from(v).map_or(Self::UNKNOWN, AttrValue::Uint),
            Some(AttrTag::Int) => AttrValue::Int(v),
            Some(AttrTag::TimeMs) => AttrValue::TimeMs(v),
            _ => Self::UNKNOWN,
        })
    }

    fn visit_u128<E: serde::de::Error>(self, _v: u128) -> Result<AttrValue, E> {
        Ok(Self::UNKNOWN)
    }

    fn visit_i128<E: serde::de::Error>(self, _v: i128) -> Result<AttrValue, E> {
        Ok(Self::UNKNOWN)
    }

    fn visit_f64<E: serde::de::Error>(self, _v: f64) -> Result<AttrValue, E> {
        Ok(Self::UNKNOWN)
    }

    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<AttrValue, E> {
        Ok(match self.expected {
            Some(AttrTag::Bool) => AttrValue::Bool(v),
            _ => Self::UNKNOWN,
        })
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<AttrValue, E> {
        Ok(match self.expected {
            // The cap is checked BEFORE copying: a giant text is not
            // duplicated in memory just to discard it afterwards.
            Some(AttrTag::Text) if v.len() <= ATTR_TEXT_MAX => AttrValue::Text(v.to_owned()),
            // And here, before DECODING: a huge payload never reserves the
            // buffer it asks for.
            Some(AttrTag::BytesB64) if v.len() <= ATTR_BYTES_B64_MAX => decode_bytes_b64_lenient(v)
                .filter(|bytes| bytes.len() <= ATTR_BYTES_MAX)
                .map_or(Self::UNKNOWN, AttrValue::Bytes),
            _ => Self::UNKNOWN,
        })
    }

    fn visit_bytes<E: serde::de::Error>(self, _v: &[u8]) -> Result<AttrValue, E> {
        Ok(Self::UNKNOWN)
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<AttrValue, E> {
        Ok(Self::UNKNOWN)
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<AttrValue, E> {
        Ok(Self::UNKNOWN)
    }

    fn visit_some<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<AttrValue, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(Self::UNKNOWN)
    }

    fn visit_seq<S: serde::de::SeqAccess<'de>>(self, mut access: S) -> Result<AttrValue, S::Error> {
        // It is DRAINED without materializing (`IgnoredAny` walks the tokens,
        // builds nothing): an array nested 200 levels deep costs whatever it
        // occupies in the frame and degrades this cell, never the page.
        while access.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(Self::UNKNOWN)
    }

    fn visit_map<M: serde::de::MapAccess<'de>>(self, mut access: M) -> Result<AttrValue, M::Error> {
        let Some(_) = self.expected else {
            return Self::envelope(access);
        };
        // An object UNDER a tag: drained just like an array.
        while access
            .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
            .is_some()
        {}
        Ok(Self::UNKNOWN)
    }
}

impl CellVisitor {
    /// Scans the cell envelope: every key is looked up as a tag, the payload of
    /// the ONE known tag is decoded in place, and everything else is drained.
    ///
    /// The decision is taken AFTER every key has been seen, so it does not
    /// depend on the order the peer serialised them in — JSON objects are
    /// unordered (RFC 8259 §4) and a relay that round-trips through a sorted
    /// map must not be able to change the meaning of the very same document.
    /// Exactly one known tag yields its variant; zero (an unrecognised tag from
    /// a newer peer, or an empty object) and two-or-more (ambiguous, which a
    /// conforming peer never emits) both yield [`AttrValue::Unknown`].
    fn envelope<'de, M: serde::de::MapAccess<'de>>(mut access: M) -> Result<AttrValue, M::Error> {
        let mut chosen: Option<(AttrTag, AttrValue)> = None;
        let mut ambiguous = false;

        while let Some(TagKey(tag)) = access.next_key::<TagKey>()? {
            // The SAME tag repeated is not ambiguity: it is a duplicate key,
            // and it resolves last-wins like any JSON parser (and like the
            // intermediate `serde_json::Map` this replaces used to do).
            let repeated = chosen.as_ref().is_some_and(|(seen, _)| Some(*seen) == tag);
            match tag {
                Some(t) if !ambiguous && (chosen.is_none() || repeated) => {
                    let value = access.next_value_seed(CellVisitor { expected: Some(t) })?;
                    chosen = Some((t, value));
                }
                Some(_) => {
                    ambiguous = true;
                    access.next_value::<serde::de::IgnoredAny>()?;
                }
                // Extra unrecognised keys: ignored (the same lenient
                // criterion as any unknown wire field), and their payload is
                // drained without being materialized.
                None => {
                    access.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }

        Ok(if ambiguous {
            AttrValue::Unknown
        } else {
            chosen.map_or(AttrValue::Unknown, |(_, value)| value)
        })
    }
}

impl Serialize for AttrValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;

        // The tag comes from the SAME table the deserialiser reads
        // (`AttrTag::wire`): there is no way to emit a name the reader does
        // not recognize.
        let mut map = serializer.serialize_map(Some(1))?;
        match self {
            AttrValue::Uint(v) => map.serialize_entry(AttrTag::Uint.wire(), v)?,
            AttrValue::Int(v) => map.serialize_entry(AttrTag::Int.wire(), v)?,
            AttrValue::Text(v) => map.serialize_entry(AttrTag::Text.wire(), v)?,
            AttrValue::Bytes(v) => {
                map.serialize_entry(AttrTag::BytesB64.wire(), &encode_bytes_b64(v))?;
            }
            AttrValue::TimeMs(v) => map.serialize_entry(AttrTag::TimeMs.wire(), v)?,
            AttrValue::Bool(v) => map.serialize_entry(AttrTag::Bool.wire(), v)?,
            AttrValue::Unknown => {
                map.serialize_entry(AttrTag::Reserved.wire(), &Option::<()>::None)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for AttrValue {
    /// Reads the cell with a STREAMING visitor, never through an intermediate
    /// [`serde_json::Value`]. `deserialize_any` is what turns "the payload has
    /// the wrong JSON type" into a match arm instead of a deserialiser error,
    /// and it is also the entry point for a non-map input (`42`, `null`,
    /// `"text"`), which degrades where `deserialize_map` would have raised.
    /// JSON is the protocol's only encoding (ADR 0011), so requiring a
    /// self-describing format costs nothing.
    ///
    /// Materialising the payload FIRST would have been shorter and is wrong
    /// twice over, in ways that both contradict §3 of the ADR:
    ///
    /// - `serde_json`'s recursion limit applies while BUILDING a `Value`, so a
    ///   ~250-byte payload nested 125 deep inside one cell of one entry failed
    ///   the whole `fs.list` page with "recursion limit exceeded" — a hard
    ///   error at the value level, and one that the very same bytes in an
    ///   unknown field (the 0.29 shape) never caused, because derived serde
    ///   skips those with [`IgnoredAny`](serde::de::IgnoredAny). Draining is
    ///   iterative, so depth now costs only the bytes it occupies in the frame.
    /// - a `Value` costs several times the bytes it came from, which is the
    ///   amplification [`ATTRS_MAX_CATALOG_SCAN`], the `+ 1` request witness
    ///   and the pre-decode base64 length check all exist to prevent. Here the
    ///   caps are checked BEFORE anything is copied, and a payload that is not
    ///   the shape its tag needs is never built at all.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(CellVisitor { expected: None })
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
                "text": {
                    "type": "string",
                    "description": "UTF-8 text. NOTE: this maxLength counts CODE \
                                    POINTS (JSON Schema 2020-12), while the cap it \
                                    publishes is enforced in BYTES — a 256-code-point \
                                    non-ASCII value is legal here and degrades to \
                                    Unknown at decode. Deliberate; see ADR 0039 §5.",
                    "maxLength": ATTR_TEXT_MAX
                },
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
    fn valid_and_rejected_ids() {
        for ok in ["posix.mode", "s3.storage_class", "archive.packed-size"] {
            assert!(is_valid_attr_id(ok), "{ok} should be valid");
        }

        // Exactly ATTR_ID_MAX bytes is valid (inclusive boundary).
        let exactly_max = format!("a.{}", "b".repeat(ATTR_ID_MAX - 2));
        assert_eq!(exactly_max.len(), ATTR_ID_MAX);
        assert!(
            is_valid_attr_id(&exactly_max),
            "exactly ATTR_ID_MAX bytes should be valid"
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
            // Every segment starts with a LETTER (0.30.0): an id shaped like
            // argv (`--attrs -x.y` from block 2) or like a float (`0.0` in a
            // configuration file) is read as something else downstream.
            "-.-",
            "0.0",
            "9-9.9-9",
            "__.__",
            "-x.y",
            "x.-y",
            "posix.0mode",
        ] {
            assert!(!is_valid_attr_id(bad), "{bad:?} should be rejected");
        }

        // A DIGIT inside the segment still counts: only the first byte is
        // restricted (`s3.etag`, `posix.ctime_ms`).
        for ok in ["s3.etag", "a1.b2", "posix.ctime_ms"] {
            assert!(is_valid_attr_id(ok), "{ok} should be valid");
        }

        // ATTR_ID_MAX + 1 bytes is rejected (exclusive boundary: pins `<=`, not `<`).
        let one_more_than_max = format!("a.{}", "b".repeat(ATTR_ID_MAX - 1));
        assert_eq!(one_more_than_max.len(), ATTR_ID_MAX + 1);
        assert!(
            !is_valid_attr_id(&one_more_than_max),
            "ATTR_ID_MAX + 1 bytes should be rejected"
        );
    }

    #[test]
    fn unknown_attr_type_degrades() {
        let t: AttrType = serde_json::from_str("\"tipo_del_futuro\"").unwrap();
        assert_eq!(t, AttrType::Unknown);
    }

    #[test]
    fn unknown_attr_hint_degrades() {
        let h: AttrHint = serde_json::from_str("\"pista_del_futuro\"").unwrap();
        assert_eq!(h, AttrHint::Unknown);
    }

    fn round_trip(v: &AttrValue) -> AttrValue {
        let wire = serde_json::to_string(v).unwrap();
        serde_json::from_str(&wire).unwrap()
    }

    #[test]
    fn every_variant_round_trips() {
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
    fn bytes_wire_is_base64() {
        let wire = serde_json::to_string(&AttrValue::Bytes(vec![0xFF, 0xFE])).unwrap();
        assert_eq!(wire, r#"{"bytes_b64":"//4="}"#);
    }

    #[test]
    fn future_variant_degrades_to_unknown() {
        let v: AttrValue = serde_json::from_str(r#"{"quaternion":[1,2,3,4]}"#).unwrap();
        assert_eq!(v, AttrValue::Unknown);
    }

    #[test]
    fn corrupt_base64_degrades_to_unknown() {
        let v: AttrValue = serde_json::from_str(r#"{"bytes_b64":"not base64 !!"}"#).unwrap();
        assert_eq!(v, AttrValue::Unknown);
    }

    #[test]
    fn broken_envelope_degrades_to_unknown() {
        // Not even a broken envelope is a VALUE error: a bad cell costs one
        // cell. Only unreadable JSON (or an `attrs` that is not an object)
        // breaks, and that is serde's call on the parent type.
        for broken in ["42", "{}", "null", r#""texto""#] {
            assert_eq!(
                serde_json::from_str::<AttrValue>(broken).unwrap(),
                AttrValue::Unknown,
                "{broken} should degrade"
            );
        }
    }

    #[test]
    fn the_tag_does_not_depend_on_key_order() {
        // RFC 8259: a JSON object is NOT ordered. A relay that passes through
        // an ordered map must not be able to change the document's meaning.
        for wire in [r#"{"uint":1,"aaa":0}"#, r#"{"aaa":0,"uint":1}"#] {
            assert_eq!(
                serde_json::from_str::<AttrValue>(wire).unwrap(),
                AttrValue::Uint(1),
                "{wire} should give Uint(1)"
            );
        }
    }

    #[test]
    fn two_known_tags_are_ambiguous_and_degrade() {
        for wire in [r#"{"uint":1,"bool":true}"#, r#"{"bool":true,"uint":1}"#] {
            assert_eq!(
                serde_json::from_str::<AttrValue>(wire).unwrap(),
                AttrValue::Unknown,
                "{wire} is ambiguous: should degrade in either order"
            );
        }
    }

    #[test]
    fn payload_of_the_wrong_type_degrades() {
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
                "{wire} should degrade"
            );
        }
    }

    #[test]
    fn text_over_the_cap_degrades() {
        let exact = "a".repeat(ATTR_TEXT_MAX);
        let v: AttrValue = serde_json::from_value(serde_json::json!({ "text": exact })).unwrap();
        assert_eq!(v, AttrValue::Text(exact), "the exact cap is valid");

        let over = "a".repeat(ATTR_TEXT_MAX + 1);
        let v: AttrValue = serde_json::from_value(serde_json::json!({ "text": over })).unwrap();
        assert_eq!(v, AttrValue::Unknown, "one byte over degrades");
    }

    #[test]
    fn bytes_over_the_cap_degrade() {
        use base64::Engine as _;

        let exact = base64::engine::general_purpose::STANDARD.encode(vec![0xFFu8; ATTR_BYTES_MAX]);
        assert!(exact.len() <= ATTR_BYTES_B64_MAX);
        let v: AttrValue =
            serde_json::from_value(serde_json::json!({ "bytes_b64": exact })).unwrap();
        assert_eq!(v, AttrValue::Bytes(vec![0xFF; ATTR_BYTES_MAX]));

        let over =
            base64::engine::general_purpose::STANDARD.encode(vec![0xFFu8; ATTR_BYTES_MAX + 1]);
        let v: AttrValue =
            serde_json::from_value(serde_json::json!({ "bytes_b64": over })).unwrap();
        assert_eq!(v, AttrValue::Unknown, "one decoded byte over degrades");

        // A HUGE payload is rejected by the text's length, without reserving
        // the buffer it asks for.
        let bomb = "A".repeat(1_000_000);
        let v: AttrValue =
            serde_json::from_value(serde_json::json!({ "bytes_b64": bomb })).unwrap();
        assert_eq!(v, AttrValue::Unknown);
    }

    #[test]
    fn accepts_all_four_base64_dialects() {
        // 0xFB 0xFF -> "+/8=" in the standard alphabet, "-_8" unpadded and
        // URL-safe: a Go producer using RawStdEncoding cannot lose the cell.
        let expected = AttrValue::Bytes(vec![0xFB, 0xFF]);
        for b64 in ["+/8=", "+/8", "-_8=", "-_8"] {
            let v: AttrValue =
                serde_json::from_value(serde_json::json!({ "bytes_b64": b64 })).unwrap();
            assert_eq!(v, expected, "{b64} should decode");
        }
    }

    #[test]
    fn a_bad_cell_does_not_drag_down_its_siblings() {
        // The invariant of the whole type: a broken cell costs ONE cell.
        let json = r#"{
            "a": {"uint": 1},
            "b": {"uint": "33188"},
            "c": {"quaternion": [0]},
            "d": {"uint": 1, "bool": true},
            "e": {"bytes_b64": "not base64 !!"},
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
    fn a_very_deep_payload_degrades_and_does_not_cost_the_page() {
        // The case that the hop through `serde_json::Value` turned into a
        // HARD error: 200 levels of nesting inside ONE cell blew the
        // recursion limit and with it the entire response, when the SAME
        // bytes in an unknown field (the 0.29 shape) were always ignored
        // without drama.
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        let json = format!(
            r#"{{"path":"file:///a","kind":"file","attrs":{{
                "a.bomb": {{"uint": {deep}}},
                "b.sibling": {{"bool": true}}
            }}}}"#
        );
        let e: crate::Entry = serde_json::from_str(&json).expect("a deep cell breaks NOTHING");
        assert_eq!(e.attrs["a.bomb"], AttrValue::Unknown, "the cell degrades");
        assert_eq!(
            e.attrs["b.sibling"],
            AttrValue::Bool(true),
            "and its sibling survives: neither the entry nor the page is lost"
        );

        // The same payload under an UNKNOWN tag (drained the same way).
        let loose: AttrValue =
            serde_json::from_str(&format!(r#"{{"cuaternion": {deep}}}"#)).expect("degrades");
        assert_eq!(loose, AttrValue::Unknown);
    }

    #[test]
    fn known_tags_go_and_come_back() {
        // A single table for both directions: what is emitted is recognized,
        // and all seven wire tags are present.
        let names: Vec<&str> = AttrTag::ALL.iter().map(|t| t.wire()).collect();
        assert_eq!(
            names,
            [
                "uint",
                "int",
                "text",
                "bytes_b64",
                "time_ms",
                "bool",
                "unknown"
            ],
            "the wire tags are FROZEN (0.30.0)"
        );
        for tag in AttrTag::ALL {
            assert_eq!(AttrTag::from_key(tag.wire()), Some(tag));
        }
        assert!(AttrTag::from_key("cuaternion").is_none());
    }

    #[test]
    fn the_same_repeated_tag_is_last_wins_not_ambiguous() {
        // A DUPLICATE key is not the same as two different tags: any JSON
        // parser keeps the last one, and that is what the intermediate
        // `serde_json::Map` the visitor replaces used to do. Pinned so the
        // implementation change does not move what is observable.
        let v: AttrValue = serde_json::from_str(r#"{"uint":1,"uint":2}"#).unwrap();
        assert_eq!(v, AttrValue::Uint(2));
        // And the duplicate does not make a valid cell ambiguous either.
        let v: AttrValue = serde_json::from_str(r#"{"uint":1,"uint":2,"aaa":0}"#).unwrap();
        assert_eq!(v, AttrValue::Uint(2));
    }

    #[test]
    fn unknown_serializes_and_comes_back_as_unknown() {
        assert_eq!(
            serde_json::to_string(&AttrValue::Unknown).unwrap(),
            r#"{"unknown":null}"#
        );
        assert_eq!(round_trip(&AttrValue::Unknown), AttrValue::Unknown);
    }

    #[test]
    fn an_entry_with_one_future_cell_survives_whole() {
        // The point of the degradation: the ENTRY is not lost over one cell.
        let json = r#"{"a":{"uint":1},"b":{"quaternion":[0]},"c":{"bool":false}}"#;
        let map: std::collections::BTreeMap<String, AttrValue> =
            serde_json::from_str(json).unwrap();
        assert_eq!(map["a"], AttrValue::Uint(1));
        assert_eq!(map["b"], AttrValue::Unknown);
        assert_eq!(map["c"], AttrValue::Bool(false));
    }

    /// Decodes a catalog through the REAL wire FIELD, which is where
    /// [`AttrCatalog`]'s deserialisation lives: testing the filter another
    /// way would test something else.
    fn catalog(attrs: &serde_json::Value) -> AttrCatalog {
        let wire = serde_json::json!({
            "capabilities": { "flags": "", "max_path": null },
            "attrs": attrs,
        });
        serde_json::from_value::<crate::methods::FsCapabilitiesResult>(wire)
            .expect("a hostile catalog NEVER breaks the response")
            .attrs
    }

    fn descriptor(id: &str) -> serde_json::Value {
        serde_json::json!({ "id": id, "label": "L", "type": "uint", "hint": "opaque" })
    }

    #[test]
    fn catalog_drops_malformed_ids_without_breaking() {
        let survivors = catalog(&serde_json::json!([
            descriptor("posix.mode"),
            descriptor("MODE"),
            descriptor("mode"),
            descriptor("../etc/passwd"),
            descriptor("posix."),
            descriptor(""),
            descriptor("s3.storage_class"),
        ]));
        let ids: Vec<&str> = survivors.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["posix.mode", "s3.storage_class"]);
    }

    #[test]
    fn catalog_truncates_to_the_cap_keeping_wire_order() {
        // Ids in DESCENDING order: if the filter sorted (instead of keeping
        // the order the provider chose), this test would catch it.
        let total = ATTRS_MAX_ADVERTISED + 10;
        let ids: Vec<String> = (0..total)
            .map(|i| format!("ns.a{:03}", total - i))
            .collect();
        let survivors = catalog(&serde_json::Value::Array(
            ids.iter().map(|id| descriptor(id)).collect(),
        ));

        assert_eq!(
            survivors.len(),
            ATTRS_MAX_ADVERTISED,
            "it truncates, it does not reject"
        );
        let seen: Vec<&str> = survivors.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            seen,
            ids[..ATTRS_MAX_ADVERTISED]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            "the FIRST ones in wire order are kept, unreordered"
        );
    }

    #[test]
    fn a_bomb_catalog_does_not_materialize_the_excess() {
        // 10,000 descriptors: whatever is past the cap is drained without
        // building it.
        let bomb: Vec<serde_json::Value> = (0..10_000)
            .map(|i| descriptor(&format!("ns.a{i}")))
            .collect();
        assert_eq!(
            catalog(&serde_json::Value::Array(bomb)).len(),
            ATTRS_MAX_ADVERTISED
        );
    }

    #[test]
    fn a_catalog_of_invalid_ids_stops_being_examined() {
        // With INVALID ids the `ATTRS_MAX_ADVERTISED` cap never fills, so
        // without the SCAN cap the whole vector would get parsed for an empty
        // result: the work has to be bounded regardless.
        let mut bomb: Vec<serde_json::Value> = (0..ATTRS_MAX_CATALOG_SCAN + 500)
            .map(|i| descriptor(&format!("INVALID{i}")))
            .collect();
        // A legitimate id PAST the scan: it is not seen, and that is the
        // documented price of bounding it (a peer like this is already
        // hostile, not merely buggy).
        bomb.push(descriptor("s3.etag"));
        assert!(catalog(&serde_json::Value::Array(bomb)).is_empty());

        // And right AT the scan limit the good id does get in.
        let mut at_the_limit: Vec<serde_json::Value> = (0..ATTRS_MAX_CATALOG_SCAN - 1)
            .map(|i| descriptor(&format!("INVALID{i}")))
            .collect();
        at_the_limit.push(descriptor("s3.etag"));
        let survivors = catalog(&serde_json::Value::Array(at_the_limit));
        assert_eq!(survivors.len(), 1);
        assert_eq!(survivors[0].id, "s3.etag");
    }

    #[test]
    fn catalog_deduplicates_by_id_the_first_one_wins() {
        // Same bytes, two consumers: one that folds into a map and one that
        // uses `find()`. With both copies alive they would render the
        // attribute differently.
        let survivors = catalog(&serde_json::json!([
            { "id": "posix.mode", "label": "Mode", "type": "uint", "hint": "mode" },
            { "id": "posix.mode", "label": "Modo", "type": "text", "hint": "opaque" },
        ]));
        assert_eq!(survivors.len(), 1, "one id, one descriptor");
        assert_eq!(survivors[0].ty, AttrType::Uint, "the FIRST one wins");
        assert_eq!(survivors[0].label, "Mode");
    }

    #[test]
    fn duplicates_do_not_eat_the_catalogs_budget() {
        // 64 copies of the same id followed by a legitimate one: if the
        // duplicate took up a slot, `s3.etag` would fall outside the cap.
        let mut wire: Vec<serde_json::Value> = (0..ATTRS_MAX_ADVERTISED)
            .map(|_| descriptor("posix.mode"))
            .collect();
        wire.push(descriptor("s3.etag"));
        let survivors = catalog(&serde_json::Value::Array(wire));
        let ids: Vec<&str> = survivors.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["posix.mode", "s3.etag"]);
    }

    #[test]
    fn a_label_over_the_cap_is_clamped_on_a_character_boundary() {
        // Multibyte on purpose: clamping blindly would split a UTF-8 sequence
        // (and `String::truncate` would panic).
        let long = "ñ".repeat(ATTR_LABEL_MAX); // 2 bytes per character
        let survivors = catalog(&serde_json::json!([{
            "id": "posix.mode", "label": long, "type": "uint", "hint": "mode",
        }]));
        assert_eq!(
            survivors.len(),
            1,
            "a fat label does NOT cost the descriptor"
        );
        assert!(survivors[0].label.len() <= ATTR_LABEL_MAX);
        assert_eq!(
            survivors[0].label,
            "ñ".repeat(ATTR_LABEL_MAX / 2),
            "it is cut on a character boundary, no loose bytes"
        );

        // The exact cap is left untouched.
        let exact = "a".repeat(ATTR_LABEL_MAX);
        let survivors = catalog(&serde_json::json!([{
            "id": "posix.mode", "label": exact, "type": "uint", "hint": "mode",
        }]));
        assert_eq!(survivors[0].label, exact);
    }

    #[test]
    fn the_caps_are_in_bytes_and_the_schema_counts_code_points() {
        // DELIBERATE divergence (ADR 0039 §5): JSON Schema 2020-12's
        // `maxLength` counts code points, the code bounds BYTES. With
        // non-ASCII the two counts split apart, and it is pinned in both
        // directions so nobody "fixes" it by loosening the memory cap.
        let cjk_text = "文".repeat(200); // 200 code points, 600 bytes
        assert_eq!(cjk_text.chars().count(), 200, "legal for the schema");
        assert!(cjk_text.len() > ATTR_TEXT_MAX, "but it is over in BYTES");
        let v: AttrValue = serde_json::from_value(serde_json::json!({ "text": cjk_text })).unwrap();
        assert_eq!(v, AttrValue::Unknown, "and it degrades, it is not clamped");

        let cjk_label = "文".repeat(60); // 60 code points, 180 bytes
        assert_eq!(cjk_label.chars().count(), 60, "legal for the schema");
        let survivors = catalog(&serde_json::json!([{
            "id": "posix.mode", "label": cjk_label, "type": "uint", "hint": "mode",
        }]));
        assert_eq!(
            survivors.len(),
            1,
            "a fat label does not cost the descriptor"
        );
        assert_eq!(
            survivors[0].label.chars().count(),
            ATTR_LABEL_MAX / 3,
            "it is clamped to ~21 characters: 64 BYTES on a char boundary"
        );
    }

    #[test]
    fn sanitize_catalog_is_idempotent() {
        // The safety net of the EMBEDDED path: applying it twice (wire then
        // in-process, or the other way round) must not change the result.
        let dirty = vec![
            AttrInfo {
                id: "MODE".to_owned(),
                label: "x".repeat(ATTR_LABEL_MAX + 10),
                ty: AttrType::Uint,
                hint: AttrHint::Mode,
            },
            AttrInfo {
                id: "posix.mode".to_owned(),
                label: "x".repeat(ATTR_LABEL_MAX + 10),
                ty: AttrType::Uint,
                hint: AttrHint::Mode,
            },
        ];
        let once = sanitize_catalog(dirty);
        let twice = sanitize_catalog(once.clone());
        assert_eq!(once, twice);
        assert_eq!(once.len(), 1);
        assert_eq!(once[0].label.len(), ATTR_LABEL_MAX);
    }

    #[test]
    fn the_request_is_bounded_in_memory_but_not_validated() {
        use crate::methods::FsListParams;

        // Cap + 1: the "went over" witness has to survive so the daemon can
        // answer -32602 (block 2).
        let ids: Vec<String> = (0..10_000).map(|i| format!("ns.a{i}")).collect();
        let wire = serde_json::json!({ "path": "file:///", "attrs": ids });
        let params: FsListParams = serde_json::from_value(wire).expect("not a decode error");
        assert_eq!(params.attrs.len(), ATTRS_MAX_REQUEST + 1);
        assert!(
            params.attrs.len() > ATTRS_MAX_REQUEST,
            "going over stays OBSERVABLE"
        );
        assert_eq!(params.attrs[0], "ns.a0", "and what remains is VERBATIM");

        // Nothing is validated or deduplicated below the cap.
        let wire = serde_json::json!({ "path": "file:///", "attrs": ["MODE", "MODE", ""] });
        let params: FsListParams = serde_json::from_value(wire).expect("not a decode error");
        assert_eq!(params.attrs, ["MODE", "MODE", ""]);
    }

    #[test]
    fn an_absent_or_empty_catalog_is_empty() {
        assert!(catalog(&serde_json::json!([])).is_empty());
        let no_field = serde_json::json!({ "capabilities": { "flags": "", "max_path": null } });
        let caps: crate::methods::FsCapabilitiesResult =
            serde_json::from_value(no_field).expect("valid 0.29 wire");
        assert!(
            caps.attrs.is_empty(),
            "absent = the provider publishes none"
        );
    }

    #[test]
    fn a_catalog_with_a_future_type_or_hint_survives_by_degrading() {
        let survivors = catalog(&serde_json::json!([{
            "id": "futuro.attr",
            "label": "L",
            "type": "quaternion",
            "hint": "holograma",
        }]));
        assert_eq!(survivors.len(), 1);
        assert_eq!(survivors[0].ty, AttrType::Unknown);
        assert_eq!(survivors[0].hint, AttrHint::Unknown);
    }

    #[test]
    fn a_structurally_broken_descriptor_is_an_error() {
        // The leniency is for ids and extra ids; a descriptor WITHOUT `label`
        // is missing a mandatory field: a broken peer, not a newer one.
        let wire = serde_json::json!({
            "capabilities": { "flags": "", "max_path": null },
            "attrs": [{ "id": "posix.mode", "type": "uint", "hint": "mode" }],
        });
        assert!(serde_json::from_value::<crate::methods::FsCapabilitiesResult>(wire).is_err());
    }

    #[test]
    fn a_dirty_catalog_cannot_be_built_even_in_process() {
        // The EMBEDDED path (default TUI/CLI) does not cross deserialisation,
        // so the rule cannot live only there: the type does not accept an
        // invalid id, whether it comes from the wire or a WASM plugin.
        let caps = crate::methods::FsCapabilitiesResult {
            capabilities: crate::Capabilities {
                flags: crate::CapabilityFlags::empty(),
                max_path: None,
            },
            attrs: AttrCatalog::new(vec![
                AttrInfo {
                    id: "MODE".to_owned(),
                    label: "Mode".to_owned(),
                    ty: AttrType::Uint,
                    hint: AttrHint::Mode,
                },
                AttrInfo {
                    id: "posix.mode".to_owned(),
                    label: "x".repeat(ATTR_LABEL_MAX + 10),
                    ty: AttrType::Uint,
                    hint: AttrHint::Mode,
                },
            ]),
        };
        let wire = serde_json::to_string(&caps).expect("serializable");
        assert!(
            !wire.contains("MODE"),
            "the invalid id does not exist: {wire}"
        );
        assert!(
            wire.contains(r#""attrs":[{"#),
            "and the wire is still the plain ARRAY as always: {wire}"
        );
        assert_eq!(caps.attrs.len(), 1);
        assert_eq!(caps.attrs[0].label.len(), ATTR_LABEL_MAX);

        // EXACT round trip: there is no state the decode could change.
        let back: crate::methods::FsCapabilitiesResult =
            serde_json::from_str(&wire).expect("decodes");
        assert_eq!(back, caps);
    }

    #[test]
    fn the_catalog_iterates_and_derefs_like_a_slice() {
        let catalog = AttrCatalog::new(vec![AttrInfo {
            id: "posix.mode".to_owned(),
            label: "Mode".to_owned(),
            ty: AttrType::Uint,
            hint: AttrHint::Mode,
        }]);
        let by_ref: Vec<&str> = (&catalog).into_iter().map(|i| i.id.as_str()).collect();
        assert_eq!(by_ref, ["posix.mode"]);
        assert_eq!(catalog.first().map(|i| i.ty), Some(AttrType::Uint));
        let by_value: Vec<AttrInfo> = catalog.into_iter().collect();
        assert_eq!(by_value.len(), 1);
        assert!(AttrCatalog::default().is_empty());
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
            "the field is named `type` on the wire: {wire}"
        );
        assert_eq!(serde_json::from_str::<AttrInfo>(&wire).unwrap(), info);
    }
}
