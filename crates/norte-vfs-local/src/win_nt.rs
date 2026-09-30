//! One name at a time, relative to a directory handle (ADR 0158, #217).
//!
//! Win32 resolves whole paths, with DOS devices, `..` and reparse points on
//! the way; nothing here hands it one. A name is opened with `NtCreateFile`
//! relative to its parent's handle, renamed with `RootDirectory` set to a
//! handle, and deleted through its own handle. The reading side (`location`)
//! and the writing side (`confined_windows`) share this layer so they give
//! the same verdicts.

use std::fs::File;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};

use norte_vfs::NodeId;
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT, FileRenameInformation, NtCreateFile,
    NtSetInformationFile,
};
use windows_sys::Win32::Foundation::{
    HANDLE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
    FILE_ID_128, FILE_ID_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileDispositionInfo, FileDispositionInfoEx, FileIdInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

/// Every sharing flag: a reader must not lock the user out of their own
/// file, and a writer only keeps others out of a staging nobody else names.
pub(crate) const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// `NtCreateFile` of ONE name relative to `dir`, synchronous. `name` empty
/// reopens `dir` itself as a new file object.
#[allow(unsafe_code)]
pub(crate) fn nt_create(
    dir: &File,
    name: &[u16],
    access: u32,
    disposition: u32,
    options: u32,
    share: u32,
) -> std::io::Result<File> {
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
            share,
            disposition,
            options | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(from_status(status));
    }
    // SAFETY: `NtCreateFile` succeeded, so `handle` is a fresh handle that
    // nobody else owns; `File` becomes its only owner.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// The node's identity: `FILE_ID_INFO` (128-bit, covers `ReFS`), or the
/// 64-bit index where the volume does not give that (FAT).
#[allow(unsafe_code)]
pub(crate) fn file_id(file: &File) -> std::io::Result<NodeId> {
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
        return Err(std::io::Error::last_os_error());
    }
    Ok(NodeId {
        volume: u64::from(info.dwVolumeSerialNumber),
        index: (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow),
    })
}

/// Why [`open_child`] did not hand back a file.
#[derive(Debug)]
pub(crate) enum ChildError {
    /// The name is a link (a name surrogate), or became one while looked
    /// at: never crossed.
    Escapes,
    Io(std::io::Error),
}

/// Opens `name` under `dir` without crossing a link.
///
/// A name-surrogate reparse point (symlink, junction) is never crossed,
/// not even one that stays inside — the safe side of unix's "followed if
/// it does not escape". Any other reparse point (`OneDrive`, dedup, WOF)
/// redirects no name, so it is opened again through its filter and must be
/// the SAME node as the one looked at.
pub(crate) fn open_child(
    dir: &File,
    name: &[u16],
    access: u32,
    options: u32,
) -> Result<File, ChildError> {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Wdk::Storage::FileSystem::{FILE_OPEN, FILE_OPEN_REPARSE_POINT};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    };
    let looked = nt_create(
        dir,
        name,
        access | FILE_READ_ATTRIBUTES,
        FILE_OPEN,
        options | FILE_OPEN_REPARSE_POINT,
        SHARE_ALL,
    )
    .map_err(ChildError::Io)?;
    let md = looked.metadata().map_err(ChildError::Io)?;
    if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(looked);
    }
    if md.file_type().is_symlink() {
        return Err(ChildError::Escapes);
    }
    let through = nt_create(
        dir,
        name,
        access | FILE_READ_ATTRIBUTES,
        FILE_OPEN,
        options,
        SHARE_ALL,
    )
    .map_err(ChildError::Io)?;
    if solid_id(&through).map_err(ChildError::Io)? != solid_id(&looked).map_err(ChildError::Io)? {
        return Err(ChildError::Escapes);
    }
    Ok(through)
}

