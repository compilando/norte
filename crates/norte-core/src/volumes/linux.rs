//! `/proc/mounts` parsing (pure) and the free-space query with a deadline.
//!
//! Design §A: a mount point is BYTES (rule 1) — `/proc/mounts` escapes space,
//! tab, newline and backslash in octal, and the unescape must produce bytes
//! and never pass through `String`. `statvfs` on a hung network mount never
//! returns, so the space query runs under a deadline instead of a wait.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

use crate::blocking::spawn_blocking;

use super::{Error, Volume, VolumeKind, is_pseudo};

/// Where the kernel publishes the mount table.
const PROC_MOUNTS: &str = "/proc/mounts";

/// Where per-block-device attributes live.
const SYS_CLASS_BLOCK: &str = "/sys/class/block";

/// How long a space query gets before the caller decides "no answer" beats
/// "wait." Picked in the low hundreds of milliseconds: short enough that one
/// dead network mount does not visibly stall opening the picker, generous
/// enough that a spinning disk or a healthy-but-slow network filesystem still
/// answers under normal load. A laptop that suspended with a share mounted
/// hits the slow path every time (design §A) — this is the number that keeps
/// that from being a hang.
const SPACE_QUERY_DEADLINE: Duration = Duration::from_millis(200);

/// One line of `/proc/mounts`, unescaped but otherwise unprocessed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MountRecord {
    /// Source device or spec: `/dev/sdb1`, `nfs-server:/export`, or a
    /// synthetic name (`tmpfs`, `overlay`). Bytes, for the same reason the
    /// mount point is: a device path is a filesystem path too.
    pub(crate) source: Vec<u8>,
    /// The mount point, unescaped. BYTES — never validated as UTF-8.
    pub(crate) mount: Vec<u8>,
    /// `ext4`, `nfs4`, `vfat`… `/proc/mounts` never escapes this field (the
    /// kernel's vocabulary of filesystem names has no space/tab/newline/`\`
    /// in it), so it goes straight to a `String`.
    pub(crate) fs_type: String,
    pub(crate) read_only: bool,
}

/// Parses one line of `/proc/mounts`: `source mount fstype options freq
/// passno`, whitespace-separated (a single ASCII space between fields —
/// that is what the kernel emits), with `source` and `mount` octal-escaped
/// for space/tab/newline/backslash.
///
/// A line with fewer than four fields comes back `None` rather than
/// panicking: `/proc` is a kernel interface and a short read mid-enumeration
/// is a truncated last line, not a corrupt system.
pub(crate) fn parse_line(line: &[u8]) -> Option<MountRecord> {
    let mut fields = line.split(|&b| b == b' ');
    let source = fields.next()?;
    let mount = fields.next()?;
    let fs_type = fields.next()?;
    let options = fields.next()?;
    // freq/passno may be absent on a truncated line; neither is used here.

    if source.is_empty() || mount.is_empty() || fs_type.is_empty() {
        return None;
    }

    let fs_type = String::from_utf8_lossy(fs_type).into_owned();
    let read_only = options.split(|&b| b == b',').any(|opt| opt == b"ro");

    Some(MountRecord {
        source: unescape_octal(source),
        mount: unescape_octal(mount),
        fs_type,
        read_only,
    })
}

/// Un-escapes the octal sequences `/proc/mounts` uses for space, tab,
/// newline and the backslash itself (`\040`, `\011`, `\012`, `\134`).
///
/// This is a single left-to-right scan over the INPUT bytes: when a
/// recognized escape is found the cursor jumps forward by four (the
/// backslash and its three octal digits) and the byte it emits is never
/// looked at again. That is what keeps `\134` (a literal backslash) from
/// being re-interpreted as the start of a new escape — the scan only ever
/// consults `field`, never `out`, so a decoded `\` cannot combine with
/// whatever follows it in `field` to look like another escape sequence. A
/// sequential find-and-replace approach (unescape `\134` first, then
/// `\040`, …) does NOT have this property: replacing `\134` with `\` and
/// only afterwards scanning for `\040` can turn an original `\134" "040"`
/// (a literal backslash byte followed by the four literal characters `0`,
/// `4`, `0`) into a newly-assembled `\040` that gets wrongly unescaped into
/// a space. A single forward scan over the original bytes cannot do that.
fn unescape_octal(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\'
            && i + 4 <= field.len()
            && let Some(byte) = decode_octal_escape(&field[i + 1..i + 4])
        {
            out.push(byte);
            i += 4;
            continue;
        }
        out.push(field[i]);
        i += 1;
    }
    out
}

