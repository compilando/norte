//! freedesktop.org trash implemented HERE, without delegating to the
//! `trash` crate (Linux/BSD; macOS and Windows keep delegating).
//!
//! # Why this module exists
//! The `trash` crate knows how to bury a file but doesn't say WHERE it put
//! it, so [`Provider::trash`](norte_vfs::Provider::trash) used to answer
//! `Ok(None)` and the journal was left without a `reversal_ref`. Undoing an
//! overwrite then had to match by ORIGINAL path and pick the most recent
//! item — which by the time it got there was the file undo itself had just
//! buried while reverting the `created` half of the pair. It restored the
//! NEW file over itself, left the user's original inside the trash, and
//! counted it as a success (BLOCKER from the security review of task 11 of
//! the sync plan).
//!
//! The freedesktop spec is short and we choose the destination: **if we
//! choose it, we know it**. It's the same shape the LOGICAL trash of
//! `norte-vfs-sftp` already has (`.norte-trash/<id>/payload`).
//!
//! # Hard rules that drive the design
//! - **Rule 1 (names are BYTES).** The file keeps its bytes as they are
//!   inside `files/`; the `info/<name>.trashinfo` sidecar is TEXT and
//!   stores the percent-encoded original path, which comes out as pure
//!   ASCII no matter what's in front of it. The two can't disagree because
//!   they don't name the same thing: the sidecar names the SOURCE (which
//!   is what restore needs) and the file is named however it can be
//!   (deduplicated, and truncated if `NAME_MAX` squeezes). Nothing gets
//!   normalized here: not NFC, not NFD, not case.
//! - **Rule 2 (no blocking I/O in async).** This whole module is
//!   synchronous and the provider calls it from `blocking()`.
//! - **Rule 5 (`unsafe` justified).** Only `getuid` and `localtime_r`,
//!   both with their `// SAFETY:`.
//!
//! # Order matters, and the spec says which
//! First the sidecar with `O_EXCL` — that's what makes the name
//! reservation ATOMIC against another process —, then the victim's move.
//! If the move fails, the sidecar is unlinked and the name is free again.
//!
//! The reservation covers `info/<name>.trashinfo`, not `files/<name>`: the
//! latter is covered by the no-replace `rename`, and there's a gap on FSes
//! without `renameat2(RENAME_NOREPLACE)` (NFS, and the unix systems that
//! aren't Linux or macOS), where `do_rename` degrades to check-and-rename.
//! Another trash implementation that wrote its `files/x` inside that
//! window would lose its file. Implementations that respect themselves
//! reserve `info/` first just like we do, so the gap needs one that
//! doesn't AND an FS without the primitive (security-reviewer MINOR).

use std::ffi::OsStr;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use norte_proto::{ConflictKind, Error};
use norte_vfs::trash::TrashId;

use crate::provider::{do_rename, map_io};

/// Subdirectory of the buried files.
pub(crate) const FILES: &str = "files";
/// Subdirectory of the metadata sidecars.
pub(crate) const INFO: &str = "info";
/// The sidecar's suffix, which the spec fixes.
const INFO_SUFFIX: &[u8] = b".trashinfo";
/// Ceiling on an entry name's length on an ordinary unix FS. The sidecar
/// adds [`INFO_SUFFIX`], so the entry's name is trimmed for both to fit.
///
/// It's a STARTING POINT, not a fact: eCryptfs cuts around 143 bytes,
/// gocryptfs around 175, and an NFS or CIFS server sets its own. That's why
/// [`trash`]'s loop trims and retries when the FS rejects the name instead
/// of giving up (encoding-auditor finding MAJOR-1).
const NAME_MAX: usize = 255;
/// Floor of the trim: below it, the name stops resembling anything and
/// it's better to fail cleanly.
const MIN_NAME_BUDGET: usize = 16;
/// How many names `x`, `x.2`… `x.8` are tried before moving on to the name
/// derived from the `id`.
///
/// The ceiling isn't cosmetic. With an open scale (`x.2`, `x.3`, …
/// `x.4096`), seeding four thousand empty sidecars leaves a name that can
/// NEVER be trashed, and a `Mirror` of a tree with thousands of
/// `index.html` files costs one `open()` per slot. After eight probes it
/// jumps to slot `x.<id>`, which is unique per operation and hits on the
/// first try.
const PROBES: u32 = 8;
/// How many times the name gets trimmed on an FS rejection before giving up.
const MAX_SHRINKS: u32 = 4;

