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
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// The shared deadline, now in `norte-vfs` (#213): the providers need it too
/// —`capabilities_at` probes the filesystem of whatever path it's named—
/// and two copies that could diverge on whether the thread is DETACHED are
/// exactly the failure this guards against. The prose on why a loose thread
/// and not the tokio pool lives with the function.
pub(crate) use norte_vfs::deadline::blocking_with_deadline;

/// Converts a mount point's raw bytes to a `file://` [`VPath`], via
/// [`norte_vfs_local::vpath_from_native`] — the SAME conversion the local
/// provider uses (rule 1: a host service does not get to re-derive the local
/// scheme's segment rules a second time). Hoisted out of `linux` (task V4) so
/// `macos` shares it verbatim rather than growing its own copy: both
/// platforms hand this function raw OS bytes for the same reason (`/proc/
/// mounts`' escaped fields on Linux, `f_mntonname` on macOS), and both are
/// `cfg(unix)`, so `OsStr::from_bytes` (the unix path-from-bytes extension)
/// applies to either unchanged. Windows has no such extension — a Windows
/// mount "point" is always a bare drive root the caller can spell directly
/// (`"C:\"`), so `windows` builds its `PathBuf` a different way and calls
/// [`norte_vfs_local::vpath_from_native`] on that instead of this function.
///
/// `None` for a mount point that cannot become a `VPath` (a component that is
/// literally `.`/`..`, a NUL byte) — in practice unreachable for a real
/// kernel-reported mount, since none of those are legal path components on
/// unix either. The mount is skipped rather than failing the whole
/// enumeration: one bad entry should not take down the picker.
#[cfg(unix)]
pub(crate) fn mount_to_vpath(mount: &[u8]) -> Option<VPath> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(mount));
    match norte_vfs_local::vpath_from_native(path) {
        Ok(vpath) => Some(vpath),
        Err(err) => {
            tracing::warn!(?err, "volumes: a mount point could not become a VPath");
            None
        }
    }
}

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
    /// - **macOS** (V4, unverified here). Also `None`, by DECISION rather
    ///   than platform limitation: `getmntinfo`/`statfs` — this task's own
    ///   scope (design §F) — carry no volume name field at all; the real
    ///   answer needs `getattrlist(ATTR_CMN_NAME)` on the mount point (the
    ///   DISPLAY name Finder shows, e.g. "Macintosh HD" — a variable-length
    ///   result this task's `norte-vfs-local::mounts_macos` does not
    ///   implement), left for a follow-up rather than guessed at without a
    ///   Mac to verify it against.
    /// - **Windows** (V4, unverified here). `GetVolumeInformationW` returns
    ///   UTF-16 — code UNITS, not bytes-in-the-unix-sense, and not UTF-8
    ///   either. `norte-core::volumes::windows` encodes it as WTF-8 before
    ///   the result lands here, via [`norte_vfs::wtf8::os_to_bytes`] — the
    ///   SAME encoding CONVENTION `norte_vfs_local`'s `native_path` module
    ///   already applies to Windows path segments, now HOISTED into
    ///   `norte-vfs` rather than re-derived (`norte-proto` cannot depend on
    ///   `norte-vfs-local`, the dependency runs the other way; both
    ///   `norte-vfs-local` and `norte-core` already depend on `norte-vfs`).
    ///   WTF-8 is what lets an arbitrary UTF-16 string — including an
    ///   unpaired surrogate, which a FAT/NTFS label field can legally
    ///   contain — survive losslessly as `Vec<u8>`. A naive
    ///   `String::from_utf16_lossy` would silently replace such a surrogate
    ///   with `U+FFFD` before rule 1 ever gets a say, which is exactly the
    ///   bug a from-scratch reimplementation risked reintroducing if it
    ///   drifted from the reference — the hoist means there is no second
    ///   copy to drift.
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
///
/// `devfs`/`fdesc` are macOS's own additions (task V4): every macOS boot
/// mounts both (`/dev`, `/dev/fd`), and they are exactly the kind of noise
/// `devtmpfs`/`devpts` are on Linux — added here rather than in `macos.rs`
/// so the "one declared list, one visible diff" property this comment
/// promises stays true across platforms, not just within Linux.
pub const PSEUDO_FS: &[&str] = &[
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "devtmpfs",
    "devpts",
    "devfs",
    "fdesc",
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
    // The FUSE mounts the DESKTOP mounts in `/run/user/<uid>`, not a person:
    // Flatpak's document portal and the gvfs bridge. They answer 0 bytes and
    // used to show up in the places bar as "0B free". A FUSE that IS mounted
    // on purpose (`fuse.sshfs`, `fuse.rclone`) is not here.
    "fuse.portal",
    "fuse.gvfsd-fuse",
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
    #[cfg(target_os = "macos")]
    {
        macos::enumerate(include_pseudo).await
    }
    #[cfg(windows)]
    {
        windows::enumerate(include_pseudo).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        // Design §F / task V4: Linux, macOS and Windows are the three
        // platforms this plan builds. A host that is none of those (a BSD, a
        // niche target) reports no volumes rather than failing outright —
        // there is nothing wrong to report, the platform layer simply does
        // not exist for it.
        let _ = include_pseudo;
        Ok(Vec::new())
    }
}
