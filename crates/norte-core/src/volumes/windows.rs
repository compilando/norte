//! Windows volume enumeration (2026-08-10-volumes.md task V4, design §F).
//!
//! **Unverified here.** The gate is one Linux machine and GitHub CI is off,
//! so nothing in this crate's test suite ever runs this file — it has been
//! checked with `cargo check --target x86_64-pc-windows-msvc` (type-correct
//! against `windows-sys`'s bindings via
//! [`norte_vfs_local::mounts_windows`]), never against a real Win32 host.
//!
//! All raw FFI (`GetLogicalDrives`/`GetDriveTypeW`/`GetVolumeInformationW`/
//! `GetDiskFreeSpaceExW`) is confined to [`norte_vfs_local::mounts_windows`]
//! (hard rule 5); this module only calls its safe surface and owns policy:
//! the kind classification (from [`RawDriveType`] alone — never guessed from
//! the filesystem name, design §F), and wrapping EVERY blocking Win32 call
//! per drive — `GetDriveTypeW`, `GetVolumeInformationW`,
//! `GetDiskFreeSpaceExW` — in [`super::blocking_with_deadline`].
//! `GetDriveTypeW` is easy to assume is instant (it looks like a pure
//! bitmask read next to `GetLogicalDrives`), but Microsoft's own docs say it
//! can touch the device to answer, and it is a documented source of
//! multi-second stalls on a disconnected mapped network drive — the same
//! hazard class the other two calls are wrapped for, so leaving it
//! unwrapped (an earlier draft did, caught by rust-reviewer) would have been
//! the one silent exception to this module's own stated policy. Windows has
//! no non-blocking variant of any of the three the way macOS's `MNT_NOWAIT`
//! covers the whole mount table in one call, so the PER-DRIVE granularity
//! here mirrors Linux's per-mount `statvfs` deadline exactly, not macOS's
//! per-table one — and, like Linux, "no answer" here means `RawDriveType::
//! Unknown` rather than dropping the drive from the list (design §A's "the
//! query timed out" policy applied to the kind itself, not only to sizes).
//!
//! Three sequential per-drive calls (up to three detached threads per
//! letter, up to 26 letters) is a wider worst case than macOS's ONE deadline
//! for the whole table, and this module accepts that rather than adding a
//! whole-enumeration budget on top of the per-call ones: no single dead
//! drive can block another (each gets its own thread and its own
//! [`QUERY_DEADLINE`]), so the worst case is bounded and known — three
//! calls times 26 letters times the deadline, on the order of fifteen
//! seconds if EVERY letter were a dead share, a scenario real hosts do not
//! produce — just wider than Linux's or macOS's. A documented decision, not
//! an oversight.
//!
//! No Windows drive is ever [`VolumeKind::Pseudo`] — `include_pseudo` is
//! accepted for symmetry with the other two platforms and does nothing here,
//! same as an unfiltered toggle with nothing to unfilter.
//!
//! `Volume::label`'s rustdoc has the full reasoning; the short version: a
//! FAT/NTFS label is UTF-16 that can legally contain an unpaired surrogate,
//! and [`encode_label`] preserves that losslessly as WTF-8 via
//! [`norte_vfs::wtf8::encode_from_wide`] — never `String::from_utf16_lossy`,
//! which would silently replace such a surrogate with `U+FFFD` before rule 1
//! ever gets a say. `encode_from_wide` (portable, no `cfg(windows)`) rather
//! than the simpler `OsStringExt::from_wide` + `os_to_bytes` this module
//! shipped with first: encoding-auditor review found that pair had ZERO test
//! coverage anywhere in this repository (only compiles under
//! `cfg(windows)`, and the gate is Linux with GitHub CI off) for exactly the
//! surrogate-preservation property this paragraph claims — see
//! `norte_vfs::wtf8`'s module rustdoc for the fuller account and the round-
//! trip test against the hostile-names corpus that now backs this claim.

use std::path::PathBuf;
use std::time::Duration;