/// Buries `victim` in the trash that applies to it and returns the exact
/// NATIVE path of `files/<name>` — the recoverable destination the journal
/// stores as `reversal_ref`.
///
/// `data_home` substitutes `$XDG_DATA_HOME` when given (test seam: see
/// [`LocalProvider::with_trash_home`](crate::LocalProvider::with_trash_home)).
///
/// # Idempotence (#99)
/// `id` is NOT invented here: the engine generates it once per operation
/// and the sidecar's `DeletionDate` comes from it, so a retry writes the
/// SAME sidecar byte for byte. There are two paths, and both converge:
///
/// - **The previous attempt failed to move.** It undid its reservation, so
///   the retry picks the SAME free name and the same destination again.
/// - **The previous attempt moved, but its response was lost.** The victim
///   is no longer there; the retry recognizes its own entry — same
///   `Path=` and same `DeletionDate=` — and returns its destination
///   instead of creating a second one or losing the `reversal_ref`.
///
/// Honest residue, worth reading in full. An entry is identified by
/// (original path, deletion instant), which is all a standard
/// `.trashinfo` can hold — and that instant has SECOND resolution. Two
/// operations sharing both things — the same path and the same second —
/// are indistinguishable, and if the second one finds the path already
/// empty it would inherit the first one's destination. Telling them apart
/// completely would require putting a private key in the user's trash,
/// which other trash implementations read.
///
/// What BOUNDS that residue isn't the `.trashinfo` but the directory: the
/// trash is 0700 and owned by this uid ([`ensure_dir_owned`]), and both the
/// sidecar and the payload are checked to be ours ([`is_ours`]), so the
/// inherited entry came from this user and that same path. What this
/// module CANNOT promise is that the CONTENT hasn't changed between the
/// two attempts: another process of this same user may have replaced the
/// payload, and that's outside the threat model (`SECURITY.md`).
///
/// # Errors
/// [`Error::NotFound`] if the victim isn't there (and there's no earlier
/// entry of ours); [`Error::Unsupported`] if there's no usable trash for
/// that mount point (the frontend re-offers permanent deletion, ADR 0009);
/// [`Error::Conflict`] if every slot was occupied.
pub(crate) fn trash(
    victim: &Path,
    data_home: Option<&Path>,
    id: &TrashId,
) -> Result<PathBuf, Error> {
    let dir = prepare_trash_dir(victim, data_home)?;
    let name = victim
        .file_name()
        .ok_or(Error::InvalidPath)?
        .as_bytes()
        .to_vec();
    let body = trashinfo(&original_path(victim)?, id.deleted_ms());

    // The victim is no longer there: either it never existed, or an
    // earlier attempt of THIS SAME operation moved it. The latter is
    // recognized by the sidecar.
    if std::fs::symlink_metadata(victim).is_err() {
        return converged(&dir, &name, &body, id).ok_or(Error::NotFound);
    }

    let mut budget = NAME_MAX - INFO_SUFFIX.len();
    let mut shrinks = 0;
    let mut k = 1;
    while k <= PROBES + 1 {
        let cand = candidate(&name, k, budget, id);
        let sidecar = dir.join(INFO).join(join_bytes(&cand, INFO_SUFFIX));
        let dest = dir.join(FILES).join(OsStr::from_bytes(&cand));
        match write_new(&sidecar, &body) {
            Ok(()) => match do_rename(victim, &dest) {
                Ok(()) => return Ok(dest),
                // The name was reserved in `info/` but OCCUPIED in
                // `files/` (a half-written entry, or something seeded):
                // drop the reservation and try the next one.
                Err(Error::Conflict { .. }) => {
                    let _ = std::fs::remove_file(&sidecar);
                    k += 1;
                }
                // Any other failure leaves the name free: a sidecar
                // without a file would be a ghost entry in the user's
                // trash.
                Err(e) => {
                    let _ = std::fs::remove_file(&sidecar);
                    return Err(e);
                }
            },
            // The name is reserved by someone: the next one. Here it does
            // NOT converge even if the sidecar is identical to ours — the
            // victim is right there, so there's something to move;
            // claiming an earlier attempt's entry would leave the file
            // where it is and count it as buried.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => k += 1,
            // **The FS rejects the NAME, not the operation.**
            // `ENAMETOOLONG` on a `$HOME` over eCryptfs (ceiling ~143),
            // `EINVAL`/`EILSEQ` on a vfat mounted `utf8=1` or an ext4 with
            // `casefold` if the trim split a character. The entry's name
            // is STORAGE — the good path lives in the sidecar —, so it's
            // trimmed and the same slot is retried instead of declaring
            // untrashable a file the volume itself accepted
            // (encoding-auditor finding MAJOR-1).
            Err(e) if name_rejected(&e) && shrinks < MAX_SHRINKS && budget > MIN_NAME_BUDGET => {
                budget = (budget / 2).max(MIN_NAME_BUDGET);
                shrinks += 1;
            }
            Err(e) => return Err(map_io(&e)),
        }
    }
    Err(Error::Conflict {
        conflict: ConflictKind::Exists,
    })
}

/// Does this error say "that NAME isn't good for me" rather than "that
/// operation can't be done"? `ENAMETOOLONG`, `EINVAL` and `EILSEQ` — all
/// three are answered by the FS looking at the name's bytes, and all three
/// are fixed by shortening it.
fn name_rejected(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::InvalidFilename | std::io::ErrorKind::InvalidInput
    ) || matches!(
        e.raw_os_error(),
        Some(libc::ENAMETOOLONG | libc::EILSEQ | libc::EINVAL)
    )
}

/// Forgets `dest`'s sidecar after restoring it (best-effort).
///
/// Called AFTER moving the file back, never before: a `files/x` without
/// its `info/x.trashinfo` is an entry no graphical trash shows, so losing
/// the sidecar first and then failing the move would hide the file. The
/// other way around, the worst that's left is an orphan sidecar.
pub(crate) fn forget_sidecar(dest: &Path) {
    let Some(sidecar) = sidecar_of(dest) else {
        return;
    };
    // The SHAPE of the path isn't enough to delete: `sidecar_of` accepts
    // anything whose parent is called `files`, so a tampered
    // `reversal_ref` — or simply a user project with a `files/` and an
    // `info/` next to it — would point at a file of theirs. It's required
    // to be a REGULAR file and to start with the spec's header: only then
    // is it a `.trashinfo`, and deleting it is the right call
    // (encoding-auditor MINOR-5, security-reviewer MINOR).
    let regular = std::fs::symlink_metadata(&sidecar).is_ok_and(|md| md.file_type().is_file());
    if regular && std::fs::read(&sidecar).is_ok_and(|b| b.starts_with(HEADER.as_bytes())) {
        let _ = std::fs::remove_file(&sidecar);
    }
}

