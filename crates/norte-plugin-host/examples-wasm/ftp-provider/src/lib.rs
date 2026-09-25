//! WASM guest (#30 stage 3c): the COMPLETE FTP provider. SYNCHRONOUS
//! projection of the `norte_vfs::Provider` trait over `suppaftp::FtpStream`
//! (sync) via `wasi:sockets`. A port of `norte-vfs-ftp` with the SAME
//! CR/LF anti-injection defenses and the same MLSD/LIST handling. No TLS
//! (FTPS=debt: aws-lc-rs does not compile to wasm32-wasip2).
//!
//! Single-threaded wasm: the control connection lives in a `thread_local`
//! `RefCell<Option<Session>>`, not an `Arc<Mutex>`. Raw names in bytes
//! (rule 1); FTP requires UTF-8 → a non-representable name is
//! `invalid-path`.
//!
//! READ (#30 M1): the WIT `read(segs, offset, len)` interface is bounded,
//! but the guest CACHES the RETR's `DataStream` in the session and reuses
//! it while reads stay sequential (offset = end of the previous chunk) →
//! a single RETR per file, O(n). Any other op (or a non-sequential offset)
//! drains and finalizes the cache BEFORE issuing its control command
//! (`flush_cached_read`), so the pending `226` never interleaves. DEBT
//! (timeout/cancellation, ADR 0033): a read blocked on the socket is not
//! cut off by the epoch deadline (it only cuts off guest code), and the
//! host's `spawn_blocking` thread stays held — future mitigation:
//! `tokio::time::timeout` in the adapter.

use std::cell::RefCell;
use std::io::Write;

use suppaftp::list::{File, ListParser};
use suppaftp::types::FileType;
use suppaftp::{FtpError, FtpStream, Status};

wit_bindgen::generate!({
    world: "norte:provider/norte-provider",
    path: "wit",
    // `host-log`/`host-config` live in ANOTHER package since the split
    // (ADR 0041 decision 4); wit-bindgen requires explicitly deciding what
    // to do with imports from outside the world's package.
    generate_all,
});

use exports::norte::provider::provider::{
    Caps, Entry, EntryKind, Guest, GuestWriter, Page, ProviderConfig, VfsError, Writer,
};

/// Defensive cap on entries materialized per listing (issue #40). The real
/// OOM bound is upstream (suppaftp buffers the lines).
const MAX_LIST_ENTRIES: usize = 1 << 20;
/// Write staging prefix (ADR 0012, same convention as local/sftp).
const PARTIAL_PREFIX: &str = ".norte-partial.";

/// An established FTP session: control connection + remote root state.
struct Session {
    ftp: FtpStream,
    /// Absolute remote root everything lives under. No `..`, no trailing
    /// slash.
    base: String,
    /// The server supports MLSD/MLST (machine-readable). If not, it
    /// degrades to `LIST` (`ls -l`), universal but fragile with hostile
    /// names (ADR 0014 C).
    has_mlsd: bool,
    /// Staging counter (unique ephemeral name for `open_writer`).
    seq: u64,
    /// An in-flight RETR reusable across sequential reads (#30 M1): avoids
    /// re-RETR per chunk (O(n²)→O(n)). `None` = no read in flight.
    cached_read: Option<CachedRead>,
}

/// A live cached RETR: the DATA connection + the path and the next offset
/// it will deliver. The `reader` is independent of the control one; it is
/// drained and finalized via [`flush_cached_read`] BEFORE any control
/// command, so the pending `226` never interleaves.
struct CachedRead {
    remote: String,
    next_offset: u64,
    reader: Box<dyn std::io::Read>,
}