/// Is the open file owned by whoever this process creates files as?
///
/// `TokenOwner`, not `TokenUser`: an elevated administrator's new files are
/// owned by BUILTIN\Administrators, and a file of our own must pass. The
/// handle needs `READ_CONTROL`.
#[allow(unsafe_code)]
pub(crate) fn owned_by_us(file: &File) -> std::io::Result<bool> {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        TOKEN_OWNER, TOKEN_QUERY, TokenOwner,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the handle is alive for the call; the out-pointers are
    // locals. On success `descriptor` owns the memory `owner` points into
    // and is freed below with `LocalFree`.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(i32::MAX),
        ));
    }
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo-handle of the current process needs no closing;
    // `token` is a local out-pointer.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) };
    let result = if opened == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // `u64` words: `TOKEN_OWNER` holds a pointer.
        let mut buf = vec![0u64; 16];
        let mut needed = 0u32;
        let size = u32::try_from(buf.len() * 8).unwrap_or(u32::MAX);
        // SAFETY: `token` is open; `buf` is an owned, aligned buffer of
        // `size` bytes; `needed` is a local.
        let got = unsafe {
            GetTokenInformation(
                token,
                TokenOwner,
                buf.as_mut_ptr().cast(),
                size,
                &raw mut needed,
            )
        };
        let out = if got == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            // SAFETY: the call succeeded, so `buf` starts with a valid
            // `TOKEN_OWNER` whose SID lives inside `buf`; both SIDs are
            // valid for `EqualSid`.
            let ours = unsafe { (*buf.as_ptr().cast::<TOKEN_OWNER>()).Owner };
            Ok(unsafe { EqualSid(owner, ours) } != 0)
        };
        // SAFETY: `token` was opened above and is closed once.
        unsafe { CloseHandle(token) };
        out
    };
    // SAFETY: `descriptor` was allocated by `GetSecurityInfo` and is freed
    // once, after the last use of `owner`.
    unsafe { LocalFree(descriptor.cast()) };
    result
}

/// How many names (hard links) the open file has.
#[allow(unsafe_code)]
pub(crate) fn link_count(file: &File) -> std::io::Result<u32> {
    // SAFETY: `BY_HANDLE_FILE_INFORMATION` is plain integers, so all-zeros
    // is a valid value, and the call below overwrites it before it is read.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is alive for the call; the buffer is one
    // `BY_HANDLE_FILE_INFORMATION`.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info.nNumberOfLinks)
}

/// [`file_id`], refusing an index of 0 (some SMB servers): no identity at
/// all, and every comparison would pass.
pub(crate) fn solid_id(file: &File) -> std::io::Result<NodeId> {
    let id = file_id(file)?;
    if id.index == 0 {
        return Err(std::io::Error::other("the volume gives no file identity"));
    }
    Ok(id)
}

/// A component as UTF-16, or `None` for what the NT parser would not read
/// as ONE plain name: `\` is a separator and `:` an alternate data stream.
pub(crate) fn wide_component(bytes: &[u8]) -> Option<Vec<u16>> {
    if bytes.iter().any(|b| matches!(b, b'\\' | b':')) {
        return None;
    }
    norte_vfs::wtf8::decode_to_wide(bytes)
}

/// Renames the open `file` to `name` inside `dir`, by handle. Without
/// `replace`, an existing `name` fails (`ERROR_ALREADY_EXISTS`) and is left
/// alone. `file` must have been opened with `DELETE` access.
///
/// `NtSetInformationFile`, not `SetFileInformationByHandle`: the Win32
/// wrapper refuses a `RootDirectory` with `ERROR_INVALID_PARAMETER`
/// (measured), and the handle is the whole point.
#[allow(unsafe_code)]
pub(crate) fn rename_beneath(
    file: &File,
    dir: &File,
    name: &[u16],
    replace: bool,
) -> std::io::Result<()> {
    debug_assert!(!name.is_empty(), "an empty name is the directory itself");
    let name_bytes = name.len() * 2;
    let start = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName);
    let total = start + name_bytes + 2;
    // `u64` words: the structure holds a HANDLE and must be 8-byte aligned.
    let mut buf = vec![0u64; total.div_ceil(8)];
    let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: `buf` is zeroed, aligned for `FILE_RENAME_INFORMATION` and at
    // least `total` bytes long, which covers the header and the name
    // written after `FileName`; nothing else aliases it.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = replace;
        (*info).RootDirectory = dir.as_raw_handle();
        (*info).FileNameLength = u32::try_from(name_bytes)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidFilename))?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
    }
    let size = u32::try_from(total)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidFilename))?;
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: `file` and `dir` are alive for the call; `buf` holds a valid
    // `FILE_RENAME_INFORMATION` of `size` bytes; `status_block` is a local.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &raw mut status_block,
            buf.as_ptr().cast(),
            size,
            FileRenameInformation,
        )
    };
    if status < 0 {
        return Err(from_status(status));
    }
    Ok(())
}

