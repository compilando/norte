//! Host volume enumeration (plan `2026-08-10-volumes.md`, design
//! `2026-08-10-volumes-design.md`).
//!
//! A volume is a property of the HOST, not of a path, so this is a plain
//! async function rather than a [`norte_vfs::Provider`] method (design §A):
//! putting `volumes()` on the trait would mean every wrapper and every
//! composing provider (`SessionProvider`, the archive provider, …)
//! implements it just to have exactly one implementor answer and the rest
//! decline.
//!
//! Two hazards shape the type and the Linux implementation in
//! [`linux`]: a mount point is bytes (`/proc/mounts` octal-escapes space,
//! tab, newline and the backslash itself, rule 1), and a space query can hang
//! forever on a dead network mount, so it runs under a deadline instead of a
//! wait (rule 2: it is blocking I/O either way, and belongs in
//! `spawn_blocking`).

use norte_proto::VPath;

#[cfg(target_os = "linux")]
mod linux;

/// One volume the host has mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// Mount point. A [`VPath`], so the bytes survive (rule 1) — this is
    /// never a `String` anywhere on the way here.
    pub mount: VPath,
    /// What the OS or the filesystem calls it, when it says. Linux's
    /// `/proc/mounts` does not carry a label, so this is `None` there; a
    /// future enhancement could read `/dev/disk/by-label`.
    pub label: Option<String>,
    /// `ext4`, `apfs`, `ntfs`, `nfs4`… as the platform spells it.
    pub fs_type: String,
    /// What kind of volume this is, so far as the platform can tell.
    pub kind: VolumeKind,
    /// `None` when the filesystem did not answer in time — never a zero
    /// standing in for "unknown".
    pub total_bytes: Option<u64>,
    /// `None` when the filesystem did not answer in time — never a zero
    /// standing in for "unknown".
    pub free_bytes: Option<u64>,
    /// Whether the mount is read-only.
    pub read_only: bool,
}

/// What kind of volume a [`Volume`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeKind {
    /// A local, non-removable disk.
    Fixed,
    /// A local disk the OS considers removable (USB, SD, …).
    Removable,
    /// A network filesystem (NFS, CIFS/SMB, sshfs, …).
    Network,
    /// A synthetic/virtual filesystem (`proc`, `tmpfs`, …) — hidden by
    /// default, see [`PSEUDO_FS`].
    Pseudo,
    /// The platform could not tell. Deliberately NOT folded into `Fixed`:
    /// guessing a kind the platform did not answer is worse than admitting
    /// it does not know (design §A).
    Unknown,
}

/// Filesystem types that are synthetic or virtual rather than something a
/// person mounted on purpose: hidden from the picker unless it asks for
/// everything (design §E). A single declared list with its own test, so
/// hiding a new type is a visible diff rather than a branch added somewhere
/// nobody will find again.
///
/// `tmpfs` is the arguable entry: `/run`, `/dev/shm` and the per-user
/// runtime directories are tmpfs and are noise, while a deliberate `tmpfs`
/// mounted somewhere meaningful is still reachable through the unfiltered
/// list.
pub const PSEUDO_FS: &[&str] = &[
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "devtmpfs",
    "devpts",
    "tmpfs",
    "ramfs",
    "overlay",
    "squashfs",
    "autofs",
    "debugfs",
    "tracefs",
    "securityfs",
    "pstore",
    "bpf",
    "configfs",
    "fusectl",
    "mqueue",
    "hugetlbfs",
    "binfmt_misc",
    "efivarfs",
    "nsfs",
];

/// `true` if `fs_type` is in [`PSEUDO_FS`].
#[must_use]
pub fn is_pseudo(fs_type: &str) -> bool {
    PSEUDO_FS.contains(&fs_type)
}

/// Failure to enumerate the host's volumes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The platform's mount table could not be read at all. A single mount
    /// that cannot be classified, or whose space query times out, is NOT an
    /// error (see [`Volume::total_bytes`]) — this variant is for when there
    /// is no mount table to read in the first place.
    #[error("reading the mount table: {0}")]
    MountTable(#[from] std::io::Error),
}

/// Every volume the host mounts. `include_pseudo` is the picker's "show
/// everything" toggle (design §E); with it `false`, anything in
/// [`PSEUDO_FS`] is left out.
///
/// # Errors
/// Only a failure to read the platform's mount table. A mount whose space
/// query fails or times out is NOT an error: it comes back with `None`
/// sizes.
#[tracing::instrument(level = "debug", skip_all, fields(include_pseudo))]
pub async fn enumerate(include_pseudo: bool) -> Result<Vec<Volume>, Error> {
    #[cfg(target_os = "linux")]
    {
        linux::enumerate(include_pseudo).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        // V4 (plan 2026-08-10-volumes.md) adds macOS and Windows behind
        // their own `cfg`s. Until then an unsupported host reports no
        // volumes rather than failing outright — there is nothing wrong to
        // report, the platform layer simply does not exist yet.
        let _ = include_pseudo;
        Ok(Vec::new())
    }
}