/// Minimal entry metadata, agnostic of the parsing backend (MLSD
/// self-parse or suppaftp's `ls -l`). Replaces suppaftp's `File` on
/// [`stat_remote`]'s surface so the size is u64 (not suppaftp's `usize`,
/// a 4 GiB ceiling on wasm32, #30 H2).
struct StatEntry {
    kind: EntryKind,
    size: Option<u64>,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Runs `f` with the established session, or `provider-unavailable` if
/// `configure` was never called (or failed). The wasm is single-threaded:
/// `borrow_mut` never overlaps.
fn with_session<T>(f: impl FnOnce(&mut Session) -> Result<T, VfsError>) -> Result<T, VfsError> {
    SESSION.with_borrow_mut(|s| match s.as_mut() {
        Some(sess) => f(sess),
        None => Err(VfsError::ProviderUnavailable),
    })
}

/// Drains and finalizes the cached RETR (if any), leaving control CLEAN
/// for the next command. Best-effort and idempotent (`None` = no-op).
/// Every op that issues a control command calls it BEFORE (#30 M1's
/// invariant: never a command with a pending `226` on the control
/// connection).
///
/// COST (debt, ADR 0033): the drain goes all the way to the data
/// connection's EOF, so abandoning a large file's read makes the NEXT op
/// pay for transferring the unread tail; and a hostile server that
/// streams forever hangs the host's `spawn_blocking` thread (the epoch
/// deadline does not cut off socket I/O). Same debt as
/// timeout/cancellation; the clean fix would be `ABOR` or reconnecting
/// control. The `8192`-byte scratch bounds MEMORY, not the total.
fn flush_cached_read(s: &mut Session) {
    let Some(mut cr) = s.cached_read.take() else {
        return;
    };
    use std::io::Read;
    // Drains the rest of the data connection (RETR goes offset→EOF;
    // stopping without draining would desync control), then reads the
    // transfer's response.
    let mut scratch = [0u8; 8192];
    loop {
        match cr.reader.read(&mut scratch) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let _ = s.ftp.finalize_retr_stream(cr.reader);
}

struct FtpProvider;

impl Guest for FtpProvider {
    fn configure(cfg: ProviderConfig) -> Result<(), VfsError> {
        // `base` is trusted config (the host sets it), but it is validated
        // as defense in depth: absolute and without CR/LF/NUL (which
        // would inject an FTP command into every op, bypassing the
        // per-segment filter).
        let mut base = cfg.base;
        while base.len() > 1 && base.ends_with('/') {
            base.pop();
        }
        if !base.starts_with('/') || base.contains(['\r', '\n', '\0']) {
            return Err(VfsError::InvalidPath);
        }
        // The endpoint is ALREADY a numeric `ip:port` (the host resolved
        // DNS): connect resolves no hostnames (the guest has no DNS).
        let mut ftp =
            FtpStream::connect(cfg.endpoint.as_str()).map_err(|_| VfsError::ProviderUnavailable)?;
        ftp.login(cfg.user.as_str(), cfg.password.as_str())
            .map_err(|e| map_err(&e))?;
        let has_mlsd = setup_conn(&mut ftp).map_err(|e| map_err(&e))?;
        SESSION.set(Some(Session {
            ftp,
            base,
            has_mlsd,
            seq: 0,
            cached_read: None,
        }));
        Ok(())
    }

    fn capabilities() -> Caps {
        // Honest (ADR 0014): remote POSIX case-sensitive and
        // case-preserving. It does NOT declare symlinks/trash/server-copy
        // (the adapter maps them to absent → Unsupported), nor resume
        // (the adapter uses the default open_resumable, which does not
        // resume). No READ_ONLY (writable).
        Caps {
            read_only: false,
            case_sensitive: true,
            case_preserving: true,
        }
    }

    fn stat(segments: Vec<Vec<u8>>) -> Result<Entry, VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            // The provider's root is the base directory (it has no parent
            // to list).
            if segments.is_empty() {
                return Ok(Entry {
                    name: Vec::new(),
                    kind: EntryKind::Dir,
                    size: None,
                });
            }
            let remote = remote(&s.base, &segments)?;
            match stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                Some(st) => Ok(Entry {
                    name: last_name(&segments),
                    kind: st.kind,
                    size: st.size,
                }),
                None => Err(VfsError::NotFound),
            }
        })
    }

    fn list_dir(segments: Vec<Vec<u8>>, _cursor: Option<Vec<u8>>) -> Result<Page, VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            let remote = remote(&s.base, &segments)?;
            let lines = if s.has_mlsd {
                s.ftp.mlsd(Some(remote.as_str()))
            } else {
                s.ftp.list(Some(remote.as_str()))
            }
            .map_err(|e| map_err(&e))?;
            let mut entries = Vec::new();
            // suppaftp decodes names with `from_utf8_lossy`: a non-UTF-8
            // byte arrives already substituted by U+FFFD and unrecoverable
            // → CLEAN rejection (rule 1, ADR 0014 D2). A `/` or NUL
            // injected by a hostile server tries to escape/truncate the
            // path: the page fails LOUD (encoding M2).
            let reject = |name: &str| name.contains('\u{FFFD}') || name.contains(['/', '\0']);
            for line in lines {
                // Defensive cap on our own materialization (issue #40).
                if entries.len() >= MAX_LIST_ENTRIES {
                    return Err(VfsError::Io);
                }
                // Separate branches so each `name` owns its lifetime
                // (LIST's `f.name()` borrows from `f`, which dies on
                // exit).
                if s.has_mlsd {
                    // MLSD self-parse (#30 H2): (kind, size u64, raw name).
                    let Some((kind, size, name)) = parse_mlsd_facts(&line) else {
                        // Machine-readable MLSD: an unreadable line is
                        // anomalous.
                        return Err(VfsError::Io);
                    };
                    if name == "." || name == ".." {
                        continue;
                    }
                    if reject(name) {
                        return Err(VfsError::InvalidPath);
                    }
                    entries.push(Entry {
                        name: name.as_bytes().to_vec(),
                        kind,
                        size,
                    });
                } else {
                    // `ls -l`: unparseable lines (a `total N` header) are
                    // dropped.
                    let Some(f) = parse_list_line(&line) else {
                        continue;
                    };
                    let name = f.name();
                    if name == "." || name == ".." {
                        continue;
                    }
                    if reject(name) {
                        return Err(VfsError::InvalidPath);
                    }
                    let st = stat_entry_from_file(&f);
                    entries.push(Entry {
                        name: name.as_bytes().to_vec(),
                        kind: st.kind,
                        size: st.size,
                    });
                }
            }
            Ok(Page {
                entries,
                next_cursor: None,
            })
        })
    }

    fn read(segments: Vec<Vec<u8>>, offset: u64, len: u64) -> Result<Vec<u8>, VfsError> {
        use std::io::Read;
        with_session(|s| {
            let remote = remote(&s.base, &segments)?;
            // Reuses the cached RETR if the path AND sequential offset
            // match (#30 M1). If not (or none), finalizes the previous one
            // and opens a new one.
            let hit = s
                .cached_read
                .as_ref()
                .is_some_and(|cr| cr.remote == remote && cr.next_offset == offset);
            if !hit {
                flush_cached_read(s);
                // Rejects a dir (reading it is an error) and an absent one
                // (NotFound).
                match stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                    None => return Err(VfsError::NotFound),
                    Some(st) if st.kind == EntryKind::Dir => return Err(VfsError::Conflict),
                    Some(_) => {}
                }
                // REST offset (resume/range): positions the RETR's start.
                if offset > 0 {
                    let off = usize::try_from(offset).map_err(|_| VfsError::Io)?;
                    s.ftp.resume_transfer(off).map_err(|e| map_err(&e))?;
                }
                let reader = s
                    .ftp
                    .retr_as_stream(remote.as_str())
                    .map_err(|e| map_err(&e))?;
                s.cached_read = Some(CachedRead {
                    remote: remote.clone(),
                    next_offset: offset,
                    reader: Box::new(reader),
                });
            }
            // Reads up to `want` bytes from the cached reader. It reads
            // DIRECTLY into a buffer of the requested size (not a fixed
            // buf + truncation): this way it never pulls more than
            // requested off the socket, which would corrupt the next
            // sequential read (it would lose those bytes from its
            // window).
            let want = usize::try_from(len).unwrap_or(usize::MAX);
            // Ceiling in case `want == u64::MAX` (the adapter asks for
            // 64 KiB; it never bites).
            let cap = want.min(1 << 20);
            // A hit reuses the cache; a miss just installed it above — in
            // both cases `cached_read` is Some. The else is unreachable;
            // it is treated as Io instead of panicking (hard rule 6, no
            // `expect`).
            let Some(cr) = s.cached_read.as_mut() else {
                return Err(VfsError::Io);
            };
            let mut out = vec![0u8; cap];
            let mut filled = 0usize;
            let mut eof = false;
            let mut read_err = false;
            while filled < cap {
                // An error CANNOT use `?` (it would skip the flush →
                // pending 226, rust review B1). It is flagged and
                // finalized outside the loop.
                match cr.reader.read(&mut out[filled..]) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => filled += n,
                    Err(_) => {
                        read_err = true;
                        break;
                    }
                }
            }
            out.truncate(filled);
            cr.next_offset += filled as u64;
            // EOF or error: finalizes the cache (clean, or best-effort on
            // error).
            if eof || read_err {
                flush_cached_read(s);
            }
            if read_err {
                return Err(VfsError::Io);
            }
            Ok(out)
        })
    }

    // ---- write ----

    type Writer = FtpWriter;

    fn open_writer(segments: Vec<Vec<u8>>) -> Result<Writer, VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            let final_remote = remote(&s.base, &segments)?;
            let parent_len = segments.len().saturating_sub(1);
            let parent = remote(&s.base, &segments[..parent_len])?;
            // The final destination must not exist (create-new; the
            // overwrite policy belongs to the core). A documented TOCTOU
            // window.
            if exists(&mut s.ftp, &final_remote, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            let seq = s.seq;
            s.seq += 1;
            let staging = format!("{parent}/{PARTIAL_PREFIX}eph.{seq}");
            // Creates the staging EMPTY (STOR with no data): a base for a
            // 0-byte write to rename, and later write() calls just APPE.
            create_empty(&mut s.ftp, &staging)?;
            Ok(Writer::new(FtpWriter {
                staging: RefCell::new(Some(staging)),
                final_remote,
            }))
        })
    }

    fn make_dir(segments: Vec<Vec<u8>>) -> Result<(), VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            let remote = remote(&s.base, &segments)?;
            if exists(&mut s.ftp, &remote, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            s.ftp.mkdir(&remote).map_err(|e| map_err(&e))
        })
    }

    fn remove(segments: Vec<Vec<u8>>) -> Result<(), VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            let remote = remote(&s.base, &segments)?;
            // Need to know if it is a dir to choose RMD vs DELE.
            let st =
                stat_remote(&mut s.ftp, &remote, s.has_mlsd, &s.base)?.ok_or(VfsError::NotFound)?;
            if st.kind == EntryKind::Dir {
                s.ftp.rmdir(&remote).map_err(|e| map_err(&e))
            } else {
                s.ftp.rm(&remote).map_err(|e| map_err(&e))
            }
        })
    }

    fn rename(src: Vec<Vec<u8>>, dst: Vec<Vec<u8>>) -> Result<(), VfsError> {
        with_session(|s| {
            flush_cached_read(s);
            let from_r = remote(&s.base, &src)?;
            let to_r = remote(&s.base, &dst)?;
            // RNFR/RNTO does not guarantee no-replace: checked beforehand
            // (documented TOCTOU) to give Conflict, not overwrite.
            if exists(&mut s.ftp, &to_r, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            s.ftp.rename(&from_r, &to_r).map_err(|e| map_err(&e))
        })
    }
}

