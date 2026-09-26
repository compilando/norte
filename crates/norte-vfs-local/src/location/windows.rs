//! [`super::ConfinedRoot`]'s primitives on Windows (ADR 0158).
//!
//! Win32 has no `openat`: every `CreateFileW` resolves a whole path, with
//! DOS-device names, `..` and reparse points along the way. So nothing here
//! hands Windows a path below the root. Each component is opened with
//! `NtCreateFile` RELATIVE to the handle of its parent, one name at a time,
//! with `FILE_OPEN_REPARSE_POINT`:
//!
//! - a **name-surrogate** reparse point (symlink, junction) is never
//!   crossed, not even one that stays inside — the safe side of unix's
//!   "followed if it does not escape";
//! - any **other** reparse point (`OneDrive` placeholder, dedup, WOF
//!   compression) redirects no name, so the node is opened again through
//!   its filter and must have the SAME `FileId` as the node looked at: a
//!   link swapped in between is caught, not followed.
//!
//! A name carrying `\` or `:` is refused before the kernel sees it: the NT
//! parser would read the first as a separator and the second as an
//! alternate data stream.

use std::fs::File;
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
use std::path::Path;

use norte_proto::Segment;
use norte_vfs::NodeId;
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    NtCreateFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_CANT_ACCESS_FILE, ERROR_DIRECTORY, ERROR_FILE_NOT_FOUND,
    ERROR_NO_MORE_FILES, ERROR_PATH_NOT_FOUND, ERROR_STOPPED_ON_SYMLINK, HANDLE,
    OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FULL_DIR_INFO,
    FILE_GENERIC_READ, FILE_ID_128, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FileFullDirectoryInfo,
    FileFullDirectoryRestartInfo, FileIdInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::{LocationDirent, LocationError, LocationKind, LocationMeta};

/// A directory resolved under the root.
pub(super) type Dir = File;

/// What a directory handle needs: to be listed, traversed and looked at.
const DIR_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// Reparse tags with this bit substitute another name for this one.
const NAME_SURROGATE: u32 = 0x2000_0000;

/// The opened root.
#[derive(Debug)]
pub(super) struct Root {
    file: File,
    id: NodeId,
}

impl Root {
    /// The root itself is opened by path and FOLLOWING links, like unix's
    /// `open(O_DIRECTORY)`: which directory it is was the caller's decision
    /// (and `open_verified` checks that decision by identity).
    pub(super) fn open(dir: &Path) -> Result<Self, LocationError> {
        let file = std::fs::OpenOptions::new()
            .access_mode(DIR_ACCESS)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)
            .map_err(|e| from_io(&e))?;
        if !file.metadata().map_err(|e| from_io(&e))?.is_dir() {
            return Err(LocationError::TypeMismatch);
        }
        let id = dir_id(&file)?;
        Ok(Self { file, id })
    }

    pub(super) const fn id(&self) -> NodeId {
        self.id
    }

    pub(super) fn dir(&self, comps: &[Segment]) -> Result<Dir, LocationError> {
        let mut current = reopen(&self.file, self.id)?;
        for seg in comps {
            current = open_child(&current, seg, DIR_ACCESS, FILE_DIRECTORY_FILE)?;
        }
        Ok(current)
    }
}

/// `dir` again, as a NEW file object: a duplicated handle would share its
/// enumeration cursor with every concurrent `list`.
///
/// An empty name is resolved again, reparse data included: a directory made
/// a junction since it was looked at would reopen as its TARGET. So the
/// node must still be `expected`.
fn reopen(dir: &File, expected: NodeId) -> Result<File, LocationError> {
    let fresh = nt_open(dir, &[], DIR_ACCESS, FILE_DIRECTORY_FILE).map_err(|e| from_io(&e))?;
    if dir_id(&fresh)? != expected {
        return Err(LocationError::Escapes);
    }
    Ok(fresh)
}

/// The node's identity: `FILE_ID_INFO` (128-bit, covers `ReFS`), or the
/// 64-bit index where the volume does not give that (FAT). Never an inode
/// made up from the path; an index of 0 (some SMB servers) is no identity
/// at all, and every comparison would pass.
pub(super) fn dir_id(file: &File) -> Result<NodeId, LocationError> {
    let id = raw_id(file)?;
    if id.index == 0 {
        return Err(LocationError::Io);
    }
    Ok(id)
}

#[allow(unsafe_code)]
fn raw_id(file: &File) -> Result<NodeId, LocationError> {
    let mut info = FILE_ID_INFO {
        VolumeSerialNumber: 0,
        FileId: FILE_ID_128 {
            Identifier: [0; 16],
        },
    };
    // SAFETY: the handle is alive for the call (`file` is borrowed), and the
    // buffer is exactly one `FILE_ID_INFO` whose size is what is passed.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast(),
            size_of_u32::<FILE_ID_INFO>(),
        )
    };
    if ok != 0 {
        return Ok(NodeId {
            volume: info.VolumeSerialNumber,
            index: u128::from_le_bytes(info.FileId.Identifier),
        });
    }
    // SAFETY: `BY_HANDLE_FILE_INFORMATION` is plain integers, so all-zeros
    // is a valid value, and the call below overwrites it before it is read.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: as above; the buffer is one `BY_HANDLE_FILE_INFORMATION`.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) };
    if ok == 0 {
        return Err(from_io(&std::io::Error::last_os_error()));
    }
    Ok(NodeId {
        volume: u64::from(info.dwVolumeSerialNumber),
        index: (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow),
    })
}