/// The sidecar that corresponds to `<trash>/files/<name>`, or `None` if
/// `dest` doesn't have that shape (it didn't come from this trash).
fn sidecar_of(dest: &Path) -> Option<PathBuf> {
    let name = dest.file_name()?;
    let files = dest.parent()?;
    if files.file_name() != Some(OsStr::new(FILES)) {
        return None;
    }
    Some(
        files
            .parent()?
            .join(INFO)
            .join(join_bytes(name.as_bytes(), INFO_SUFFIX)),
    )
}

/// The entry an earlier attempt left, if it left one.
///
/// EVERY slot [`trash`] could have used is walked, trims included: stopping
/// at the first one whose sidecar is missing would assume slots fill from
/// the bottom up, and this module makes holes on its own — the attempt
/// that reserves `x` and fails the rename drops `x` and moves on to `x.2`,
/// and [`forget_sidecar`] empties an arbitrary slot on restore. With that
/// assumption, a retry wouldn't find its own entry and would answer
/// `NotFound` about a file that was ALREADY buried, losing the
/// `reversal_ref`: the bug this module exists not to have
/// (encoding-auditor finding MAJOR-2).
///
/// The walk is bounded by construction ([`PROBES`] × [`MAX_SHRINKS`]) and
/// only runs when the victim is no longer there, which is the rare path.
fn converged(dir: &Path, name: &[u8], body: &[u8], id: &TrashId) -> Option<PathBuf> {
    let mut budget = NAME_MAX - INFO_SUFFIX.len();
    for _ in 0..=MAX_SHRINKS {
        for k in 1..=PROBES + 1 {
            let cand = candidate(name, k, budget, id);
            let sidecar = dir.join(INFO).join(join_bytes(&cand, INFO_SUFFIX));
            let dest = dir.join(FILES).join(OsStr::from_bytes(&cand));
            if is_ours(&sidecar, &dest, body) {
                return Some(dest);
            }
        }
        if budget <= MIN_NAME_BUDGET {
            break;
        }
        budget = (budget / 2).max(MIN_NAME_BUDGET);
    }
    None
}

/// Is the sidecar EXACTLY the one we'd write and is the file where it
/// says? The comparison is byte-for-byte of the whole body: same original
/// path and same `DeletionDate`, which comes from the engine's `id`.
///
/// What that distinguishes and what it doesn't is [`trash`]'s residue:
/// two burials of the SAME path in the SAME second write the same body and
/// are indistinguishable. Any other trash entry — another path, another
/// second, another application — doesn't pass.
fn is_ours(sidecar: &Path, dest: &Path, body: &[u8]) -> bool {
    // OURS, both: the sidecar and the payload. The trash is 0700 and owned
    // by this uid (`ensure_dir_owned` enforces it), so this is belt over
    // suspenders — but it's the belt that keeps a seeded entry from
    // passing for ours and ending up as `reversal_ref` in the journal
    // (security-reviewer MAJOR).
    let ours = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|md| md.uid() == uid());
    ours(sidecar) && ours(dest) && std::fs::read(sidecar).is_ok_and(|found| found == body)
}

/// The `k`-th candidate with whatever byte budget the name has left.
///
/// `k` from 1 to [`PROBES`] is the deduplication the spec describes (`x`,
/// `x.2`, `x.3`…). Slot [`PROBES`]`+1` is `x.<id>`, unique per operation:
/// it closes the scale without leaving it open for someone else to fill
/// (see [`PROBES`]).
///
/// The name is trimmed if it doesn't fit. Trimming loses nothing — the
/// good path lives in the sidecar, and the one under `files/` is just
/// storage —, but it's trimmed at a CHARACTER BOUNDARY when the name is
/// valid UTF-8: splitting a character produces a name a vfat mounted
/// `utf8=1` or an ext4 with `casefold` REJECT, and then a file the volume
/// had accepted becomes untrashable. A name that was never UTF-8 is cut by
/// bytes, which is consistent: that volume already accepted it that way.
fn candidate(name: &[u8], k: u32, budget: usize, id: &TrashId) -> Vec<u8> {
    let suffix = match k {
        1 => Vec::new(),
        k if k <= PROBES => format!(".{k}").into_bytes(),
        _ => format!(".{}", id.as_segment()).into_bytes(),
    };
    let room = budget.saturating_sub(suffix.len()).max(1);
    let mut out = truncate_bytes(name, room).to_vec();
    // A trim can't leave a name the FS won't accept as an entry.
    if out.is_empty() || out == b"." || out == b".." {
        out = b"trashed".to_vec();
    }
    out.extend_from_slice(&suffix);
    out
}

