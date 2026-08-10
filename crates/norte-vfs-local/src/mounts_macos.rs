//! Raw `getfsstat`/`statfs` FFI for macOS (2026-08-10-volumes.md task V4).
//!
//! ALL `unsafe` for macOS volume enumeration is confined to this module (hard
//! rule 5: only `norte-vfs-local` may use `unsafe`) — `norte-core::volumes`
//! is not allowed to, so it calls the safe surface below and owns the policy
//! (the detached-thread deadline, kind classification, the pseudo filter). No
//! `Provider` involvement: this is a raw platform primitive, the same role
//! `native_path`'s Windows FFI plays for path segments.
//!
//! **`getfsstat`, not `getmntinfo`.** The design (§F) names `getmntinfo` as
//! the source, and functionally this IS that — same kernel data, same
//! `struct statfs` array — but `getmntinfo(3)`'s own contract hands back a
//! pointer into a buffer `libc` owns internally (Apple's Libc keeps it in a
//! process-wide static, reallocated across calls) and says nothing about
//! concurrent callers. This module's caller (`norte-core::volumes`) runs
//! each enumeration on its own detached `std::thread`
//! (`blocking_with_deadline`) and — by design, so a hung call cannot starve
//! anything — never kills a straggler past its deadline, so two overlapping
//! `host.volumes` requests (two connections, or a re-opened picker after a
//! timeout) can genuinely run this code on two threads at once. `getfsstat`
//! is the same kernel call with a CALLER-OWNED buffer instead (the two-call
//! idiom below: ask for the count, allocate, fill) — no shared mutable state
//! to race, so no cross-call synchronization is needed at all, rather than
//! adding a `Mutex` around the simpler-looking call.
//!
//! **Unverified here.** This file only compiles under `cfg(target_os =
//! "macos")`; the gate is one Linux machine and GitHub CI is off (design §F),
//! so nothing in this crate's test suite ever runs this code. It has been
//! checked with `cargo check --target x86_64-apple-darwin` (type-correct
//! against the `libc` crate's macOS bindings), never against a real kernel.

#![cfg(target_os = "macos")]

use std::ffi::CStr;
use std::os::raw::c_char;

/// The BSD `MNT_LOCAL` bit (`<sys/mount.h>`): ABSENT means a network
/// filesystem. Re-exported so `norte-core::volumes::macos` does not need its
/// own `libc` dependency just to read one flag (rule 5 keeps `libc` itself
/// out of `norte-core` entirely).
pub const MNT_LOCAL: u32 = libc::MNT_LOCAL as u32;
/// The BSD `MNT_RDONLY` bit.
pub const MNT_RDONLY: u32 = libc::MNT_RDONLY as u32;

/// One mount, straight off a `struct statfs` — bytes for the two name
/// fields, not `String` (rule 1): `f_mntonname`/`f_mntfromname` are
/// NUL-terminated C strings the kernel fills from whatever the mount command
/// passed, with no encoding contract of their own.
#[derive(Debug, Clone)]
pub struct RawMount {
    /// `f_mntonname`: the mount point, raw bytes up to the first NUL.
    pub mount: Vec<u8>,
    /// `f_mntfromname`: the source device or spec, same byte hazard as
    /// `mount` — kept for parity with Linux's `MountRecord::source` even
    /// though today's classification does not need it (macOS answers
    /// local/remote from `f_flags` directly, unlike Linux's
    /// `/sys/class/block` probe).
    pub source: Vec<u8>,
    /// `f_fstypename`: `apfs`, `hfs`, `nfs`, `smbfs`… BSD's short vfs-type
    /// name table is ASCII in practice, so — like Linux's `fs_type` field —
    /// this crosses as `String`, lossily if a future filesystem's name ever
    /// were not valid UTF-8.
    pub fs_type: String,
    /// `f_flags`, raw: [`MNT_LOCAL`]/[`MNT_RDONLY`] tell the caller
    /// network/read-only; the rest is not this module's business to
    /// interpret.
    pub flags: u32,
    /// `f_blocks * f_bsize`.
    pub total_bytes: u64,
    /// `f_bavail * f_bsize` — blocks available to an UNPRIVILEGED caller
    /// (parity with Linux's `f_bavail` choice in `linux::statvfs_native`),
    /// not `f_bfree`, which includes the root-reserved margin an ordinary
    /// copy cannot use.
    pub free_bytes: u64,
}

