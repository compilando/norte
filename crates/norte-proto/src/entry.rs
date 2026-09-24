//! `Entry`: what the VFS knows about a node (result of `fs.stat` / `fs.list`).

use serde::{Deserialize, Serialize};

use crate::VPath;

/// Class of a VFS node.
///
/// Unknown values (protocol N+1) deserialize to [`EntryKind::Other`]: an old
/// client degrades, it does not break.
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
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symlink (M0: never followed; preserved or `Unsupported`).
    Symlink,
    /// Anything else (device, socket, fifo, or a future protocol's kind).
    #[serde(other)]
    Other,
}

/// Metadata of a VFS node.
///
/// Optional fields `None` = "the provider does not know it", never a
/// fabricated 0.
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
/// // Without attrs, the wire is EXACTLY 0.29's.
/// let plain = Entry { attrs: Default::default(), ..e };
/// assert!(!serde_json::to_string(&plain).unwrap().contains("attrs"));
///
/// // A malformed attribute key does NOT reach the map: it is dropped on
/// // decode, with no error (see [`Entry::attrs`]).
/// let wire = r#"{"path":"file:///a.txt","kind":"file","attrs":{"MODE":{"uint":1}}}"#;
/// let filtered: Entry = serde_json::from_str(wire).unwrap();
/// assert!(filtered.attrs.is_empty());
/// ```
///
/// NOTE: derived `PartialEq`/`Eq`/`Hash` are REPRESENTATIONAL, not of
/// IDENTITY: two `Entry`s of the SAME node differ if one was requested with
/// attributes and the other was not, or if different ids were requested. For
/// "is this the same node?" compare [`Entry::path`]; these derivations exist
/// for fixtures, tests and deduplicating identical listings, not for
/// deciding identity.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Entry {
    /// Full path of the node (frontends operate on it, not on the name).
    pub path: VPath,
    /// Class of the node.
    pub kind: EntryKind,
    /// Size in bytes; `None` if the provider does not know it (e.g.
    /// directories).
    #[serde(default)]
    pub size: Option<u64>,
    /// Last modification in milliseconds since the UTC epoch; `i64` because
    /// pre-1970 dates exist on real FSes. `None` = unknown.
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

/// Exact length, in characters, of a well-formed [`DirAnchor`].
pub const DIR_ANCHOR_LEN: usize = 32;

/// The OPAQUE identity of the directory a listing returned, so a request that
/// writes into it can say WHICH one it was (#295, 0.54.0).
///
/// # What problem it solves
///
/// The core opens the destination directory as a confined root (ADR 0072), so
/// a substitution AFTER that open no longer redirects anything. What it
/// cannot tell apart is a link that was **already in place** the first time it
/// looked: from inside the core, a `dest/sub -> /etc` planted just now and a
/// legitimate `~/copias -> /mnt/disco/copias` are identical — both resolve
/// somewhere else — and rejecting both breaks copying to `/tmp` on macOS or to
/// `/bin` on a Linux with usrmerge.
///
/// The only thing that tells them apart is the identity that was observed
/// **when the human approved**: the listing they were looking at. That is
/// this.
///
/// # It is opaque on purpose
///
/// Inside there is no inode or volume number, but a value derived from them
/// with a daemon secret: two paths of the same node give the same anchor, and
/// a client can neither fabricate one nor deduce which node is behind it. A
/// client treats it as bytes: stores it, returns it, and never interprets or
/// constructs it.
///
/// An anchor **does not survive a daemon restart**, which renews the secret.
/// A client that reconnects has lost its listing anyway and asks for it
/// again, so the window that matters — look, approve, write — falls entirely
/// within one session.
///
/// ```
/// use norte_proto::entry::{DIR_ANCHOR_LEN, DirAnchor};
/// let a = DirAnchor::new("0123456789abcdef0123456789abcdef".to_owned());
/// assert!(a.is_well_formed());
/// assert_eq!(a.as_str().len(), DIR_ANCHOR_LEN);
/// // It travels on the wire as a plain string and nothing else.
/// assert_eq!(
///     serde_json::to_string(&a).unwrap(),
///     "\"0123456789abcdef0123456789abcdef\""
/// );
///
/// // Something that is not well-formed is NOT a decoding error: it arrives,
/// // and whoever compares it will never find a node that matches, which is
/// // the safe answer (an anchor that is not recognized authorizes nothing).
/// let odd: DirAnchor = serde_json::from_str("\"../etc\"").unwrap();
/// assert!(!odd.is_well_formed());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DirAnchor(String);

impl DirAnchor {
    /// Wraps the value the daemon produced.
    ///
    /// Does not validate: whoever emits it knows what it emits, and whoever
    /// receives it over the wire asks with [`DirAnchor::is_well_formed`]. That
    /// asymmetry is the same one [`Entry::attrs`] documents and for the same
    /// reason — a producer's failure has to stay visible.
    ///
    /// ```
    /// use norte_proto::entry::DirAnchor;
    /// assert_eq!(DirAnchor::new("ab".to_owned()).as_str(), "ab");
    /// ```
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The value as is, to store or return it. Never to read it.
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

    /// Does it have the shape a daemon emits: [`DIR_ANCHOR_LEN`] lowercase hex
    /// digits?
    ///
    /// A malformed anchor is not a wire error — it does not break the
    /// request — but it also does not match any node, so the operation that
    /// carried it is rejected. Failing closed is the right call here: the
    /// anchor exists to authorize, not to dispense.
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
