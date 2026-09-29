//! The Recycle Bin through `IFileOperation`, refusing any delete the shell
//! would not recycle (#25, ADR 0009).
//!
//! The `trash` crate runs this operation with `FOF_NO_UI`, whose
//! `FOF_NOCONFIRMATION` answers the shell's "cannot be recycled, delete
//! permanently?" with yes. Three ways in, each closed where it can be seen:
//!
//! - a drive without a bin, or a volume set not to use it: `PreDeleteItem`
//!   arrives without `TSF_DELETE_RECYCLE_IF_POSSIBLE`, and the sink aborts;
//! - an item bigger than the bin: `PreDeleteItem` still says "recycle", so
//!   the size is checked first against the volume's `MaxCapacity`;
//! - whatever else: `PostDeleteItem` without a recycled item is reported as
//!   a failure, never as a recoverable trash.
//!
//! A refusal is `Unsupported`, which the TUI answers by re-offering a
//! permanent delete with a warning.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use norte_proto::Error;
use windows::Win32::Foundation::{
    E_ABORT, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, WIN32_ERROR,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::UI::Shell::{
    FOF_ALLOWUNDO, FOF_NO_UI, FileOperation, IFileOperation, IFileOperationProgressSink,
    IShellItem, SHCreateItemFromParsingName,
};
use windows::core::{HRESULT, PCWSTR};

/// Moves `p` to the Recycle Bin, or leaves it where it is.
///
/// `Unsupported` means the shell would have destroyed it; nothing was
/// touched.
pub(crate) fn recycle(p: &Path) -> Result<(), Error> {
    let verbatim: Vec<u16> = p.as_os_str().encode_wide().collect();
    let name = crate::shell_name::shell_name(&verbatim)?;
    let capacity = bin_capacity(&verbatim).ok_or(Error::Unsupported)?;
    if reaches(p, capacity).map_err(|e| crate::provider::map_io(&e))? {
        return Err(Error::Unsupported);
    }
    // `IFileOperation` wants a single-threaded apartment. A thread of our
    // own gives it one without depending on what the blocking pool's
    // thread was initialised as (the `trash` crate's restore initialises
    // its threads too).
    std::thread::scope(|s| s.spawn(|| recycle_in_sta(&name)).join())
        .unwrap_or(Err(Error::Internal { panic: true }))
}

/// What the sink saw.
#[derive(Default)]
struct Verdict {
    /// Aborted before the delete: the shell would not recycle.
    refused: AtomicBool,
    /// Deleted without a recycled item to show for it.
    destroyed: AtomicBool,
}

#[allow(unsafe_code)]
fn recycle_in_sta(name: &[u16]) -> Result<(), Error> {
    // SAFETY: a fresh thread, initialised once and uninitialised below on
    // the same thread, after every COM object used here is dropped (the
    // error included: it can carry an `IErrorInfo`).
    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    if init.is_err() {
        return Err(Error::Io { retryable: false });
    }
    let verdict = Arc::new(Verdict::default());
    let out = match perform(name, &verdict) {
        _ if verdict.refused.load(Ordering::SeqCst) => Err(Error::Unsupported),
        _ if verdict.destroyed.load(Ordering::SeqCst) => Err(Error::Io { retryable: false }),
        Ok(()) => Ok(()),
        Err(e) => Err(map_hresult(e.code())),
    };
    // SAFETY: pairs with the successful `CoInitializeEx` above.
    unsafe { CoUninitialize() };
    out
}

#[allow(unsafe_code)]
fn perform(name: &[u16], verdict: &Arc<Verdict>) -> windows::core::Result<()> {
    let sink: IFileOperationProgressSink = sink::RecycleOnly::new(Arc::clone(verdict)).into();
    // SAFETY: `name` is NUL-terminated and outlives the call (the item
    // copies it); every interface is used on the apartment's thread and
    // dropped before `CoUninitialize`.
    unsafe {
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
        op.SetOperationFlags(FOF_NO_UI | FOF_ALLOWUNDO)?;
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None)?;
        op.DeleteItem(&item, &sink)?;
        op.PerformOperations()?;
        // It can report success having aborted: the sink's verdict is read
        // by the caller, any other abort is a failure.
        if op.GetAnyOperationsAborted()?.as_bool() {
            return Err(E_ABORT.into());
        }
    }
    Ok(())
}

