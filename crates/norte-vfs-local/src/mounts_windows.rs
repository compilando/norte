//! Raw `GetLogicalDrives`/`GetDriveTypeW`/`GetVolumeInformationW`/
//! `GetDiskFreeSpaceExW` FFI for Windows (2026-08-10-volumes.md task V4).
//!
//! ALL `unsafe` for Windows volume enumeration is confined to this module
//! (hard rule 5: only `norte-vfs-local` may use `unsafe`) — `norte-core::
//! volumes` is not allowed to, so it calls the safe surface below and owns
//! the policy (the detached-thread deadline per drive, kind classification).
//! No `Provider` involvement: this is a raw platform primitive, the same
//! role `native_path`'s WTF-8 boundary plays for path segments.
//!
//! **Unverified here.** This file only compiles under `cfg(windows)`; the
//! gate is one Linux machine and GitHub CI is off (design §F), so nothing in
//! this crate's test suite ever runs this code. It has been checked with
//! `cargo check --target x86_64-pc-windows-msvc` (type-correct against the
//! `windows-sys` crate's bindings), never against a real Win32 host.
//!
//! Watch the drive-prefix trap this repository has hit before (`native_path`
//! §`os_root_base`): a bare `C:` is NOT the same path as `C:\` — a
//! drive-relative path, not the drive's root — so every root this module
//! builds is `"<letter>:\"`, never the two-character form alone. None of
//! these calls need the `\\?\` long-path prefix: they take a drive ROOT
//! (three characters), never a path long enough to hit the 260-character
//! limit that prefix exists for.

#![cfg(windows)]

use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
// `FILE_READ_ONLY_VOLUME` lives in `SystemServices`, not next to the
// filesystem functions that use it.
use windows_sys::Win32::System::SystemServices::FILE_READ_ONLY_VOLUME;
// The `DRIVE_*` constants live in `WindowsProgramming`, not next to the
// functions that consume them (`GetDriveTypeW` is in `Storage::FileSystem`).
use windows_sys::Win32::System::WindowsProgramming::{
    DRIVE_CDROM, DRIVE_FIXED, DRIVE_NO_ROOT_DIR, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
};

/// What `GetDriveTypeW` answered for a drive letter — the ONLY source this
/// module trusts for removable/fixed/network (design §F: never guessed from
/// the filesystem name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawDriveType {
    /// `DRIVE_REMOVABLE`.
    Removable,
    /// `DRIVE_FIXED`.
    Fixed,
    /// `DRIVE_REMOTE`.
    Remote,
    /// `DRIVE_CDROM`.
    CdRom,
    /// `DRIVE_RAMDISK`.
    RamDisk,
    /// `DRIVE_NO_ROOT_DIR`: the letter is reported in use by
    /// [`logical_drive_letters`] but has no accessible root (an empty
    /// optical drive, a phantom letter) — the caller's job to decide whether
    /// that means "skip it".
    NoRootDir,
    /// `DRIVE_UNKNOWN`, or any code this table does not recognize.
    Unknown,
}

/// The drive letters [`GetLogicalDrives`] reports in use, `b'A'..=b'Z'`.
#[must_use]
#[allow(unsafe_code)]
pub fn logical_drive_letters() -> Vec<u8> {
    // SAFETY: `GetLogicalDrives` takes no arguments and cannot fail per the
    // Win32 docs — a `0` return means no drives are in use, not an error.
    let mask = unsafe { GetLogicalDrives() };
    (0..26_u32)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| b'A' + u8::try_from(i).unwrap_or(0))
        .collect()
}

/// A NUL-terminated UTF-16 `"<letter>:\"` root path, the shape every call in
/// this module wants (the drive-prefix trap this module's rustdoc warns
/// about: `"<letter>:"` alone is drive-RELATIVE, not the root).
fn root_path_wide(letter: u8) -> Vec<u16> {
    [letter, b':', b'\\']
        .iter()
        .map(|&b| u16::from(b))
        .chain(std::iter::once(0))
        .collect()
}

/// `GetDriveTypeW` for the drive `letter` names.
#[must_use]
#[allow(unsafe_code)]
pub fn drive_type(letter: u8) -> RawDriveType {
    let root = root_path_wide(letter);
    // SAFETY: `root` is NUL-terminated (built above) and outlives the call
    // (local variable, not dropped until this function returns);
    // `GetDriveTypeW` reads it and returns a plain integer code — no output
    // buffer for this function to misuse.
    let code = unsafe { GetDriveTypeW(root.as_ptr()) };
    match code {
        DRIVE_REMOVABLE => RawDriveType::Removable,
        DRIVE_FIXED => RawDriveType::Fixed,
        DRIVE_REMOTE => RawDriveType::Remote,
        DRIVE_CDROM => RawDriveType::CdRom,
        DRIVE_RAMDISK => RawDriveType::RamDisk,
        DRIVE_NO_ROOT_DIR => RawDriveType::NoRootDir,
        _ => RawDriveType::Unknown,
    }
}

