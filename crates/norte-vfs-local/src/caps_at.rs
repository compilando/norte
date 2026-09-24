//! Capabilities of ONE location, not of the whole backend (ADR 0054, #153/#145).
//!
//! A machine mounts ext4, exFAT, SMB and a `+F` all at once; `LocalProvider`
//! serves all of that behind a single `file://`, so asking the provider —
//! which is what `capabilities()` used to do — answers for `base`'s mount
//! and stays silent about whatever happens on any other. Here the
//! DIRECTORY is asked.
//!
//! **The ladder is read-only first, and not out of tidiness.**
//! `capabilities_at` is called on every root someone compares or syncs, and
//! the historical probe CREATES a file (`.norte-probe-…`): on a read-only
//! mount it fails and doesn't distinguish "not writable" from "doesn't
//! fold", and in someone else's directory watchers and backups see it. The
//! order is a syscall that mutates nothing → a write probe only if nothing
//! answered and the directory supports writing.
//!
//! No answer is invented: what the platform can't say comes out as `None`
//! and the caller keeps whatever the provider declares (ADR 0054 —
//! `Capabilities` can't say "I don't know", and its degradation is the
//! usual behavior).

use std::path::Path;

/// What a probe found out about ONE directory.
///
/// `case_sensitive: None` = no branch of the ladder could answer; the
/// caller keeps the provider's declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct LocationCaps {
    /// Are `Foo` and `foo` two names in this directory?
    pub(crate) case_sensitive: Option<bool>,
    /// Does this directory's folding EXPAND (`ß` → `ss`)? Only ext4/f2fs's
    /// casefold does it, and only if the directory carries the flag.
    ///
    /// `None` = couldn't find out, which is NOT "doesn't expand": the
    /// caller keeps whatever the provider declares instead of turning off
    /// a flag nobody contradicted.
    pub(crate) full_fold: Option<bool>,
}

/// Probes `dir` and answers whatever the platform can say WITHOUT writing
/// anything. BLOCKING: goes inside `spawn_blocking` (hard rule 2).
///
/// **There's no write probe here, and it's a security decision.**
/// `capabilities_at` is answered behind the READ gate (`fs.capabilities`,
/// `fs.compare`, `sync.plan`), so an actor with read permission on a
/// directory would, if this ladder wrote, cause a file to be created there
/// — without going through the write gate (rule 9) and without a journal
/// entry (rule 4). The write probe still exists, for the provider's OWN
/// root and only once (`probe_capabilities`), which is where the provider
/// already has permission by construction.
///
/// Consequence, stated rather than hidden: on a filesystem this ladder
/// doesn't recognize (tmpfs, btrfs, xfs, nfs, cifs, fuse…) the answer is
/// "I don't know", and the caller keeps whatever the provider declares.
pub(crate) fn probe_location(dir: &Path) -> LocationCaps {
    fs_probe(dir).unwrap_or_default()
}

/// Platform steps that mutate NOTHING. `None` = this platform (or this
/// filesystem) can't answer without writing.
#[cfg(target_os = "linux")]
fn fs_probe(dir: &Path) -> Option<LocationCaps> {
    linux::probe(dir)
}

/// macOS has always answered PER VOLUME and without writing (`pathconf`);
/// no Apple filesystem expands on folding.
#[cfg(target_os = "macos")]
fn fs_probe(dir: &Path) -> Option<LocationCaps> {
    Some(LocationCaps {
        case_sensitive: Some(macos::case_sensitive(dir)?),
        // No Apple filesystem expands on folding.
        full_fold: Some(false),
    })
}