/// `name` trimmed to `room` bytes, at a character boundary if it's UTF-8.
fn truncate_bytes(name: &[u8], room: usize) -> &[u8] {
    if name.len() <= room {
        return name;
    }
    match std::str::from_utf8(name) {
        Ok(text) => {
            let mut end = room;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            &name[..end]
        }
        Err(_) => &name[..room],
    }
}

/// `a` followed by `b` as a file name, without going through `str` (rule 1).
fn join_bytes(a: &[u8], b: &[u8]) -> std::ffi::OsString {
    let mut out = a.to_vec();
    out.extend_from_slice(b);
    std::ffi::OsString::from_vec(out)
}

/// Creates the file with `O_EXCL` and writes `body` to it.
///
/// `create_new` IS `O_EXCL|O_CREAT`: it doesn't follow symlinks and fails
/// if the name exists, which is exactly what turns the reservation atomic
/// against another process writing into the same trash.
fn write_new(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    // If the CONTENT doesn't make it (ENOSPC, EIO), the name is already
    // created: it has to be released. Without this, an empty
    // `.trashinfo` is left behind, which is a ghost entry in the user's
    // trash and a slot spent forever (security-reviewer MINOR).
    if let Err(e) = f.write_all(body).and_then(|()| f.sync_all()) {
        drop(f);
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(())
}

/// Section header of the `.trashinfo`, which the spec fixes.
const HEADER: &str = "[Trash Info]\n";

/// The `.trashinfo`'s body: header, percent-encoded original path and
/// deletion date in LOCAL time, which is what the spec asks for.
fn trashinfo(original: &[u8], deleted_ms: u64) -> Vec<u8> {
    format!(
        "{HEADER}Path={}\nDeletionDate={}\n",
        percent_encode(original),
        local_datetime(deleted_ms)
    )
    .into_bytes()
}

/// The ORIGINAL path that goes into the sidecar: the CANONICALIZED parent
/// plus the name as is.
///
/// The parent is canonicalized and not the victim because the victim may
/// be a symlink and what's being buried is the symlink, not its target.
/// And it's canonicalized because that's what other implementations store
/// — and what `restore_trashed` (which lists the trash with the `trash`
/// crate) expects to match against: a root hanging off a symlink wouldn't
/// match any other way.
///
/// # Errors
/// An OS [`Error`] if the parent can't be canonicalized.
fn original_path(victim: &Path) -> Result<Vec<u8>, Error> {
    let name = victim.file_name().ok_or(Error::InvalidPath)?;
    let parent = victim.parent().ok_or(Error::InvalidPath)?;
    let real = parent.canonicalize().map_err(|e| map_io(&e))?;
    Ok(real.join(name).into_os_string().into_vec())
}

/// Percent-encodes BYTES per the `.trashinfo` spec (RFC 2396 over the raw
/// bytes, leaving `/` readable).
///
/// Leaves unescaped only RFC 3986's `unreserved` set plus `/`. It escapes
/// more than glib does (`!~*'()` are let through there), and that's
/// deliberate: it decodes identically in any reader and there's no need to
/// reason about which character is special in which context. What matters
/// is that **any byte that could break the key=value format comes out
/// escaped**: a `\n` in a file name becomes `%0A` and can't inject a fake
/// `Path=` line.
///
/// The result is pure ASCII, so the sidecar is valid UTF-8 even when the
/// file's name isn't (rule 1: the name is never decoded, it's encoded byte
/// by byte).
fn percent_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
    }
    out
}

