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

/// Is `id` a well-formed, NAMESPACED attribute id: at least one `.`,
/// non-empty `.`-separated segments each made of `[a-z0-9_-]`, at most
/// [`ATTR_ID_MAX`] bytes total. Namespaced by its origin (`posix.mode`,
/// `archive.packed_size`), with no central registry — a provider owns its
/// namespace (ADR 0039). A bare word with no dot (`mode`), a dot with an
/// empty segment on either side (`posix.`, `.mode`, `.`, `..`), or anything
/// outside the byte class is rejected: relaxing this rule later is
/// backward-compatible, tightening it after 0.30 ships is not.
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
            "pattern" = r"^[a-z0-9_-]+(\.[a-z0-9_-]+)+$"
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
    #[cfg_attr(feature = "schema", schemars(extend("maxLength" = ATTR_LABEL_MAX)))]
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
/// [`FsCapabilitiesResult::attrs`](crate::methods::FsCapabilitiesResult) runs
/// this at decode. An EMBEDDED backend, which never crosses that boundary,
/// must call it explicitly on any catalog it obtains in-process — from a
/// provider or, in block 2, from a WASM provider plugin the threat model
/// treats as untrusted. One implementation, both paths.
///
/// ```
/// use norte_proto::attrs::{AttrHint, AttrInfo, AttrType, sanitize_catalog};
/// let info = |id: &str, ty| AttrInfo {
///     id: id.to_owned(),
///     label: "L".to_owned(),
///     ty,
///     hint: AttrHint::Opaque,
/// };
/// let limpio = sanitize_catalog(vec![
///     info("MODE", AttrType::Uint),        // id mal formado: fuera
///     info("posix.mode", AttrType::Uint),  // gana el PRIMERO
///     info("posix.mode", AttrType::Text),  // id repetido: fuera
/// ]);
/// assert_eq!(limpio.len(), 1);
/// assert_eq!(limpio[0].ty, AttrType::Uint);
/// ```
#[must_use]
pub fn sanitize_catalog(catalog: Vec<AttrInfo>) -> Vec<AttrInfo> {
    let mut out: Vec<AttrInfo> = Vec::new();
    for mut info in catalog {
        if out.len() >= ATTRS_MAX_ADVERTISED {
            break;
        }
        // Id inválido o repetido: se descarta ANTES de ocupar hueco. La
        // búsqueda lineal recorre a lo sumo `ATTRS_MAX_ADVERTISED` entradas,
        // así que no compensa un set aparte.
        if !is_valid_attr_id(&info.id) || out.iter().any(|visto| visto.id == info.id) {
            continue;
        }
        clamp_label(&mut info.label);
        out.push(info);
    }
    out
}

