//! Does it fit at the destination? (#149)
//!
//! A copy never used to ask whether the destination had room, and free space
//! has been enumerated since item 3 of the roadmap. What was missing was the
//! question, and above all **what to do with the answer**: here it WARNS and
//! lets things proceed, it never refuses.
//!
//! The reason for not refusing is not timidity. "Does not fit" is often
//! wrong: sparse files, the filesystem's own compression, per-user quotas,
//! and a destination that reports the space of a DIFFERENT filesystem than
//! the one that will receive the bytes. Denying a copy that did fit is worse
//! than letting the human decide with the number in front of them.
//!
//! # When it stays quiet, which is half the contract
//!
//! - **The destination cannot answer.** SFTP, S3 and an archive have no
//!   concept of free space, or have one that is not reliable.
//!   [`norte_proto::methods::Volume::free_bytes`] is `Option<u64>` and absent
//!   means "did not answer in time", NEVER zero — confusing the two would
//!   turn every slow mount into a false alarm.
//! - **It is not known how much is about to move.** A directory carries no
//!   size in the listing, and adding up only what does carry one would give
//!   a total lower than the real one: warning with it would be warning too
//!   little, and not warning at all is the honest choice.

use norte_i18n::{Lang, ta_in};

/// The space warning, or `None` when there is nothing honest to say.
///
/// `total` is what is about to be written and `free` what the destination
/// says it has; either one being absent stays quiet. Fitting also stays
/// quiet: a "yes, it fits" on every copy is noise that teaches people to
/// ignore the line.
///
/// ```
/// use norte_frontend::space::warning;
/// use norte_i18n::Lang;
///
/// // Does not fit: it is said, with both numbers.
/// assert!(warning(Some(4_200_000_000), Some(1_100_000_000), Lang::En).is_some());
/// // Fits: nothing to say.
/// assert!(warning(Some(10), Some(1_000), Lang::En).is_none());
/// // The destination does not answer, or how much is moving is not known:
/// // silence, never a false alarm.
/// assert!(warning(Some(10), None, Lang::En).is_none());
/// assert!(warning(None, Some(10), Lang::En).is_none());
/// ```
#[must_use]
pub fn warning(total: Option<u64>, free: Option<u64>, lang: Lang) -> Option<String> {
    let (total, free) = (total?, free?);
    if total <= free {
        return None;
    }
    Some(ta_in(
        lang,
        "space-warning",
        &[
            ("size", &crate::human_bytes(total)),
            ("free", &crate::human_bytes(free)),
        ],
    ))
}

/// The bytes a transfer is about to write, or `None` if some item does not
/// say how much it takes up.
///
/// The other half of [`warning`]'s contract, and the one that decides whether
/// there is a question to ask. It is ALL or nothing: a directory carries no
/// size in the listing, so adding up only what does carry one would give a
/// total lower than the real one and warn too little — which is worse than
/// staying quiet, because the line that does show up reads as complete.
///
/// It lives here and not in a frontend because both ask the same question
/// when opening the same dialog, and a total computed with a different rule
/// is an alarm that shows up in one frontend and not the other (ADR 0077).
///
/// An item not in `entries` does not add up either: it is not that it takes
/// up zero, it is that it is not known.
///
/// ```
/// use norte_frontend::space::total_to_write;
/// use norte_proto::{Entry, EntryKind, VPath};
///
/// let entry = |wire: &str, kind, size| Entry {
///     attrs: std::collections::BTreeMap::new(),
///     path: VPath::parse(wire).unwrap(),
///     kind,
///     size,
///     mtime_ms: None,
/// };
/// let one = VPath::parse("file:///a").unwrap();
/// let two = VPath::parse("file:///b").unwrap();
/// let dir = VPath::parse("file:///d").unwrap();
/// let listing = [
///     entry("file:///a", EntryKind::File, Some(10)),
///     entry("file:///b", EntryKind::File, Some(32)),
///     entry("file:///d", EntryKind::Dir, None),
/// ];
///
/// assert_eq!(total_to_write(&listing, &[one.clone(), two]), Some(42));
/// // A directory does not say how much it takes up: there is NO total, not
/// // even a partial one.
/// assert_eq!(total_to_write(&listing, &[one, dir]), None);
/// ```
#[must_use]
pub fn total_to_write(entries: &[norte_proto::Entry], items: &[norte_proto::VPath]) -> Option<u64> {
    // By index when the product gets out of hand, and linear otherwise. The
    // common case is three marks over a normal listing, where building a map
    // costs more than searching; the case that matters is 512 marks — a
    // batch's cap — over a directory of a hundred thousand entries, which is
    // fifty million `VPath` comparisons with the interface frozen, because
    // this runs on the actor's thread before emitting the patch.
    if items.len().saturating_mul(entries.len()) > 100_000 {
        let index: std::collections::HashMap<&norte_proto::VPath, &norte_proto::Entry> =
            entries.iter().map(|e| (&e.path, e)).collect();
        return items
            .iter()
            .try_fold(0_u64, |total, p| bytes_of(index.get(p).copied()?, total));
    }
    items.iter().try_fold(0_u64, |total, path| {
        bytes_of(entries.iter().find(|e| &e.path == path)?, total)
    })
}

/// Adds one entry to the total, or `None` if that entry does not say how much
/// it takes up.
fn bytes_of(entry: &norte_proto::Entry, total: u64) -> Option<u64> {
    if entry.kind != norte_proto::EntryKind::File {
        return None;
    }
    total.checked_add(entry.size?)
}