/// Decodes three ASCII octal digits to the byte they spell, or `None` if
/// they are not all octal digits or the value does not fit in a byte
/// (`/proc/mounts` never emits an escape above `\377`, but a malformed
/// input should not panic on the `u8` conversion).
fn decode_octal_escape(digits: &[u8]) -> Option<u8> {
    if digits.iter().any(|d| !(b'0'..=b'7').contains(d)) {
        return None;
    }
    let value = digits
        .iter()
        .fold(0u32, |acc, d| acc * 8 + u32::from(d - b'0'));
    u8::try_from(value).ok()
}

/// `true` if `fs_type` names a network filesystem (design §A's kind rule).
fn is_network_fs_type(fs_type: &str) -> bool {
    fs_type.starts_with("nfs")
        || fs_type == "cifs"
        || fs_type.starts_with("smb")
        || fs_type == "sshfs"
        || fs_type == "fuse.sshfs"
}

/// The `/sys/class/block` basename for a `/dev/...` source — a bare device
/// name, whole-disk or partition (`sda`, `sda1`, `nvme0n1p2`). `None` for a
/// source that is not a `/dev` path at all (a network spec, an rclone mount,
/// a synthetic name): those never have a block device to ask.
fn block_device_basename(source: &[u8]) -> Option<&[u8]> {
    let rest = source.strip_prefix(b"/dev/")?;
    if rest.is_empty() || rest.contains(&b'/') {
        return None;
    }
    Some(rest)
}

/// Strips a Linux partition suffix to recover the whole-disk name, because
/// `/sys/class/block/<partition>/removable` does not exist — only the
/// whole-disk entry carries it. Verified on this machine:
/// `/sys/class/block/nvme0n1p2/removable` is absent,
/// `/sys/class/block/nvme0n1/removable` exists. Handles the `pN` scheme
/// (`nvme0n1p2` → `nvme0n1`, `mmcblk0p1` → `mmcblk0`) and the bare-digit
/// scheme (`sda1` → `sda`). `None` if `dev` has no trailing digits (already
/// a whole-disk name, or an unrecognized scheme).
fn strip_partition_suffix(dev: &[u8]) -> Option<Vec<u8>> {
    let digit_count = dev.iter().rev().take_while(|b| b.is_ascii_digit()).count();
    if digit_count == 0 {
        return None;
    }
    let head = &dev[..dev.len() - digit_count];
    let head = match head.strip_suffix(b"p") {
        // Only a partition separator if what is left still ends in a
        // digit (`nvme0n1p` → `nvme0n1`); otherwise the `p` was never a
        // separator (there is no real-world device name this would
        // wrongly strip, since a device ending in a bare letter has no
        // digits before it to satisfy this check).
        Some(before) if before.last().is_some_and(u8::is_ascii_digit) => before,
        _ => head,
    };
    Some(head.to_vec())
}