/// Transactional writer over FTP (ADR 0014): each `write` does an `APPE`
/// of its chunk to the staging; `commit` renames staging→final; `abort`
/// deletes the staging. State (the staging path) lives behind `RefCell`
/// because the resource's WIT methods take `&self`. The connection is
/// reached via the `thread_local`.
struct FtpWriter {
    /// `Some` while it has not been published/discarded.
    staging: RefCell<Option<String>>,
    final_remote: String,
}

impl GuestWriter for FtpWriter {
    fn write(&self, chunk: Vec<u8>) -> Result<(), VfsError> {
        if chunk.is_empty() {
            return Ok(());
        }
        let staging = self.staging.borrow().clone().ok_or(VfsError::Io)?;
        with_session(|s| {
            flush_cached_read(s);
            let mut data = s
                .ftp
                .append_with_stream(&staging)
                .map_err(|e| map_err(&e))?;
            let res = data.write_all(&chunk);
            // Closes the data connection and reads the response ALWAYS
            // (even if the write failed), or control ends up desynced.
            let fin = s.ftp.finalize_put_stream(data);
            res.map_err(|_| VfsError::Io)?;
            fin.map_err(|e| map_err(&e))
        })
    }

    fn commit(&self) -> Result<(), VfsError> {
        let staging = self.staging.borrow_mut().take().ok_or(VfsError::Io)?;
        with_session(|s| {
            flush_cached_read(s);
            // The final destination must not exist (create-new): checked
            // when opening; the window up to here is TOCTOU (FTP has no
            // atomic rename).
            if exists(&mut s.ftp, &self.final_remote, s.has_mlsd, &s.base)? {
                return Err(VfsError::Conflict);
            }
            s.ftp
                .rename(&staging, &self.final_remote)
                .map_err(|e| map_err(&e))
        })
    }

