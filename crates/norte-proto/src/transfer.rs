//! Transfer types (ADR 0005/0009, spec §5): read range and copy/delete
//! policies. They travel in the params of `fs.copy`/`fs.move`/`fs.delete` and
//! in the `Provider` trait's API.

use serde::{Deserialize, Serialize};

/// Byte range of a read: starting `offset` and optional length (`None` = to
/// EOF). Required by M2's resume (`.norte-partial` + offset) and the viewer
/// (partial reads of large files).
///
/// ```
/// use norte_proto::ByteRange;
/// let r: ByteRange = serde_json::from_str(r#"{"offset": 65536, "len": null}"#).unwrap();
/// assert_eq!(r, ByteRange { offset: 65536, len: None });
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ByteRange {
    /// First byte to read (0 = start).
    pub offset: u64,
    /// How many bytes to read; `None` = to the end of the file.
    pub len: Option<u64>,
}

/// What to do when the destination of a copy/move already exists (spec §5;
/// exact semantics in ADR 0005).
///
/// M1's engine treats [`Ask`](Self::Ask) as [`Fail`](Self::Fail): interactive
/// per-file resolution arrives with the TUI's dialogs (phase 5). An old core
/// that does not know a new policy MUST fail the request, never guess.
///
/// ```
/// use norte_proto::CollisionPolicy;
/// let p: CollisionPolicy = serde_json::from_str(r#""rename_auto""#).unwrap();
/// assert_eq!(p, CollisionPolicy::RenameAuto);
/// assert_eq!(CollisionPolicy::default(), CollisionPolicy::Fail);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionPolicy {
    /// The collision is an error: `Conflict` and the task fails (default).
    #[default]
    Fail,
    /// Ask per file (TUI, phase 5). The M1 engine treats it as
    /// [`Fail`](Self::Fail).
    Ask,
    /// Do not copy the conflicting entry; the task ends `Completed`.
    Skip,
    /// Replace the destination (remove + write, two journal mutations; type
    /// against a different type is still a `Conflict`).
    Overwrite,
    /// Look for a free name: a ` (n)` suffix before the last extension,
    /// n = 1..=1000; exhausted → `Conflict`.
    RenameAuto,
    /// Replace only if the source is newer (`mtime`); otherwise skip. With no
    /// comparable mtime on either side → `Conflict`.
    Newer,
}

/// What to do with symlinks when copying (spec §17.9; ADR 0005).
///
/// In M1, [`Follow`](Self::Follow) on a symlink to a DIRECTORY returns
/// `Unsupported` (following dirs requires cycle detection — M2).
///
/// ```
/// use norte_proto::SymlinkPolicy;
/// let p: SymlinkPolicy = serde_json::from_str(r#""preserve""#).unwrap();
/// assert_eq!(p, SymlinkPolicy::Preserve);
/// assert_eq!(SymlinkPolicy::default(), SymlinkPolicy::Preserve);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymlinkPolicy {
    /// Copy the pointed-to CONTENT. Symlink to a dir: `Unsupported` in M1.
    Follow,
    /// Recreate the symlink at the destination, target bytes untouched
    /// (default: what `cp -a` does).
    #[default]
    Preserve,
    /// Do not copy symlinks (counted as skipped).
    Skip,
}

/// Resuming an interrupted transfer (ADR 0012, spec §5).
///
/// ```
/// use norte_proto::ResumePolicy;
/// assert_eq!(ResumePolicy::default(), ResumePolicy::Off);
/// let p: ResumePolicy = serde_json::from_str(r#""on""#).unwrap();
/// assert_eq!(p, ResumePolicy::On);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumePolicy {
    /// No resuming: cancelling/failing leaves the destination CLEAN (M1's
    /// contract). Default — zero surprises for whoever does not ask for it.
    #[default]
    Off,
    /// Resume: cancelling or a transient failure KEEPS the
    /// `.norte-partial`; the next copy of the same `src→dst` continues from
    /// where it was (the provider's `already`, ADR 0012).
    On,
}

/// How to verify the `.norte-partial` before resuming onto it (ADR 0012).
/// Only applies with [`ResumePolicy::On`].
///
/// ```
/// use norte_proto::VerifyPolicy;
/// assert_eq!(VerifyPolicy::default(), VerifyPolicy::Length);
/// let v: VerifyPolicy = serde_json::from_str(r#""hash""#).unwrap();
/// assert_eq!(v, VerifyPolicy::Hash);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyPolicy {
    /// Length only: if the partial is longer than the source it is discarded
    /// and started from scratch. Cheap (default).
    #[default]
    Length,
    /// Additionally, the hash of `source[..already]` must match the
    /// partial's; if not, it is discarded. Rereads `already` bytes on both
    /// sides.
    Hash,
}

/// How to delete (ADR 0009). The WIRE's default is the safe one:
/// [`Trash`](Self::Trash). The engine NEVER degrades on its own — asking for
/// `Trash` without the `TRASH` capability is `Unsupported` and the frontend
/// decides with the user informed.
///
/// SKEW: a core older than 0.3 IGNORES `mode` (struct tolerance, ADR 0004)
/// and deletes PERMANENTLY — condition `Trash` on the `TRASH` capability
/// (which an old core never announces), NEVER on your version.
///
/// ```
/// use norte_proto::DeleteMode;
/// assert_eq!(DeleteMode::default(), DeleteMode::Trash);
/// let m: DeleteMode = serde_json::from_str(r#""permanent""#).unwrap();
/// assert_eq!(m, DeleteMode::Permanent);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMode {
    /// To the provider's trash (recoverable).
    #[default]
    Trash,
    /// Permanent deletion (an EXPLICIT choice).
    Permanent,
}