use norte_vfs_local::mounts_windows::{self, READ_ONLY_VOLUME, RawDriveType};

use super::{Error, Volume, VolumeKind};

/// Same number as Linux's `linux::SPACE_QUERY_DEADLINE` and macOS's
/// `macos::ENUMERATE_DEADLINE`, same reasoning: short enough that one dead
/// share does not visibly stall opening the picker, generous enough that a
/// busy-but-healthy call still answers. Applied PER CALL (label+fs_type is
/// one call, free/total is another), like Linux's per-mount grain — a drive
/// with a hung info call and a healthy space call (or vice versa) still
/// shows what it could get, not nothing.
const QUERY_DEADLINE: Duration = Duration::from_millis(200);

/// [`RawDriveType`] into a [`VolumeKind`] — the ONLY source this module
/// trusts for the kind (design §F: `GetDriveTypeW`, never guessed from the
/// filesystem name). [`RawDriveType::NoRootDir`] has no `VolumeKind` at all:
/// [`enumerate`] skips it before this function is ever called (see there).
fn classify_kind(t: RawDriveType) -> VolumeKind {
    match t {
        RawDriveType::Removable | RawDriveType::CdRom => VolumeKind::Removable,
        RawDriveType::Fixed | RawDriveType::RamDisk => VolumeKind::Fixed,
        RawDriveType::Remote => VolumeKind::Network,
        RawDriveType::NoRootDir | RawDriveType::Unknown => VolumeKind::Unknown,
    }
}

/// UTF-16 code units → WTF-8 bytes, via [`norte_vfs::wtf8::encode_from_wide`]
/// — see the module rustdoc and `Volume::label`'s rustdoc for why this, and
/// not `String::from_utf16_lossy`, is the only lossless option. `None` for an
/// empty label (no label set — the common case for a fixed internal disk),
/// matching [`Volume::label`]'s "when the OS or filesystem does not say"
/// contract; an actually-empty-but-PRESENT label and an absent one are
/// indistinguishable at this API anyway (`GetVolumeInformationW` does not
/// tell them apart), so treating empty as "no label" is not a loss.
fn encode_label(utf16: &[u16]) -> Option<Vec<u8>> {
    if utf16.is_empty() {
        return None;
    }
    Some(norte_vfs::wtf8::encode_from_wide(utf16))
}

/// The drive root (`"C:\"`) as a `VPath`, via
/// [`norte_vfs_local::vpath_from_native`] — the SAME conversion the local
/// provider uses (rule 1). Drive letters are always ASCII, so this never
/// touches the WTF-8 boundary [`encode_label`] handles. `None` if the
/// conversion fails (in practice unreachable — a bare drive letter is always
/// a legal `VPath` prefix segment); the drive is skipped rather than failing
/// the whole enumeration, matching Linux's and macOS's "one bad entry does
/// not take down the picker" policy.
///
/// Deliberately the two-character-plus-separator form (`"C:\"`), never the
/// bare `"C:"` the drive-prefix trap this module's Cargo-neighbour
/// (`mounts_windows`) rustdoc warns about would produce — `"C:"` alone is
/// drive-RELATIVE (resolves against that drive's current directory), not the
/// root.
fn drive_root_vpath(letter: u8) -> Option<norte_proto::VPath> {
    let native = PathBuf::from(format!("{}:\\", letter as char));
    match norte_vfs_local::vpath_from_native(&native) {
        Ok(vpath) => Some(vpath),
        Err(err) => {
            tracing::warn!(?err, letter = %(letter as char), "volumes: a drive root could not become a VPath");
            None
        }
    }
}