    fn abort(&self) -> Result<(), VfsError> {
        // Deletes the staging (each write left it durable on the server).
        if let Some(staging) = self.staging.borrow_mut().take() {
            let _ = with_session(|s| {
                flush_cached_read(s);
                let _ = s.ftp.rm(&staging);
                Ok(())
            });
        }
        Ok(())
    }
}

/// Absolute remote path under `base` from raw segments. SAME defenses as
/// the native provider: UTF-8 required, no `/`/`.`/`..`, no CR/LF, ≤255
/// bytes.
///
/// FTP is a LINE-based protocol (a command ends in CRLF): a name with
/// CR/LF would inject an arbitrary FTP command
/// (`STOR path\r\nDELE victim`) — rejected. FTP-SPECIFIC containment.
fn remote(base: &str, segments: &[Vec<u8>]) -> Result<String, VfsError> {
    let mut out = String::from(base);
    for seg in segments {
        let name = std::str::from_utf8(seg).map_err(|_| VfsError::InvalidPath)?;
        // An empty segment would make a path with `//` that aliases the
        // parent (encoding M1); `/`/`.`/`..` would escape the base. The
        // host's `Segment` already rejects them, but the guest revalidates
        // (raw bytes in the WIT).
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(VfsError::InvalidPath);
        }
        // CR/LF would inject an FTP command; NUL truncates paths on
        // C-based servers. The host's `Segment` already rejects NUL, but
        // the guest revalidates (the WIT interface carries raw bytes):
        // defense in depth, same as `configure` applies to `base` (rust
        // review m2 / security LOW).
        if name.contains(['\r', '\n', '\0']) {
            return Err(VfsError::InvalidPath);
        }
        // NAME_MAX: most FS's reject >255 bytes with ENAMETOOLONG; the
        // server would fail mid-op with an ambiguous 550. Rejected
        // CLEANLY.
        if seg.len() > 255 {
            return Err(VfsError::InvalidPath);
        }
        if out.len() > 1 || !out.ends_with('/') {
            out.push('/');
        }
        out.push_str(name);
    }
    Ok(out)
}

