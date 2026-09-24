//! norte's VFS central contract: the `Provider` trait and its types.
//!
//! Every storage backend (local, sftp, s3, archive, memory) implements
//! this trait and passes the same contract suite (`provider_contract!`, or
//! `readonly_provider_contract!` if it declares `READ_ONLY` — ADR 0018).
//! Providers don't know about each other; composite operations live in
//! `norte-core` (spec §5).
#![forbid(unsafe_code)]

mod contract;
mod contract_ro;
pub mod deadline;
/// Conversion between `VPath` and the system's NATIVE paths.
///
/// Shape rules, not disk access: that's why they live here and not in the
/// local provider, which is the only crate with `unsafe` and which a
/// daemon-only frontend must not be dragged into (ADR 0066, #254).
pub mod native;
mod options;
mod provider;
mod sink;
pub mod trash;
pub mod wtf8;

pub use norte_proto as proto;
pub use norte_proto::{ByteRange, Capabilities, CapabilityFlags, Entry, EntryKind, Error, VPath};
pub use options::{AttrRequest, ListOptions};
pub use provider::{
    ByteStream, ConfinedRoot, EntryStream, FollowLinks, NodeId, Provider, SymlinkKind,
};
pub use sink::ByteSink;

/// Internal re-exports for [`provider_contract!`]'s expansion.
/// NOT API: it can change without notice.
#[doc(hidden)]
pub mod __private {
    pub use bytes;
    pub use futures;
    pub use norte_proto;

    /// Attr contract assertions (#108 block 2), SHARED by the two contract
    /// macros — duplicating them would let a divergence (e.g. a new
    /// `AttrType` variant in only one matcher) silently weaken a suite.
    pub mod contract_attrs {
        use norte_proto::{
            ATTR_BYTES_MAX, ATTR_TEXT_MAX, ATTRS_MAX_ADVERTISED, AttrInfo, AttrType, AttrValue,
            Entry, is_valid_attr_id,
        };

        /// Does the value's variant match the declared type?
        #[must_use]
        pub fn attr_type_matches(ty: AttrType, v: &AttrValue) -> bool {
            matches!(
                (ty, v),
                (AttrType::Uint, AttrValue::Uint(_))
                    | (AttrType::Int, AttrValue::Int(_))
                    | (AttrType::Text, AttrValue::Text(_))
                    | (AttrType::Bytes, AttrValue::Bytes(_))
                    | (AttrType::TimeMs, AttrValue::TimeMs(_))
                    | (AttrType::Bool, AttrValue::Bool(_))
            )
        }

        /// Sane catalogue: bounded, valid ids, no duplicates.
        ///
        /// # Panics
        /// If the catalogue violates any of the three conditions.
        pub fn assert_catalog_sane(catalog: &[AttrInfo]) {
            assert!(
                catalog.len() <= ATTRS_MAX_ADVERTISED,
                "catalogue over the ceiling"
            );
            let mut seen = std::collections::BTreeSet::new();
            for info in catalog {
                assert!(
                    is_valid_attr_id(&info.id),
                    "invalid id in the catalogue: {:?}",
                    info.id
                );
                assert!(seen.insert(info.id.clone()), "duplicate id: {:?}", info.id);
            }
        }

        /// Per-entry contract: only requested ids, all advertised, declared
        /// type ⟺ produced variant, Text/Bytes within the byte ceiling.
        ///
        /// # Panics
        /// If any cell of `entry.attrs` violates the contract.
        pub fn assert_attrs_contract(
            catalog: &[AttrInfo],
            requested: &crate::AttrRequest,
            entry: &Entry,
        ) {
            for (id, v) in &entry.attrs {
                assert!(
                    requested.wants(id),
                    "attr NOT requested in {:?}: {id:?}",
                    entry.path.display_lossy()
                );
                let info = catalog
                    .iter()
                    .find(|a| &a.id == id)
                    .unwrap_or_else(|| panic!("attr not advertised: {id:?}"));
                assert!(
                    attr_type_matches(info.ty, v),
                    "declared type {:?} does not match {v:?} for {id:?}",
                    info.ty
                );
                match v {
                    AttrValue::Text(s) => {
                        assert!(s.len() <= ATTR_TEXT_MAX, "Text over the ceiling: {id:?}");
                    }
                    AttrValue::Bytes(b) => {
                        assert!(b.len() <= ATTR_BYTES_MAX, "Bytes over the ceiling: {id:?}");
                    }
                    _ => {}
                }
            }
        }
    }
}

/// How a LOCATION folds names, according to what its capabilities declare.
///
/// Lives here — and not in `norte-compare`, where it came from — because
/// it's the rule that says what the [`Provider`] flags whose contract this
/// crate defines MEAN, and because three layers that can't see each other
/// ask it: the comparison engine, the core when deciding whether two paths
/// are the same node, and the window when checking whether two marks in a
/// batch would collide at the destination (#268). Three copies of three
/// lines is how they're kept apart.
///
/// `CASE_SENSITIVE` wins over nothing, and `FULL_FOLD` wins over
/// `CASE_SENSITIVE`: an ext4 with `+F` declares both and folds, which is
/// what the order says.
///
/// Asked BY LOCATION, never by provider (#215): a FAT thumb drive mounted
/// under the same `file://` as a case-sensitive `/home` gives a different
/// answer, and answering by provider is answering for the wrong place.
///
/// ```
/// use norte_encoding::FoldMode;
/// use norte_proto::{Capabilities, CapabilityFlags};
/// use norte_vfs::fold_mode_of;
///
/// let ext4 = Capabilities { flags: CapabilityFlags::CASE_SENSITIVE, max_path: None };
/// assert_eq!(fold_mode_of(ext4), FoldMode::None);
///
/// let apfs = Capabilities { flags: CapabilityFlags::empty(), max_path: None };
/// assert_eq!(fold_mode_of(apfs), FoldMode::Simple);
///
/// let ext4_f = Capabilities {
///     flags: CapabilityFlags::CASE_SENSITIVE | CapabilityFlags::FULL_FOLD,
///     max_path: None,
/// };
/// assert_eq!(fold_mode_of(ext4_f), FoldMode::Full);
/// ```
#[must_use]
pub fn fold_mode_of(caps: norte_proto::Capabilities) -> norte_encoding::FoldMode {
    use norte_proto::CapabilityFlags;
    if caps.flags.contains(CapabilityFlags::FULL_FOLD) {
        norte_encoding::FoldMode::Full
    } else if caps.flags.contains(CapabilityFlags::CASE_SENSITIVE) {
        norte_encoding::FoldMode::None
    } else {
        norte_encoding::FoldMode::Simple
    }
}
