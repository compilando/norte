//! macOS volume enumeration (2026-08-10-volumes.md task V4, design §F).
//!
//! **Unverified here.** The gate is one Linux machine and GitHub CI is off,
//! so nothing in this crate's test suite ever runs this file — it has been
//! checked with `cargo check --target x86_64-apple-darwin` (type-correct
//! against `libc`'s macOS bindings via [`norte_vfs_local::mounts_macos`]),
//! never against a real kernel.
//!
//! All raw FFI (`getmntinfo`/`statfs`) is confined to
//! [`norte_vfs_local::mounts_macos`] (hard rule 5); this module only calls
//! its safe surface and owns policy: the pseudo filter, the kind
//! classification, and wrapping the whole enumeration in
//! [`super::blocking_with_deadline`] (see that function's rustdoc for why —
//! `getmntinfo(MNT_NOWAIT)` is documented not to block, but this crate has no
//! way to verify that on real hardware, so the same detached-thread deadline
//! V1 built for Linux's per-mount `statvfs` wraps this platform's per-table
//! call too).
//!
//! `Volume::label` stays `None` on macOS, same choice Linux makes: getting a
//! real answer needs `getattrlist(ATTR_CMN_NAME)` on the mount point (the
//! volume DISPLAY name Finder shows, e.g. "Macintosh HD" — a different,
//! variable-length-result API from anything `statfs`/`getmntinfo` return),
//! which this task's own scope (design §F: "`getmntinfo` + `statfs`") does
//! not cover and which this crate cannot verify at all without a Mac. Left
//! for a follow-up rather than guessed at.

use std::time::Duration;

use norte_vfs_local::mounts_macos::{self, MNT_LOCAL, MNT_RDONLY, RawMount};

use super::{Error, Volume, VolumeKind, is_pseudo};

/// How long the WHOLE `getmntinfo` call gets before "no answer" beats "wait"
/// — same number as Linux's PER-MOUNT deadline
/// (`linux::SPACE_QUERY_DEADLINE`), picked for the same reason: short enough
/// that a hung call does not visibly stall opening the picker, generous
/// enough that a busy-but-healthy host still answers under normal load.
/// Unlike Linux this is the ENTIRE enumeration's budget, not one mount's —
/// accepted because `MNT_NOWAIT` is documented not to wait on any single
/// mount in the first place, so this deadline is defence in depth against
/// that documentation being wrong, not the primary mechanism.
const ENUMERATE_DEADLINE: Duration = Duration::from_millis(200);

/// [`RawMount`] classified into a [`VolumeKind`], per design §A/§F: pseudo
/// filesystems first (`devfs`/`fdesc` — macOS's `/dev`, joining the same
/// declared [`super::PSEUDO_FS`] list Linux's `devtmpfs`/`devpts` live in),
/// then `MNT_LOCAL` for network. A local, non-pseudo mount classifies
/// [`VolumeKind::Unknown`] rather than a guessed [`VolumeKind::Fixed`]/
/// [`VolumeKind::Removable`]: unlike Linux's `/sys/class/block/*/removable`
/// or Windows' `GetDriveTypeW`, plain `statfs`/`getmntinfo` carries no
/// removable bit at all on macOS (real removable detection needs IOKit/
/// DiskArbitration, out of this task's scope) — design's own rule for an
/// unclear ANSWER ("Unknown stays Unknown rather than being guessed into
/// Fixed") applies even harder to a question this module never got to ask.
fn classify_kind(fs_type: &str, flags: u32) -> VolumeKind {
    if is_pseudo(fs_type) {
        VolumeKind::Pseudo
    } else if flags & MNT_LOCAL == 0 {
        VolumeKind::Network
    } else {
        VolumeKind::Unknown
    }
}

/// One [`RawMount`] into a [`Volume`], applying the pseudo filter.
/// `None` if `include_pseudo` is `false` and this mount classifies
/// [`VolumeKind::Pseudo`], or if its mount point cannot become a `VPath`
/// (design's own "skip, do not fail the whole enumeration" policy, matching
/// Linux's truncated-line handling).
fn to_volume(raw: RawMount, include_pseudo: bool) -> Option<Volume> {
    let kind = classify_kind(&raw.fs_type, raw.flags);
    if !include_pseudo && kind == VolumeKind::Pseudo {
        return None;
    }
    let mount = super::mount_to_vpath(&raw.mount)?;
    Some(Volume {
        mount,
        label: None,
        fs_type: raw.fs_type,
        kind,
        total_bytes: Some(raw.total_bytes),
        free_bytes: Some(raw.free_bytes),
        read_only: raw.flags & MNT_RDONLY != 0,
    })
}

/// The macOS implementation of [`super::enumerate`].
pub(super) async fn enumerate(include_pseudo: bool) -> Result<Vec<Volume>, Error> {
    let raw = super::blocking_with_deadline(mounts_macos::raw_mounts, ENUMERATE_DEADLINE).await;
    let raw = match raw {
        Some(Ok(raw)) => raw,
        Some(Err(err)) => return Err(Error::MountTable(err)),
        None => {
            // The deadline elapsed — see this module's rustdoc: treated as
            // defence in depth, not as a hard failure, so the picker still
            // opens (empty) rather than erroring the whole request out.
            tracing::warn!("volumes: getmntinfo did not answer within the deadline");
            Vec::new()
        }
    };
    Ok(raw
        .into_iter()
        .filter_map(|m| to_volume(m, include_pseudo))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `classify_kind` is pure (no syscall), so it can be pinned without a
    /// macOS host — but like the rest of this file, the whole module (and
    /// so this test) only COMPILES under `cfg(target_os = "macos")` (`mod
    /// macos;` in `super` is itself gated): this Linux gate never executes
    /// it, and it has only been `cargo check`ed for the Darwin target.
    ///
    /// `devfs`/`fdesc` — macOS's `/dev` and `/dev/fd` — are the two
    /// synthetic filesystem types every macOS boot mounts, joined to the
    /// shared [`super::PSEUDO_FS`] list for this platform.
    #[test]
    fn macos_pseudo_types_classify_pseudo() {
        for t in ["devfs", "fdesc"] {
            assert!(is_pseudo(t), "{t} should be hidden");
            assert_eq!(classify_kind(t, MNT_LOCAL), VolumeKind::Pseudo);
        }
    }

    /// A local, non-pseudo mount is `Unknown` (never `Fixed`/`Removable`):
    /// this module has no signal to tell them apart — see `classify_kind`'s
    /// rustdoc for why guessing would be worse than admitting that.
    #[test]
    fn a_local_non_pseudo_mount_is_unknown_not_guessed() {
        assert_eq!(classify_kind("apfs", MNT_LOCAL), VolumeKind::Unknown);
    }

    /// Absent `MNT_LOCAL`, the mount is remote.
    #[test]
    fn a_mount_without_mnt_local_is_network() {
        assert_eq!(classify_kind("nfs", 0), VolumeKind::Network);
    }
}