fn map_hresult(hr: HRESULT) -> Error {
    let win32 = |e: WIN32_ERROR| HRESULT::from_win32(e.0);
    if hr == win32(ERROR_FILE_NOT_FOUND) || hr == win32(ERROR_PATH_NOT_FOUND) {
        Error::NotFound
    } else if hr == win32(ERROR_ACCESS_DENIED) {
        Error::PermissionDenied
    } else {
        Error::Io { retryable: false }
    }
}

/// The bin's size limit on `path`'s volume, in bytes, or `None` if it
/// cannot be read. An item of at least this size is not recycled but
/// destroyed (measured: `MaxCapacity` 10 MiB, 10 MiB + 1 byte → gone).
#[allow(unsafe_code)]
fn bin_capacity(path: &[u16]) -> Option<u64> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};

    const ROOT: u32 = 1024;
    const VOLUME: u32 = 64;
    let path: Vec<u16> = path.iter().copied().chain(std::iter::once(0)).collect();
    let mut root = [0u16; ROOT as usize];
    let mut volume = [0u16; VOLUME as usize];
    // SAFETY: NUL-terminated input; the buffers' real lengths are passed.
    let ok = unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), ROOT) };
    if ok == 0 {
        return None;
    }
    // SAFETY: `root` was NUL-terminated by the call above; same for lengths.
    let ok =
        unsafe { GetVolumeNameForVolumeMountPointW(root.as_ptr(), volume.as_mut_ptr(), VOLUME) };
    if ok == 0 {
        return None;
    }
    // `\\?\Volume{GUID}\` → `{GUID}`.
    let volume = String::from_utf16(&volume[..volume.iter().position(|&u| u == 0)?]).ok()?;
    let guid = volume.strip_prefix(r"\\?\Volume")?.strip_suffix('\\')?;
    let key: Vec<u16> =
        format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
    let value: Vec<u16> = "MaxCapacity\0".encode_utf16().collect();
    let mut mb: u32 = 0;
    let mut size: u32 = 4; // one DWORD
    // SAFETY: NUL-terminated key and value names; `mb` is a DWORD and `size`
    // says so.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            std::ptr::from_mut(&mut mb).cast(),
            &raw mut size,
        )
    };
    (status == 0).then(|| u64::from(mb) * 1024 * 1024)
}