/// The free space of the volume serving `path`, or `None` if none serves it
/// or the one that does did not answer.
///
/// The volume is the one with the LONGEST mount point that is a prefix of
/// path: with `/` and `/home` mounted separately, a file under `/home/u` is
/// served by `/home`, and asking `/` would give another disk's number.
///
/// Only `file://`: an `sftp://` or an `s3://` hangs off no mount of this
/// machine, and answering with the local disk's space would be answering a
/// different question.
#[must_use]
pub fn free_for(
    path: &norte_proto::VPath,
    volumes: &[norte_proto::methods::Volume],
) -> Option<u64> {
    volume_for(path, volumes)?.free_bytes
}

/// How much of `path`'s volume is USED, as `0.0..=1.0` (spec 2026-09-11 V5:
/// the window footer's space indicator). `None` when the total or the free
/// space is not known, or the scheme is not local — the same criterion as
/// [`free_for`].
#[must_use]
pub fn used_ratio_for(
    path: &norte_proto::VPath,
    volumes: &[norte_proto::methods::Volume],
) -> Option<f32> {
    let v = volume_for(path, volumes)?;
    let total = v.total_bytes.filter(|t| *t > 0)?;
    let free = v.free_bytes?.min(total);
    // Plenty of f32 precision for a bar: the ratio fits in 24 bits long
    // before a pixel would notice.
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "a ratio in [0, 1] for a two-pixel bar"
    )]
    let ratio = (1.0 - (free as f64 / total as f64)).clamp(0.0, 1.0) as f32;
    Some(ratio)
}

/// The DEEPEST volume that contains `path`, only for local paths.
fn volume_for<'a>(
    path: &norte_proto::VPath,
    volumes: &'a [norte_proto::methods::Volume],
) -> Option<&'a norte_proto::methods::Volume> {
    if path.scheme() != "file" || path.authority().is_some() {
        return None;
    }
    volumes
        .iter()
        .filter(|v| norte_proto::methods::RelPath::under(&v.mount, path).is_some())
        .max_by_key(|v| v.mount.segments().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::VPath;
    use norte_proto::methods::{Volume, VolumeKind};

    fn vol(mount: &str, free: Option<u64>) -> Volume {
        Volume {
            mount: VPath::parse(mount).expect("wire"),
            label: None,
            fs_type: "ext4".into(),
            kind: VolumeKind::Fixed,
            total_bytes: Some(1_000),
            free_bytes: free,
            read_only: false,
        }
    }

    fn entry(wire: &str, kind: norte_proto::EntryKind, size: Option<u64>) -> norte_proto::Entry {
        norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).expect("wire"),
            kind,
            size,
            mtime_ms: None,
        }
    }

    /// The three forms of "not known", which the doctest does not touch and
    /// which matter: all three must give a whole `None`, never a partial sum.
    /// A total lower than the real one warns too little, and the line that
    /// does show up reads as complete.
    #[test]
    fn what_is_unknown_does_not_add_up_halfway() {
        use norte_proto::EntryKind;
        let listing = [
            entry("file:///a", EntryKind::File, Some(10)),
            entry("file:///unknown", EntryKind::File, None),
            entry("file:///link", EntryKind::Symlink, Some(4)),
        ];
        let p = |w: &str| VPath::parse(w).expect("wire");

        assert_eq!(
            total_to_write(&listing, &[p("file:///a"), p("file:///unknown")]),
            None,
            "a file that does not say how much it takes up voids the whole total"
        );
        assert_eq!(
            total_to_write(&listing, &[p("file:///a"), p("file:///link")]),
            None,
            "neither does a symlink: what is copied is what it points to, and \
             that is not in this listing"
        );
        assert_eq!(
            total_to_write(&listing, &[p("file:///a"), p("file:///ghost")]),
            None,
            "what is not in the listing does not take up zero: it is not known"
        );
        assert_eq!(
            total_to_write(&listing, &[]),
            Some(0),
            "copying nothing does have a known size"
        );
    }

    /// And a sum that overflows does not invent anything either:
    /// `checked_add` stays quiet.
    #[test]
    fn a_sum_that_overflows_stays_quiet() {
        use norte_proto::EntryKind;
        let listing = [
            entry("file:///a", EntryKind::File, Some(u64::MAX)),
            entry("file:///b", EntryKind::File, Some(1)),
        ];
        let items = [
            VPath::parse("file:///a").expect("wire"),
            VPath::parse("file:///b").expect("wire"),
        ];
        assert_eq!(total_to_write(&listing, &items), None);
    }

    /// The most SPECIFIC mount point wins: with `/` and `/home` mounted
    /// separately, asking about `/home/u` and getting `/`'s answer would be
    /// another disk's number.
    #[test]
    fn the_longest_mount_point_wins() {
        let vols = [vol("file:///", Some(10)), vol("file:///home", Some(99))];
        let free = free_for(&VPath::parse("file:///home/u/x.txt").expect("wire"), &vols);
        assert_eq!(free, Some(99));
    }

    /// A provider that does not hang off this machine has no volume to ask,
    /// and answering with the local disk's would be answering a different
    /// question.
    #[test]
    fn a_remote_destination_has_no_space_to_check() {
        let vols = [vol("file:///", Some(10))];
        assert_eq!(
            free_for(&VPath::parse("sftp://h/home/x").expect("wire"), &vols),
            None
        );
        assert_eq!(
            free_for(&VPath::parse("file://server/x").expect("wire"), &vols),
            None,
            "a `file://` WITH an authority is not this disk either"
        );
    }

    /// And a volume that did not answer in time is told apart from a full
    /// one: `None` is not zero, and treating it as zero would be a false
    /// alarm on every slow mount.
    #[test]
    fn a_volume_that_does_not_answer_stays_quiet() {
        let vols = [vol("file:///", None)];
        let free = free_for(&VPath::parse("file:///x").expect("wire"), &vols);
        assert_eq!(free, None);
        assert!(warning(Some(1_000_000), free, Lang::En).is_none());
    }
}