/// The last segment (name) of a path, or empty for the root.
fn last_name(segments: &[Vec<u8>]) -> Vec<u8> {
    segments.last().cloned().unwrap_or_default()
}

/// Prepares a freshly logged-in connection: BINARY (ASCII corrupts
/// binaries), MLSD/MLST detection and `OPTS UTF8 ON` if the server
/// announces it (RFC 2640, ADR 0014 D2; best-effort). Returns whether
/// MLSD is available.
fn setup_conn(ftp: &mut FtpStream) -> Result<bool, FtpError> {
    ftp.transfer_type(FileType::Binary)?;
    let feats = ftp.feat().ok();
    let has_mlsd = feats.as_ref().is_some_and(|f| {
        f.keys()
            .any(|k| k.eq_ignore_ascii_case("MLST") || k.eq_ignore_ascii_case("MLSD"))
    });
    if feats
        .as_ref()
        .is_some_and(|f| f.keys().any(|k| k.eq_ignore_ascii_case("UTF8")))
    {
        let _ = ftp.opts("UTF8", Some("ON"));
    }
    Ok(has_mlsd)
}

/// Maps a suppaftp error to the WIT `vfs-error` taxonomy (spec §17.7).
fn map_err(e: &FtpError) -> VfsError {
    match e {
        FtpError::UnexpectedResponse(r) => match r.status {
            // 550 is ambiguous in FTP (does not exist / no permission):
            // NotFound is the common case and the one the contract expects
            // for absent paths.
            Status::FileUnavailable => VfsError::NotFound,
            Status::NotLoggedIn => VfsError::PermissionDenied,
            Status::BadFilename => VfsError::InvalidPath,
            // The proto taxonomy's `retryable` flag does not cross the WIT
            // interface (closed enum): 450 and the rest fall to `io`.
            _ => VfsError::Io,
        },
        // `SecureError` is gated by the TLS feature (disabled: FTPS is
        // debt) — it does not exist in this build.
        FtpError::ConnectionError(_) => VfsError::ProviderUnavailable,
        FtpError::InvalidAddress(_) => VfsError::InvalidPath,
        FtpError::BadResponse | FtpError::DataConnectionAlreadyOpen => VfsError::Io,
    }
}

