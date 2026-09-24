//! `Capabilities`: what a provider knows how to do (spec §5). The core picks
//! its strategy by consulting them (never by probing live) and the frontends
//! adapt the UI.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

bitflags::bitflags! {
    /// Capability flags of a provider (M0; the rest arrives with their
    /// milestones).
    ///
    /// Wire: a string with names separated by ` | ` (`bitflags`'s format),
    /// also in binary encodings (readability > 4 bytes). Deserialisation
    /// policy (ADR 0004):
    /// - An unknown name with a valid shape (`[A-Z0-9_]+`): IGNORED. A
    ///   capability is an announcement; an N-1 client that does not know it
    ///   simply does not exploit it — it never breaks on an N+1 flag.
    /// - Hex (`0x…`) or a malformed token: ERROR. Bits with no name do not
    ///   travel.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct CapabilityFlags: u32 {
        /// Atomic `rename()` within the provider.
        const RENAME_ATOMIC = 1 << 0;
        /// Server-side copy (S3 CopyObject, SFTP ext, reflink/clonefile).
        const SERVER_COPY = 1 << 1;
        /// Supports symlinks (creating them may need a privilege on Windows).
        const SYMLINKS = 1 << 2;
        /// The FS is case sensitive (ext4 is; NTFS/APFS by default are not).
        const CASE_SENSITIVE = 1 << 3;
        /// The FS preserves case even though it does not distinguish it
        /// (NTFS/APFS).
        const CASE_PRESERVING = 1 << 4;
        /// Can append to the end of an existing file (resume, M2).
        const APPEND = 1 << 5;
        /// Can write at an arbitrary offset (verification/patching).
        const RANDOM_WRITE = 1 << 6;
        /// There is a trash: `trash()` moves to a recoverable place (ADR 0009).
        const TRASH = 1 << 7;
        /// The provider is read-only (0.9.0, ADR 0018: archives as
        /// directories): EVERY mutation answers `Unsupported`. The UI vetoes
        /// upfront and the copy engine rejects destinations here without a
        /// round trip.
        const READ_ONLY = 1 << 8;
        /// Case folding at this LOCATION **expands** (0.45.0, #145,
        /// ADR 0054): ext4/f2fs with the directory in `+F`, whose kernel
        /// table is built from `CaseFolding.txt` with `C + F` state, so
        /// `straße.txt` and `strasse.txt` are ONE file there.
        ///
        /// Only makes sense WITHOUT [`Self::CASE_SENSITIVE`] — a directory
        /// that is case sensitive folds nothing — and only
        /// `Provider::capabilities_at` answers it: it belongs to the
        /// directory, not the backend.
        const FULL_FOLD = 1 << 9;
        /// A write under this location can be confined under the root the
        /// caller names, with a kernel guarantee (0.45.0, #164, ADR 0054):
        /// `Provider::open_root` returns a handle instead of `Unsupported`.
        ///
        /// Answered by `Provider::capabilities_at` and NEVER `capabilities()`:
        /// it depends on the mount, the platform and the running kernel. Its
        /// absence does not prevent anything — the core degrades to walking
        /// with `lstat` and says so — but it means a symlink in an
        /// INTERMEDIATE component can redirect the write outside its root.
        const CONFINED_WRITES = 1 << 10;
        /// Nodes at this location have POSIX permissions that CAN BE CHANGED
        /// (0.60.0, #314): `fs.set_mode` works here.
        ///
        /// Declared by whoever can do both things, read them and write them.
        /// A `.zip` has nothing to change and an object bucket has no mode;
        /// without this flag, the frontend turns the gesture off with its
        /// reason instead of offering it only to fail with `Unsupported`.
        const POSIX_MODE = 1 << 11;
    }
}

impl Serialize for CapabilityFlags {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = String::new();
        bitflags::parser::to_writer(self, &mut out).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&out)
    }
}

impl<'de> Deserialize<'de> for CapabilityFlags {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FlagsVisitor;

        impl serde::de::Visitor<'_> for FlagsVisitor {
            type Value = CapabilityFlags;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a `A | B` capability flags string")
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<CapabilityFlags, E> {
                parse_flags(s).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(FlagsVisitor)
    }
}

// `CapabilityFlags` serializes as a `A | B` string (ADR 0004), so its JSON
// Schema is a string — the bitflags serde is hand-written and cannot derive.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for CapabilityFlags {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CapabilityFlags".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Capability flags as a `NAME | NAME` string \
                            (e.g. `RENAME_ATOMIC | CASE_SENSITIVE`). Unknown \
                            well-formed names are ignored for forward-compat.",
        })
    }
}

/// Parser for the wire form of flags, under ADR 0004's policy: known names
/// accumulate, well-formed unknown names are ignored (forward-compat), and
/// hex or malformed tokens are an error (`bitflags::parser::from_str` would
/// silently keep unknown bits — unacceptable on the wire).
fn parse_flags(s: &str) -> Result<CapabilityFlags, &'static str> {
    let mut flags = CapabilityFlags::empty();
    if s.trim().is_empty() {
        return Ok(flags);
    }
    for token in s.split('|') {
        let token = token.trim();
        if token.is_empty() {
            return Err("empty flag between separators");
        }
        if token.starts_with("0x") || token.starts_with("0X") {
            return Err("hex flag values are not allowed on the wire");
        }
        if !token
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err("malformed flag name (expected `[A-Z0-9_]+`)");
        }
        if let Some(known) = CapabilityFlags::from_name(token) {
            flags |= known;
        }
        // A well-formed but unknown name: a capability of a newer protocol —
        // ignored, not exploited.
    }
    Ok(flags)
}

/// Capabilities a provider declares.
///
/// ```
/// use norte_proto::{Capabilities, CapabilityFlags};
/// let c = Capabilities {
///     flags: CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_SENSITIVE,
///     max_path: Some(4096),
/// };
/// assert!(c.flags.contains(CapabilityFlags::RENAME_ATOMIC));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capabilities {
    /// Capability flags.
    pub flags: CapabilityFlags,
    /// Maximum native path length in bytes; `None` = no known limit.
    #[serde(default)]
    pub max_path: Option<u32>,
}