/// Reads `/sys/class/block/<dev>/removable`: `Some(true)`/`Some(false)` for
/// `1`/`0`, `None` if the file is missing or its content is not one of
/// those two bytes — an answer the caller must not guess past.
fn read_removable_file(dev: &[u8]) -> Option<bool> {
    let mut path = PathBuf::from(SYS_CLASS_BLOCK);
    path.push(OsStr::from_bytes(dev));
    path.push("removable");
    match std::fs::read_to_string(&path).ok()?.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// `removable` for the block device named by `source`, trying the exact
/// basename first and its whole-disk parent second (partitions do not carry
/// their own `removable` file).
fn read_removable_flag(dev: &[u8]) -> Option<bool> {
    read_removable_file(dev).or_else(|| read_removable_file(&strip_partition_suffix(dev)?))
}

/// The kind of a volume, per design §A: pseudo filesystems first (no device
/// to probe), then the declared network `fs_type` patterns, then whatever
/// `/sys/class/block` says about a `/dev` source's removable flag. A source
/// that is neither a pseudo/network `fs_type` nor a `/dev` path (an rclone
/// mount, a FUSE helper with its own naming) is `Fixed`: there is nothing to
/// suggest otherwise, and that is different from having ASKED and not
/// gotten an answer, which is [`VolumeKind::Unknown`].
fn classify_kind(source: &[u8], fs_type: &str) -> VolumeKind {
    if is_pseudo(fs_type) {
        return VolumeKind::Pseudo;
    }
    if is_network_fs_type(fs_type) {
        return VolumeKind::Network;
    }
    match block_device_basename(source) {
        Some(dev) => match read_removable_flag(dev) {
            Some(true) => VolumeKind::Removable,
            Some(false) => VolumeKind::Fixed,
            None => VolumeKind::Unknown,
        },
        None => VolumeKind::Fixed,
    }
}

/// A mount, classified — everything [`enumerate`] can determine without a
/// possibly-hanging space query.
struct Classified {
    mount: Vec<u8>,
    fs_type: String,
    kind: VolumeKind,
    read_only: bool,
}

/// Reads and parses `/proc/mounts` and classifies each surviving line.
/// Synchronous: the caller runs this inside [`spawn_blocking`] (rule 2 — this
/// is `std::fs::read` plus, per mount, a small `/sys` read for the removable
/// flag; neither can hang the way a network `statvfs` can, so both happen
/// here rather than under the per-mount deadline).
fn read_and_classify(include_pseudo: bool) -> Result<Vec<Classified>, Error> {
    let bytes = std::fs::read(PROC_MOUNTS)?;
    let mut out = Vec::new();
    for line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Some(rec) = parse_line(line) else {
            tracing::debug!("volumes: skipping an unparseable /proc/mounts line");
            continue;
        };
        if !include_pseudo && is_pseudo(&rec.fs_type) {
            continue;
        }
        let kind = classify_kind(&rec.source, &rec.fs_type);
        out.push(Classified {
            mount: rec.mount,
            fs_type: rec.fs_type,
            kind,
            read_only: rec.read_only,
        });
    }
    Ok(out)
}

/// `statvfs` via `rustix` — safe (no `unsafe`, rule 5: norte-core is not
/// norte-vfs-local): `rustix::fs::statvfs` accepts any `path::Arg`, including
/// a raw-bytes `&OsStr` (`OsStr::from_bytes`), so this never requires the
/// mount point to be UTF-8.
///
/// `free_bytes` is `f_bavail` (blocks available to an unprivileged caller),
/// the number that answers "how much can I actually put here" — not
/// `f_bfree`, which includes the root-reserved margin an ordinary copy can't
/// use.
fn statvfs_native(mount: &[u8]) -> std::io::Result<(u64, u64)> {
    let os = OsStr::from_bytes(mount);
    let vfs = rustix::fs::statvfs(os)?;
    let total = vfs.f_frsize.saturating_mul(vfs.f_blocks);
    let free = vfs.f_frsize.saturating_mul(vfs.f_bavail);
    Ok((total, free))
}

/// Runs `query` (blocking) with a deadline. `(None, None)` if it does not
/// answer in time OR if it errors — either way the volume still shows up in
/// the picker, just without sizes (design §A).
///
/// A thin wrapper over [`super::blocking_with_deadline`] (hoisted there in
/// task V4 so `macos`/`windows` share the exact same detached-thread
/// mechanism instead of each growing their own — see that function's
/// rustdoc for the full "why not `spawn_blocking`" reasoning, unchanged from
/// when it lived here).
async fn space_with_deadline<F>(query: F, deadline: Duration) -> (Option<u64>, Option<u64>)
where
    F: FnOnce() -> std::io::Result<(u64, u64)> + Send + 'static,
{
    match super::blocking_with_deadline(query, deadline).await {
        Some(Ok((total, free))) => (Some(total), Some(free)),
        _ => (None, None),
    }
}

/// Runs `f` on the blocking pool, folding a panic into [`Error::MountTable`]
/// (the same `JoinError` handling `norte-vfs-local`'s own `blocking` helper
/// uses).
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    spawn_blocking(f)
        .await
        .map_err(|_| Error::MountTable(std::io::Error::other("volumes: blocking task panicked")))?
}