/// Parses a `LIST` line (POSIX `ls -l`, with a DOS fallback). `None` for
/// unparseable lines (`total N` headers): they are dropped.
fn parse_list_line(line: &str) -> Option<File> {
    ListParser::parse_posix(line)
        .ok()
        .or_else(|| ListParser::parse_dos(line).ok())
}

/// Parses an MLSD/MLST line (RFC 3659: `[facts] SP pathname`). Returns
/// `(kind, size, raw_name)`: `kind` from the `type` fact, `size` as u64
/// (`None` if the `size` fact is missing or not numeric — a dir omits
/// it), `raw_name` after the FIRST space (raw; the caller applies the
/// U+FFFD/`/`/NUL rejection). Replaces suppaftp's
/// `ListParser::parse_mlsd`/`parse_mlst`, which parses the size as
/// `usize` (a 4 GiB ceiling on wasm32) and truncated the name at `;`
/// (#30 H2). `None` if the line does not have the `facts SP name` shape
/// (no space, or empty name).
fn parse_mlsd_facts(line: &str) -> Option<(EntryKind, Option<u64>, &str)> {
    let (facts, name) = line.split_once(' ')?;
    if name.is_empty() {
        return None;
    }
    let mut kind = EntryKind::File; // default if `type` is missing
    let mut size = None;
    for fact in facts.split(';') {
        let Some((key, value)) = fact.split_once('=') else {
            continue;
        };
        if key.eq_ignore_ascii_case("type") {
            kind = match value.to_ascii_lowercase().as_str() {
                "dir" | "cdir" | "pdir" => EntryKind::Dir,
                "file" => EntryKind::File,
                "link" => EntryKind::Symlink,
                _ => EntryKind::Other,
            };
        } else if key.eq_ignore_ascii_case("size") {
            size = value.parse::<u64>().ok();
        }
    }
    Some((kind, size, name))
}