/// Recorta `label` a [`ATTR_LABEL_MAX`] bytes por una FRONTERA DE CARÁCTER:
/// cortar a media secuencia UTF-8 no es una opción (`String::truncate`
/// entraría en pánico, y el byte suelto no sería texto).
fn clamp_label(label: &mut String) {
    if label.len() <= ATTR_LABEL_MAX {
        return;
    }
    let mut corte = ATTR_LABEL_MAX;
    while corte > 0 && !label.is_char_boundary(corte) {
        corte -= 1;
    }
    label.truncate(corte);
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
            // Se EXAMINAN a lo sumo `ATTRS_MAX_CATALOG_SCAN` descriptores y el
            // resto se drena sin materializarlo: acotar solo lo que se GUARDA
            // dejaría a un peer que manda cuatro millones de ids inválidos
            // parseándose entero para un resultado vacío. Lo examinado ya está
            // acotado por el tamaño del frame, así que no hay amplificación.
            // `size_hint` viene del peer: la reserva se acota igual, para que
            // un seq que dice traer un millón no reserve un millón.
            let cabidos = access.size_hint().unwrap_or(0).min(ATTRS_MAX_CATALOG_SCAN);
            let mut crudo: Vec<AttrInfo> = Vec::with_capacity(cabidos);
            while crudo.len() < ATTRS_MAX_CATALOG_SCAN {
                let Some(info) = access.next_element::<AttrInfo>()? else {
                    return Ok(sanitize_catalog(crudo));
                };
                crudo.push(info);
            }
            while access.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(sanitize_catalog(crudo))
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
            const TESTIGO: usize = ATTRS_MAX_REQUEST + 1;
            let cabidos = access.size_hint().unwrap_or(0).min(TESTIGO);
            let mut out: Vec<String> = Vec::with_capacity(cabidos);
            while out.len() < TESTIGO {
                let Some(id) = access.next_element::<String>()? else {
                    return Ok(out);
                };
                // Sin validar y sin deduplicar: tal cual vino.
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

    /// Decodifica un catálogo por el CAMPO REAL del wire, que es donde vive el
    /// `deserialize_with`: probar el filtro por otra vía probaría otra cosa.
    fn catalogo(attrs: &serde_json::Value) -> Vec<AttrInfo> {
        let wire = serde_json::json!({
            "capabilities": { "flags": "", "max_path": null },
            "attrs": attrs,
        });
        serde_json::from_value::<crate::methods::FsCapabilitiesResult>(wire)
            .expect("un catálogo hostil NUNCA rompe la respuesta")
            .attrs
    }

    fn descriptor(id: &str) -> serde_json::Value {
        serde_json::json!({ "id": id, "label": "L", "type": "uint", "hint": "opaque" })
    }

    #[test]
    fn catalogo_descarta_los_ids_mal_formados_sin_romper() {
        let vivos = catalogo(&serde_json::json!([
            descriptor("posix.mode"),
            descriptor("MODE"),
            descriptor("mode"),
            descriptor("../etc/passwd"),
            descriptor("posix."),
            descriptor(""),
            descriptor("s3.storage_class"),
        ]));
        let ids: Vec<&str> = vivos.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["posix.mode", "s3.storage_class"]);
    }

    #[test]
    fn catalogo_se_trunca_al_tope_conservando_el_orden_del_wire() {
        // Ids en orden DESCENDENTE: si el filtro ordenase (en vez de conservar
        // el orden que eligió el provider), este test lo vería.
        let total = ATTRS_MAX_ADVERTISED + 10;
        let ids: Vec<String> = (0..total)
            .map(|i| format!("ns.a{:03}", total - i))
            .collect();
        let vivos = catalogo(&serde_json::Value::Array(
            ids.iter().map(|id| descriptor(id)).collect(),
        ));

        assert_eq!(
            vivos.len(),
            ATTRS_MAX_ADVERTISED,
            "se trunca, no se rechaza"
        );
        let vistos: Vec<&str> = vivos.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            vistos,
            ids[..ATTRS_MAX_ADVERTISED]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            "se conservan los PRIMEROS en orden de wire, sin reordenar"
        );
    }

    #[test]
    fn catalogo_bomba_no_materializa_lo_que_sobra() {
        // 10 000 descriptores: se drena lo que pasa del tope sin construirlo.
        let bomba: Vec<serde_json::Value> = (0..10_000)
            .map(|i| descriptor(&format!("ns.a{i}")))
            .collect();
        assert_eq!(
            catalogo(&serde_json::Value::Array(bomba)).len(),
            ATTRS_MAX_ADVERTISED
        );
    }

    #[test]
    fn catalogo_de_ids_invalidos_deja_de_examinarse() {
        // Con ids INVÁLIDOS el tope de `ATTRS_MAX_ADVERTISED` no se llena
        // nunca, así que sin el tope de ESCANEO se parsearía el vector entero
        // para un resultado vacío: el trabajo tiene que estar acotado igual.
        let mut bomba: Vec<serde_json::Value> = (0..ATTRS_MAX_CATALOG_SCAN + 500)
            .map(|i| descriptor(&format!("INVALIDO{i}")))
            .collect();
        // Un id legítimo MÁS ALLÁ del escaneo: no se ve, y ese es el precio
        // documentado de acotar (un peer así ya es hostil, no solo buggy).
        bomba.push(descriptor("s3.etag"));
        assert!(catalogo(&serde_json::Value::Array(bomba)).is_empty());

        // Y justo EN el límite del escaneo el id bueno sí entra.
        let mut al_limite: Vec<serde_json::Value> = (0..ATTRS_MAX_CATALOG_SCAN - 1)
            .map(|i| descriptor(&format!("INVALIDO{i}")))
            .collect();
        al_limite.push(descriptor("s3.etag"));
        let vivos = catalogo(&serde_json::Value::Array(al_limite));
        assert_eq!(vivos.len(), 1);
        assert_eq!(vivos[0].id, "s3.etag");
    }

    #[test]
    fn catalogo_deduplica_por_id_ganando_el_primero() {
        // Mismos bytes, dos consumidores: uno que pliega a mapa y otro que usa
        // `find()`. Con las dos copias vivas pintarían el atributo distinto.
        let vivos = catalogo(&serde_json::json!([
            { "id": "posix.mode", "label": "Mode", "type": "uint", "hint": "mode" },
            { "id": "posix.mode", "label": "Modo", "type": "text", "hint": "opaque" },
        ]));
        assert_eq!(vivos.len(), 1, "un id, un descriptor");
        assert_eq!(vivos[0].ty, AttrType::Uint, "gana el PRIMERO");
        assert_eq!(vivos[0].label, "Mode");
    }

    #[test]
    fn los_duplicados_no_se_comen_el_presupuesto_del_catalogo() {
        // 64 copias del mismo id seguidas de uno legítimo: si el duplicado
        // ocupase hueco, `s3.etag` se quedaría fuera del tope.
        let mut wire: Vec<serde_json::Value> = (0..ATTRS_MAX_ADVERTISED)
            .map(|_| descriptor("posix.mode"))
            .collect();
        wire.push(descriptor("s3.etag"));
        let vivos = catalogo(&serde_json::Value::Array(wire));
        let ids: Vec<&str> = vivos.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["posix.mode", "s3.etag"]);
    }

    #[test]
    fn label_sobre_el_tope_se_recorta_por_frontera_de_caracter() {
        // Multibyte a propósito: recortar a ciegas partiría una secuencia UTF-8
        // (y `String::truncate` entraría en pánico).
        let largo = "ñ".repeat(ATTR_LABEL_MAX); // 2 bytes por carácter
        let vivos = catalogo(&serde_json::json!([{
            "id": "posix.mode", "label": largo, "type": "uint", "hint": "mode",
        }]));
        assert_eq!(vivos.len(), 1, "un label gordo NO cuesta el descriptor");
        assert!(vivos[0].label.len() <= ATTR_LABEL_MAX);
        assert_eq!(
            vivos[0].label,
            "ñ".repeat(ATTR_LABEL_MAX / 2),
            "se corta en frontera de carácter, sin bytes sueltos"
        );

        // El tope exacto no se toca.
        let justo = "a".repeat(ATTR_LABEL_MAX);
        let vivos = catalogo(&serde_json::json!([{
            "id": "posix.mode", "label": justo, "type": "uint", "hint": "mode",
        }]));
        assert_eq!(vivos[0].label, justo);
    }

    #[test]
    fn sanitize_catalog_es_idempotente() {
        // La red del camino EMBEBIDO: aplicarla dos veces (wire y luego en
        // proceso, o al revés) no puede cambiar el resultado.
        let sucio = vec![
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
        let una = sanitize_catalog(sucio);
        let dos = sanitize_catalog(una.clone());
        assert_eq!(una, dos);
        assert_eq!(una.len(), 1);
        assert_eq!(una[0].label.len(), ATTR_LABEL_MAX);
    }

    #[test]
    fn la_peticion_se_acota_en_memoria_pero_no_se_valida() {
        use crate::methods::FsListParams;

        // Tope + 1: el testigo de "se pasó" tiene que sobrevivir para que el
        // daemon pueda responder -32602 (bloque 2).
        let ids: Vec<String> = (0..10_000).map(|i| format!("ns.a{i}")).collect();
        let wire = serde_json::json!({ "path": "file:///", "attrs": ids });
        let params: FsListParams = serde_json::from_value(wire).expect("no es error de decode");
        assert_eq!(params.attrs.len(), ATTRS_MAX_REQUEST + 1);
        assert!(
            params.attrs.len() > ATTRS_MAX_REQUEST,
            "pasarse sigue siendo OBSERVABLE"
        );
        assert_eq!(params.attrs[0], "ns.a0", "y lo que queda viene VERBATIM");

        // Nada se valida ni se deduplica por debajo del tope.
        let wire = serde_json::json!({ "path": "file:///", "attrs": ["MODE", "MODE", ""] });
        let params: FsListParams = serde_json::from_value(wire).expect("no es error de decode");
        assert_eq!(params.attrs, ["MODE", "MODE", ""]);
    }

    #[test]
    fn catalogo_ausente_o_vacio_es_vacio() {
        assert!(catalogo(&serde_json::json!([])).is_empty());
        let sin_campo = serde_json::json!({ "capabilities": { "flags": "", "max_path": null } });
        let caps: crate::methods::FsCapabilitiesResult =
            serde_json::from_value(sin_campo).expect("wire 0.29 válido");
        assert!(caps.attrs.is_empty(), "ausente = el provider no publica");
    }

    #[test]
    fn catalogo_con_tipo_o_pista_del_futuro_sobrevive_degradando() {
        let vivos = catalogo(&serde_json::json!([{
            "id": "futuro.attr",
            "label": "L",
            "type": "quaternion",
            "hint": "holograma",
        }]));
        assert_eq!(vivos.len(), 1);
        assert_eq!(vivos[0].ty, AttrType::Unknown);
        assert_eq!(vivos[0].hint, AttrHint::Unknown);
    }

    #[test]
    fn descriptor_estructuralmente_roto_si_es_error() {
        // La indulgencia es para ids e ids de más; a un descriptor SIN `label`
        // le falta un campo obligatorio: peer roto, no peer más nuevo.
        let wire = serde_json::json!({
            "capabilities": { "flags": "", "max_path": null },
            "attrs": [{ "id": "posix.mode", "type": "uint", "hint": "mode" }],
        });
        assert!(serde_json::from_value::<crate::methods::FsCapabilitiesResult>(wire).is_err());
    }

    #[test]
    fn el_catalogo_no_se_filtra_al_serializar() {
        // Espejo de `Entry::attrs`: el bug de un productor se ve en el wire que
        // emite, no lo blanquea el serializador.
        let caps = crate::methods::FsCapabilitiesResult {
            capabilities: crate::Capabilities {
                flags: crate::CapabilityFlags::empty(),
                max_path: None,
            },
            attrs: vec![AttrInfo {
                id: "MODE".to_owned(),
                label: "Mode".to_owned(),
                ty: AttrType::Uint,
                hint: AttrHint::Mode,
            }],
        };
        let wire = serde_json::to_string(&caps).expect("serializable");
        assert!(wire.contains("MODE"), "se emite tal cual: {wire}");
        let vuelta: crate::methods::FsCapabilitiesResult =
            serde_json::from_str(&wire).expect("decodifica");
        assert!(
            vuelta.attrs.is_empty(),
            "y al volver, el filtro lo descarta"
        );
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