#[cfg(windows)]
fn fs_probe(dir: &Path) -> Option<LocationCaps> {
    windows::case_sensitive(dir)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn fs_probe(_dir: &Path) -> Option<LocationCaps> {
    None
}

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::io::AsRawFd as _;
    use std::path::Path;

    use super::LocationCaps;

    /// `FS_CASEFOLD_FL` from `<linux/fs.h>`: the directory is in `+F`.
    const FS_CASEFOLD_FL: libc::c_uint = 0x4000_0000;

    /// `FS_IOC_GETFLAGS`, i.e. `_IOR('f', 1, long)`.
    ///
    /// The encoding lies about the size and it still has to be honored:
    /// the number carries `sizeof(long)` inside it, so on 32 bits it's
    /// `0x8004_6601` and on 64 bits `0x8008_6601` — passing the 64-bit one
    /// on a 32-bit machine returns `ENOTTY` and would silently turn off
    /// `+F` detection. What the kernel WRITES, however, is 4 bytes in both
    /// cases (`ioctl_getflags` does a `put_user` of an `unsigned int`),
    /// which is why the buffer below is a `c_uint` and not a `c_long`.
    pub(super) fn fs_ioc_getflags() -> libc::Ioctl {
        const IOC_READ: u64 = 2;
        let size = std::mem::size_of::<libc::c_long>() as u64;
        let request = (IOC_READ << 30) | (size << 16) | (u64::from(b'f') << 8) | 1;
        request as libc::Ioctl
    }

    // Magic numbers from `<linux/magic.h>`. Only the filesystems whose
    // answer is known WITHOUT writing are here; anything else falls to
    // the next step.
    const EXT4: i64 = 0xEF53;
    const F2FS: i64 = 0xF2F5_2010;
    const MSDOS: i64 = 0x4d44;
    const EXFAT: i64 = 0x2011_BAB0;

    /// Linux's answer, or `None` if this filesystem doesn't give it
    /// without writing.
    ///
    /// **The `+F` flag is NOT read alone.** `FS_IOC_GETFLAGS` is also
    /// answered by a vfat, which has no casefold and still doesn't
    /// distinguish case: reading "no `FS_CASEFOLD_FL`" as "distinguishes
    /// case" would turn every mounted FAT into an ext4 in the comparator's
    /// eyes. The flag only decides where it means something — ext4 and
    /// f2fs —, and the rest is answered by filesystem family.
    pub(super) fn probe(dir: &Path) -> Option<LocationCaps> {
        probe_from_magic(fs_type(dir)?, || directory_is_casefold(dir))
    }

    /// The decision, separated from the syscalls so each branch can be
    /// tested without a volume of that type mounted — which is the only
    /// way to test them on this machine.
    pub(super) fn probe_from_magic(
        magic: i64,
        casefold: impl FnOnce() -> Option<bool>,
    ) -> Option<LocationCaps> {
        match magic {
            EXT4 | F2FS => {
                let casefold = casefold()?;
                Some(LocationCaps {
                    case_sensitive: Some(!casefold),
                    full_fold: Some(casefold),
                })
            }
            // vfat and exFAT never distinguish case — vfat by definition,
            // exFAT by its on-disk Up-case table, which is 1:1 and doesn't
            // expand — and there's no mount option that changes it. It's
            // an answer that also holds on a read-only mount.
            MSDOS | EXFAT => Some(LocationCaps {
                case_sensitive: Some(false),
                full_fold: Some(false),
            }),
            // NTFS does NOT go here, and the two drivers are why: `ntfs3`
            // compares case-SENSITIVELY unless given `-o nocase`, and the
            // legacy `ntfs` does exactly the opposite. Two opposite
            // defaults, both overridable at mount time = the same
            // situation as cifs, and it's answered the same way: I don't
            // know.
            //
            // tmpfs, btrfs, xfs, nfs, cifs, fuse…: either it depends on
            // mount options or there's no reliable constant. The caller
            // keeps whatever the provider declares.
            _ => None,
        }
    }

    /// `statfs.f_type` of `dir`. `None` if the call fails.
    #[allow(unsafe_code)]
    fn fs_type(dir: &Path) -> Option<i64> {
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut buf = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: `c` is a NUL-terminated CString alive for the whole
        // call; `statfs` writes a complete `struct statfs` into the
        // pointer, and `buf` is exactly that, owned and aligned. Only read
        // after checking the call returned 0.
        let rc = unsafe { libc::statfs(c.as_ptr(), buf.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        // SAFETY: `statfs` returned 0, so it left `buf` initialized.
        let st = unsafe { buf.assume_init() };
        // `f_type`'s type changes with the architecture and the libc
        // (`i64` on glibc/x86_64, `u32` on some 32-bit musl), so the
        // conversion is redundant ONLY on the target compiling today.
        #[allow(clippy::useless_conversion)]
        i64::try_from(st.f_type).ok()
    }

    /// Does this DIRECTORY carry the casefold flag (`chattr +F`)?
    ///
    /// `None` = the filesystem doesn't answer this ioctl or the directory
    /// couldn't be opened. Only asked where the flag means something.
    #[allow(unsafe_code)]
    fn directory_is_casefold(dir: &Path) -> Option<bool> {
        // O_PATH won't do: the ioctl needs a real fd. O_RDONLY on a
        // directory reads nothing and doesn't mutate it.
        let file = std::fs::File::open(dir).ok()?;
        let mut flags: libc::c_uint = 0;
        // SAFETY: `file` is alive for the whole call and its fd is valid.
        // The kernel's handler (`ioctl_getflags`) does a `put_user` of an
        // `unsigned int` through this pointer — FOUR bytes, whatever the
        // number's encoding's `long` may say —, and `flags` is exactly an
        // owned, aligned `c_uint`. The return value is checked before
        // reading it.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), fs_ioc_getflags(), &raw mut flags) };
        if rc != 0 {
            return None;
        }
        Some(flags & FS_CASEFOLD_FL != 0)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    /// `pathconf(_PC_CASE_SENSITIVE)`, PER VOLUME and mutating nothing.
    #[allow(unsafe_code)]
    pub(super) fn case_sensitive(dir: &Path) -> Option<bool> {
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: `c` is a NUL-terminated CString alive for the whole
        // call; `_PC_CASE_SENSITIVE` is an ABI constant. Validated in
        // `tests/local.rs` against the real FS.
        let rc = unsafe { libc::pathconf(c.as_ptr(), libc::_PC_CASE_SENSITIVE) };
        match rc {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::path::Path;

    use super::LocationCaps;

    /// Windows has NO read-only step, and `FILE_CASE_SENSITIVE_SEARCH` is
    /// why it doesn't.
    ///
    /// That `GetVolumeInformationW` flag means "the volume's driver KNOWS
    /// how to hold case-sensitive names", not "lookups here distinguish
    /// case": NTFS has it set and Win32's object manager still folds on
    /// top of the filesystem. Reading it as the answer would turn every
    /// NTFS into a sensitive volume, turn off folding, and with it the
    /// detection of `README`/`readme` collisions across the whole
    /// platform — a regression, not an improvement, and no machine in
    /// this project compiles for Windows to see it.
    ///
    /// The per-directory answer that DOES govern resolution is
    /// `FileCaseSensitiveInformation` (`GetFileInformationByHandleEx`),
    /// and until it exists the answer is "I don't know": the provider
    /// declares its default — insensitive — and its root keeps the usual
    /// write probe.
    pub(super) fn case_sensitive(_dir: &Path) -> Option<LocationCaps> {
        None
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Every branch of the magic-number table, without a volume of that
    /// type mounted — which is the only way to test them on this machine.
    /// The casefold probe is injected: if a branch calls it when it
    /// shouldn't, it shows.
    #[test]
    fn the_magic_table_answers_what_it_says_it_answers() {
        let never = || panic!("this filesystem must not ask about the +F flag");

        // ext4/f2fs: the DIRECTORY's flag decides.
        assert_eq!(
            linux::probe_from_magic(0xEF53, || Some(true)),
            Some(LocationCaps {
                case_sensitive: Some(false),
                full_fold: Some(true),
            }),
            "ext4 with +F: doesn't distinguish case and EXPANDS"
        );
        assert_eq!(
            linux::probe_from_magic(0xEF53, || Some(false)),
            Some(LocationCaps {
                case_sensitive: Some(true),
                full_fold: Some(false),
            }),
            "ext4 without +F: distinguishes case"
        );
        assert_eq!(
            linux::probe_from_magic(0xF2F5_2010, || Some(true)),
            Some(LocationCaps {
                case_sensitive: Some(false),
                full_fold: Some(true),
            }),
            "f2fs casefold, same as ext4"
        );
        // An ioctl that doesn't answer doesn't get an invented response.
        assert_eq!(linux::probe_from_magic(0xEF53, || None), None);

        // vfat and exFAT: fixed answer, without asking about a flag they
        // don't have.
        for magic in [0x4d44, 0x2011_BAB0] {
            assert_eq!(
                linux::probe_from_magic(magic, never),
                Some(LocationCaps {
                    case_sensitive: Some(false),
                    full_fold: Some(false),
                }),
                "FAT family: doesn't distinguish case and doesn't expand ({magic:#x})"
            );
        }

        // The kernel's two NTFS drivers depend on mount options and have
        // OPPOSITE defaults from each other: they're not answered for.
        for magic in [0x5346_544e_i64, 0x7366_746E] {
            assert_eq!(
                linux::probe_from_magic(magic, never),
                None,
                "NTFS is not answered from memory ({magic:#x})"
            );
        }

        // tmpfs, btrfs, xfs, nfs, cifs, fuse: outside the table.
        for magic in [
            0x0102_1994_i64,
            0x9123_683E,
            0x5846_5342,
            0x6969,
            0xFF53_4D42,
        ] {
            assert_eq!(linux::probe_from_magic(magic, never), None);
        }
    }

    /// The ioctl's number carries `sizeof(long)` inside it, and getting it
    /// wrong returns `ENOTTY` — i.e., silently turns off `+F` detection.
    #[test]
    fn the_ioctl_number_matches_this_architecture() {
        let expected: libc::Ioctl = if std::mem::size_of::<libc::c_long>() == 8 {
            0x8008_6601_u64 as libc::Ioctl
        } else {
            0x8004_6601_u64 as libc::Ioctl
        };
        assert_eq!(linux::fs_ioc_getflags(), expected);
    }
}