/// The Windows implementation of [`super::enumerate`].
pub(super) async fn enumerate(include_pseudo: bool) -> Result<Vec<Volume>, Error> {
    let _ = include_pseudo; // no Windows drive is ever Pseudo — see module rustdoc.
    let mut volumes = Vec::new();
    for letter in mounts_windows::logical_drive_letters() {
        // rust-reviewer MAJOR: `GetDriveTypeW` is documented to sometimes
        // touch the device to answer (removable/CD-ROM detection) and is a
        // known source of multi-second stalls on a disconnected mapped
        // network drive — the same hazard class `volume_info`/
        // `disk_free_space` below are wrapped for, so this gets the same
        // wrapping rather than being the one call in this file exempt from
        // it. A timeout answers `Unknown`, NOT a skip: "the platform did not
        // answer in time" and "this letter has no root" are different
        // things, and only the second should drop the volume from the list.
        let drive_type = super::blocking_with_deadline(
            move || mounts_windows::drive_type(letter),
            QUERY_DEADLINE,
        )
        .await
        .unwrap_or(RawDriveType::Unknown);
        if drive_type == RawDriveType::NoRootDir {
            // No accessible root (an empty optical drive, a phantom letter)
            // — nothing to show, matching Linux's truncated-line skip.
            continue;
        }
        let kind = classify_kind(drive_type);

        let info = super::blocking_with_deadline(
            move || mounts_windows::volume_info(letter),
            QUERY_DEADLINE,
        )
        .await
        .flatten();
        let (label, fs_type, read_only) = match info {
            Some(i) => (
                encode_label(&i.label),
                i.fs_type,
                i.flags & READ_ONLY_VOLUME != 0,
            ),
            // The query did not answer, or the drive answered "no info" (no
            // media, unformatted, access denied) — design §A's "None sizes,
            // not an error" policy extends to these fields too: the volume
            // still shows up.
            None => (None, String::new(), false),
        };

        let space = super::blocking_with_deadline(
            move || mounts_windows::disk_free_space(letter),
            QUERY_DEADLINE,
        )
        .await
        .flatten();
        let (total_bytes, free_bytes) = match space {
            Some((total, free)) => (Some(total), Some(free)),
            None => (None, None),
        };

        let Some(mount) = drive_root_vpath(letter) else {
            continue;
        };
        volumes.push(Volume {
            mount,
            label,
            fs_type,
            kind,
            total_bytes,
            free_bytes,
            read_only,
        });
    }
    Ok(volumes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure policy, pinned without a Windows host — but like the rest of
    /// this file, it only COMPILES under `cfg(windows)` (`mod windows;` in
    /// `super` is itself gated): this Linux gate never executes it, and it
    /// has only been `cargo check`ed for the `x86_64-pc-windows-msvc`
    /// target.
    #[test]
    fn classify_kind_never_guesses_from_the_filesystem_name() {
        assert_eq!(
            classify_kind(RawDriveType::Removable),
            VolumeKind::Removable
        );
        assert_eq!(classify_kind(RawDriveType::CdRom), VolumeKind::Removable);
        assert_eq!(classify_kind(RawDriveType::Fixed), VolumeKind::Fixed);
        assert_eq!(classify_kind(RawDriveType::RamDisk), VolumeKind::Fixed);
        assert_eq!(classify_kind(RawDriveType::Remote), VolumeKind::Network);
        assert_eq!(classify_kind(RawDriveType::Unknown), VolumeKind::Unknown);
    }

    /// An empty label (the common "no label set" case) is `None`, not
    /// `Some(vec![])` — see `encode_label`'s rustdoc for why the two are
    /// indistinguishable at this API anyway.
    #[test]
    fn an_empty_label_is_none() {
        assert_eq!(encode_label(&[]), None);
    }

    /// A real label round-trips as WTF-8: ASCII stays ASCII, and this is the
    /// one case this test can pin without a Windows target — the unpaired-
    /// surrogate case `Volume::label`'s rustdoc argues for needs
    /// `OsStringExt::from_wide` to actually run under a Windows-flavoured
    /// `OsString`, which this Linux gate's `cfg(windows)` never compiles.
    #[test]
    fn an_ascii_label_encodes_unchanged() {
        let utf16: Vec<u16> = "USB DRIVE".encode_utf16().collect();
        assert_eq!(encode_label(&utf16).as_deref(), Some(&b"USB DRIVE"[..]));
    }
}