/// `YYYY-MM-DDThh:mm:ss` in LOCAL time from an instant in milliseconds.
///
/// Local time because that's what the spec asks for and what graphical
/// trashes show. It comes from `localtime_r`, which is the only way to
/// apply the system's zone without pulling in a calendar dependency (rule
/// 8) — the alternative would be writing UTC and having the user's trash
/// lie by their zone's offset.
#[allow(unsafe_code)]
fn local_datetime(deleted_ms: u64) -> String {
    const FALLBACK: &str = "1970-01-01T00:00:00";
    let Ok(secs) = libc::time_t::try_from(deleted_ms / 1000) else {
        return FALLBACK.to_owned();
    };
    // SAFETY: `libc::tm` is POD (integers plus a `*const c_char` for the
    // zone's name, and the null pointer is a valid value), so the
    // all-zeros pattern is a valid instance.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into the `tm` it's given and keeps
    // neither pointer; both point at locals alive for the whole call and
    // are correctly aligned. It's the REENTRANT variant: it touches no
    // shared global state. Contract tested in
    // `tests::the_deletion_date_is_local_iso`.
    let ok = unsafe { libc::localtime_r(std::ptr::from_ref(&secs), std::ptr::from_mut(&mut tm)) };
    if ok.is_null() {
        return FALLBACK.to_owned();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Gets `victim`'s trash ready and returns its root.
///
/// The "home" trash (`$XDG_DATA_HOME/Trash`) only works if it's on the
/// SAME device as the victim; if not, the spec says to use the victim's
/// mount point's. That's not a performance detail: trashing across
/// devices would be a long, uncancellable copy+delete mid-way (a platform
/// exception ADR 0009 used to note and that this path no longer has: here
/// the move is ALWAYS a `rename` within one device). It's issue #26, and
/// `tests::the_trash_never_crosses_a_device` pins it down with two real
/// devices — without a test, "it doesn't copy anymore" is a sentence, not
/// a guarantee.
///
/// # The two trashes are NOT validated the same way, and that's deliberate
/// The home one hangs off `$HOME`: whoever can write there is already this
/// user, and a `~/.local/share/Trash` that's a symlink to another disk is
/// their own decision, respected as is (glib does the same). The TOPDIR
/// one hangs off the root of a mount that may be shared and writable by
/// others, and there it's required that the directory be OURS — lstat, not
/// stat, and our own `st_uid` — before putting anything inside. Without
/// that check, seeding `/tmp/.Trash-1000` (mode 0777, another uid) is
/// enough for the files this user deletes to end up in someone else's
/// tree, and for the journal's `reversal_ref` to point at a place where
/// someone else can change the content before the undo —
/// security-reviewer BLOCKER.
///
/// # Errors
/// [`Error::Unsupported`] if there's no usable trash at all — including
/// the case where the topdir one exists and isn't ours. The frontend then
/// re-offers PERMANENT deletion with a warning (ADR 0009).
fn prepare_trash_dir(victim: &Path, data_home: Option<&Path>) -> Result<PathBuf, Error> {
    let parent = victim.parent().ok_or(Error::InvalidPath)?;
    let dev = std::fs::metadata(parent).map_err(|e| map_io(&e))?.dev();
    if let Some(home) = home_trash(data_home)
        && device_of_nearest(&home) == Some(dev)
    {
        ensure_home_layout(&home)?;
        return Ok(home);
    }
    let top = top_dir(parent).ok_or(Error::Unsupported)?;
    let uid = uid();
    // `$top/.Trash/$uid` first (spec), and if it's no good it falls back
    // to `$top/.Trash-$uid` instead of giving up: a `$uid` someone else
    // seeded inside the shared `.Trash` can't leave anyone without a
    // trash (glib behaves the same way).
    if let Some(shared) = shared_topdir_trash(&top, uid)
        && ensure_owned_layout(&shared).is_ok()
    {
        return Ok(shared);
    }
    let own = top.join(format!(".Trash-{uid}"));
    ensure_owned_layout(&own)?;
    Ok(own)
}

/// Creates `<trash>/`, `files/` and `info/` under `$HOME` with `0700`
/// permissions.
///
/// `0700` because a trash's content belongs to the user and nobody else:
/// the names of what they've deleted are already information.
///
/// # Errors
/// An OS [`Error`] if any of them can't be created (a trash on a
/// read-only mount, for instance).
fn ensure_home_layout(dir: &Path) -> Result<(), Error> {
    if let Some(parent) = dir.parent() {
        // The container (`~/.local/share`) with whatever permissions it
        // has: only the trash itself is 0700.
        std::fs::create_dir_all(parent).map_err(|e| map_io(&e))?;
    }
    for d in [dir.to_path_buf(), dir.join(FILES), dir.join(INFO)] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&d)
            .map_err(|e| map_io(&e))?;
    }
    Ok(())
}

/// The same on a TOPDIR, where the directory has to be OURS.
///
/// # Errors
/// [`Error::Unsupported`] if any of the three can't be created or doesn't
/// pass the ownership check: "no usable trash here", which is exactly what
/// ADR 0009 has the frontend know how to handle. An `EROFS` isn't
/// distinguished from someone else's `.Trash-$uid` on purpose — to
/// whoever's deleting they're the same answer, and the errno of a mount we
/// don't control isn't information the frontend can use.
fn ensure_owned_layout(dir: &Path) -> Result<(), Error> {
    ensure_dir_owned(dir)?;
    ensure_dir_owned(&dir.join(FILES))?;
    ensure_dir_owned(&dir.join(INFO))
}

/// Creates `dir` with `0700` if it's missing, and in any case checks that
/// it's a real DIRECTORY (not a symlink) and OURS.
///
/// `create_dir` without `recursive`: the recursive version, on `EEXIST`,
/// asks with `metadata`, which FOLLOWS symlinks — a symlink to someone
/// else's tree would pass as a valid directory. Here it's checked with
/// `symlink_metadata`, which follows nothing, and our own `st_uid` is
/// required: whoever doesn't own it can't have made it ours (there's no
/// `chown` to another uid without privileges).
///
/// If it's ours but with loose permissions, it gets TIGHTENED to `0700`
/// instead of being rejected: the names of what one deletes belong to
/// nobody else, and the core's spool directory uses the same criterion.
fn ensure_dir_owned(dir: &Path) -> Result<(), Error> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(Error::Unsupported),
    }
    let md = std::fs::symlink_metadata(dir).map_err(|_| Error::Unsupported)?;
    if !md.file_type().is_dir() || md.uid() != uid() {
        return Err(Error::Unsupported);
    }
    if md.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::Unsupported)?;
    }
    Ok(())
}

/// `$XDG_DATA_HOME/Trash`, falling back to `$HOME/.local/share/Trash`.
///
/// A RELATIVE `$XDG_DATA_HOME` is ignored, as the XDG spec requires: a
/// trash root relative to the daemon's cwd is not a root.
pub(crate) fn home_trash(data_home: Option<&Path>) -> Option<PathBuf> {
    let base = match data_home {
        Some(d) => d.to_path_buf(),
        None => match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
            Some(d) if d.is_absolute() => d,
            _ => PathBuf::from(std::env::var_os("HOME")?)
                .join(".local")
                .join("share"),
        },
    };
    base.is_absolute().then(|| base.join("Trash"))
}