/// Marks the open `file` for deletion, by handle; the name goes when this
/// handle closes, even if others are open (POSIX semantics). It must have
/// been opened with `DELETE` access.
#[allow(unsafe_code)]
pub(crate) fn delete_by_handle(file: &File) -> std::io::Result<()> {
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: `file` is alive for the call; `info` is one
    // `FILE_DISPOSITION_INFO_EX` of the size passed.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfoEx,
            std::ptr::from_ref(&info).cast(),
            size_of_u32::<FILE_DISPOSITION_INFO_EX>(),
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    // FAT, exFAT and many SMB servers know no POSIX semantics: invalid
    // function, not supported, invalid parameter. The classic disposition
    // removes the name when the last handle closes, and we hold the only
    // one with DELETE access.
    if !matches!(err.raw_os_error(), Some(1 | 50 | 87)) {
        return Err(err);
    }
    let classic = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: as above, with one `FILE_DISPOSITION_INFO`.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            std::ptr::from_ref(&classic).cast(),
            size_of_u32::<FILE_DISPOSITION_INFO>(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[allow(unsafe_code)]
fn from_status(status: i32) -> std::io::Error {
    // SAFETY: a pure translation of a status code.
    let code = unsafe { RtlNtStatusToDosError(status) };
    std::io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(i32::MAX))
}

pub(crate) fn size_of_u32<T>() -> u32 {
    u32::try_from(std::mem::size_of::<T>()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Wdk::Storage::FileSystem::{FILE_CREATE, FILE_NON_DIRECTORY_FILE, FILE_OPEN};
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_GENERIC_WRITE, SYNCHRONIZE,
    };

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn open_dir(p: &std::path::Path) -> File {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(p)
            .expect("dir handle")
    }

    /// `FILE_CREATE` is the exclusive create a staging needs: an existing
    /// name is refused, not truncated.
    #[test]
    fn create_is_exclusive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = open_dir(tmp.path());
        let access = FILE_GENERIC_WRITE | SYNCHRONIZE;
        nt_create(
            &dir,
            &w("a"),
            access,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
            0,
        )
        .expect("new");
        let again = nt_create(
            &dir,
            &w("a"),
            access,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
            0,
        );
        assert_eq!(
            again.err().map(|e| e.kind()),
            Some(std::io::ErrorKind::AlreadyExists)
        );
    }

    /// The publish step: renamed by handle into a directory handle, never
    /// over an existing name.
    #[test]
    fn rename_beneath_never_replaces() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("staging"), b"new").expect("write");
        std::fs::write(tmp.path().join("taken"), b"old").expect("write");
        let dir = open_dir(tmp.path());
        let staging = nt_create(
            &dir,
            &w("staging"),
            DELETE | FILE_GENERIC_READ | SYNCHRONIZE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
            SHARE_ALL,
        )
        .expect("open");

        let taken = rename_beneath(&staging, &dir, &w("taken"), false);
        assert_eq!(
            taken.err().map(|e| e.kind()),
            Some(std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(
            std::fs::read(tmp.path().join("taken")).expect("old"),
            b"old"
        );

        rename_beneath(&staging, &dir, &w("final"), false).expect("free name");
        drop(staging);
        assert_eq!(
            std::fs::read(tmp.path().join("final")).expect("final"),
            b"new"
        );
        assert!(!tmp.path().join("staging").exists());
    }

    /// The name goes when the marking handle closes.
    #[test]
    fn delete_by_handle_removes_the_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("x"), b"x").expect("write");
        let dir = open_dir(tmp.path());
        let f = nt_create(
            &dir,
            &w("x"),
            DELETE | SYNCHRONIZE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
            SHARE_ALL,
        )
        .expect("open");
        delete_by_handle(&f).expect("delete");
        drop(f);
        assert!(!tmp.path().join("x").exists());
    }
}