/// The Linux implementation of [`super::enumerate`].
pub(super) async fn enumerate(include_pseudo: bool) -> Result<Vec<Volume>, Error> {
    let prepared = blocking(move || read_and_classify(include_pseudo)).await?;

    let mut volumes = Vec::with_capacity(prepared.len());
    for p in prepared {
        let for_query = p.mount.clone();
        let (total_bytes, free_bytes) =
            space_with_deadline(move || statvfs_native(&for_query), SPACE_QUERY_DEADLINE).await;
        let Some(mount) = super::mount_to_vpath(&p.mount) else {
            continue;
        };
        volumes.push(Volume {
            mount,
            label: None,
            fs_type: p.fs_type,
            kind: p.kind,
            total_bytes,
            free_bytes,
            read_only: p.read_only,
        });
    }
    Ok(volumes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/proc/mounts` escapes four characters in octal, and the unescape
    /// produces BYTES. A mount at `/media/USB de Ñico` — or at a name that is
    /// not UTF-8 at all — has to survive enumeration, the wire and the picker
    /// intact (rule 1). This is the line that decides whether it does.
    #[test]
    fn a_mount_point_with_escapes_comes_back_as_bytes() {
        let line = b"/dev/sdb1 /media/USB\\040de\\040Nico vfat rw,nosuid 0 0";
        let m = parse_line(line).expect("a well-formed line");
        assert_eq!(m.mount, b"/media/USB de Nico");
        assert_eq!(m.fs_type, "vfat");
        assert!(!m.read_only);
    }

    /// All four escapes, including the backslash itself — `\134` must not be
    /// unescaped into a character that then re-enters the unescape.
    #[test]
    fn every_octal_escape_is_understood_once() {
        let line = b"dev /a\\040b\\011c\\012d\\134e ext4 rw 0 0";
        let m = parse_line(line).unwrap();
        assert_eq!(m.mount, b"/a b\tc\nd\\e");
    }

    /// A name that is not UTF-8 is a name. It reaches `VPath` as bytes and
    /// nothing on the way turns it into a replacement character.
    #[test]
    fn a_non_utf8_mount_point_survives() {
        let line = b"/dev/sdc1 /media/\xff\xfe ext4 rw 0 0";
        let m = parse_line(line).unwrap();
        assert_eq!(m.mount, b"/media/\xff\xfe");
    }

    /// The adversarial shape `unescape_octal`'s rustdoc argues against by
    /// name: a literal backslash immediately followed by literal ASCII
    /// digits that spell out another escape code. The kernel only escapes
    /// the backslash (`\134`); a sequential find-and-replace unescaper
    /// (fix `\134` first, then look for `\040` and friends) would
    /// mis-fire here, turning the newly-written `\` plus the untouched
    /// literal `"040"` into a second, bogus space escape. A single
    /// forward scan cannot do that: the cursor has already moved past the
    /// only backslash by the time it reaches the literal digits.
    #[test]
    fn a_literal_backslash_followed_by_digits_is_not_double_decoded() {
        let line = b"dev /a\\134040b ext4 rw 0 0";
        let m = parse_line(line).unwrap();
        assert_eq!(m.mount, b"/a\x5c040b");
    }

    /// The design's own motivating example (`/media/USB de Ñico`): an octal
    /// escape (space) sitting directly next to raw multi-byte UTF-8 (`Ñ` =
    /// `C3 91`). Escaping and non-ASCII bytes are independent concerns and
    /// this proves they compose — the escape decoder does not get confused
    /// by a preceding or following multi-byte sequence, and the multi-byte
    /// sequence passes through the escape decoder untouched.
    #[test]
    fn an_octal_escape_next_to_multibyte_utf8_decodes_correctly() {
        let line = "/dev/sdb1 /media/USB\\040de\\040Ñico vfat rw,nosuid 0 0".as_bytes();
        let m = parse_line(line).unwrap();
        assert_eq!(m.mount, "/media/USB de Ñico".as_bytes());
    }

    /// `ro` in the option list is the read-only flag; `rw` is not, and
    /// neither is an option that merely CONTAINS `ro` (`errors=remount-ro`
    /// is the one that catches a naive `contains`).
    #[test]
    fn read_only_is_an_option_not_a_substring() {
        let ro = parse_line(b"/dev/sda1 /mnt ext4 ro,noatime 0 0").unwrap();
        assert!(ro.read_only);
        let rw = parse_line(b"/dev/sda1 /mnt ext4 rw,errors=remount-ro 0 0").unwrap();
        assert!(!rw.read_only);
    }

    /// A line with too few fields is skipped, not a panic and not a
    /// half-filled Volume: /proc is a kernel interface and a short read
    /// during enumeration is a truncated last line, not a corrupt system.
    #[test]
    fn a_truncated_line_is_skipped() {
        assert!(parse_line(b"/dev/sda1 /mnt").is_none());
        assert!(parse_line(b"").is_none());
    }

    /// The filter is a declared list, so adding a hidden type is a visible
    /// diff and not a new branch somewhere.
    #[test]
    fn the_filter_hides_pseudo_filesystems_and_keeps_real_ones() {
        for t in ["proc", "sysfs", "cgroup2", "tmpfs", "squashfs", "overlay"] {
            assert!(is_pseudo(t), "{t} should be hidden");
        }
        for t in [
            "ext4", "btrfs", "xfs", "vfat", "ntfs3", "apfs", "nfs4", "cifs",
        ] {
            assert!(!is_pseudo(t), "{t} is a filesystem a person mounts");
        }
    }

    /// A real `/proc/mounts` line for a `fuse.rclone` mount: a source with no
    /// `/dev` prefix and a colon in it. Not pseudo, not one of the declared
    /// network `fs_type` patterns (rclone's mount does not spell its
    /// `fs_type` as `nfs*`/`cifs`/`smb*`/`sshfs`), so it classifies `Fixed` —
    /// documented in `classify_kind`'s rustdoc, exercised here so the
    /// decision has a test and not just a comment.
    #[test]
    fn a_source_with_no_dev_prefix_classifies_fixed() {
        assert_eq!(
            classify_kind(b"drivebcds_all:", "fuse.rclone"),
            VolumeKind::Fixed
        );
    }

    /// The declared network `fs_type` patterns, taken from real `fs_type`
    /// spellings this repo's fixtures already use (`nfs4` in the corpus of
    /// providers, `cifs`/`smb3` as the common real-world spellings).
    #[test]
    fn network_fs_types_are_recognized() {
        for t in ["nfs", "nfs4", "cifs", "smb3", "sshfs", "fuse.sshfs"] {
            assert_eq!(classify_kind(b"anything", t), VolumeKind::Network, "{t}");
        }
    }

    /// `/sys/class/block` has no `removable` file directly under a
    /// partition's own directory — only the whole-disk entry carries it
    /// (verified on this machine, see the rustdoc on
    /// `strip_partition_suffix`). This pins the two naming schemes that
    /// matters for the strip, without touching the real filesystem.
    #[test]
    fn strip_partition_suffix_recovers_the_whole_disk_name() {
        assert_eq!(
            strip_partition_suffix(b"nvme0n1p2").as_deref(),
            Some(&b"nvme0n1"[..])
        );
        assert_eq!(
            strip_partition_suffix(b"mmcblk0p1").as_deref(),
            Some(&b"mmcblk0"[..])
        );
        assert_eq!(
            strip_partition_suffix(b"sda1").as_deref(),
            Some(&b"sda"[..])
        );
        assert_eq!(strip_partition_suffix(b"sda"), None);
    }

    /// A mount that will not answer must not delay the others or fail the
    /// enumeration. Staged with an injected space query, not a real hung
    /// mount: the point is the policy, and a test that needs a dead NFS
    /// server is a test nobody runs.
    #[tokio::test]
    async fn a_mount_that_never_answers_reports_unknown_sizes() {
        let (total, free) = space_with_deadline(
            || {
                std::thread::sleep(Duration::from_hours(1));
                Ok((0, 0))
            },
            Duration::from_millis(20),
        )
        .await;
        assert_eq!((total, free), (None, None));
    }

    /// The counterpart: a query that answers well within the deadline
    /// reports the sizes it found, not `None`.
    #[tokio::test]
    async fn a_mount_that_answers_promptly_reports_its_sizes() {
        let (total, free) =
            space_with_deadline(|| Ok((1_000, 400)), Duration::from_millis(200)).await;
        assert_eq!((total, free), (Some(1_000), Some(400)));
    }

    /// `enumerate` against the REAL `/proc/mounts` of this machine: not a
    /// fixture, the actual host state. It must at minimum find the root
    /// filesystem, and every mount classified `Pseudo` must be absent unless
    /// asked for.
    #[tokio::test]
    async fn enumerate_finds_the_real_root_filesystem() {
        let volumes = enumerate(false).await.expect("reading /proc/mounts");
        assert!(
            volumes.iter().any(|v| v.mount.to_wire() == "file:///"),
            "expected `/` among {volumes:?}"
        );
        assert!(
            volumes.iter().all(|v| v.kind != VolumeKind::Pseudo),
            "a pseudo filesystem leaked past the default filter: {volumes:?}"
        );
    }
}