/// Does `p` weigh `limit` bytes or more? A directory is summed, stopping as
/// soon as it gets there; links are not followed (they weigh nothing).
fn reaches(p: &Path, limit: u64) -> std::io::Result<bool> {
    let mut total = 0u64;
    let mut pending = vec![p.to_path_buf()];
    while let Some(next) = pending.pop() {
        let md = std::fs::symlink_metadata(&next)?;
        if md.is_dir() {
            for entry in std::fs::read_dir(&next)? {
                pending.push(entry?.path());
            }
        } else if md.is_file() {
            total = total.saturating_add(md.len());
        }
        if total >= limit {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The progress sink. Nothing here is hand-written `unsafe`: the allows are
/// for the vtables `#[implement]` generates.
#[allow(unsafe_code, non_snake_case, clippy::ref_as_ptr, clippy::inline_always)]
mod sink {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use windows::Win32::Foundation::E_ABORT;
    use windows::Win32::UI::Shell::{
        IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem,
        TSF_DELETE_RECYCLE_IF_POSSIBLE,
    };
    use windows::core::{HRESULT, PCWSTR, Ref, Result, implement};

    use super::Verdict;

    /// Lets a delete through only if the shell will recycle it, and says
    /// when it did not.
    #[implement(IFileOperationProgressSink)]
    pub(super) struct RecycleOnly {
        verdict: Arc<Verdict>,
    }

    impl RecycleOnly {
        pub(super) fn new(verdict: Arc<Verdict>) -> Self {
            RecycleOnly { verdict }
        }
    }

    impl IFileOperationProgressSink_Impl for RecycleOnly_Impl {
        fn PreDeleteItem(&self, flags: u32, _: Ref<'_, IShellItem>) -> Result<()> {
            if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 != 0 {
                return Ok(());
            }
            self.verdict.refused.store(true, Ordering::SeqCst);
            Err(E_ABORT.into())
        }

        fn PostDeleteItem(
            &self,
            _: u32,
            _: Ref<'_, IShellItem>,
            hr: HRESULT,
            recycled: Ref<'_, IShellItem>,
        ) -> Result<()> {
            if hr.is_ok() && recycled.is_null() {
                self.verdict.destroyed.store(true, Ordering::SeqCst);
            }
            Ok(())
        }

        fn StartOperations(&self) -> Result<()> {
            Ok(())
        }
        fn FinishOperations(&self, _hr: HRESULT) -> Result<()> {
            Ok(())
        }
        fn PreRenameItem(&self, _: u32, _: Ref<'_, IShellItem>, _: &PCWSTR) -> Result<()> {
            Ok(())
        }
        fn PostRenameItem(
            &self,
            _: u32,
            _: Ref<'_, IShellItem>,
            _: &PCWSTR,
            _: HRESULT,
            _: Ref<'_, IShellItem>,
        ) -> Result<()> {
            Ok(())
        }
        fn PreMoveItem(
            &self,
            _: u32,
            _: Ref<'_, IShellItem>,
            _: Ref<'_, IShellItem>,
            _: &PCWSTR,
        ) -> Result<()> {
            Ok(())
        }
        fn PostMoveItem(
            &self,
            _: u32,
            _: Ref<'_, IShellItem>,
            _: Ref<'_, IShellItem>,
            _: &PCWSTR,
            _: HRESULT,
            _: Ref<'_, IShellItem>,
        ) -> Result<()> {
            Ok(())
        }
        fn PreCopyItem(
            &self,
            _: u32,
            _: Ref<'_, IShellItem>,
            _: Ref<'_, IShellItem>,
            _: &PCWSTR,
        ) -> Result<()> {
            Ok(())
        }
        fn PostCopyItem(
            &self,
            _: u32,
            _: Ref<'_, IShellItem>,
            _: Ref<'_, IShellItem>,
            _: &PCWSTR,
            _: HRESULT,
            _: Ref<'_, IShellItem>,
        ) -> Result<()> {
            Ok(())
        }
        fn PreNewItem(&self, _: u32, _: Ref<'_, IShellItem>, _: &PCWSTR) -> Result<()> {
            Ok(())
        }
        fn PostNewItem(
            &self,
            _: u32,
            _: Ref<'_, IShellItem>,
            _: &PCWSTR,
            _: &PCWSTR,
            _: u32,
            _: HRESULT,
            _: Ref<'_, IShellItem>,
        ) -> Result<()> {
            Ok(())
        }
        fn UpdateProgress(&self, _: u32, _: u32) -> Result<()> {
            Ok(())
        }
        fn ResetTimer(&self) -> Result<()> {
            Ok(())
        }
        fn PauseTimer(&self) -> Result<()> {
            Ok(())
        }
        fn ResumeTimer(&self) -> Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::recycle;
    use norte_proto::Error;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// A `subst` drive has no Recycle Bin: the shell deletes from it
    /// permanently. Before #25 this call destroyed the file and said `Ok`.
    #[test]
    fn a_drive_without_a_bin_refuses_and_keeps_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("keep-me.txt");
        std::fs::write(&victim, b"precious").expect("write");
        let drive = SubstDrive::new(dir.path());

        let res = recycle(&drive.path("keep-me.txt"));

        assert!(matches!(res, Err(Error::Unsupported)), "got {res:?}");
        assert_eq!(std::fs::read(&victim).expect("still there"), b"precious");
    }

    /// An item the bin cannot hold must be caught BEFORE the shell sees it:
    /// `PreDeleteItem` says "recycle" for it and the shell then destroys it
    /// (measured with `MaxCapacity` lowered to 10 MiB). Lowering it here
    /// would touch the user's registry, so the two halves are pinned apart:
    /// the limit is readable, and the weighing counts a tree.
    #[test]
    fn the_bin_limit_is_read_and_a_tree_is_weighed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let capacity = super::bin_capacity(&verbatim(dir.path())).expect("this volume has a bin");
        assert!(capacity > 0);

        let sub = dir.path().join("a").join("b");
        std::fs::create_dir_all(&sub).expect("mkdir");
        std::fs::write(dir.path().join("a").join("x"), [0u8; 600]).expect("write");
        std::fs::write(sub.join("y"), [0u8; 600]).expect("write");
        assert!(!super::reaches(dir.path(), 1201).expect("weighs"));
        assert!(super::reaches(dir.path(), 1200).expect("weighs"));
        assert!(super::reaches(&sub.join("y"), 600).expect("a file too"));
    }

    /// Names Win32 would read as another object never reach the shell, even
    /// with that other object right beside them.
    #[test]
    fn a_name_win32_would_rewrite_keeps_both_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = PathBuf::from(format!(r"\\?\{}", dir.path().display()));
        std::fs::write(base.join("foo"), b"decoy").expect("decoy");
        std::fs::write(base.join("foo."), b"victim").expect("victim");

        let res = recycle(&base.join("foo."));

        assert!(matches!(res, Err(Error::Unsupported)), "got {res:?}");
        assert_eq!(std::fs::read(base.join("foo")).expect("decoy"), b"decoy");
        assert_eq!(std::fs::read(base.join("foo.")).expect("victim"), b"victim");
    }

    /// The ordinary case still recycles, and the item is in the bin.
    #[test]
    fn a_fixed_drive_recycles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stem = format!("norte-recycle-test-{}", std::process::id());
        let victim = dir.path().join(format!("{stem}.txt"));
        std::fs::write(&victim, b"x").expect("write");

        recycle(&victim).expect("recycled");

        assert!(!victim.exists());
        // By stem: the bin's display name hides a known extension when
        // Explorer is set to.
        let ours: Vec<_> = trash::os_limited::list()
            .expect("bin listable")
            .into_iter()
            .filter(|i| i.name.to_string_lossy().starts_with(stem.as_str()))
            .collect();
        assert_eq!(ours.len(), 1, "it is in the bin, not destroyed");
        let _ = trash::os_limited::purge_all(ours);
    }

    fn verbatim(p: &Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        p.as_os_str().encode_wide().collect()
    }

    /// A `subst` of a directory, removed on drop.
    struct SubstDrive(char);

    impl SubstDrive {
        fn new(target: &Path) -> Self {
            for letter in ('M'..='Z').rev() {
                let ok = Command::new("subst")
                    .arg(format!("{letter}:"))
                    .arg(target)
                    .status()
                    .is_ok_and(|s| s.success());
                if ok {
                    return SubstDrive(letter);
                }
            }
            panic!("no free drive letter for subst");
        }

        fn path(&self, name: &str) -> PathBuf {
            PathBuf::from(format!("{}:\\{name}", self.0))
        }
    }

    impl Drop for SubstDrive {
        fn drop(&mut self) {
            let _ = Command::new("subst")
                .arg(format!("{}:", self.0))
                .arg("/d")
                .status();
        }
    }
}
