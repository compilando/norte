//! `VPath`: the VFS's path representation — bytes with a URI shape.
//!
//! Filenames are NOT UTF-8 (spec principle 3): each segment stores raw bytes
//! (Unix: the OS's bytes as is; Windows: the WTF-8 form of
//! `OsStr::as_encoded_bytes`). UTF-8 is only a display view, lossy and
//! marked. The wire format (percent-encoding) is pinned by ADR 0001 and its
//! golden tests.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::wire::vpath_codec;

/// Validation or parse error of [`VPath`] and its components.
///
/// `non_exhaustive`: like [`Error`](crate::Error) — an external consumer must
/// not break when a new version adds a rejection cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum VPathError {
    /// The wire does not contain the `://` separator.
    #[error("missing scheme: expected `scheme://…`")]
    MissingScheme,
    /// The scheme does not match `[a-z][a-z0-9+.-]*` (no case-folding).
    #[error("invalid scheme: expected `[a-z][a-z0-9+.-]*`")]
    InvalidScheme,
    /// The authority is empty, falls outside the charset (printable ASCII
    /// without `/` or `%`), or carries a `:` in the userinfo (inline
    /// password, #46).
    #[error(
        "invalid authority: non-empty printable ASCII without `/`/`%`, and no `:` in the userinfo"
    )]
    InvalidAuthority,
    /// Empty segment (double `//` or a trailing slash outside the root).
    #[error("empty path segment")]
    EmptySegment,
    /// A `.` or `..` segment (literal or via escape): `VPath` does not
    /// resolve relative paths.
    #[error("dot segment (`.`/`..`) is not allowed")]
    DotSegment,
    /// A NUL byte in a segment (literal or via escape).
    #[error("NUL byte in segment")]
    NulByte,
    /// Invalid byte in a segment (e.g. a `/` introduced via `%2F`).
    #[error("invalid byte in segment (separator cannot be escaped in)")]
    InvalidByte,
    /// Malformed percent escape (`%G1`, `%4`, a trailing `%`).
    #[error("malformed percent escape")]
    BadEscape,
    /// Malformed file-as-directory addressing (ADR 0018): missing `!` marker
    /// on a compound scheme, `!` in a forbidden position while composing, or
    /// a format outside [`ARCHIVE_FORMATS`].
    #[error("malformed archive addressing (`!` marker / format, ADR 0018)")]
    ArchiveAddressing,
}

/// Recognized file-as-directory format tokens (ADR 0018, ADR 0028 for
/// `tar+gz`).
///
/// A scheme is compound if and only if its prefix up to the `+` that
/// separates it from the inner scheme matches EXACTLY one of these tokens;
/// growing the list is a protocol change. `tar+gz` is a COMPOUND token
/// (it contains its own `+`): the gzip layer is opaque inside the format, not
/// a general layering mechanism (GENERAL nesting is #56: chained formats,
/// `zip+tar+file`, each layer with its own marker). Resolution is
/// longest-match against this whitelist (`scheme_format_prefix`):
/// `tar+gz+file` is format `tar+gz` over `file`, never format `tar` over an
/// orphan interior `gz+file`. Normative reservation: no provider registers
/// schemes starting with `<format>+`.
pub const ARCHIVE_FORMATS: &[&str] = &["zip", "tar", "tar+gz", "rar"];

/// Disassembled reference of a file-as-directory path (ADR 0018):
/// `<format>+<scheme>://auth/<outer>/!/<inner>`.
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
    /// Container format (a token from [`ARCHIVE_FORMATS`]).
    pub format: String,
    /// Path of the CONTAINER FILE in its inner provider.
    pub outer: VPath,
    /// Inner segments relative to the archive's root.
    pub inner: Vec<Segment>,
}

/// Scheme of a [`VPath`] (`file`, `sftp`, `mem`…), validated as
/// `[a-z][a-z0-9+.-]*`.
///
/// ```
/// use norte_proto::Scheme;
/// assert!(Scheme::new("file").is_ok());
/// assert!(Scheme::new("FILE").is_err()); // no case-folding
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Scheme(String);