/// TOLERANT extraction of an `ls -l` line's name, ONLY for the
/// anti-overwrite safeguard (never as a real name): `perms links owner
/// group size mon day time name` → the name is everything after the 8th
/// whitespace-separated field. `None` if the line has <9 fields (e.g. a
/// `total N` header). Names with a leading space are lost (a known
/// `ls -l` limitation): that leaves ONE gap in the anti-overwrite
/// safeguard — a file ≥4 GiB with a leading-space name on a server
/// WITHOUT MLSD would not reliably match `child` and could be
/// overwritten. An intersection of 3 rare preconditions + inherent to
/// `ls -l` (suppaftp loses it the same way); the real fix is MLSD (which
/// does preserve the space).
fn ls_l_name(line: &str) -> Option<&str> {
    // Skips 8 fields (each = a token + its following whitespace).
    let mut rest = line;
    for _ in 0..8 {
        let trimmed = rest.trim_start();
        let end = trimmed.find(char::is_whitespace)?;
        rest = &trimmed[end..];
    }
    let name = rest.trim_start();
    if name.is_empty() { None } else { Some(name) }
}

/// `StatEntry` from a suppaftp `File` (the LIST branch; the size comes
/// from suppaftp's `usize` — a 4 GiB limit accepted for servers WITHOUT
/// MLSD).
fn stat_entry_from_file(f: &File) -> StatEntry {
    let kind = if f.is_symlink() {
        EntryKind::Symlink
    } else if f.is_directory() {
        EntryKind::Dir
    } else if f.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    let size = (kind == EntryKind::File).then(|| f.size() as u64);
    StatEntry { kind, size }
}

