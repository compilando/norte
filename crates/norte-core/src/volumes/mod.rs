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
//! `linux`: a mount point is bytes (`/proc/mounts` octal-escapes space,
//! tab, newline and the backslash itself, rule 1), and a space query can hang
//! forever on a dead network mount, so it runs under a deadline instead of a
//! wait (rule 2: it is blocking I/O either way, and belongs in
//! `spawn_blocking`). [`Volume::label`] carries the same byte hazard as the
//! mount point — see its rustdoc for the per-platform breakdown, which
//! matters starting with V4 since Linux never populates it.

use norte_proto::VPath;

#[cfg(target_os = "linux")]
mod linux;

/// One volume the host has mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// Mount point. A [`VPath`], so the bytes survive (rule 1) — this is
    /// never a `String` anywhere on the way here.
    pub mount: VPath,
    /// What the OS or the filesystem calls it, when it says — raw bytes,
    /// never a `String` (rule 1). Found by encoding-auditor while V3 was in
    /// review and deferred to task V3.5 of the volumes plan: an ext4/vfat
    /// label has exactly the same status as a mount point or a filename —
    /// no platform promises it is UTF-8 — and a `String` field would either
    /// lie (lossy) or refuse a legal label.
    ///
    /// # Per-platform encoding (read this before wiring V4)
    ///
    /// - **Linux.** `/proc/mounts` carries no label at all, so this stays
    ///   `None` here; a future `/dev/disk/by-label` reader would read a
    ///   symlink NAME — bytes with no more of an encoding contract than any
    ///   other Linux filename, decided by whatever wrote the filesystem
    ///   (`mkfs.vfat -n`, `e2label`, …).
    /// - **macOS** (V4, unverified here). `getmntinfo`/the volume-name APIs
    ///   hand back a NUL-terminated C string that HFS+/APFS usually
    ///   populate as UTF-8, but nothing enforces that at the filesystem
    ///   level — treat it the same as any other macOS path component: raw
    ///   OS bytes, not guaranteed UTF-8.
    /// - **Windows** (V4, unverified here). `GetVolumeInformationW` returns
    ///   UTF-16 — code UNITS, not bytes-in-the-unix-sense, and not UTF-8
    ///   either. V4's Windows implementation MUST encode it as WTF-8 before
    ///   the result lands here — the SAME encoding CONVENTION
    ///   `norte_vfs_local`'s (private) `native_path`/`wtf8` modules already
    ///   apply to Windows path segments, replicated here rather than
    ///   literally reused: `norte-proto` cannot depend on `norte-vfs-local`
    ///   (the dependency runs the other way) and those helpers are
    ///   `pub(crate)` to that crate today, so V4 either re-derives the
    ///   handful of lines or — better, and worth doing AS PART OF V4 rather
    ///   than assumed here — hoists them into `norte-vfs`, which both
    ///   `norte-vfs-local` and `norte-core` already depend on. WTF-8 is what
    ///   lets an arbitrary UTF-16 string — including an unpaired surrogate,
    ///   which a FAT/NTFS label field can legally contain — survive
    ///   losslessly as `Vec<u8>`. A naive `String::from_utf16_lossy` would
    ///   silently replace such a surrogate with `U+FFFD` before rule 1 ever
    ///   gets a say, which is exactly the bug a from-scratch reimplementation
    ///   risks reintroducing if it drifts from the reference.
    pub label: Option<Vec<u8>>,
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