impl Scheme {
    /// Validates and builds a scheme.
    ///
    /// # Errors
    /// [`VPathError::InvalidScheme`] if it does not match `[a-z][a-z0-9+.-]*`.
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

    /// The scheme as a `&str` (always ASCII lowercase).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Authority of a [`VPath`] (host, `host:port`, connection name…).
///
/// Validated charset: printable ASCII (0x21–0x7E) without `/` or `%`; never
/// empty (absence of authority is `None`, not `""`). There is no
/// percent-encoding in the authority: it travels literally, and that is why
/// the first `/` after `://` always separates authority from path
/// (injectivity of the wire). Non-ASCII hosts go in punycode — a provider
/// decision, not proto's.
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
    /// Validates and builds an authority.
    ///
    /// Rejects a `:` in the userinfo (`user:pass@host`): an inline password
    /// in the URL would end up in config/logs (rule 10). ROOT defense, proto
    /// 0.8.0 (#46): the CLI/`norte-connect` guards remain as defense in
    /// depth. The `:` of `host:port` (after `@`, or without `@`) and the one
    /// in a bracketed IPv6 stay valid.
    ///
    /// # Errors
    /// [`VPathError::InvalidAuthority`] if it is empty, contains bytes
    /// outside printable ASCII / `/` / `%`, or carries a `:` in the userinfo.
    pub fn new(s: &str) -> Result<Self, VPathError> {
        let charset_ok = !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_graphic() && b != b'/' && b != b'%');
        // userinfo = whatever precedes the LAST `@`; a `:` there is
        // `user:pass`. The last `@` is used (not the first) so a pathological
        // authority with several `@`s (`a@b:c@host`) cannot sneak a `:` into
        // an intermediate segment; a legitimate authority has at most one `@`
        // (the host itself carries no `@`), so this rejects nothing valid.
        // Matches the `rsplit_once` in the CLI's guard (consistent defense in
        // depth).
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

    /// The authority as a `&str` (always printable ASCII).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A path segment: raw bytes, never forced into UTF-8.
///
/// Invariants (validated on construction, never silently sanitized): not
/// empty, no NUL, no `/`, different from `.` and `..`.
///
/// The wire form is a percent-encoded string (ADR 0001, the same codec
/// `VPath` uses), and it is what serde (de)serializes — see
/// [`Segment::to_wire`] / [`Segment::parse_wire`].
///
/// ```
/// use norte_proto::Segment;
/// let s = Segment::new(vec![0xFF, 0xFE]).unwrap(); // non-UTF-8 bytes: valid
/// assert_eq!(s.as_bytes(), &[0xFF, 0xFE]);
/// assert_eq!(s.to_string(), "%FF%FE");
/// assert!(Segment::new(b"a/b".to_vec()).is_err());
/// ```
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Segment(Vec<u8>);

impl Segment {
    /// Validates and builds a segment from raw bytes.
    ///
    /// # Errors
    /// [`VPathError::EmptySegment`], [`VPathError::NulByte`],
    /// [`VPathError::InvalidByte`] (contains `/`), or
    /// [`VPathError::DotSegment`].
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

    /// The segment's raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The percent-encoded wire form (ADR 0001) — the same codec `VPath`
    /// uses, and what serde serializes.
    ///
    /// ```
    /// use norte_proto::Segment;
    /// let s = Segment::new(vec![0xFF, 0xFE]).unwrap();
    /// assert_eq!(s.to_wire(), "%FF%FE");
    /// ```
    #[must_use]
    pub fn to_wire(&self) -> String {
        let mut out = String::new();
        vpath_codec::encode_segment(&self.0, &mut out);
        out
    }