/// `st_dev` of the closest EXISTING ancestor of `p`.
///
/// Needed because the "home" trash may not exist yet and its device has to
/// be decided BEFORE creating it — creating it just to find out would
/// leave a directory in a place where nothing might end up being buried.
fn device_of_nearest(p: &Path) -> Option<u64> {
    let mut cur = p;
    loop {
        if let Ok(md) = std::fs::metadata(cur) {
            return Some(md.dev());
        }
        cur = cur.parent()?;
    }
}

/// `p`'s mount point: the highest ancestor with the same `st_dev`. Without
/// reading `/proc/mounts` — which isn't UTF-8 by contract, and has made
/// the upstream `trash` crate panic.
fn top_dir(p: &Path) -> Option<PathBuf> {
    let dev = std::fs::metadata(p).ok()?.dev();
    let mut top = p.to_path_buf();
    while let Some(parent) = top.parent() {
        match std::fs::metadata(parent) {
            Ok(md) if md.dev() == dev => top = parent.to_path_buf(),
            _ => return Some(top),
        }
    }
    Some(top)
}

/// `$top/.Trash/$uid` when `$top/.Trash` exists and the spec accepts it: a
/// DIRECTORY, with the sticky bit and not a symlink. `None` otherwise.
///
/// `symlink_metadata` and not `metadata`: the spec explicitly requires
/// that `$top/.Trash` not be a symlink, and with this call a symlink isn't
/// a directory. It's the check that stops whoever can write to a shared
/// mount's root from redirecting everyone else's trash into a tree of
/// their own. Sticky is the other half: without it, anyone can delete
/// anyone's things.
///
/// That the `$uid` inside is OURS is checked by [`ensure_dir_owned`]; the
/// parent folder's sticky bit doesn't guarantee it, because seeding a new
/// entry with another uid's name is legal in a sticky directory.
fn shared_topdir_trash(top: &Path, uid: u32) -> Option<PathBuf> {
    let shared = top.join(".Trash");
    let md = std::fs::symlink_metadata(&shared).ok()?;
    (md.file_type().is_dir() && md.permissions().mode() & 0o1000 != 0)
        .then(|| shared.join(uid.to_string()))
}