/// What [`volume_info`] read off `GetVolumeInformationW`.
#[derive(Debug, Clone)]
pub struct VolumeInfo {
    /// The volume label, raw UTF-16 code units — the caller
    /// (`norte-core::volumes::windows`) encodes it to WTF-8 via
    /// [`norte_vfs::wtf8::os_to_bytes`], the same convention `native_path`
    /// trusts for Windows path segments.
    pub label: Vec<u16>,
    /// The filesystem name (`NTFS`, `FAT32`, `exFAT`, …).
    pub fs_type: String,
    /// `lpFileSystemFlags`, raw: the caller reads `FILE_READ_ONLY_VOLUME`
    /// out of it for [`crate::mounts_windows`]'s share of `Volume::
    /// read_only` — the rest is not this module's business to interpret.
    pub flags: u32,
}

/// The `FILE_READ_ONLY_VOLUME` bit of [`VolumeInfo::flags`].
pub const READ_ONLY_VOLUME: u32 = FILE_READ_ONLY_VOLUME;

/// `GetVolumeInformationW` for the drive `letter` names. `None` if the call
/// fails — no media, access denied, unformatted, a share that answered but
/// refused — the caller treats that exactly like Linux's "the query did not
/// answer": the volume still shows up, just without this information.
///
/// This call, like [`disk_free_space`], can block on a stalled network
/// drive — unlike macOS's single `getmntinfo(MNT_NOWAIT)` for the whole
/// table, Windows has no non-blocking variant of this per-drive call, so
/// `norte-core::volumes::windows` wraps EACH call to this function in the
/// shared detached-thread deadline (design's hazard for Linux's per-mount
/// `statvfs`, same shape here).
#[must_use]
#[allow(unsafe_code)]
pub fn volume_info(letter: u8) -> Option<VolumeInfo> {
    let root = root_path_wide(letter);
    // 128 WCHARs: real on-disk label maxima are far smaller (NTFS 32,
    // exFAT 15, FAT32 11), so this is headroom, not a tight fit — no known
    // filesystem driver truncates into it. `wide_c_str_slice` below reports
    // whatever the kernel wrote up to the first NUL; it has no way to tell
    // "genuinely short label" from "silently truncated" if a future,
    // unknown filesystem ever DID need more than 128 units, which is why
    // the headroom matters more than a hard size check here would.
    let mut label = [0u16; 128];
    let mut fs_name = [0u16; 32];
    let mut flags: u32 = 0;
    // SAFETY: `label`/`fs_name` are stack arrays whose length in WCHARs
    // (Win32's `*Size` parameters for the `…W` variants count UTF-16 code
    // units, not bytes) is passed as the matching size parameter, so the
    // call cannot write past either buffer; `flags` is a single `u32`
    // out-parameter; `root` is NUL-terminated and outlives the call. The two
    // `null_mut()` arguments ask the API to skip the serial number / max
    // component length — Win32 accepts null there for "not interested".
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            label.as_mut_ptr(),
            u32::try_from(label.len()).unwrap_or(0),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut flags,
            fs_name.as_mut_ptr(),
            u32::try_from(fs_name.len()).unwrap_or(0),
        )
    };
    if ok == 0 {
        return None;
    }
    Some(VolumeInfo {
        label: wide_c_str_slice(&label).to_vec(),
        fs_type: String::from_utf16_lossy(wide_c_str_slice(&fs_name)),
        flags,
    })
}

/// `GetDiskFreeSpaceExW`: `(total, free)` bytes, where `free` is
/// `lpFreeBytesAvailable` — the quota available to THIS caller, not
/// `lpTotalNumberOfFreeBytes` — the same per-caller-quota distinction
/// Linux's `f_bavail` (vs `f_bfree`) makes in `linux::statvfs_native`.
/// `None` on failure. See [`volume_info`]'s rustdoc for why the caller wraps
/// this in a deadline.
#[must_use]
#[allow(unsafe_code)]
pub fn disk_free_space(letter: u8) -> Option<(u64, u64)> {
    let root = root_path_wide(letter);
    let mut free_available: u64 = 0;
    let mut total: u64 = 0;
    // SAFETY: three `u64` out-parameters, all provided (no null skipping);
    // `root` is NUL-terminated and outlives the call.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            root.as_ptr(),
            &raw mut free_available,
            &raw mut total,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        None
    } else {
        Some((total, free_available))
    }
}

/// The UTF-16 slice up to (not including) the first NUL, or the whole slice
/// if no NUL appears (defensive; every call site above zero-inits its buffer
/// first, so this only matters if the buffer was entirely filled).
fn wide_c_str_slice(buf: &[u16]) -> &[u16] {
    buf.iter().position(|&c| c == 0).map_or(buf, |i| &buf[..i])
}