pub(super) fn open_read(dir: &Dir, name: &Segment) -> Result<File, LocationError> {
    open_child(dir, name, FILE_GENERIC_READ, 0)
}

pub(super) fn stat(dir: &Dir, name: Option<&Segment>) -> Result<LocationMeta, LocationError> {
    let md = match name {
        // `lstat`: the reparse point itself, whatever kind it is.
        Some(name) => {
            let wide = wide_name(name)?;
            nt_open(
                dir,
                &wide,
                FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                FILE_OPEN_REPARSE_POINT,
            )
            .and_then(|f| f.metadata())
        }
        None => dir.metadata(),
    }
    .map_err(|e| from_io(&e))?;
    let attrs = md.file_attributes();
    let kind = if md.file_type().is_symlink() {
        LocationKind::Symlink
    } else if md.is_dir() {
        LocationKind::Dir
    } else if md.is_file() {
        LocationKind::File
    } else {
        LocationKind::Other
    };
    let written = unix_time(md.last_write_time());
    // Git for Windows' `st_ctime` is the CREATION time.
    let created = unix_time(md.creation_time());
    Ok(LocationMeta {
        kind,
        size: if kind == LocationKind::File {
            md.file_size()
        } else {
            0
        },
        mtime_sec: written.0,
        mtime_nsec: written.1,
        ctime_sec: created.0,
        ctime_nsec: created.1,
        ino: 0,
        dev: 0,
        mode: mode_of(kind, attrs),
    })
}

pub(super) fn list(dir: &Dir, max: u32) -> Result<Vec<LocationDirent>, LocationError> {
    let dir = reopen(dir, dir_id(dir)?)?;
    let mut out = Vec::new();
    let mut class = FileFullDirectoryRestartInfo;
    // `u64` words: the entries must be 8-byte aligned.
    let mut buf = vec![0u64; 8 * 1024];
    loop {
        let size = u32::try_from(buf.len() * 8).unwrap_or(u32::MAX);
        #[allow(unsafe_code)]
        // SAFETY: the handle is alive for the call and `buf` is an owned,
        // aligned buffer of exactly `size` bytes the call writes into.
        let ok = unsafe {
            GetFileInformationByHandleEx(dir.as_raw_handle(), class, buf.as_mut_ptr().cast(), size)
        };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error().and_then(|c| u32::try_from(c).ok()) == Some(ERROR_NO_MORE_FILES) {
                return Ok(out);
            }
            return Err(from_io(&err));
        }
        class = FileFullDirectoryInfo;
        let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_ne_bytes()).collect();
        let mut at = 0usize;
        loop {
            let entry = bytes.get(at..).ok_or(LocationError::Io)?;
            let next = u32_at(
                entry,
                std::mem::offset_of!(FILE_FULL_DIR_INFO, NextEntryOffset),
            )?;
            let attrs = u32_at(
                entry,
                std::mem::offset_of!(FILE_FULL_DIR_INFO, FileAttributes),
            )?;
            // With a reparse point, `EaSize` carries its tag instead.
            let tag = u32_at(entry, std::mem::offset_of!(FILE_FULL_DIR_INFO, EaSize))?;
            let len = u32_at(
                entry,
                std::mem::offset_of!(FILE_FULL_DIR_INFO, FileNameLength),
            )?;
            let start = std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName);
            let raw = entry
                .get(start..start + usize::try_from(len).map_err(|_| LocationError::Io)?)
                .ok_or(LocationError::Io)?;
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|c| u16::from_ne_bytes([c[0], c[1]]))
                .collect();
            if units != [u16::from(b'.')] && units != [u16::from(b'.'), u16::from(b'.')] {
                out.push(LocationDirent {
                    name: norte_vfs::wtf8::encode_from_wide(&units),
                    kind: listed_kind(attrs, tag),
                });
                if u32::try_from(out.len()).unwrap_or(u32::MAX) >= max {
                    return Ok(out);
                }
            }
            if next == 0 {
                break;
            }
            at += usize::try_from(next).map_err(|_| LocationError::Io)?;
        }
    }
}

/// Opens `name` under `dir` without crossing a link (see the module doc).
fn open_child(
    dir: &File,
    name: &Segment,
    access: u32,
    options: u32,
) -> Result<File, LocationError> {
    let wide = wide_name(name)?;
    let looked = nt_open(
        dir,
        &wide,
        access | FILE_READ_ATTRIBUTES,
        options | FILE_OPEN_REPARSE_POINT,
    )
    .map_err(|e| from_io(&e))?;
    let md = looked.metadata().map_err(|e| from_io(&e))?;
    if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(looked);
    }
    if md.file_type().is_symlink() {
        return Err(LocationError::Escapes);
    }
    let through =
        nt_open(dir, &wide, access | FILE_READ_ATTRIBUTES, options).map_err(|e| from_io(&e))?;
    if dir_id(&through)? != dir_id(&looked)? {
        return Err(LocationError::Escapes);
    }
    Ok(through)
}