/// The process's real uid, which is what names a topdir's trash.
#[allow(unsafe_code)]
fn uid() -> u32 {
    // SAFETY: `getuid` takes no pointers, can't fail, and touches no state
    // this process shares; its ABI is fixed by POSIX.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::{
        candidate, local_datetime, percent_encode, shared_topdir_trash, sidecar_of, trashinfo,
    };
    use norte_vfs::trash::TrashId;
    use std::path::Path;

    /// #26: the trash NEVER crosses a device boundary.
    ///
    /// The bug this test pins down isn't hypothetical: the `trash` crate,
    /// which is what did this before this module existed, degrades to
    /// copy-the-tree-and-delete-the-source when the victim's mount doesn't
    /// support trashing. That's GBs inside ONE `spawn_blocking`, with no
    /// progress and no cancellation (hard rule 3), and the file stops
    /// being where it was before anyone can stop anything.
    ///
    /// Here the trash is chosen PER DEVICE ([`super::prepare_trash_dir`])
    /// and the transfer is a `rename`, which fails instead of copying
    /// across a boundary. The test verifies this for real, with two real
    /// devices: the victim on `/dev/shm` (tmpfs) and an injected home
    /// trash on disk.
    ///
    /// Without two devices there's nothing to check and the test bows out
    /// saying so: faking it would be worse than not running it.
    ///
    /// # Why BOTH halves live in a single test (#216)
    ///
    /// The second half — that the chosen trash is the victim's mount's
    /// topdir one, not the home one — used to be a separate test, and the
    /// two were intermittently red under `cargo test --lib` (green under
    /// nextest, which is what `just t` runs, so the gate never saw it).
    ///
    /// The shared resource wasn't an environment variable or the cwd: it
    /// was a **REAL system path**, `/dev/shm/.Trash-$uid`. A topdir's
    /// trash is fixed by the freedesktop spec, so the two tests had no
    /// choice but to use exactly the same one, and each one's cleanup
    /// deleted the one the other was using. It failed in both directions:
    /// "and it's left ready" when its freshly created `files/` got
    /// deleted, and "neither a trash on the device nor Unsupported" when
    /// its trash got deleted mid-`trash()`.
    ///
    /// A mutex isn't enough: nextest gives one PROCESS per test, so the
    /// exclusion would have to be between processes. Merging them is what
    /// removes the race under both runners, and it also puts two
    /// assertions about the same rule side by side. Reproduction of the
    /// red, before the fix:
    /// `cargo test -p norte-vfs-local --lib -- --test-threads=2 dispositivo`
    /// in a loop — one in fifteen failed.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_trash_never_crosses_a_device() {
        use std::os::unix::fs::MetadataExt;

        let home = tempfile::tempdir().expect("tempdir");
        let Ok(shm) = std::fs::metadata("/dev/shm") else {
            eprintln!("no /dev/shm: no two devices to check");
            return;
        };
        if shm.dev() == std::fs::metadata(home.path()).expect("stat").dev() {
            eprintln!("/dev/shm and the tempdir are the SAME device: nothing to check");
            return;
        }
        let base = Path::new("/dev/shm").join(format!("norte-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("dir in /dev/shm");
        let victim = base.join("v.txt");
        std::fs::write(&victim, b"x").expect("victim");

        let dest = super::trash(&victim, Some(home.path()), &id());

        let verdict = match &dest {
            Ok(dest) => {
                let d = std::fs::metadata(dest)
                    .expect("stat of the destination")
                    .dev();
                assert_eq!(d, shm.dev(), "the entry stayed on the victim's device");
                assert!(
                    !home
                        .path()
                        .join("Trash")
                        .join("files")
                        .join("v.txt")
                        .exists(),
                    "and NOTHING was copied to the $HOME trash"
                );
                let _ = std::fs::remove_file(dest);
                let _ = std::fs::remove_file(super::sidecar_of(dest).expect("sidecar"));
                // And the topdir trash this test just created, ONLY if it's
                // left empty: `remove_dir` fails with content, which is
                // exactly the protection needed to avoid sweeping anyone's
                // real trash.
                if let Some(trash_dir) = dest.parent().and_then(Path::parent) {
                    let _ = std::fs::remove_dir(trash_dir.join(super::FILES));
                    let _ = std::fs::remove_dir(trash_dir.join(super::INFO));
                    let _ = std::fs::remove_dir(trash_dir);
                }
                true
            }
            // `Unsupported` is the OTHER correct answer (a mount with no
            // usable trash): the frontend re-offers permanent deletion
            // with a warning, ADR 0009. What's not acceptable is copying.
            Err(norte_proto::Error::Unsupported) => {
                assert!(victim.exists(), "an impossible trash moves nothing");
                true
            }
            Err(e) => panic!("neither a trash on the device nor Unsupported: {e:?}"),
        };
        let _ = std::fs::remove_dir_all(&base);
        assert!(verdict);

        // Second half: the CHOSEN trash is the victim's mount's topdir
        // one, not the home one. It goes here, in sequence, for the
        // reason the doc above gives: it shares `/dev/shm/.Trash-$uid`
        // with the part above.
        let victim = tempfile::tempdir_in("/dev/shm").expect("tempdir in /dev/shm");
        let expected = Path::new("/dev/shm").join(format!(".Trash-{}", super::uid()));
        let existed = expected.exists();
        let dir = super::prepare_trash_dir(&victim.path().join("v.txt"), Some(home.path()))
            .expect("there is a trash");
        assert_eq!(
            dir, expected,
            "the trash is the victim's mount's, not the home one"
        );
        assert!(dir.join(super::FILES).is_dir(), "and it's left ready");
        if !existed {
            let _ = std::fs::remove_dir_all(&expected);
        }
    }

    fn id() -> TrashId {
        TrashId::new(1_726_000_000_123, 7)
    }

    /// The byte budget [`super::trash`]'s loop starts with.
    fn budget() -> usize {
        super::NAME_MAX - super::INFO_SUFFIX.len()
    }

    /// Rule 1: the sidecar is ASCII (and therefore valid UTF-8) even when
    /// the name isn't, and no byte of the name survives unescaped in a
    /// position where it could break the format.
    #[test]
    fn a_non_utf8_name_comes_out_escaped_and_the_sidecar_is_ascii() {
        let body = trashinfo(b"/tmp/x/h\xffstil\n=raro.bin", 0);
        let text = String::from_utf8(body).expect("the sidecar is UTF-8 by construction");
        assert!(text.is_ascii(), "{text}");
        assert!(
            text.contains("Path=/tmp/x/h%FFstil%0A%3Draro.bin"),
            "{text}"
        );
        // Exactly three lines: injecting a `\n` can't add a fourth.
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(text.starts_with("[Trash Info]\n"));
    }

    #[test]
    fn percent_encoding_leaves_readable_what_does_not_get_in_the_way() {
        assert_eq!(percent_encode(b"/home/u/a-b_c.d~e"), "/home/u/a-b_c.d~e");
        assert_eq!(percent_encode(b" %#?"), "%20%25%23%3F");
        assert_eq!(percent_encode("café".as_bytes()), "caf%C3%A9");
    }

    /// The spec's deduplication, and the trimming `NAME_MAX` forces: the
    /// entry's name plus `.trashinfo` has to fit.
    #[test]
    fn candidates_deduplicate_and_fit() {
        assert_eq!(candidate(b"a.txt", 1, budget(), &id()), b"a.txt");
        assert_eq!(candidate(b"a.txt", 2, budget(), &id()), b"a.txt.2");
        assert_eq!(candidate(b"a.txt", 8, budget(), &id()), b"a.txt.8");
        // Past the probes, the id names the slot: unique per operation,
        // and therefore nobody else can fill it.
        assert_eq!(
            candidate(b"a.txt", 9, budget(), &id()),
            b"a.txt.1726000000123-7"
        );
        let long_name = vec![b'x'; 255];
        for k in [1u32, 2, 9] {
            let c = candidate(&long_name, k, budget(), &id());
            assert!(
                c.len() + ".trashinfo".len() <= 255,
                "candidate {k} at {} bytes",
                c.len()
            );
        }
        // A trim never leaves a name the FS won't accept.
        assert_eq!(candidate(b"", 1, budget(), &id()), b"trashed");
    }

    /// encoding-auditor finding MAJOR-1: a long UTF-8 name is trimmed at a
    /// CHARACTER boundary. Splitting it produces a name a vfat `utf8=1` or
    /// an ext4 with `casefold` reject, and then a file the volume had
    /// accepted becomes untrashable.
    #[test]
    fn a_truncation_never_splits_a_character() {
        // 85 × U+3042 = 255 bytes; the budget (245) falls mid-way through
        // the 82nd.
        let long_name = "あ".repeat(85).into_bytes();
        assert_eq!(long_name.len(), 255);
        let c = candidate(&long_name, 1, budget(), &id());
        assert!(c.len() <= budget());
        std::str::from_utf8(&c).expect("the trim leaves valid UTF-8");
        assert_eq!(c.len() % 3, 0, "cut at a boundary: {}", c.len());

        // And a name that was NEVER UTF-8 is cut by bytes, without trying
        // to decode it: the volume already accepted it that way (rule 1).
        let mut raw = vec![b'a'; 245];
        raw.extend_from_slice(&[0xff; 10]);
        let c = candidate(&raw, 1, budget(), &id());
        assert_eq!(c.len(), 245);
        assert_eq!(c, vec![b'a'; 245]);
    }

    #[test]
    fn the_deletion_date_is_local_iso() {
        let s = local_datetime(1_726_000_000_123);
        assert_eq!(s.len(), 19, "{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        // The SAME id gives the SAME date: that's what makes the retry idempotent.
        assert_eq!(s, local_datetime(1_726_000_000_123));
        // LOCAL time, so the epoch's depends on the runner's zone: what
        // doesn't depend on it is the SHAPE and that a day is a day.
        assert!(local_datetime(0).starts_with("19"), "{}", local_datetime(0));
        assert_ne!(s, local_datetime(1_726_000_000_123 + 86_400_000));
    }

    #[test]
    fn the_sidecar_is_only_deduced_from_a_trash_shaped_path() {
        assert_eq!(
            sidecar_of(Path::new("/t/Trash/files/a.txt")),
            Some(Path::new("/t/Trash/info/a.txt.trashinfo").to_path_buf())
        );
        assert_eq!(sidecar_of(Path::new("/t/otro/a.txt")), None);
    }

    /// A `.Trash` that's a SYMLINK isn't used: it falls back to
    /// `.Trash-$uid`, which is what prevents redirecting a shared mount's
    /// trash.
    #[test]
    fn a_shared_trash_that_is_a_symlink_is_not_used() {
        let dir = tempfile::tempdir().expect("tempdir");
        let top = dir.path();
        std::os::unix::fs::symlink("/tmp", top.join(".Trash")).expect("symlink");
        assert_eq!(
            shared_topdir_trash(top, 1000),
            None,
            "falls back to .Trash-$uid"
        );
    }

    /// On the same device, the home trash wins.
    #[test]
    fn on_the_same_device_the_home_trash_wins() {
        use super::prepare_trash_dir;
        let home = tempfile::tempdir().expect("tempdir");
        let dir = prepare_trash_dir(&home.path().join("v.txt"), Some(home.path()))
            .expect("there is a trash");
        assert_eq!(dir, home.path().join("Trash"));
        assert!(dir.join(super::INFO).is_dir());
    }

    /// security-reviewer BLOCKER: a topdir trash directory that isn't
    /// OURS isn't used. Here the half a privilege-less test can build is
    /// tested — a symlink in its place —, which is the one
    /// `create_dir_all` used to happily follow: seeding
    /// `/tmp/.Trash-1000 -> /home/attacker/loot` would send another
    /// user's files there.
    #[test]
    fn a_topdir_trash_that_is_not_ours_is_not_used() {
        use super::ensure_dir_owned;
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let foreign = dir.path().join("foreign");
        std::fs::create_dir(&foreign).expect("mkdir");
        std::os::unix::fs::symlink(&foreign, dir.path().join(".Trash-1000")).expect("symlink");
        assert_eq!(
            ensure_dir_owned(&dir.path().join(".Trash-1000")),
            Err(norte_proto::Error::Unsupported),
            "a symlink is not a trash of ours"
        );

        // And one that IS ours but was left open gets TIGHTENED to 0700
        // instead of being rejected: the names of what one deletes belong
        // to nobody else.
        let mine = dir.path().join(".Trash-2000");
        std::fs::create_dir(&mine).expect("mkdir");
        std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        ensure_dir_owned(&mine).expect("it is ours");
        let mode = std::fs::symlink_metadata(&mine)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "tightened: {mode:o}");
    }

    /// And a `.Trash` without sticky doesn't work either (spec): a shared
    /// directory without sticky lets anyone delete anyone else's things.
    #[test]
    fn a_shared_trash_without_sticky_is_not_used() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let top = dir.path();
        std::fs::create_dir(top.join(".Trash")).expect("mkdir");
        std::fs::set_permissions(top.join(".Trash"), std::fs::Permissions::from_mode(0o777))
            .expect("chmod");
        assert_eq!(
            shared_topdir_trash(top, 1000),
            None,
            "falls back to .Trash-$uid"
        );
        // With sticky, yes: `$top/.Trash/$uid`.
        std::fs::set_permissions(top.join(".Trash"), std::fs::Permissions::from_mode(0o1777))
            .expect("chmod sticky");
        assert_eq!(
            shared_topdir_trash(top, 1000),
            Some(top.join(".Trash").join("1000"))
        );
    }
}