/// `stat` of `remote`: with MLSD, a direct `MLST` (self-parse, u64 size);
/// without MLSD, `LIST` of the PARENT directory + search by name
/// (universal — pure-ftpd; ADR 0014 C). `None` = does not exist.
fn stat_remote(
    ftp: &mut FtpStream,
    remote: &str,
    has_mlsd: bool,
    base: &str,
) -> Result<Option<StatEntry>, VfsError> {
    if has_mlsd {
        return match ftp.mlst(Some(remote)) {
            Ok(line) => match parse_mlsd_facts(&line) {
                Some((kind, size, _name)) => Ok(Some(StatEntry { kind, size })),
                None => Err(VfsError::Io), // an unreadable MLST is anomalous
            },
            Err(e) => match map_err(&e) {
                VfsError::NotFound => Ok(None),
                other => Err(other),
            },
        };
    }
    // Containment (security): the LIST branch lists the PARENT directory.
    // For the provider's ROOT (`remote == base`) the parent would be
    // OUTSIDE the base — it is never listed above it. The root is a
    // degenerate dir: None (fail-safe).
    if remote == base {
        return Ok(None);
    }
    let (parent, child) = match remote.rfind('/') {
        Some(0) => ("/", &remote[1..]),
        Some(i) => (&remote[..i], &remote[i + 1..]),
        None => return Ok(None),
    };
    if child.is_empty() {
        return Ok(None);
    }
    let lines = match ftp.list(Some(parent)) {
        Ok(l) => l,
        Err(e) => {
            return match map_err(&e) {
                VfsError::NotFound => Ok(None),
                other => Err(other),
            };
        }
    };
    for line in lines {
        match parse_list_line(&line) {
            Some(f) => {
                // The server's name arrives lossy: a non-UTF-8 (U+FFFD)
                // never reliably matches a UTF-8 `child` → skipped, not
                // compared.
                let n = f.name();
                if n.contains('\u{FFFD}') {
                    continue;
                }
                if n == child {
                    return Ok(Some(stat_entry_from_file(&f)));
                }
            }
            // Anti-overwrite safeguard (#30 H2): an `ls -l` line that does
            // NOT parse (e.g. size ≥ 4 GiB breaks suppaftp's `usize`
            // parse) but whose name matches `child` is NOT silently
            // dropped — it fails LOUD, so `exists()` does not say "does
            // not exist" and write/rename/mkdir do not overwrite an
            // invisible file. `total N` headers (ls_l_name=None) keep
            // getting dropped.
            None => {
                if let Some(n) = ls_l_name(&line) {
                    if !n.contains('\u{FFFD}') && n == child {
                        return Err(VfsError::Io);
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Does `remote` exist? (via `stat_remote`.)
fn exists(ftp: &mut FtpStream, remote: &str, has_mlsd: bool, base: &str) -> Result<bool, VfsError> {
    Ok(stat_remote(ftp, remote, has_mlsd, base)?.is_some())
}

/// Creates `remote` as an EMPTY file (STOR with no data): a base to APPE
/// onto.
fn create_empty(ftp: &mut FtpStream, remote: &str) -> Result<(), VfsError> {
    let data = ftp.put_with_stream(remote).map_err(|e| map_err(&e))?;
    ftp.finalize_put_stream(data).map_err(|e| map_err(&e))
}

export!(FtpProvider);

#[cfg(test)]
mod parse_tests {
    use super::{EntryKind, ls_l_name, parse_mlsd_facts};

    #[test]
    fn mlsd_facts_size_u64_beyond_4gib() {
        // 5 GiB = 5368709120 > u32::MAX: suppaftp used to break on it; here
        // it is exact u64.
        let line = "type=file;size=5368709120;modify=20200101000000; big.bin";
        let (kind, size, name) = parse_mlsd_facts(line).expect("parses");
        assert_eq!(kind, EntryKind::File);
        assert_eq!(size, Some(5_368_709_120));
        assert_eq!(name, "big.bin");
    }

    #[test]
    fn mlsd_facts_dir_without_size() {
        let (kind, size, name) =
            parse_mlsd_facts("type=dir;modify=20200101000000; sub").expect("dir");
        assert_eq!(kind, EntryKind::Dir);
        assert_eq!(size, None);
        assert_eq!(name, "sub");
    }

    #[test]
    fn mlsd_facts_name_with_semicolon_survives() {
        // The name goes after the FIRST space: a `;` in the name does NOT
        // truncate it.
        let (_, _, name) = parse_mlsd_facts("type=file;size=1; a;b.txt").expect("parses");
        assert_eq!(name, "a;b.txt");
    }

    #[test]
    fn mlsd_facts_missing_type_defaults_file_and_unknown_is_other() {
        assert_eq!(parse_mlsd_facts("size=1; f").unwrap().0, EntryKind::File);
        assert_eq!(parse_mlsd_facts("type=cdir; .").unwrap().0, EntryKind::Dir);
        assert_eq!(
            parse_mlsd_facts("type=slink; x").unwrap().0,
            EntryKind::Other
        );
    }

    #[test]
    fn mlsd_facts_rejects_malformed() {
        assert!(parse_mlsd_facts("no-space-no-name").is_none());
        assert!(parse_mlsd_facts("type=file;size=1; ").is_none()); // empty name
    }

    #[test]
    fn ls_l_name_extracts_after_eight_fields() {
        let n = ls_l_name("-rw-r--r-- 1 owner group 5368709120 Jan 12 10:00 big.bin");
        assert_eq!(n, Some("big.bin"));
        let n2 = ls_l_name("-rw-r--r-- 1 o g 5 Jan 12 10:00 with spaces.txt");
        assert_eq!(n2, Some("with spaces.txt"));
    }

    #[test]
    fn ls_l_name_rejects_header_and_short() {
        assert_eq!(ls_l_name("total 8"), None);
        assert_eq!(ls_l_name(""), None);
    }
}