/// Every currently mounted filesystem, via `getfsstat(MNT_NOWAIT)` — ONE
/// syscall for the whole table, kernel-cached, returning without blocking on
/// a stalled network filesystem (per Apple's own documentation of the
/// `MNT_NOWAIT` flag: the answer may be stale, but the call does not wait for
/// one). That is macOS's answer to the same hazard Linux's per-mount
/// `statvfs` deadline exists for (design §A) — a single call for every mount
/// instead of one per mount, so there is no PER-MOUNT deadline to apply the
/// way Linux does; `norte-core::volumes::macos` still wraps this whole call
/// in the shared detached-thread deadline as defence in depth, because this
/// crate has no way to verify `MNT_NOWAIT`'s behaviour on real hardware and
/// "unverified" means exactly that.
///
/// Two calls (the standard `getfsstat` idiom, see the module rustdoc for why
/// this and not the simpler `getmntinfo`): the first with a null/zero-size
/// buffer asks the kernel how many mounts there are; the second allocates
/// exactly that much and asks the kernel to fill it. The mount table can
/// change between the two (a USB drive can appear/disappear at any time) —
/// `getfsstat` handles that by writing at most as many entries as fit and
/// returning the count it ACTUALLY wrote, which this function trusts over
/// the first call's count for how much of the buffer to read back.
///
/// # Errors
/// Either `getfsstat` call failing (returns `< 0`, `errno` set).
#[allow(unsafe_code)]
pub fn raw_mounts() -> std::io::Result<Vec<RawMount>> {
    // SAFETY: a null buffer with size 0 asks `getfsstat` only for the
    // current mount count — the kernel writes nothing (nowhere to write to),
    // so there is no buffer for this call to overrun.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let Ok(count) = usize::try_from(count) else {
        return Ok(Vec::new());
    };
    if count == 0 {
        return Ok(Vec::new());
    }

    let mut buf: Vec<libc::statfs> = Vec::with_capacity(count);
    let Ok(bufsize) = i32::try_from(count.saturating_mul(size_of::<libc::statfs>())) else {
        // A mount table too large to size in `c_int` bytes is not something
        // any real host will produce; fail closed rather than truncate the
        // request silently.
        return Err(std::io::Error::other("volumes: mount table too large"));
    };
    // SAFETY: `buf` has spare CAPACITY (not yet initialized length) for
    // `count` `statfs` structures, entirely owned by this function — no
    // process-wide static to race against a concurrent call on another
    // thread (the module rustdoc's whole reason for `getfsstat` over
    // `getmntinfo`). `bufsize` is that same capacity expressed in BYTES,
    // exactly matching `getfsstat`'s documented unit for its size parameter
    // (unlike `GetVolumeInformationW`'s WCHAR-counted buffers elsewhere in
    // this crate — different API, different unit, checked against Apple's
    // own header). The kernel writes at most `bufsize` bytes — i.e. at most
    // `count` structures — and returns the number it actually wrote, which
    // can be `<= count` (the table can only have SHRUNK between the two
    // calls under `MNT_NOWAIT`, never grown past what THIS call sized for)
    // but is trusted below via `.min(count)` regardless.
    let written = unsafe { libc::getfsstat(buf.as_mut_ptr(), bufsize, libc::MNT_NOWAIT) };
    if written < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let written = usize::try_from(written).unwrap_or(0).min(count);
    // SAFETY: the kernel call above just initialized the first `written`
    // `statfs` structures of `buf`'s allocation (`written <= count`, the
    // reserved capacity) — `set_len` only makes that already-true fact
    // visible to `Vec`, it does not itself write anything.
    unsafe {
        buf.set_len(written);
    }

    Ok(buf
        .iter()
        .map(|entry| RawMount {
            mount: cstr_bytes(entry.f_mntonname.as_ptr()),
            source: cstr_bytes(entry.f_mntfromname.as_ptr()),
            fs_type: String::from_utf8_lossy(&cstr_bytes(entry.f_fstypename.as_ptr())).into_owned(),
            flags: entry.f_flags,
            total_bytes: entry.f_blocks.saturating_mul(u64::from(entry.f_bsize)),
            free_bytes: entry.f_bavail.saturating_mul(u64::from(entry.f_bsize)),
        })
        .collect())
}

/// Bytes up to (not including) the first NUL of a NUL-terminated C string.
#[allow(unsafe_code)]
fn cstr_bytes(ptr: *const c_char) -> Vec<u8> {
    // SAFETY: every call site above passes a pointer to the first element of
    // a fixed-size `[c_char; N]` array inside a `statfs` structure this
    // function's caller just read out of its OWN, caller-allocated `buf`
    // (`raw_mounts`, entirely local — no pointer arithmetic into
    // kernel/libc-owned memory happens anywhere in this module); the kernel
    // NUL-terminates those fields by contract of `getfsstat`, so reading up
    // to the first NUL never runs past the array.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    cstr.to_bytes().to_vec()
}
