//! Types of the `norte` protocol: the JSON-RPC wire format, with no business
//! logic.
//!
//! Any change in this crate is a wire format change: it requires an updated
//! golden test, a protocol version bump and a double review
//! (hard rule of `CLAUDE.md`; procedure in spec §11).
#![forbid(unsafe_code)]

pub mod hashing;
pub mod wire;

pub mod attrs;
pub mod caps;
pub mod catalog;
pub mod entry;
pub mod error;
pub mod methods;
pub mod task;
pub mod transfer;
pub mod vpath;

pub use attrs::{
    ATTR_BYTES_MAX, ATTR_ID_MAX, ATTR_LABEL_MAX, ATTR_TEXT_MAX, ATTRS_MAX_ADVERTISED,
    ATTRS_MAX_CATALOG_SCAN, ATTRS_MAX_REQUEST, AttrCatalog, AttrHint, AttrInfo, AttrType,
    AttrValue, is_valid_attr_id, sanitize_catalog,
};
pub use caps::{Capabilities, CapabilityFlags};
pub use entry::{DIR_ANCHOR_LEN, DirAnchor, Entry, EntryKind};
pub use error::{ConflictKind, Error, RootOverlap};
pub use methods::PROTOCOL_VERSION;
pub use task::{TaskId, TaskKind, TaskProgress, TaskState};
pub use transfer::{
    ByteRange, CollisionPolicy, DeleteMode, ResumePolicy, SymlinkPolicy, VerifyPolicy,
};
pub use vpath::{
    ARCHIVE_FORMATS, ArchiveRef, Authority, Scheme, Segment, VPath, VPathError,
    scheme_archive_format,
};