/// `NtCreateFile` of ONE name relative to `dir`, synchronous, sharing
/// everything (a reader must not lock the user out of their own file).
#[allow(unsafe_code)]
fn nt_open(dir: &File, name: &[u16], access: u32, options: u32) -> std::io::Result<File> {
    let bytes = u16::try_from(name.len() * 2)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidFilename))?;
    let object_name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: name.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of_u32::<OBJECT_ATTRIBUTES>(),
        RootDirectory: dir.as_raw_handle(),
        ObjectName: &raw const object_name,
        // What `CreateFileW` does; a per-directory case-sensitive folder
        // still decides for itself.
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer is to a local that outlives the call; `name`
    // is borrowed for it and `NtCreateFile` does not write through
    // `Buffer`. `dir` keeps `RootDirectory` open throughout.
    let status = unsafe {
        NtCreateFile(
            &raw mut handle,
            access,
            &raw const attributes,
            &raw mut status_block,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            options | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: a pure translation of a status code.
        let code = unsafe { RtlNtStatusToDosError(status) };
        return Err(std::io::Error::from_raw_os_error(
            i32::try_from(code).unwrap_or(i32::MAX),
        ));
    }
    // SAFETY: `NtCreateFile` succeeded, so `handle` is a fresh handle that
    // nobody else owns; `File` becomes its only owner.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// A component as UTF-16, refusing what the NT parser would not read as
/// ONE plain name.
fn wide_name(name: &Segment) -> Result<Vec<u16>, LocationError> {
    let bytes = name.as_bytes();
    if bytes.iter().any(|b| matches!(b, b'\\' | b':')) {
        return Err(LocationError::Escapes);
    }
    norte_vfs::wtf8::decode_to_wide(bytes).ok_or(LocationError::Io)
}

fn listed_kind(attrs: u32, tag: u32) -> LocationKind {
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 && tag & NAME_SURROGATE != 0 {
        LocationKind::Symlink
    } else if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
        LocationKind::Dir
    } else {
        LocationKind::File
    }
}

fn mode_of(kind: LocationKind, attrs: u32) -> u32 {
    match kind {
        LocationKind::Symlink => 0o120_777,
        LocationKind::Dir => 0o040_755,
        LocationKind::File if attrs & FILE_ATTRIBUTE_READONLY != 0 => 0o100_444,
        LocationKind::File => 0o100_644,
        LocationKind::Other => 0,
    }
}

/// A `FILETIME` (100 ns since 1601) as unix seconds and nanoseconds.
fn unix_time(filetime: u64) -> (i64, u32) {
    const EPOCH_DIFF: i128 = 116_444_736_000_000_000;
    let ticks = i128::from(filetime) - EPOCH_DIFF;
    let sec = i64::try_from(ticks.div_euclid(10_000_000)).unwrap_or(0);
    let nsec = u32::try_from(ticks.rem_euclid(10_000_000) * 100).unwrap_or(0);
    (sec, nsec)
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, LocationError> {
    let b = bytes.get(at..at + 4).ok_or(LocationError::Io)?;
    Ok(u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
}

fn size_of_u32<T>() -> u32 {
    u32::try_from(std::mem::size_of::<T>()).unwrap_or(u32::MAX)
}

pub(super) fn from_io(e: &std::io::Error) -> LocationError {
    let Some(code) = e.raw_os_error().and_then(|c| u32::try_from(c).ok()) else {
        return LocationError::Io;
    };
    match code {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => LocationError::NotFound,
        ERROR_ACCESS_DENIED | ERROR_CANT_ACCESS_FILE => LocationError::Denied,
        ERROR_STOPPED_ON_SYMLINK => LocationError::Escapes,
        // `STATUS_NOT_A_DIRECTORY` and `STATUS_FILE_IS_A_DIRECTORY` both
        // arrive as this one.
        ERROR_DIRECTORY => LocationError::TypeMismatch,
        _ => LocationError::Io,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_converts_to_the_unix_epoch() {
        assert_eq!(unix_time(116_444_736_000_000_000), (0, 0));
        assert_eq!(unix_time(116_444_736_000_000_000 + 15), (0, 1_500));
        assert_eq!(unix_time(116_444_735_999_999_999), (-1, 999_999_900));
    }

    #[test]
    fn only_name_surrogates_list_as_links() {
        // IO_REPARSE_TAG_MOUNT_POINT, a junction.
        assert_eq!(
            listed_kind(
                FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY,
                0xA000_0003
            ),
            LocationKind::Symlink
        );
        // IO_REPARSE_TAG_CLOUD_6, a OneDrive placeholder folder.
        assert_eq!(
            listed_kind(
                FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY,
                0x9000_601A
            ),
            LocationKind::Dir
        );
    }
}