    /// Parses the percent-encoded wire form of a single segment. Accepts
    /// non-canonical forms (`%41` ≡ `A`); [`Self::to_wire`] canonicalizes.
    ///
    /// # Errors
    /// [`VPathError::BadEscape`] for a malformed escape, or whatever
    /// [`Segment::new`] would return once decoded (empty, NUL, `/`, `.`/`..`
    /// — the invariant is validated POST-decode, so `%2E%2E` cannot smuggle
    /// in a `..`).
    ///
    /// ```
    /// use norte_proto::Segment;
    /// let s = Segment::parse_wire("%FF%FE").unwrap();
    /// assert_eq!(s.as_bytes(), &[0xFF, 0xFE]);
    /// ```
    pub fn parse_wire(wire: &str) -> Result<Self, VPathError> {
        let bytes = vpath_codec::decode_segment(wire)?;
        Self::new(bytes)
    }
}

impl fmt::Debug for Segment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Segment({:?})", String::from_utf8_lossy(&self.0))
    }
}

/// `Display` is the wire form (lossless), same as on `VPath`.
impl fmt::Display for Segment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_wire())
    }
}

/// VFS path: `scheme://authority/<segments in bytes>`.
///
/// Always absolute with respect to the provider's root; no `.`/`..`; the wire
/// format is a percent-encoded string (ADR 0001) and is what serde
/// serializes.
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
    /// The root of a provider: `scheme://authority/` with no segments.
    #[must_use]
    pub fn root(scheme: Scheme, authority: Option<Authority>) -> Self {
        Self {
            scheme,
            authority,
            segments: Vec::new(),
        }
    }

    /// Parses the wire form. Accepts non-canonical forms (`%41` ≡ `A`, a root
    /// without a trailing slash); [`Self::to_wire`] canonicalizes.
    ///
    /// # Errors
    /// Any [`VPathError`]; segment invariants are validated POST-decode (a
    /// `%2F` does not fabricate a separator, a `%2E%2E` does not smuggle in a
    /// `..`).
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

    /// The canonical wire form (the one that travels over the protocol and
    /// that serde serializes).
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

    /// Human-facing view, shaped `⟨scheme authority⟩/seg/…`: lossy UTF-8 with
    /// `�` marking undecodable bytes, control characters AND bidi/invisible
    /// formatters (never raw controls or RTL overrides toward a terminal —
    /// direction spoofing, issue #21). Deliberately has NO wire form (no
    /// `://`): [`Self::parse`] on a display always fails, so it can never
    /// reconstruct a path by accident (use [`Self::to_wire`]).
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

    /// The provider's scheme.
    #[must_use]
    pub fn scheme(&self) -> &str {
        self.scheme.as_str()
    }

    /// The authority (host, connection…), if there is one.
    #[must_use]
    pub fn authority(&self) -> Option<&str> {
        self.authority.as_ref().map(Authority::as_str)
    }

    /// Iterator over each segment's raw bytes.
    pub fn segments(&self) -> impl Iterator<Item = &[u8]> {
        self.segments.iter().map(Segment::as_bytes)
    }

    /// `true` if this is the provider's root (no segments).
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.segments.is_empty()
    }

    /// Child path with `segment` appended at the end.
    #[must_use]
    pub fn join(&self, segment: Segment) -> Self {
        let mut child = self.clone();
        child.segments.push(segment);
        child
    }

    /// The parent path; `None` at the root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        if self.segments.is_empty() {
            return None;
        }
        let mut p = self.clone();
        p.segments.pop();
        Some(p)
    }

    /// The last segment; `None` at the root.
    #[must_use]
    pub fn file_name(&self) -> Option<&Segment> {
        self.segments.last()
    }

    /// Replaces the last segment (e.g. to derive `x` → `x.norte-partial`);
    /// `None` at the root.
    #[must_use]
    pub fn with_file_name(&self, segment: Segment) -> Option<Self> {
        if self.segments.is_empty() {
            return None;
        }
        let mut p = self.clone();
        *p.segments.last_mut()? = segment;
        Some(p)
    }

    /// Composes the path of an entry INSIDE an archive (ADR 0018):
    /// `<format>+<outer's-scheme>://<outer's-auth>/<outer>/!/<inner>`.
    ///
    /// Only over already validated segments — it never fabricates paths
    /// [`Self::archive_split`] could not undo (roundtrip guaranteed). Purely
    /// syntactic: a root `outer` (`file:///`) composes without error and it
    /// is the provider that answers `TypeMismatch` (the root is not an
    /// archive).
    ///
    /// ```
    /// use norte_proto::VPath;
    /// let outer = VPath::parse("file:///home/o/a.zip").unwrap();
    /// let root = VPath::archive_compose("zip", &outer, &[]).unwrap();
    /// assert_eq!(root.to_wire(), "zip+file:///home/o/a.zip/!");
    /// ```
    ///
    /// # Errors
    /// [`VPathError::ArchiveAddressing`] if `format` is not in
    /// [`ARCHIVE_FORMATS`], if `outer` is already compound (v1 = one layer),
    /// if `outer`/`inner` contain a literal `!` segment (the outer part would
    /// not be addressable; the inner part cannot be matched against the
    /// index, which omits those components), or if the resulting scheme does
    /// NOT re-resolve to `format` by longest-match (compound-token ambiguity:
    /// e.g. `archive_compose("tar", <outer with scheme "gz+mem">, …)` would
    /// form `tar+gz+mem`, which [`Self::archive_split`] would read as format
    /// `tar+gz` over `mem`, not as `tar` over `gz+mem` — rejected to uphold
    /// the roundtrip guarantee, ADR 0028). [`VPathError::InvalidScheme`] if
    /// the concatenation does not form a valid scheme (impossible with
    /// formats from the whitelist; defense in depth).
    ///
    /// # Panics
    /// Never in practice: `!` is a valid segment by construction.
    pub fn archive_compose(
        format: &str,
        outer: &Self,
        inner: &[Segment],
    ) -> Result<Self, VPathError> {
        if !ARCHIVE_FORMATS.contains(&format) {
            return Err(VPathError::ArchiveAddressing);
        }
        // #56: a COMPOUND outer is legal if and only if it is itself a
        // WELL-FORMED archive path (its own split resolves) — nesting one
        // more layer. A flat outer with markers stays forbidden (check
        // below).
        let outer_nested = match outer.archive_split() {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(_) => return Err(VPathError::ArchiveAddressing),
        };
        if scheme_format_prefix(outer.scheme()).is_some() && !outer_nested {
            return Err(VPathError::ArchiveAddressing);
        }
        let composed_scheme = format!("{format}+{}", outer.scheme());
        // Roundtrip guard (ADR 0028): with compound tokens like `tar+gz`,
        // prepending `format` to `outer`'s scheme can form a scheme that
        // `scheme_format_prefix`'s longest-match resolves to ANOTHER format
        // (see the example in the rustdoc above). If it does not re-resolve
        // exactly to `format`, `archive_split` could never undo this compose
        // the way it was requested: rejected here instead of fabricating a
        // path that breaks its own contract.
        if scheme_format_prefix(&composed_scheme) != Some(format) {
            return Err(VPathError::ArchiveAddressing);
        }
        let marker = || Segment::new(MARKER.to_vec()).expect("`!` is a valid segment");
        // The inner part NEVER carries a marker; the outer part only carries
        // them if it is a well-formed archive path (#56 — its markers are its
        // own).
        if (!outer_nested && outer.segments.iter().any(|s| s.as_bytes() == MARKER))
            || inner.iter().any(|s| s.as_bytes() == MARKER)
        {
            return Err(VPathError::ArchiveAddressing);
        }
        let mut segments = outer.segments.clone();
        segments.push(marker());
        segments.extend_from_slice(inner);
        let composed = Self {
            scheme: Scheme::new(&composed_scheme)?,
            authority: outer.authority.clone(),
            segments,
        };
        // EXTENDED roundtrip guard (#56): splitting the result must return
        // EXACTLY what was composed (format, outer, inner) — with nested
        // layers the marker choice (first/last) has to undo this compose
        // exactly, or it is rejected here.
        match composed.archive_split() {
            Ok(Some(r)) if r.format == format && r.outer == *outer && r.inner == inner => {
                Ok(composed)
            }
            _ => Err(VPathError::ArchiveAddressing),
        }
    }

    /// Undoes [`Self::archive_compose`] ONE layer: `Ok(None)` if the scheme
    /// is not compound (the prefix up to the first `+` is not a format from
    /// [`ARCHIVE_FORMATS`] — `s3+v2.x-y` is a legitimate provider scheme, not
    /// an archive). With a FLAT inner part it cuts at the FIRST `!` segment
    /// (v1 rule: later `!`s stay in the inner part, whose index never
    /// contains them → `NotFound` downstream); with an inner part that is
    /// itself COMPOUND (#56, ADR 0018 A3: `zip+tar+file`) it cuts at the
    /// LAST one — the earlier markers belong to the layers below and the
    /// returned outer part is peeled recursively. Purely syntactic: it does
    /// not validate that the outer part names an archive nor that the deep
    /// layer has its marker (that fails cleanly when used).
    ///
    /// ```
    /// use norte_proto::VPath;
    /// let flat = VPath::parse("file:///a.zip").unwrap();
    /// assert!(flat.archive_split().unwrap().is_none());
    /// ```
    ///
    /// ```
    /// use norte_proto::VPath;
    /// // #56: two layers — the outer one (zip) takes the last marker.
    /// let nested = VPath::parse("zip+tar+file:///b.tar/!/i.zip/!/f").unwrap();
    /// let r = nested.archive_split().unwrap().unwrap();
    /// assert_eq!(r.format, "zip");
    /// assert_eq!(r.outer.to_wire(), "tar+file:///b.tar/!/i.zip");
    /// ```
    ///
    /// # Errors
    /// [`VPathError::ArchiveAddressing`] if the scheme is compound but there
    /// is no `!` marker in the path. [`VPathError::InvalidScheme`] if the
    /// inner scheme is empty after removing the format.
    pub fn archive_split(&self) -> Result<Option<ArchiveRef>, VPathError> {
        let Some(format) = scheme_format_prefix(self.scheme.as_str()) else {
            return Ok(None);
        };
        let inner_scheme = &self.scheme.as_str()[format.len() + 1..];
        // #56 (ADR 0018 A3, right-to-left resolution): with an inner part
        // that is itself COMPOUND, this layer (the outermost) cuts at the
        // LAST marker — earlier ones belong to the layers below. With a FLAT
        // inner part the v1 rule is kept (FIRST marker): extra `!`s go to the
        // inner part, whose index never contains them → NotFound downstream —
        // a rogue marker NEVER redirects the outer part toward a real object
        // called `!` in the flat provider.
        let nested = scheme_format_prefix(inner_scheme).is_some();
        let marker_pos = if nested {
            self.segments.iter().rposition(|s| s.as_bytes() == MARKER)
        } else {
            self.segments.iter().position(|s| s.as_bytes() == MARKER)
        }
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

/// `true` if `c` must not go raw to a display (terminal/GUI): C0/C1 controls
/// **and** bidi/invisible formatters. The latter (RTL overrides like
/// U+202E, direction marks, zero-width, BOM) allow visually spoofing a
/// name — a `.exe` that looks like a `.jpg` — without being `is_control`
/// (issue #21).
fn is_display_hazard(c: char) -> bool {
    // C0/C1 controls + BIDI formatters/overrides. ZWJ/ZWNJ (U+200C/200D) are
    // NOT masked: they are legitimate in emoji sequences and in scripts
    // (Persian, Indic) — #21's vector is bidi direction, not joining.
    c.is_control()
        || matches!(c,
            '\u{200E}' | '\u{200F}' | '\u{061C}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
        )
}

/// ADR 0018's marker segment.
const MARKER: &[u8] = b"!";

/// The format token if `scheme` is compound (`zip+file` → `Some("zip")`,
/// `tar+gz+file` → `Some("tar+gz")`); `None` if no [`ARCHIVE_FORMATS`] prefix
/// matches.
///
/// LONGEST-MATCH, not `split_once('+')`: a token can contain its own `+`
/// (`tar+gz`), so cutting at the first `+` would leave `tar+gz+file` as
/// format `tar` over an orphan interior `gz+file`. ALL formats in the
/// whitelist are tried, and the longest one that matches as a prefix
/// followed by `+` is kept (ADR 0028).
fn scheme_format_prefix(scheme: &str) -> Option<&str> {
    ARCHIVE_FORMATS
        .iter()
        .filter(|f| {
            scheme.len() > f.len() && scheme.as_bytes()[f.len()] == b'+' && scheme.starts_with(*f)
        })
        .max_by_key(|f| f.len())
        .copied()
}

/// Public wrapper of the format prefix for frontends (CLI/TUI) that need to
/// recognize a file-as-directory scheme (e.g. to accept `tar+gz+file://…`
/// URLs) without duplicating the longest-match grammar against
/// [`ARCHIVE_FORMATS`] (ADR 0028, #55).
///
/// Purely syntactic, same as [`VPath::archive_split`]: it does not validate
/// that the outer part names a real archive, and `Some` does not imply a
/// well-formed `!` marker (that is validated by
/// `archive_split`/`archive_compose`).
///
/// ```
/// use norte_proto::scheme_archive_format;
/// assert_eq!(scheme_archive_format("tar+gz+file"), Some("tar+gz"));
/// assert_eq!(scheme_archive_format("zip+file"), Some("zip"));
/// assert_eq!(scheme_archive_format("s3+v2.x-y"), None); // a provider, not an archive
/// assert_eq!(scheme_archive_format("file"), None);
/// ```
#[must_use]
pub fn scheme_archive_format(scheme: &str) -> Option<&str> {
    scheme_format_prefix(scheme)
}

impl fmt::Debug for VPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VPath({})", self.to_wire())
    }
}

/// `Display` is the wire form (lossless), meant for logs and errors.
/// For a human-facing view use [`VPath::display_lossy`].
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

// A `VPath` is (de)serialized as a single wire string (`to_wire`), so its JSON
// Schema is a string — it cannot be derived because the serde is hand-written
// (filenames are bytes; the wire form is `scheme://authority/segments`).
#[cfg(feature = "schema")]
impl schemars::JsonSchema for VPath {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VPath".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "VPath wire string: `scheme://authority/segments`. \
                            Filenames are bytes; non-UTF-8 segments use the \
                            percent-encoded wire form.",
        })
    }
}

impl Serialize for Segment {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_wire())
    }
}

impl<'de> Deserialize<'de> for Segment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SegmentVisitor;

        impl Visitor<'_> for SegmentVisitor {
            type Value = Segment;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a percent-encoded path segment")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Segment, E> {
                Segment::parse_wire(v).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(SegmentVisitor)
    }
}

// A `Segment` is (de)serialized as a single wire string with the same codec
// `VPath` uses (ADR 0001), so its JSON Schema is a string — it cannot be
// derived because the serde is hand-written (filenames are bytes).
#[cfg(feature = "schema")]
impl schemars::JsonSchema for Segment {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Segment".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": 1,
            "description": "One path component in its percent-encoded wire form \
                            (ADR 0001). Never assume UTF-8: the decoded bytes are \
                            the name. A literal `%` is always `%25`, and C0/DEL \
                            control bytes are always escaped, even inside \
                            otherwise-valid UTF-8. The decoded bytes must be \
                            non-empty, must not contain `/` or NUL, and must not \
                            be `.` or `..`.",
        })
    }
}
