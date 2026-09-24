//! [`LocalProvider`]: the local filesystem behind the [`Provider`] contract.
//!
//! Hard rule 2 of `CLAUDE.md`: NO blocking I/O in async context — every
//! syscall goes through `spawn_blocking`; streams deliver over a bounded
//! `mpsc` channel (64), so the blocking thread is freed between chunks and
//! dropping the stream cancels the producer.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{
    Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, Scheme, Segment, VPath,
};
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};
use tokio_stream::wrappers::ReceiverStream;

use norte_vfs::native::{to_native, verbatim};
use norte_vfs::wtf8::os_to_bytes;

/// Read chunk size (aligned with the copy engine: 256 KiB).
const READ_CHUNK: usize = 256 * 1024;

/// Provider for the local filesystem, rooted in a native directory.
///
/// The root `VPath` (`file:///`) maps to `base`; each segment is a native
/// component (bytes intact; on Windows, WTF-8 validated at the boundary and
/// paths ALWAYS with the `\\?\` prefix).
///
/// Capabilities are probed LAZILY, at the start of the first async operation
/// and inside `spawn_blocking` (issue #5 + rule 2): constructing the
/// provider never mutates `base`, and the runtime never blocks on the
/// probe. If the probe cannot decide (base not writable and no platform
/// API) it falls back to the OS default.
pub struct LocalProvider {
    base: PathBuf,
    caps: std::sync::Arc<std::sync::OnceLock<Capabilities>>,
    /// Capabilities probed PER DIRECTORY (ADR 0054), keyed by the
    /// directory's identity (`dev`, `ino`, `ctime`): two paths to the same
    /// place are one entry, a `..` or a symlink does not multiply the
    /// probing, and a reused inode does not inherit the dead one's answer.
    /// Bounded, with eviction of the oldest — a long session cannot end up
    /// with a map of every directory it visited.
    caps_at: std::sync::Arc<std::sync::Mutex<CapsAtCache>>,
    /// Substitute for `$XDG_DATA_HOME` for the freedesktop trash; `None` =
    /// resolve from the environment, which is what production does.
    trash_home: Option<PathBuf>,
    /// Keeps an external resource alive (e.g. a test's `TempDir`).
    _guard: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl LocalProvider {
    /// Provider rooted in `base` (must be an existing directory).
    ///
    /// Probes nothing: capability probing is lazy (it starts on the first
    /// async operation, in `spawn_blocking`, only once).
    #[must_use]
    pub fn rooted(base: impl Into<PathBuf>) -> Self {
        let base = base.into();
        // Verbatim (`\\?\`) requires an absolute, normalized path; on
        // Windows, `absolute` uses GetFullPathNameW (separators and `..`
        // resolved).
        let base = std::path::absolute(&base).unwrap_or(base);
        Self {
            base,
            caps: std::sync::Arc::new(std::sync::OnceLock::new()),
            caps_at: std::sync::Arc::new(std::sync::Mutex::new(CapsAtCache::default())),
            trash_home: None,
            _guard: None,
        }
    }

    /// Forces the component-by-component walk even when the kernel has
    /// `openat2`, for as long as the guard lives (test seam): it is the
    /// only way to exercise both branches of confinement on the same
    /// machine.
    #[cfg(target_os = "linux")]
    #[doc(hidden)]
    #[must_use]
    pub fn force_component_walk_for_test() -> crate::confined::ForceComponentWalk {
        crate::confined::ForceComponentWalk::new()
    }

    /// How many times a location has REALLY been probed (test seam: what
    /// the cache saves is not visible any other way).
    #[doc(hidden)]
    #[must_use]
    pub fn caps_at_probe_count(&self) -> u64 {
        self.caps_at.lock().expect("caps_at lock is healthy").probes
    }

    /// Probes capabilities ONCE, inside `spawn_blocking` (rule 2: no
    /// blocking I/O on the runtime): every async operation calls this
    /// before touching the FS. Once probed, it's a free atomic read.
    async fn ensure_caps(&self) {
        if self.caps.get().is_some() {
            return;
        }
        let caps = std::sync::Arc::clone(&self.caps);
        let base = self.base.clone();
        let _ = blocking(move || {
            caps.get_or_init(|| probe_capabilities(&base));
            Ok(())
        })
        .await;
    }

    /// Attaches a guard that lives as long as the provider (for tests that
    /// root in a `TempDir`).
    #[doc(hidden)]
    #[must_use]
    #[expect(
        clippy::used_underscore_binding,
        reason = "the field exists only for its Drop"
    )]
    pub fn with_guard(mut self, guard: Box<dyn std::any::Any + Send + Sync>) -> Self {
        self._guard = Some(guard);
        self
    }

    /// Overrides `$XDG_DATA_HOME` for the freedesktop trash: the "home"
    /// trash becomes `<dir>/Trash`.
    ///
    /// This is a TEST SEAM, and it exists because a test cannot touch the
    /// developer's real trash nor find out which device it lives on:
    /// `std::env::set_var` is `unsafe` in the 2024 edition (forbidden
    /// outside the justified uses of rule 5) and is also global to the
    /// process. Production never calls this and resolves from the
    /// environment.
    ///
    /// `dir` must be ABSOLUTE — a trash root relative to the cwd is not a
    /// root — and, for [`Provider::trash`] to be able to NAME its
    /// destination, it must fall under this provider's root.
    #[doc(hidden)]
    #[must_use]
    pub fn with_trash_home(mut self, dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        // A relative root would be silently ignored downstream and the
        // trash would end up being the victim's mount's trash, which is a
        // baffling failure in a test (encoding-auditor MINOR-3).
        debug_assert!(dir.is_absolute(), "the trash root is absolute");
        self.trash_home = Some(dir);
        self
    }

    /// The `VPath` of a NATIVE path under this provider's root — the
    /// inverse of [`Self::native`].
    ///
    /// `None` if the path does not hang off the root: then this provider
    /// CANNOT name it, and whoever asks has to go without a path instead of
    /// getting one that doesn't resolve. This happens with a rooted
    /// provider (tests) whose trash falls outside it; with `os_root`, which
    /// is what the daemon registers, the root is `/` and it never happens.
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    fn vpath_of(&self, native: &Path) -> Option<VPath> {
        use std::path::Component;
        let rel = native.strip_prefix(&self.base).ok()?;
        let mut out = Self::root();
        for comp in rel.components() {
            let Component::Normal(os) = comp else {
                return None;
            };
            out = out.join(Segment::new(os_to_bytes(os)).ok()?);
        }
        Some(out)
    }

    /// Provider that serves the WHOLE OS filesystem: unix roots at `/`;
    /// Windows uses an empty base (the `VPath`'s first segment is the
    /// drive, e.g. `C:`) and default OS capabilities without probing (the
    /// root is not writable and sensitivity varies by volume).
    #[must_use]
    pub fn os_root() -> Self {
        if cfg!(windows) {
            let s = Self {
                base: PathBuf::new(),
                caps: std::sync::Arc::new(std::sync::OnceLock::new()),
                caps_at: std::sync::Arc::new(std::sync::Mutex::new(CapsAtCache::default())),
                trash_home: None,
                _guard: None,
            };
            let _ = s.caps.set(Capabilities {
                flags: CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
                max_path: Some(32767),
            });
            s
        } else {
            let s = Self::rooted("/");
            // The OS root is not probed (running as root, the probe WOULD
            // WRITE to `/`): OS defaults, like the Windows branch.
            let _ = s.caps.set(default_capabilities());
            s
        }
    }

    /// This provider's root: `file:///`.
    ///
    /// # Panics
    /// Never: the scheme is constant and valid.
    #[must_use]
    pub fn root() -> VPath {
        VPath::root(Scheme::new("file").expect("constant, valid scheme"), None)
    }

    fn native(&self, p: &VPath) -> Result<PathBuf, Error> {
        // This provider only serves `file://` with no authority: anything
        // else is a path from ANOTHER provider — serving it would be
        // corruption.
        if p.scheme() != "file" || p.authority().is_some() {
            return Err(Error::InvalidPath);
        }
        to_native(&self.base, p)
    }
}

/// Native trash. macOS: `NSFileManager` (headless, no TCC prompts) — the
/// crate's default would be Finder via osascript: it would hang the task on
/// an Automation prompt and die in CI (finding B1, ADR 0009).
#[cfg(target_os = "macos")]
fn trash_delete(p: &Path) -> Result<(), trash::Error> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut ctx = trash::TrashContext::default();
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx.delete(p)
}

/// Native trash delegated to the `trash` crate (Recycle Bin, and the unix
/// systems that aren't freedesktop). On freedesktop it does NOT exist: the
/// trash is implemented by [`crate::trash_fdo`], which also knows how to say
/// where it left the file.
#[cfg(not(any(
    target_os = "macos",
    all(unix, not(target_os = "ios"), not(target_os = "android")),
)))]
fn trash_delete(p: &Path) -> Result<(), trash::Error> {
    trash::delete(p)
}

/// Staging counter: together with the pid it makes the `.norte-partial`
/// name unique (two writes to the same destination never share staging,
/// and a REAL user file named `x.norte-partial` is never touched).
static PARTIAL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Prefix of every norte staging file (ephemeral write and stable resume):
/// the GC uses it to recognize partials (ADR 0012).
const PARTIAL_PREFIX: &str = ".norte-partial.";

/// Length (in hex chars) of the stable name's hash: 32 = 128 bits.
const STABLE_HASH_HEX: usize = 32;

/// Path of the STABLE resume staging for destination `p`: same directory,
/// name `.norte-partial.<sha256-128-of-the-final-name>` — 47 bytes (nowhere
/// near `NAME_MAX`) and rediscoverable across invocations AND across Rust
/// versions. SHA-256 truncated to 128 bits: accidental collision impossible
/// (birthday 2^64) and adversarial 2^64 (names from an untrusted file) —
/// encoding-auditor finding H1/H3. Hashes the RAW BYTES of the name (rule
/// 1), never decodes it.
fn stable_partial_vpath(p: &VPath) -> Result<VPath, Error> {
    let name = p.file_name().ok_or(Error::InvalidPath)?;
    let seg = Segment::new(stable_partial_name(name.as_bytes())).map_err(|_| Error::InvalidPath)?;
    p.with_file_name(seg).ok_or(Error::InvalidPath)
}

/// The stable staging name from the BYTES of the final name.
///
/// The half of [`stable_partial_vpath`] that doesn't need a `VPath`, because
/// the confined root addresses by segments and has none to give it. A
/// single definition: two ways of naming the same staging would be two
/// files where resume expects one.
pub(crate) fn stable_partial_name(final_name: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(final_name);
    let mut hex = String::with_capacity(STABLE_HASH_HEX);
    for b in &digest[..STABLE_HASH_HEX / 2] {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    format!("{PARTIAL_PREFIX}{hex}").into_bytes()
}

/// The mode with which whatever went through the STABLE staging is
/// PUBLISHED (#299).
///
/// That staging is born `0o600` and cannot be born any other way: its name
/// is predictable, so for as long as it lasts it has to be ours and no one
/// else's (#298). But publishing is a `rename`, which doesn't touch the
/// mode, and without this a RESUMED copy would end up `0o600` while the
/// same uninterrupted copy ends up `0o644`. Same operation, two results,
/// and resumable is today the default path for a leaf.
///
/// So it reproduces what a `create` would have given: `0o666` trimmed by
/// the umask. What it does NOT do is preserve the SOURCE's mode — that's
/// what `cp -p` does, and it's a product decision norte hasn't made yet
/// (today it preserves no permissions in any copy); sneaking it in here
/// would be deciding it by default inside a fix.
#[cfg(unix)]
pub(crate) fn modo_publicado() -> u32 {
    0o666 & !process_umask()
}

/// The process's umask, WITHOUT changing it.
///
/// `umask(2)` only returns it by setting it, and that's global to the
/// process: doing it here would race any other write in flight, in a
/// daemon that writes from many tasks at once. Linux publishes it
/// read-only in `/proc/self/status` (`Umask:`, since 4.7).
///
/// Where it can't be read, `0o022` is assumed, which is a common
/// configuration's and gives the usual `0o644`. Assuming less — `0o000` —
/// would publish more openly than the user asked for, and that never
/// happens, not even once.
#[cfg(unix)]
fn process_umask() -> u32 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(value) = line.strip_prefix("Umask:")
                    && let Ok(u) = u32::from_str_radix(value.trim(), 8)
                {
                    return u;
                }
            }
        }
    }
    0o022
}

/// Gives the file JUST PUBLISHED the mode it would have had if the copy had
/// not been interrupted (#299). Does nothing if the staging wasn't the
/// stable one.
///
/// Operates on the DESCRIPTOR, which keeps pointing at the same inode after
/// the rename: by path there would be a window between publishing and
/// adjusting during which someone else could replace the name and receive
/// the `chmod`.
///
/// **Best-effort on purpose, and silent.** A failure here leaves the file
/// at `0o600`: copied, with good bytes and a good name, and more
/// restrictive than requested. Turning it into an error would throw away an
/// entire copy over a permission. And it isn't logged because this crate
/// has no `tracing` — it's the only one allowed to use `unsafe` and it
/// stays free of instrumentation dependencies —; whoever wants to know
/// looks at the file's mode.
#[cfg(unix)]
pub(crate) fn reponer_modo_publicado(file: &std::fs::File, stable: bool) {
    use std::os::fd::AsRawFd as _;

    if !stable {
        return;
    }
    // SAFETY: `file` is alive and its fd is valid for the whole call.
    // `fchmod` takes no pointers.
    #[allow(unsafe_code)]
    let _ = unsafe { libc::fchmod(file.as_raw_fd(), modo_publicado() as libc::mode_t) };
}

/// Windows has no POSIX mode to restore: the file inherits its directory's
/// ACL and the staging was never restricted by hand.
#[cfg(windows)]
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn reponer_modo_publicado(_file: &std::fs::File, _stable: bool) {}

/// Opens (or creates) the stable staging of `path` to RESUME, and says how
/// many bytes there already were.
///
/// Unix checks what it opened — `O_NOFOLLOW`, regular file, single link,
/// ours, `0o600` — because anyone who knows the destination name can
/// compute the staging name (#298).
#[cfg(unix)]
fn open_stable_staging(path: &std::path::Path) -> Result<(std::fs::File, u64), Error> {
    crate::confined::abre_staging_estable(path)
}

/// Windows: without #298's checks yet. A reparse point planted with the
/// staging's name is the same hole, and there it isn't closed with an
/// `open` flag — it needs `NtCreateFile` with `FILE_OPEN_REPARSE_POINT`,
/// which is what #220 and #217 have open.
#[cfg(windows)]
fn open_stable_staging(path: &std::path::Path) -> Result<(std::fs::File, u64), Error> {
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|e| map_io(&e))?;
    let already = file.metadata().map_err(|e| map_io(&e))?.len();
    Ok((file, already))
}

/// Opens the stable staging of `path` to READ its prefix, or `None` if
/// there isn't one of ours. Same checks and same reason as
/// [`open_stable_staging`].
#[cfg(unix)]
fn open_partial_for_digest(path: &std::path::Path) -> Result<Option<std::fs::File>, Error> {
    crate::confined::abre_parcial_verificado(path)
}

#[cfg(windows)]
fn open_partial_for_digest(path: &std::path::Path) -> Result<Option<std::fs::File>, Error> {
    match std::fs::File::open(path) {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(map_io(&e)),
    }
}

/// EPHEMERAL staging name for destination `final_name`:
/// `.norte-partial.<16 hex>.<pid>-<seq>`.
///
/// pid + sequence: (a) a real user file is never touched (whoever opens it
/// with `O_EXCL`/`create_new` also guarantees this) and (b) two concurrent
/// writes to the same destination don't share staging. The prefix makes it
/// recognizable to the GC (ADR 0012) — its shape is validated by
/// [`is_norte_partial`], so whoever builds it must do so HERE and not in a
/// second copy of the `format!`.
pub(crate) fn ephemeral_partial_name(final_name: &[u8]) -> Vec<u8> {
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        final_name.hash(&mut h);
        h.finish()
    };
    let seq = PARTIAL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{PARTIAL_PREFIX}{hash:016x}.{}-{seq}", std::process::id()).into_bytes()
}

/// Does `name` (bytes) have the SHAPE of a norte staging file? Narrow to
/// the two known shapes — NOT to the bare prefix (encoding-auditor H2: a
/// real user file `.norte-partial.backup` must NOT be swept):
/// - stable: prefix + exactly 32 hex.
/// - ephemeral: prefix + 16 hex + `.` + <pid> + `-` + <seq>.
fn is_norte_partial(name: &[u8]) -> bool {
    let Some(rest) = name.strip_prefix(PARTIAL_PREFIX.as_bytes()) else {
        return false;
    };
    let is_hex = |b: &u8| b.is_ascii_digit() || (b'a'..=b'f').contains(b);
    // Stable: 32 hex and nothing else.
    if rest.len() == STABLE_HASH_HEX && rest.iter().all(is_hex) {
        return true;
    }
    // Ephemeral: <16 hex>.<pid>-<seq>, all digits/hex and separators.
    let Some(dot) = rest.iter().position(|&b| b == b'.') else {
        return false;
    };
    let (hash, tail) = rest.split_at(dot);
    if hash.len() != 16 || !hash.iter().all(is_hex) {
        return false;
    }
    // tail = ".<pid>-<seq>": digits, a '-', digits.
    let tail = &tail[1..];
    let Some(dash) = tail.iter().position(|&b| b == b'-') else {
        return false;
    };
    let (pid, seq) = tail.split_at(dash);
    !pid.is_empty()
        && pid.iter().all(u8::is_ascii_digit)
        && seq.len() > 1
        && seq[1..].iter().all(u8::is_ascii_digit)
}

/// Maps an OS error to the protocol's taxonomy (spec §17.7): frontends
/// render by category, they never parse OS strings.
pub(crate) fn map_io(e: &std::io::Error) -> Error {
    use std::io::ErrorKind as K;
    // EILSEQ: the FS rejects the name's BYTES (APFS requires valid UTF-8).
    // std leaves it as `Uncategorized`, so the raw errno is checked. With
    // the short staging (issue #4) this rejection arrives at the commit
    // rename — without this mapping it would be an opaque `Io` (regression
    // caught in macOS CI).
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::EILSEQ) {
        return Error::InvalidPath;
    }
    match e.kind() {
        K::NotFound => Error::NotFound,
        K::PermissionDenied => Error::PermissionDenied,
        K::AlreadyExists => Error::Conflict {
            conflict: ConflictKind::Exists,
        },
        K::DirectoryNotEmpty | K::NotADirectory | K::IsADirectory => Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        },
        K::StorageFull | K::QuotaExceeded => Error::NoSpace,
        // EXDEV: the FS can't rename across devices — Unsupported triggers
        // the engine's degradation of the move to copy+delete.
        K::CrossesDevices => Error::Unsupported,
        // InvalidFilename = ENAMETOOLONG / invalid name for the FS: a PATH
        // problem (the frontend should say "name too long", not "I/O
        // error").
        K::InvalidFilename | K::InvalidInput => Error::InvalidPath,
        K::Interrupted | K::TimedOut | K::WouldBlock => Error::Io { retryable: true },
        _ => Error::Io { retryable: false },
    }
}

/// Runs a stream producer in `spawn_blocking` guarded against panics: a
/// mid-stream panic must NOT pass as a clean end-of-stream (that would be a
/// silently truncated listing or read) — the consumer receives
/// `Internal{panic}`.
fn spawn_guarded_producer<T: Send + 'static>(
    tx: tokio::sync::mpsc::Sender<Result<T, Error>>,
    body: impl FnOnce(&tokio::sync::mpsc::Sender<Result<T, Error>>) + Send + 'static,
) {
    tokio::task::spawn_blocking(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&tx)));
        if result.is_err() {
            let _ = tx.blocking_send(Err(Error::Internal { panic: true }));
        }
    });
}

/// Runs blocking I/O; a panic inside it is supervised and does NOT bring
/// down the process (panic policy from spec §17.7).
/// What `capabilities_at` waits for the filesystem to answer (#213).
///
/// The same number used by the core's volume queries
/// (`SPACE_QUERY_DEADLINE`, `ENUMERATE_DEADLINE`, `QUERY_DEADLINE`) and for
/// the same reason: the ladder is a `statfs` and an `ioctl` over a live
/// mount — microseconds — so 200 ms doesn't clip any real response and does
/// bound how long a dead mount can make whoever's asking wait.
const CAPS_AT_DEADLINE: std::time::Duration = std::time::Duration::from_millis(200);

pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| Error::Internal { panic: true })?
}

fn mtime_ms(md: &std::fs::Metadata) -> Option<i64> {
    let t = md.modified().ok()?;
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).ok(),
        Err(e) => i64::try_from(e.duration().as_millis()).ok().map(|v| -v),
    }
}

fn entry_from(path: VPath, md: &std::fs::Metadata, req: &norte_vfs::AttrRequest) -> Entry {
    let ft = md.file_type();
    let (kind, size) = if ft.is_symlink() {
        (EntryKind::Symlink, None)
    } else if ft.is_dir() {
        (EntryKind::Dir, None)
    } else if ft.is_file() {
        (EntryKind::File, Some(md.len()))
    } else {
        (EntryKind::Other, None)
    };
    Entry {
        attrs: attrs_from_md(md, req),
        path,
        kind,
        size,
        mtime_ms: mtime_ms(md),
    }
}

/// Attr catalogue of the local provider (#108 block 2): POSIX on unix,
/// `win.attributes` on Windows. Everything comes from the `Metadata`
/// already in hand — zero extra syscalls beyond `stat`; in `list` it
/// requires per-entry promotion (see `list_with`).
#[cfg(unix)]
fn local_catalog() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        let mk = |id: &str, label: &str, ty, hint| AttrInfo {
            id: id.to_owned(),
            label: label.to_owned(),
            ty,
            hint,
        };
        vec![
            mk("posix.mode", "Mode", AttrType::Uint, AttrHint::Mode),
            mk("posix.uid", "UID", AttrType::Uint, AttrHint::Identity),
            mk("posix.gid", "GID", AttrType::Uint, AttrHint::Identity),
            // The NAMES (ADR 0145): bytes, because POSIX doesn't require a
            // username to be UTF-8. They cost one NSS lookup per distinct
            // id, so they're resolved only if requested.
            mk("posix.owner", "Owner", AttrType::Bytes, AttrHint::Identity),
            mk("posix.group", "Group", AttrType::Bytes, AttrHint::Identity),
            mk("posix.nlink", "Links", AttrType::Uint, AttrHint::Opaque),
            mk(
                "posix.ctime_ms",
                "Changed",
                AttrType::TimeMs,
                AttrHint::Timestamp,
            ),
        ]
    });
    &CAT
}

/// See [`local_catalog`] (Windows variant).
#[cfg(windows)]
fn local_catalog() -> &'static [norte_proto::AttrInfo] {
    use norte_proto::{AttrHint, AttrInfo, AttrType};
    static CAT: std::sync::LazyLock<Vec<AttrInfo>> = std::sync::LazyLock::new(|| {
        vec![AttrInfo {
            id: "win.attributes".to_owned(),
            label: "Attributes".to_owned(),
            ty: AttrType::Uint,
            hint: AttrHint::Opaque,
        }]
    });
    &CAT
}

#[cfg(not(any(unix, windows)))]
fn local_catalog() -> &'static [norte_proto::AttrInfo] {
    &[]
}

/// Materializes the requested attrs from a `Metadata` ALREADY in hand.
/// `mode` is the raw `st_mode` (type bits included); the formatters decide
/// the presentation (octal/rwx).
fn attrs_from_md(
    md: &std::fs::Metadata,
    req: &norte_vfs::AttrRequest,
) -> std::collections::BTreeMap<String, norte_proto::AttrValue> {
    use norte_proto::AttrValue;
    let mut out = std::collections::BTreeMap::new();
    if req.is_empty() {
        return out;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if req.wants("posix.mode") {
            out.insert(
                "posix.mode".to_owned(),
                AttrValue::Uint(u64::from(md.mode())),
            );
        }
        if req.wants("posix.uid") {
            out.insert("posix.uid".to_owned(), AttrValue::Uint(u64::from(md.uid())));
        }
        if req.wants("posix.gid") {
            out.insert("posix.gid".to_owned(), AttrValue::Uint(u64::from(md.gid())));
        }
        // With no name (an orphan uid, a downed NSS) the cell stays blank:
        // inventing the number instead would say something else, and
        // that's what `posix.uid` and `posix.gid` are already for.
        if req.wants("posix.owner")
            && let Some(n) = crate::identidad::usuario(md.uid())
        {
            out.insert("posix.owner".to_owned(), AttrValue::Bytes(n));
        }
        if req.wants("posix.group")
            && let Some(n) = crate::identidad::grupo(md.gid())
        {
            out.insert("posix.group".to_owned(), AttrValue::Bytes(n));
        }
        if req.wants("posix.nlink") {
            out.insert("posix.nlink".to_owned(), AttrValue::Uint(md.nlink()));
        }
        if req.wants("posix.ctime_ms") {
            // ctime in ms, exactly floor(real ms): tv_nsec ∈ [0, 1e9), so
            // even pre-1970 the deviation is < 1ms (rounding toward −∞).
            // Saturating: a forged FUSE/image can return st_ctime near
            // i64::MAX and the overflow would kill the listing.
            let ms = md
                .ctime()
                .saturating_mul(1000)
                .saturating_add(md.ctime_nsec() / 1_000_000);
            out.insert("posix.ctime_ms".to_owned(), AttrValue::TimeMs(ms));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if req.wants("win.attributes") {
            out.insert(
                "win.attributes".to_owned(),
                AttrValue::Uint(u64::from(md.file_attributes())),
            );
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = md;
    out
}

/// Given an already confirmed collision: is the EXACT name (bytes) in the
/// directory, a Unicode normalization variant (macOS NFD, issue #8), or a
/// case variant? The collision is evaluated against the destination FS.
fn collision_kind_for(path: &Path) -> ConflictKind {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return ConflictKind::Exists;
    };
    match std::fs::read_dir(parent) {
        Ok(rd) => {
            // Precedence: byte-exact > case > normalization — on NTFS
            // (case-insensitive, normalization-sensitive) the real EEXIST
            // comes from the case variant even when an NFD dirent is
            // nearby.
            let mut case_hit = false;
            let mut norm_hit = false;
            for d in rd.flatten() {
                let dn = d.file_name();
                if dn == name {
                    return ConflictKind::Exists;
                }
                if !case_hit && case_eq_os(&dn, name) {
                    case_hit = true;
                }
                if !norm_hit && nfc_eq_os(&dn, name) {
                    norm_hit = true;
                }
            }
            if case_hit {
                ConflictKind::CaseCollision
            } else if norm_hit {
                ConflictKind::Normalization
            } else {
                ConflictKind::CaseCollision
            }
        }
        Err(_) => ConflictKind::Exists,
    }
}

/// Case-only variant? (std's Unicode lowercase; the FS's real fold may be
/// wider — good enough as a label for the frontend).
fn case_eq_os(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    match (a.to_str(), b.to_str()) {
        (Some(a), Some(b)) => a != b && a.to_lowercase() == b.to_lowercase(),
        _ => false,
    }
}

/// Same NFC form? Only comparable if both names are valid UTF-8
/// (normalization isn't defined over arbitrary bytes).
fn nfc_eq_os(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    use unicode_normalization::UnicodeNormalization;
    match (a.to_str(), b.to_str()) {
        (Some(a), Some(b)) => a.nfc().eq(b.nfc()),
        _ => false,
    }
}

/// Probe counter: together with the pid it makes each case probe's name
/// unique (leftovers from a crash or user files never interfere).
static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Case sensitivity via the OS API, without mutating anything: `pathconf`
/// `_PC_CASE_SENSITIVE` (macOS; per-volume). `None` = undetermined.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn case_sensitivity_from_os(base: &Path) -> Option<bool> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(base.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a NUL-terminated CString alive for the whole call;
    // `_PC_CASE_SENSITIVE` is an ABI constant. The result is validated in
    // `tests/local.rs::capabilities_are_probed` against the real FS of CI.
    let rc = unsafe { libc::pathconf(c.as_ptr(), libc::_PC_CASE_SENSITIVE) };
    match rc {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// Rest of the OSes: no reliable API — the write probe is used.
#[cfg(not(target_os = "macos"))]
fn case_sensitivity_from_os(_base: &Path) -> Option<bool> {
    None
}

/// Write-based sensitivity probe: creates a probe with a UNIQUE name (pid +
/// sequence) ending in `-A` and checks whether the `-a` variant resolves to
/// the SAME file — identity `(dev, ino)`, not `exists()`: an unrelated file
/// with the same name or a symlink would lie (issue #5).
/// `None` if `base` isn't writable or the probe is undetermined.
fn probe_case_sensitivity(base: &Path) -> Option<bool> {
    let seq = PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    // verbatim: the probe must also work under paths >260 on Windows (same
    // promise as the rest of the provider).
    let upper = verbatim(base.join(format!(".norte-probe-{pid}-{seq}-A")));
    let lower = verbatim(base.join(format!(".norte-probe-{pid}-{seq}-a")));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&upper)
        .ok()?;
    let sensitive = match (file.metadata(), std::fs::symlink_metadata(&lower)) {
        (Ok(upper_md), Ok(lower_md)) => Some(!probe_same_file(&upper_md, &lower_md)),
        (_, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Some(true),
        _ => None,
    };
    drop(file);
    let _ = std::fs::remove_file(&upper);
    sensitive
}

/// Is the lowercase variant of the probe the probe's OWN file?
#[cfg(unix)]
fn probe_same_file(upper_md: &std::fs::Metadata, lower_md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    upper_md.dev() == lower_md.dev() && upper_md.ino() == lower_md.ino()
}

/// Windows: std doesn't expose the real identity, but the probe has a
/// unique name (pid + sequence) — the variant merely "existing" already
/// means the FS folds case. Good enough here; it wouldn't be for `rename`.
#[cfg(windows)]
fn probe_same_file(_upper_md: &std::fs::Metadata, _lower_md: &std::fs::Metadata) -> bool {
    true
}

/// Real node identity on unix: `(dev, ino)` from lstat/stat depending on
/// `follow`. It's the same identity already used by the case probe and
/// case-rename (`same_node`) — here it's exposed through the trait
/// (issue #16).
#[cfg(unix)]
fn node_id_native(
    p: &Path,
    follow: norte_vfs::FollowLinks,
) -> Result<Option<norte_vfs::NodeId>, Error> {
    use std::os::unix::fs::MetadataExt;
    let md = match follow {
        norte_vfs::FollowLinks::No => std::fs::symlink_metadata(p),
        norte_vfs::FollowLinks::Yes => std::fs::metadata(p),
    }
    .map_err(|e| map_io(&e))?;
    Ok(Some(norte_vfs::NodeId {
        volume: md.dev(),
        index: u128::from(md.ino()),
    }))
}

/// Real node identity on Windows: `FILE_ID_INFO` (u64 volume serial +
/// 128-bit `FileId`, covers `ReFS`) via `GetFileInformationByHandleEx`. If
/// the volume doesn't support it (FAT32, old SMB), it degrades to
/// `Ok(None)` — "no stable identity here" is the contract's honest answer,
/// never a made-up id.
#[cfg(windows)]
#[allow(unsafe_code)]
fn node_id_native(
    p: &Path,
    follow: norte_vfs::FollowLinks,
) -> Result<Option<norte_vfs::NodeId>, Error> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_128, FILE_ID_INFO,
        FileIdInfo, GetFileInformationByHandleEx,
    };

    // access_mode 0 = query metadata only (neither read nor write: works
    // even without read permission). BACKUP_SEMANTICS is mandatory to open
    // directories; OPEN_REPARSE_POINT gives the identity of the link
    // ITSELF (lstat semantics) when follow = No.
    let mut opts = std::fs::OpenOptions::new();
    opts.access_mode(0);
    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if follow == norte_vfs::FollowLinks::No {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    opts.custom_flags(flags);
    let file = opts.open(p).map_err(|e| map_io(&e))?;

    let mut info = FILE_ID_INFO {
        VolumeSerialNumber: 0,
        FileId: FILE_ID_128 {
            Identifier: [0; 16],
        },
    };
    // SAFETY: the handle is valid and lives for the whole call (file isn't
    // dropped before it); the buffer is exactly a FILE_ID_INFO and the size
    // passed is size_of the same type. Contract verified in the test
    // `node_id_identifies_the_same_file` (and node_id's contract suite)
    // against the real FS of Windows CI.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle().cast(),
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast(),
            u32::try_from(std::mem::size_of::<FILE_ID_INFO>()).expect("small fixed size"),
        )
    };
    if ok == 0 {
        // The volume can't give a 128-bit FileId: no stable identity.
        return Ok(None);
    }
    Ok(Some(norte_vfs::NodeId {
        volume: info.VolumeSerialNumber,
        index: u128::from_le_bytes(info.FileId.Identifier),
    }))
}

/// Effective kind of a symlink to create when the caller passed `Unknown`
/// (issue #18): resolves the target RELATIVE TO THE LINK'S PARENT on this
/// FS, following the chain (metadata). A broken or undeterminable target
/// degrades to `File` — the same default as a `mklink` without `/D`. Only
/// Windows consults this (unix ignores the kind); it's compiled on every OS
/// so it can be tested on any CI.
#[cfg_attr(unix, allow(dead_code))]
fn effective_symlink_kind(
    link: &Path,
    target: &std::ffi::OsStr,
    kind: norte_vfs::SymlinkKind,
) -> norte_vfs::SymlinkKind {
    match kind {
        norte_vfs::SymlinkKind::Unknown => {
            let resolved = match link.parent() {
                // join with an absolute target RESPECTS it (Path semantics).
                Some(parent) => parent.join(target),
                None => std::path::PathBuf::from(target),
            };
            // `link` arrives verbatim (`\\?\`) on Windows, and under
            // verbatim the kernel does NOT fold `..` nor convert `/`:
            // normalize lexically (GetFullPathNameW via `absolute`) and
            // re-apply verbatim before probing, or a relative target with
            // `..` would degrade to File even when it points at a dir
            // (encoding-auditor finding).
            // Drive-relative target (`C:foo`): unresolvable without that
            // drive's CWD — degrades to File, documented.
            let resolved = verbatim(std::path::absolute(&resolved).unwrap_or(resolved));
            match std::fs::metadata(&resolved) {
                Ok(md) if md.is_dir() => norte_vfs::SymlinkKind::Dir,
                _ => norte_vfs::SymlinkKind::File,
            }
        }
        explicit => explicit,
    }
}

/// Creates the native symlink. No pre-check for collision needed: the
/// syscall fails with EEXIST atomically.
#[cfg(unix)]
fn make_symlink(
    target: &std::ffi::OsStr,
    link: &Path,
    _kind: norte_vfs::SymlinkKind,
) -> Result<(), Error> {
    std::os::unix::fs::symlink(target, link).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            Error::Conflict {
                conflict: collision_kind_for(link),
            }
        } else {
            map_io(&e)
        }
    })
}

/// Windows distinguishes file/dir at creation; it requires a privilege
/// (`SeCreateSymbolicLinkPrivilege` or Developer Mode) — that's why the
/// provider doesn't declare `SYMLINKS` on Windows and this path answers via
/// `Unsupported` before reaching here, except for future probes.
#[cfg(windows)]
fn make_symlink(
    target: &std::ffi::OsStr,
    link: &Path,
    kind: norte_vfs::SymlinkKind,
) -> Result<(), Error> {
    let res = match effective_symlink_kind(link, target, kind) {
        norte_vfs::SymlinkKind::Dir => std::os::windows::fs::symlink_dir(target, link),
        // `Unknown` was already resolved above; this arm exists for exhaustiveness.
        norte_vfs::SymlinkKind::File | norte_vfs::SymlinkKind::Unknown => {
            std::os::windows::fs::symlink_file(target, link)
        }
    };
    res.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            Error::Conflict {
                conflict: collision_kind_for(link),
            }
        } else {
            map_io(&e)
        }
    })
}

/// Default OS capabilities, without touching the FS: what `capabilities()`
/// answers if no async operation has run yet.
fn default_capabilities() -> Capabilities {
    let mut flags = CapabilityFlags::RENAME_ATOMIC
        | CapabilityFlags::CASE_PRESERVING
        // Local FS: write at an arbitrary offset and append (resume M2).
        | CapabilityFlags::APPEND
        | CapabilityFlags::RANDOM_WRITE
        // Native trash on all 3 OSes (crate trash, ADR 0009).
        | CapabilityFlags::TRASH;
    if cfg!(unix) {
        // Creating symlinks on Windows requires a privilege: not declared in M0.
        flags |= CapabilityFlags::SYMLINKS;
        // POSIX permissions (#314): Windows has none — `set_permissions`
        // only knows the read-only bit —, and announcing them there would
        // promise that `fs.set_mode` does something it doesn't.
        flags |= CapabilityFlags::POSIX_MODE;
    }
    if cfg!(all(unix, not(target_os = "macos"))) {
        flags |= CapabilityFlags::CASE_SENSITIVE;
    }
    Capabilities {
        flags,
        // With the verbatim prefix, Windows's real limit is 32767 UTF-16.
        max_path: cfg!(windows).then_some(32767),
    }
}

/// Bounded per-directory capability cache, keyed by the node's identity.
///
/// It's not an access LRU but an INSERTION one: what must be prevented is a
/// long walk growing the map without end, and evicting the oldest entry is
/// enough for that. A comparator touches a handful of roots; an indexer
/// that walks thousands will pay one extra syscall when it comes back to
/// the first one, which is cheaper than the bookkeeping of a real LRU.
#[derive(Debug, Default)]
struct CapsAtCache {
    /// `(dev, ino)` → already-probed capabilities.
    map: std::collections::HashMap<(u64, u64, i64), Capabilities>,
    /// Insertion order, for eviction.
    order: std::collections::VecDeque<(u64, u64, i64)>,
    /// REAL probes (the ones that didn't come out of here). Test seam.
    probes: u64,
}

/// Cache ceiling. A directory occupies a few dozen bytes; 256 comfortably
/// covers the roots of a comparison, a sync, and the two panes.
const CAPS_AT_CACHE_MAX: usize = 256;

impl CapsAtCache {
    fn get(&self, key: (u64, u64, i64)) -> Option<Capabilities> {
        self.map.get(&key).copied()
    }

    fn insert(&mut self, key: (u64, u64, i64), caps: Capabilities) {
        if self.map.insert(key, caps).is_none() {
            self.order.push_back(key);
            while self.order.len() > CAPS_AT_CACHE_MAX {
                if let Some(oldest) = self.order.pop_front() {
                    self.map.remove(&oldest);
                }
            }
        }
    }
}

/// A directory's identity for the cache: `(dev, ino, ctime_nsec)`.
///
/// The `ctime` is there because of inode REUSE, which is what makes
/// `(dev, ino)` alone insufficient: ext4 recycles inode numbers within the
/// same block group, so deleting a `+F` directory and creating an ordinary
/// one can return the same pair and serve it the dead one's answer — a
/// false `FULL_FOLD`, pairing up two files that are distinct. `ctime`
/// changes on every inode reassignment and already comes in the `Metadata`
/// that was just read, so it costs nothing.
///
/// (A LIVE directory's `+F` flag doesn't change: it's inherited at
/// creation, and it can't be set on a non-empty directory nor removed.
/// What's invalidated here is the identity, not the verdict.)
#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "shared signature with Windows, which has no inode; on Unix there's always identity"
)]
fn dir_identity(md: &std::fs::Metadata) -> Option<(u64, u64, i64)> {
    use std::os::unix::fs::MetadataExt as _;
    Some((md.dev(), md.ino(), md.ctime_nsec()))
}

/// Windows: `std` doesn't expose the volume identity nor the file index
/// from `Metadata`, so there's no key here and every question gets probed.
/// On Windows the read-only ladder doesn't answer anything yet (see
/// `caps_at::windows`), so probing is reading a `Metadata` and little else.
#[cfg(windows)]
fn dir_identity(_md: &std::fs::Metadata) -> Option<(u64, u64, i64)> {
    None
}

/// A directory's cache key, by stat-ing it.
fn dir_key(dir: &Path) -> Option<(u64, u64, i64)> {
    std::fs::metadata(dir).ok().as_ref().and_then(dir_identity)
}

fn probe_capabilities(base: &Path) -> Capabilities {
    let mut caps = default_capabilities();
    let default_sensitive = caps.flags.contains(CapabilityFlags::CASE_SENSITIVE);
    let sensitive = case_sensitivity_from_os(base)
        .or_else(|| probe_case_sensitivity(base))
        .unwrap_or(default_sensitive);
    caps.flags.set(CapabilityFlags::CASE_SENSITIVE, sensitive);
    caps
}

#[async_trait]
impl Provider for LocalProvider {
    // The trait's signature is `-> &str`; returning a literal here is correct.
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "The trait's signature is `-> &str`; returning a literal here is correct"
    )]
    fn scheme(&self) -> &str {
        "file"
    }

    fn capabilities(&self) -> Capabilities {
        // Pure read (rule 2: no I/O can happen here — this is called from
        // async context). Exact after the first async operation (the probe
        // runs there, in spawn_blocking); before that, the OS default.
        self.caps
            .get()
            .copied()
            .unwrap_or_else(default_capabilities)
    }

    async fn capabilities_at(&self, p: &VPath) -> Result<Capabilities, Error> {
        self.ensure_caps().await;
        let mut declared = self.capabilities();
        // Confinement is a PLATFORM property, not one of the location or
        // the tree's state: on unix there's `openat` — with `openat2` or
        // with the walk, both guarantee the same thing —, and on Windows
        // not yet. It goes before any probe because it must also hold in
        // the degraded path below: otherwise,
        // `file:///destination-that-doesnt-exist-yet` would say "I can't
        // confine" and `file:///` would say yes, which is a different
        // answer for the same machine and the common case of planning a
        // mirror.
        declared
            .flags
            .set(CapabilityFlags::CONFINED_WRITES, cfg!(unix));
        let native = self.native(p)?;
        let cache = std::sync::Arc::clone(&self.caps_at);
        // With a DEADLINE, and on a detached thread (#213). Everything
        // inside — the `symlink_metadata`, the `statfs` and the ladder's
        // `ioctl` — hangs indefinitely over a dead NFS or CIFS, and an
        // in-flight syscall can't be cancelled. In the blocking pool that
        // would tie up a slot SHARED by a downed mount; worse still,
        // `sync.plan` asks this twice BEFORE `sched.submit`, i.e. outside
        // any Task and any `CancellationToken` (hard rule 3): the RPC would
        // hang with nothing to cancel.
        //
        // Once the deadline expires, whatever the provider DECLARED is
        // returned, which is the degradation ADR 0054 already defines for
        // "I don't know", and nothing is cached: a mount that comes back
        // answers next time.
        let declared_on_timeout = declared;
        norte_vfs::deadline::blocking_with_deadline(
            move || {
                // The question is ALWAYS about the directory that CONTAINS
                // the name: what the answer decides is whether two names
                // can coexist there, and that's ruled by the directory
                // they're going to be in. For a file — or for a symlink,
                // which is a name in the link's directory and not in its
                // target's — that's its parent; for a directory, itself.
                // `symlink_metadata`, therefore, and not `metadata`.
                let Ok(md) = std::fs::symlink_metadata(&native) else {
                    // A path that isn't there (or can't be looked at) is
                    // NOT an error here: `capabilities()` could never
                    // fail, and making its per-location version fail would
                    // turn "planning toward a destination that doesn't
                    // exist yet" — the common case of a mirror — into an
                    // error, on top of changing an already published wire
                    // method's contract. What the provider declares is
                    // returned (ADR 0054: degradation is the usual
                    // behavior).
                    return Ok(declared);
                };
                let dir: &Path = if md.is_dir() {
                    &native
                } else {
                    native.parent().unwrap_or(&native)
                };
                let key = dir_key(dir);

                if let Some(k) = key
                    && let Some(hit) = cache.lock().expect("caps_at lock is healthy").get(k)
                {
                    return Ok(hit);
                }

                let found = crate::caps_at::probe_location(dir);
                let mut caps = declared;
                if let Some(sensitive) = found.case_sensitive {
                    caps.flags.set(CapabilityFlags::CASE_SENSITIVE, sensitive);
                }
                // `None` = the ladder didn't know; what was declared is
                // left as is instead of turning off a flag nobody
                // contradicted.
                if let Some(full) = found.full_fold {
                    caps.flags.set(CapabilityFlags::FULL_FOLD, full);
                }
                let mut guard = cache.lock().expect("caps_at lock is healthy");
                guard.probes += 1;
                if let Some(k) = key {
                    guard.insert(k, caps);
                }
                Ok(caps)
            },
            CAPS_AT_DEADLINE,
        )
        .await
        .unwrap_or(Ok(declared_on_timeout))
    }

    /// This PLATFORM's naming rules (#163).
    ///
    /// On unix, any byte sequence without `/` or NUL — and a `Segment`
    /// already guarantees that, so there's nothing to reject here.
    ///
    /// On Windows there is: device names (`CON`, `NUL`, `COM1`…) aren't
    /// files, `<>:"|?*` and control characters aren't legal, and a
    /// TRAILING dot or space is silently stripped by Win32 — leaving a
    /// file that isn't the one requested. `f:ads` is the worst of all,
    /// which is why the colon is on the list: it doesn't fail there, it
    /// writes an alternate data stream, and the copy reports success while
    /// the file isn't there.
    ///
    /// **Unverified on a Windows machine**, like the rest of that
    /// platform's debt (#217, #220, #221, #222): the rules come from Win32
    /// documentation, not from a run. What IS tested is the WIRING — that
    /// a rejected name blocks the plan instead of being discovered at
    /// execution — with a test provider that refuses on purpose.
    fn name_is_legal(&self, name: &[u8]) -> bool {
        /// Win32's device names, which aren't files.
        const RESERVED: &[&[u8]] = &[
            b"CON", b"PRN", b"AUX", b"NUL", b"COM1", b"COM2", b"COM3", b"COM4", b"COM5", b"COM6",
            b"COM7", b"COM8", b"COM9", b"LPT1", b"LPT2", b"LPT3", b"LPT4", b"LPT5", b"LPT6",
            b"LPT7", b"LPT8", b"LPT9",
        ];

        if !cfg!(windows) {
            return true;
        }
        if name.is_empty() {
            return false;
        }
        // The bytes forbidden by Win32, plus control characters.
        if name.iter().any(|b| {
            matches!(
                b,
                0..=0x1F | b'<' | b'>' | b':' | b'"' | b'|' | b'?' | b'*' | b'\\'
            )
        }) {
            return false;
        }
        // Trailing dot or space: Win32 strips them, so the name that's
        // left isn't the one requested.
        if matches!(name.last(), Some(b'.' | b' ')) {
            return false;
        }
        // Device names, with or without an extension after them.
        let stem: &[u8] = name.split(|b| *b == b'.').next().unwrap_or(name);
        !RESERVED
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&stem.to_ascii_uppercase()))
    }

    #[cfg(unix)]
    async fn open_root(&self, root: &VPath) -> Result<Box<dyn norte_vfs::ConfinedRoot>, Error> {
        self.ensure_caps().await;
        let native = self.native(root)?;
        let vpath = root.clone();
        blocking(move || {
            let opened = crate::confined::LocalRoot::open(&native)?;
            Ok(
                Box::new(crate::confined::LocalConfinedRoot::new(opened, vpath))
                    as Box<dyn norte_vfs::ConfinedRoot>,
            )
        })
        .await
    }

    async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.stat_with(p, &norte_vfs::ListOptions::default()).await
    }

    async fn stat_with(&self, p: &VPath, opt: &norte_vfs::ListOptions) -> Result<Entry, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        let vpath = p.clone();
        let req = opt.attrs.clone();
        blocking(move || {
            let md = std::fs::symlink_metadata(&native).map_err(|e| map_io(&e))?;
            Ok(entry_from(vpath, &md, &req))
        })
        .await
    }

    fn attrs(&self) -> &[norte_proto::AttrInfo] {
        local_catalog()
    }

    async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        // Synchronous upfront validation: NotFound / not-a-dir are
        // returned in the Result, not as the stream's first item.
        // `metadata` FOLLOWS symlinks (opendir semantics): listing a
        // dir-symlink lists its target, and a broken link is NotFound —
        // same as the real FS underneath.
        {
            let probe = native.clone();
            blocking(move || {
                let md = std::fs::metadata(&probe).map_err(|e| map_io(&e))?;
                if md.is_dir() {
                    Ok(())
                } else {
                    Err(Error::Conflict {
                        conflict: ConflictKind::TypeMismatch,
                    })
                }
            })
            .await?;
        }
        let base_vpath = p.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Entry, Error>>(64);
        spawn_guarded_producer(tx, move |tx| {
            let rd = match std::fs::read_dir(&native) {
                Ok(rd) => rd,
                Err(e) => {
                    let _ = tx.blocking_send(Err(map_io(&e)));
                    return;
                }
            };
            for dent in rd {
                let item = dent.map_err(|e| map_io(&e)).and_then(|d| {
                    let seg = Segment::new(os_to_bytes(&d.file_name()))
                        .map_err(|_| Error::InvalidPath)?;
                    // #52: kind from readdir's d_type (std only stats on
                    // DT_UNKNOWN); size/mtime LAZY (None = "I don't know",
                    // Entry's contract) — the copy engine hydrates its
                    // leaves (hydrate_plan) and the UI probes the focused
                    // one.
                    let ft = d.file_type().map_err(|e| map_io(&e))?;
                    let kind = if ft.is_symlink() {
                        EntryKind::Symlink
                    } else if ft.is_dir() {
                        EntryKind::Dir
                    } else if ft.is_file() {
                        EntryKind::File
                    } else {
                        EntryKind::Other
                    };
                    Ok(Entry {
                        attrs: std::collections::BTreeMap::new(),
                        path: base_vpath.join(seg),
                        kind,
                        size: None,
                        mtime_ms: None,
                    })
                });
                let stop = item.is_err();
                if tx.blocking_send(item).is_err() {
                    // Receiver dropped: cooperative cancellation of the listing.
                    return;
                }
                if stop {
                    return;
                }
            }
        });
        Ok(ReceiverStream::new(rx).boxed())
    }

    async fn list_with(
        &self,
        p: &VPath,
        opt: &norte_vfs::ListOptions,
    ) -> Result<EntryStream, Error> {
        // No attr advertised in the request: the lazy fast path (#52) stays intact.
        let advertised = local_catalog();
        if !opt
            .attrs
            .iter()
            .any(|id| advertised.iter().any(|a| a.id == id))
        {
            return self.list(p).await;
        }
        self.ensure_caps().await;
        let native = self.native(p)?;
        // Same synchronous upfront validation as `list` (NotFound / not-a-dir
        // in the Result, not as the stream's first item).
        {
            let probe = native.clone();
            blocking(move || {
                let md = std::fs::metadata(&probe).map_err(|e| map_io(&e))?;
                if md.is_dir() {
                    Ok(())
                } else {
                    Err(Error::Conflict {
                        conflict: ConflictKind::TypeMismatch,
                    })
                }
            })
            .await?;
        }
        let base_vpath = p.clone();
        let req = opt.attrs.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Entry, Error>>(64);
        spawn_guarded_producer(tx, move |tx| {
            let rd = match std::fs::read_dir(&native) {
                Ok(rd) => rd,
                Err(e) => {
                    let _ = tx.blocking_send(Err(map_io(&e)));
                    return;
                }
            };
            for dent in rd {
                let item = dent.map_err(|e| map_io(&e)).and_then(|d| {
                    let seg = Segment::new(os_to_bytes(&d.file_name()))
                        .map_err(|_| Error::InvalidPath)?;
                    // Promotion (#108 block 2): requested attrs → one lstat
                    // per entry (`DirEntry::metadata` does NOT follow
                    // symlinks), which also hydrates size/mtime for free.
                    // Still inside the blocking producer — never I/O on
                    // the async executor.
                    //
                    // readdir→lstat race: an entry deleted between the two
                    // no longer exists — it's SKIPPED (None), it doesn't
                    // kill a listing of a live dir (/tmp, build dirs).
                    // Other errors are still fatal, as in the non-promoted
                    // path.
                    match d.metadata() {
                        Ok(md) => Ok(Some(entry_from(base_vpath.join(seg), &md, &req))),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                        Err(e) => Err(map_io(&e)),
                    }
                });
                let item = match item {
                    Ok(None) => continue,
                    Ok(Some(entry)) => Ok(entry),
                    Err(e) => Err(e),
                };
                let stop = item.is_err();
                if tx.blocking_send(item).is_err() {
                    // Receiver dropped: cooperative cancellation of the listing.
                    return;
                }
                if stop {
                    return;
                }
            }
        });
        Ok(ReceiverStream::new(rx).boxed())
    }

    async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<ByteStream, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        let file = blocking(move || {
            // `metadata` FOLLOWS symlinks (open semantics): reading a
            // dir-symlink is TypeMismatch — that's how the copy engine's
            // probe distinguishes file/dir — and a broken link is
            // NotFound.
            let md = std::fs::metadata(&native).map_err(|e| map_io(&e))?;
            if md.is_dir() {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            // Non-regular files (FIFO/socket/device): the open can BLOCK
            // the thread indefinitely (a FIFO with no writer) and
            // cancellation can't interrupt it (rule 3) — an honest
            // rejection BEFORE the open. The engine treats them as
            // Other/Unsupported anyway.
            if !md.is_file() {
                return Err(Error::Unsupported);
            }
            let mut file = std::fs::File::open(&native).map_err(|e| map_io(&e))?;
            if let Some(r) = range {
                use std::io::Seek;
                // pread semantics (ADR 0005): an offset past EOF isn't an
                // error — the stream simply ends empty.
                file.seek(std::io::SeekFrom::Start(r.offset))
                    .map_err(|e| map_io(&e))?;
            }
            Ok(file)
        })
        .await?;
        // `None` = no limit (until EOF).
        let mut remaining: Option<u64> = range.and_then(|r| r.len);
        // 8-chunk buffer: 2 MiB maximum retained if the consumer stalls
        // (with blocking_send the producer waits just as well anyway).
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Error>>(8);
        spawn_guarded_producer(tx, move |tx| {
            use std::io::Read;
            let mut file = file;
            let mut buf = vec![0u8; READ_CHUNK];
            loop {
                let want = match remaining {
                    Some(0) => return,
                    // INVARIANT: min(n, READ_CHUNK=256Ki) always fits.
                    Some(n) => usize::try_from(n.min(READ_CHUNK as u64))
                        .expect("min with READ_CHUNK fits in usize"),
                    None => READ_CHUNK,
                };
                match file.read(&mut buf[..want]) {
                    Ok(0) => return,
                    Ok(n) => {
                        if let Some(rem) = &mut remaining {
                            *rem -= n as u64;
                        }
                        if tx
                            .blocking_send(Ok(Bytes::copy_from_slice(&buf[..n])))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        let _ = tx.blocking_send(Err(map_io(&e)));
                        return;
                    }
                }
            }
        });
        Ok(ReceiverStream::new(rx).boxed())
    }

    async fn node_id(
        &self,
        p: &VPath,
        follow: norte_vfs::FollowLinks,
    ) -> Result<Option<norte_vfs::NodeId>, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        blocking(move || node_id_native(&native, follow)).await
    }

    /// Freedesktop (Linux/BSD): this crate implements the trash (internal
    /// module `trash_fdo`, the freedesktop.org spec), and it NAMES its
    /// destination.
    ///
    /// It's the difference between being able to undo an overwrite and
    /// not: with `Ok(None)` the journal is left without a `reversal_ref`
    /// and undo has to guess by original path, which over a
    /// `trashed`+`created` pair digs up the wrong file. Here the
    /// destination comes from a decision of ours, so it's known.
    ///
    /// `Ok(None)` is still possible in one case: the trash that applies
    /// falls OUTSIDE this provider's root (a rooted provider, a testing
    /// thing — `os_root`, which is what the daemon registers, can't). The
    /// effect already happened; what's missing is a path this provider
    /// knows how to resolve, and returning one that doesn't resolve would
    /// be worse.
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    async fn trash(
        &self,
        p: &VPath,
        id: &norte_vfs::trash::TrashId,
    ) -> Result<Option<VPath>, Error> {
        self.ensure_caps().await;
        if !self.capabilities().flags.contains(CapabilityFlags::TRASH) {
            return Err(Error::Unsupported);
        }
        let native = self.native(p)?;
        let home = self.trash_home.clone();
        let id = *id;
        let dest = blocking(move || crate::trash_fdo::trash(&native, home.as_deref(), &id)).await?;
        Ok(self.vpath_of(&dest))
    }

    /// macOS and Windows: still delegates to the `trash` crate, which
    /// doesn't expose where it put the file — hence the `Ok(None)`, and
    /// hence why [`Provider::trash_restorable`] says no.
    ///
    /// Reimplementing those two platforms' trash isn't the same as
    /// implementing a three-file spec: `NSFileManager` and the Recycle Bin
    /// are APIs with their own index, and faking one would be worse than
    /// telling the truth (issues #25/#26).
    #[cfg(not(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )))]
    async fn trash(
        &self,
        p: &VPath,
        // The OS's NATIVE trash has no stable recoverable destination: the
        // engine's deterministic id (#99) doesn't apply here (dest = None;
        // undo degrades as always in native trash). Only used by the
        // logics.
        _id: &norte_vfs::trash::TrashId,
    ) -> Result<Option<VPath>, Error> {
        self.ensure_caps().await;
        if !self.capabilities().flags.contains(CapabilityFlags::TRASH) {
            return Err(Error::Unsupported);
        }
        let native = self.native(p)?;
        blocking(move || {
            // Existence first: the trash crate gives assorted errors.
            // (Cosmetic TOCTOU: if the victim disappears between the stat
            // and the delete, PermissionDenied comes out instead of
            // NotFound.)
            std::fs::symlink_metadata(&native).map_err(|e| map_io(&e))?;
            trash_delete(&native).map_err(|e| match e {
                trash::Error::CouldNotAccess { .. } => Error::PermissionDenied,
                trash::Error::TargetedRoot => Error::InvalidPath,
                // "No usable trash HERE" (mount without a topdir, no
                // $HOME…): Unsupported — the TUI re-offers PERMANENT with a
                // warning (ADR 0009). The crate's stringly variants: Unknown
                // is its catch-all for "couldn't do it"; upstream also
                // panics on non-UTF8 /proc/mounts (contained by
                // `blocking()` as Internal{panic}).
                trash::Error::Unknown { .. } => Error::Unsupported,
                _ => Error::Io { retryable: false },
            })
        })
        .await?;
        // The OS's NATIVE trash: we don't expose a stable destination
        // path; the restore handle is resolved in undo (M3-2, ADR 0009).
        Ok(None)
    }

    /// Freedesktop yes, **but only if this provider also knows how to NAME
    /// its trash**.
    ///
    /// It's not a platform constant: `trash()` returns the path translated
    /// to `VPath`, and that can't name what falls outside the provider's
    /// root. A provider rooted in a directory whose trash falls outside it
    /// would answer `Ok(None)` after having promised yes — and the journal
    /// would be left without a `reversal_ref` right where the plan said
    /// `RestoreTrash`, which is this whole task's bug wearing another
    /// disguise (encoding-auditor MAJOR-3, security-reviewer MINOR). So the
    /// promise is measured, and whoever can't keep it answers `false`: the
    /// plan marks IRREVERSIBLE steps before anyone approves (hard rule 4).
    ///
    /// `os_root`, which is what the daemon registers, has its root at `/`
    /// and names any path.
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    fn trash_restorable(&self) -> bool {
        self.base == Path::new("/")
            || crate::trash_fdo::home_trash(self.trash_home.as_deref())
                .is_some_and(|t| t.starts_with(&self.base))
    }

    /// macOS and Windows: no — there the `trash` crate provides the trash,
    /// and it doesn't say where it leaves things (issues #25/#26,
    /// ADR 0009).
    #[cfg(not(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )))]
    fn trash_restorable(&self) -> bool {
        false
    }

    /// Pulls out of the freedesktop trash the file [`Provider::trash`]
    /// buried, and takes its sidecar along with it.
    ///
    /// The move first and the sidecar after, never the other way around: a
    /// `files/x` without its `info/x.trashinfo` isn't shown by any
    /// graphical trash, so deleting the metadata and then failing the move
    /// would hide the file instead of returning it. The other way around,
    /// the worst that's left is an orphan sidecar, which is cosmetic.
    ///
    /// Deleting the sidecar is best-effort on purpose: this method's
    /// contract is "the file is back at `original`", and that's already
    /// been fulfilled once the `rename` returned `Ok`. Returning `Err`
    /// because metadata couldn't be cleaned up would make undo count as
    /// blocked an entry that DID get reverted.
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    async fn restore_from(&self, dest: &VPath, original: &VPath) -> Result<(), Error> {
        self.ensure_caps().await;
        let from = self.native(dest)?;
        let to = self.native(original)?;
        blocking(move || {
            do_rename(&from, &to)?;
            crate::trash_fdo::forget_sidecar(&from);
            Ok(())
        })
        .await
    }

    /// Restores from the OS's native trash the item whose ORIGINAL path is
    /// `original` (undo M3-2). Lists the trash (`os_limited`), matches by
    /// original path the most recent item (stable tiebreak by id) and
    /// restores it. Strict: if the destination already exists, `Conflict`
    /// (never overwrites).
    ///
    /// ONLY freedesktop (Linux/BSD): the trash stores the CANONICALIZED
    /// parent (symlinks resolved), so `original`'s parent is canonicalized
    /// before matching; without that, a root hanging off a symlink would
    /// never match. Windows (verbatim prefix vs. the shell's `C:\`) and
    /// macOS/iOS/Android stay `Unsupported` via the trait's default (debt:
    /// Windows/macOS restore).
    #[cfg(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    ))]
    async fn restore_trashed(&self, original: &VPath) -> Result<(), Error> {
        self.ensure_caps().await;
        let native = self.native(original)?;
        blocking(move || {
            // FREE destination (strict: never overwrites). symlink_metadata
            // does NOT follow the link (a dangling symlink at the
            // destination IS "occupied"), consistent with `trash()`.
            if native.symlink_metadata().is_ok() {
                return Err(Error::Conflict {
                    conflict: ConflictKind::Exists,
                });
            }
            // The trash stores `parent.canonicalize().join(name)`: match
            // against that SAME form or the match fails if the root hangs
            // off a symlink.
            let name = native.file_name().ok_or(Error::InvalidPath)?;
            let parent = native.parent().ok_or(Error::InvalidPath)?;
            let target = parent.canonicalize().map_err(|e| map_io(&e))?.join(name);

            let items = trash::os_limited::list().map_err(|_| Error::Unsupported)?;
            let pick = items
                .into_iter()
                .filter(|it| it.original_path() == target)
                // Most recent; tiebreak by id → deterministic under a
                // same-second tie (debt: 1s resolution doesn't distinguish
                // trash-recreate-trash within the same second — capturing
                // the id at delete time would be exact).
                .max_by_key(|it| (it.time_deleted, it.id.clone()))
                .ok_or(Error::NotFound)?;
            trash::os_limited::restore_all([pick]).map_err(|e| match e {
                trash::Error::RestoreCollision { .. } => Error::Conflict {
                    conflict: ConflictKind::Exists,
                },
                trash::Error::CouldNotAccess { .. } => Error::PermissionDenied,
                _ => Error::Io { retryable: false },
            })
        })
        .await
    }

    /// GC of orphaned `.norte-partial` files in directory `dir` (ADR 0012,
    /// #11): deletes staging files whose last modification is older than
    /// `older_than`. Recognizes partials by their exact SHAPE
    /// (`is_norte_partial`), not by the bare prefix — a real user file
    /// `.norte-partial.backup` is NEVER touched (encoding-auditor H2).
    /// Returns how many it deleted.
    ///
    /// Doesn't distinguish a partial from a LIVE copy (that correlation
    /// belongs to journal M3): use a generous `older_than` (hours) so as
    /// not to sweep an in-progress resume. It's a one-off operation, not a
    /// Task.
    ///
    /// # Errors
    /// [`Error`] if `dir` can't be listed; individual deletion failures
    /// are counted as not-deleted, without aborting the sweep.
    async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        self.ensure_caps().await;
        let native = self.native(dir)?;
        blocking(move || {
            let now = std::time::SystemTime::now();
            let rd = std::fs::read_dir(&native).map_err(|e| map_io(&e))?;
            let mut removed = 0usize;
            for dent in rd.flatten() {
                let name = dent.file_name();
                if !is_norte_partial(&os_to_bytes(&name)) {
                    continue;
                }
                // Age by mtime; if metadata isn't readable, it's left alone (conservative).
                let old = dent
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| now.duration_since(t).ok())
                    .is_some_and(|age| age >= older_than);
                if old && std::fs::remove_file(dent.path()).is_ok() {
                    removed += 1;
                }
            }
            Ok(removed)
        })
        .await
    }

    async fn read_link(&self, p: &VPath) -> Result<Vec<u8>, Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        blocking(move || {
            let md = std::fs::symlink_metadata(&native).map_err(|e| map_io(&e))?;
            if !md.file_type().is_symlink() {
                return Err(Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                });
            }
            let target = std::fs::read_link(&native).map_err(|e| map_io(&e))?;
            // RAW target bytes (rule 1): never String nor VPath.
            Ok(os_to_bytes(target.as_os_str()))
        })
        .await
    }

    async fn symlink(
        &self,
        link: &VPath,
        target: &[u8],
        kind: norte_vfs::SymlinkKind,
    ) -> Result<(), Error> {
        self.ensure_caps().await;
        if !self
            .capabilities()
            .flags
            .contains(CapabilityFlags::SYMLINKS)
        {
            return Err(Error::Unsupported);
        }
        let native = self.native(link)?;
        let target = norte_vfs::native::link_target_to_os(target)?;
        blocking(move || make_symlink(&target, &native, kind)).await
    }

    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
        self.ensure_caps().await;
        let final_native = self.native(p)?;
        // Staging next to the destination, with a SHORT, unique name:
        // `.norte-partial.<hash>.<pid>-<n>`. Short because it does NOT
        // derive from the final name (242–255 bytes are legal on
        // ext4/APFS/NTFS and a suffix would give ENAMETOOLONG, issue #4)
        // but from its hash. Unique via pid + sequence: (a) a real user
        // file is never touched (create_new also guarantees this) and (b)
        // two concurrent writes to the same destination don't share
        // staging. The `.norte-partial` prefix makes it recognizable to
        // the journal's GC (M3).
        let name = p.file_name().ok_or(Error::InvalidPath)?;
        let partial_name = ephemeral_partial_name(name.as_bytes());
        let partial_seg = Segment::new(partial_name).map_err(|_| Error::InvalidPath)?;
        let partial_vpath = p.with_file_name(partial_seg).ok_or(Error::InvalidPath)?;
        let partial_native = self.native(&partial_vpath)?;

        let (file, partial_native, final_native) = blocking(move || {
            match std::fs::symlink_metadata(&final_native) {
                Ok(_) => {
                    return Err(Error::Conflict {
                        conflict: collision_kind_for(&final_native),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(map_io(&e)),
            }
            // create_new: if something with this name exists anyway, error
            // before touching a file that isn't ours.
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&partial_native)
                .map_err(|e| map_io(&e))?;
            Ok((file, partial_native, final_native))
        })
        .await?;

        Ok(Box::new(LocalSink {
            file: Some(file),
            // Freshly created staging: starts at zero.
            pos: 0,
            partial: partial_native,
            final_path: final_native,
            done: false,
            stable: false,
        }))
    }

    async fn partial_digest(&self, p: &VPath, len: u64) -> Result<Option<[u8; 32]>, Error> {
        use std::io::Read as _;

        use sha2::{Digest, Sha256};
        self.ensure_caps().await;
        // Same stable staging as open_resumable (#35): SHA-256 of its first
        // `len` bytes. Synchronous I/O in spawn_blocking (rule 2).
        let partial_native = self.native(&stable_partial_vpath(p)?)?;
        blocking(move || {
            // No staging of OURS = no digest (the engine degrades to
            // Length): the prefix of a file that isn't the one about to be
            // continued says nothing about what's about to be continued
            // (#298).
            let Some(file) = open_partial_for_digest(&partial_native)? else {
                return Ok(None);
            };
            let mut reader = file.take(len);
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 64 * 1024];
            let mut seen: u64 = 0;
            loop {
                let n = reader.read(&mut buf).map_err(|e| map_io(&e))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                seen += n as u64;
            }
            // The staging is shorter than `len` (rare: `len` comes from
            // open_resumable): without the full prefix, it degrades to
            // Length.
            if seen < len {
                return Ok(None);
            }
            Ok(Some(hasher.finalize().into()))
        })
        .await
    }

    async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
        self.ensure_caps().await;
        let final_native = self.native(p)?;
        // Staging with a STABLE name per destination (ADR 0012): no
        // pid+seq, so a second invocation finds it again and RESUMES.
        // Still short (derives from the final name's hash, not the name)
        // so as not to brush NAME_MAX (issue #4). `.norte-partial` prefix
        // recognizable to the GC.
        let partial_vpath = stable_partial_vpath(p)?;
        let partial_native = self.native(&partial_vpath)?;

        let (file, already, partial_native, final_native) = blocking(move || {
            // The final destination must NOT exist yet (same contract as
            // write): if it exists, the collision policy belongs to the
            // core.
            match std::fs::symlink_metadata(&final_native) {
                Ok(_) => {
                    return Err(Error::Conflict {
                        conflict: collision_kind_for(&final_native),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(map_io(&e)),
            }
            // Opens (or creates) the partial in APPEND: if there were
            // already bytes from a previous copy, it resumes after them.
            // And it CHECKS what it opened, because this name is
            // predictable (#298).
            let (file, already) = open_stable_staging(&partial_native)?;
            Ok((file, already, partial_native, final_native))
        })
        .await?;

        Ok((
            Box::new(LocalSink {
                file: Some(file),
                // RESUMING: the position is what's already there, not
                // whatever the descriptor says — it was opened with
                // `O_APPEND`, which leaves the offset at 0 until the first
                // write (see `write_maybe_sparse`).
                pos: already,
                partial: partial_native,
                final_path: final_native,
                done: false,
                stable: true,
            }),
            already,
        ))
    }

    async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        blocking(move || {
            std::fs::create_dir(&native).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Error::Conflict {
                        conflict: collision_kind_for(&native),
                    }
                } else {
                    map_io(&e)
                }
            })
        })
        .await
    }

    /// #314: `chmod(2)`, in `spawn_blocking` like everything else in this
    /// provider (rule 2).
    ///
    /// Unix only. On Windows `set_permissions` only knows the read-only
    /// bit, so faking a POSIX mode there would write something other than
    /// what was requested: it answers `Unsupported`, which is the same as
    /// what the capability says.
    #[cfg(unix)]
    async fn set_mode(&self, p: &VPath, mode: u32) -> Result<(), Error> {
        use std::os::unix::fs::PermissionsExt as _;

        self.ensure_caps().await;
        let native = self.native(p)?;
        blocking(move || {
            // `set_permissions` FOLLOWS the link, which is what `chmod(2)`
            // does and what whoever asks for it from a listing expects: a
            // symlink's permissions mean nothing on Linux.
            std::fs::set_permissions(&native, std::fs::Permissions::from_mode(mode))
                .map_err(|e| map_io(&e))
        })
        .await
    }

    #[cfg(not(unix))]
    async fn set_mode(&self, p: &VPath, mode: u32) -> Result<(), Error> {
        let _ = (p, mode);
        Err(Error::Unsupported)
    }

    async fn remove(&self, p: &VPath) -> Result<(), Error> {
        self.ensure_caps().await;
        let native = self.native(p)?;
        blocking(move || {
            let md = std::fs::symlink_metadata(&native).map_err(|e| map_io(&e))?;
            if md.file_type().is_dir() {
                // Not recursive: a dir with children → Conflict (the walk
                // belongs to the core).
                std::fs::remove_dir(&native).map_err(|e| map_io(&e))
            } else {
                std::fs::remove_file(&native).map_err(|e| map_io(&e))
            }
        })
        .await
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), Error> {
        self.ensure_caps().await;
        let nf = self.native(from)?;
        let nt = self.native(to)?;
        blocking(move || do_rename(&nf, &nt)).await
    }
}

/// Rename with a no-replace contract: the collision is detected by the
/// rename ITSELF (atomic, no check→rename window). An existing destination
/// is only tolerated if it's the source under a different case
/// (case-rename on an insensitive FS).
pub(crate) fn do_rename(nf: &Path, nt: &Path) -> Result<(), Error> {
    match rename_noreplace(nf, nt) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let from_md = std::fs::symlink_metadata(nf).map_err(|e| map_io(&e))?;
            let to_md = std::fs::symlink_metadata(nt).map_err(|e| map_io(&e))?;
            if same_node(&from_md, &to_md) {
                // Case-rename of the source itself: plain rename. The
                // window it reopens is minimal and only on this path (the
                // destination IS this same inode, verified by (dev,ino)).
                std::fs::rename(nf, nt).map_err(|e| map_io(&e))
            } else {
                Err(Error::Conflict {
                    conflict: collision_kind_for(nt),
                })
            }
        }
        Err(e) if noreplace_unsupported(&e) => checked_rename(nf, nt),
        Err(e) => Err(map_io(&e)),
    }
}

/// Fallback for an FS without a no-replace primitive (old NFS,
/// EINVAL/ENOSYS): M0's check→rename, with its documented TOCTOU window.
fn checked_rename(nf: &Path, nt: &Path) -> Result<(), Error> {
    let from_md = std::fs::symlink_metadata(nf).map_err(|e| map_io(&e))?;
    match std::fs::symlink_metadata(nt) {
        Ok(to_md) => {
            if !same_node(&from_md, &to_md) {
                return Err(Error::Conflict {
                    conflict: collision_kind_for(nt),
                });
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(map_io(&e)),
    }
    std::fs::rename(nf, nt).map_err(|e| map_io(&e))
}

/// Does the error say "this FS/kernel doesn't know how to do a no-replace
/// rename"? (EINVAL/ENOSYS/ENOTSUP). Different from EXDEV (degrade to
/// copy+delete) and from EEXIST (real collision): here it degrades to
/// check→rename.
///
/// EINVAL is ambiguous: `renameat2` also returns it for "destination inside
/// the source". The fallback fails the same way anyway via
/// `std::fs::rename` (correct result, just extra syscalls) and the core
/// already pre-filters descendants.
fn noreplace_unsupported(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput
    )
}

/// A rename that NEVER replaces an existing destination, atomic on the FS:
/// `renameat2(RENAME_NOREPLACE)`. Relevant errors: `AlreadyExists`
/// (destination occupied), `CrossesDevices` (EXDEV),
/// `InvalidInput`/`Unsupported` (FS or kernel without support for the flag
/// — the caller degrades).
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let f = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let t = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: `f` and `t` are NUL-terminated CStrings alive for the whole
    // call; `AT_FDCWD` and `RENAME_NOREPLACE` are ABI constants. Contract
    // tested in `tests::rename_noreplace_never_overwrites_destination`.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// macOS's atomic no-replace rename: `renamex_np(RENAME_EXCL)`.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let f = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let t = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: `f` and `t` are NUL-terminated CStrings alive for the whole
    // call; `RENAME_EXCL` is an ABI constant. Contract tested in
    // `tests::rename_noreplace_never_overwrites_destination`.
    let rc = unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Windows's no-replace rename: `MoveFileExW` with flags 0 — without
/// `MOVEFILE_REPLACE_EXISTING` (no-replace) and without
/// `MOVEFILE_COPY_ALLOWED` (cross-volume → `ERROR_NOT_SAME_DEVICE`, never a
/// silent, non-cancellable copy). The file's own case-rename DOES proceed:
/// it's NTFS's standard way to change case (issue #2, no heuristics).
#[cfg(windows)]
#[allow(unsafe_code)]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let f: Vec<u16> = from
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let t: Vec<u16> = to
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // An interior NUL (possible in an arbitrary caller `base`, not in
    // segments) would truncate the wide string and rename ANOTHER path.
    if f[..f.len() - 1].contains(&0) || t[..t.len() - 1].contains(&0) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    // SAFETY: `f` and `t` are NUL-terminated UTF-16 buffers (interior NUL
    // rejected above) alive for the whole call. Contract tested in
    // `tests::rename_noreplace_never_overwrites_destination`.
    let rc =
        unsafe { windows_sys::Win32::Storage::FileSystem::MoveFileExW(f.as_ptr(), t.as_ptr(), 0) };
    if rc == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The rest of unix (outside the CI matrix): no portable no-replace
/// primitive — check→rename emulation with a TOCTOU window.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(from, to)
}

/// Can the rename proceed even though the destination "exists"? Only if
/// the destination IS the source itself under a different case
/// (case-rename on an insensitive FS) — and with a single dirent: between
/// two hardlinks of the same inode, `rename(2)` is a successful no-op that
/// the journal would log as a move that didn't happen.
#[cfg(unix)]
fn same_node(from_md: &std::fs::Metadata, to_md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // The nlink guard only applies to files: a dir can't have hardlinks
    // (its nlink is 2 + subdirs) and would block the case-rename of
    // directories on an insensitive FS.
    from_md.dev() == to_md.dev()
        && from_md.ino() == to_md.ino()
        && (to_md.is_dir() || to_md.nlink() == 1)
}

/// Windows: the file's own case-rename is ALREADY resolved by
/// `rename_noreplace` (`MoveFileExW` allows it without `REPLACE_EXISTING`),
/// so reaching `EEXIST` means a real collision — `false` with no
/// heuristics (one by name would clobber DISTINCT files in case-sensitive
/// NTFS directories, the ones WSL creates). Closes issue #2's debt.
#[cfg(windows)]
fn same_node(_from_md: &std::fs::Metadata, _to_md: &std::fs::Metadata) -> bool {
    false
}

/// Writes `chunk`, leaving a HOLE where it's all zeros (roadmap item 8).
///
/// A whole chunk of zeros isn't written: the length is extended and the
/// position is moved to the end. Whether that's a real hole is up to the
/// filesystem — ext4, XFS and APFS do it; one without that support
/// allocates on write and comes out just as correct —, and what's read
/// back afterward is the same bytes either way.
///
/// **`pos` is tracked by the sink and is NEVER asked of the descriptor**,
/// which is the part where this broke once, and in the worst way. The
/// first version seeked with `SeekFrom::Current` and set the length from
/// whatever the seek returned, relying on the sink writing sequentially
/// from the end. That's false for the RESUMED sink: it's opened with
/// `O_APPEND`, and `O_APPEND` doesn't place the offset at the end on open —
/// it leaves it at 0 and only repositions right before each `write` — so
/// over an N-byte partial the seek started from 0 and `set_len` truncated
/// instead of extending. It ate what had already been copied, the commit
/// published it, and nobody checked. With an explicit `pos` the invariant
/// stops being a claim in a comment and becomes true by construction:
/// `pos` only grows, so `set_len` can only extend.
///
/// The length is set ON THE FLY and not at commit: `open_resumable`
/// derives its `already` from the staging's size and `partial_digest`
/// reads its first bytes. With the length deferred, a partial that ended
/// in a hole would claim fewer bytes than it has.
///
/// Honest limit: the unit is the CHUNK. A hole smaller than a chunk, or
/// misaligned with one, gets materialized — this doesn't look for holes
/// inside the data, it only refrains from writing the ones that already
/// arrive whole.
///
/// Windows: `set_len` on a handle opened only for APPEND can answer
/// `ERROR_ACCESS_DENIED`, so the resume path with a zero chunk is
/// unverified there (#222, blocked by CI like #220 and #221).
pub(crate) fn write_maybe_sparse(
    file: &mut std::fs::File,
    pos: &mut u64,
    chunk: &[u8],
) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write as _};
    if chunk.is_empty() {
        return Ok(());
    }
    let len = chunk.len() as u64;
    if chunk.iter().any(|&b| b != 0) {
        file.write_all(chunk)?;
    } else {
        let end = pos.checked_add(len).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "position overflowed")
        })?;
        // Extend first, then seek: in this order the length never passes
        // through a value smaller than what it already had.
        file.set_len(end)?;
        file.seek(SeekFrom::Start(end))?;
    }
    *pos += len;
    Ok(())
}

struct LocalSink {
    file: Option<std::fs::File>,
    /// Bytes already delivered to this sink, holes included. It's the
    /// anchor for [`write_maybe_sparse`]: the descriptor does NOT know
    /// where it is when it was opened with `O_APPEND` to resume.
    pos: u64,
    partial: PathBuf,
    final_path: PathBuf,
    /// `true` once commit/abort have already dealt with the staging (Drop
    /// touches nothing).
    done: bool,
    /// The staging is the STABLE one, meaning it was born `0o600` (#298)
    /// and needs to be given, in `commit`, the mode an uninterrupted copy
    /// would have had (#299). The ephemeral one is born `0o666` trimmed by
    /// the umask and needs nothing.
    stable: bool,
}

#[async_trait]
impl ByteSink for LocalSink {
    async fn write(&mut self, chunk: Bytes) -> Result<(), Error> {
        let file = self.file.take().ok_or(Error::Io { retryable: false })?;
        let mut pos = self.pos;
        let (file, pos, res) = tokio::task::spawn_blocking(move || {
            let mut file = file;
            let res = write_maybe_sparse(&mut file, &mut pos, &chunk).map_err(|e| map_io(&e));
            (file, pos, res)
        })
        .await
        .map_err(|_| Error::Internal { panic: true })?;
        self.file = Some(file);
        self.pos = pos;
        res
    }

    async fn commit(mut self: Box<Self>) -> Result<(), Error> {
        let file = self.file.take().ok_or(Error::Io { retryable: false })?;
        let partial = self.partial.clone();
        let final_path = self.final_path.clone();
        let stable = self.stable;
        let res = blocking(move || {
            file.sync_all().map_err(|e| map_io(&e))?;
            // The descriptor stays ALIVE during the rename on purpose
            // (#299): the mode is fixed AFTER publishing and on the fd,
            // not on the path. The other way — relaxing `0o600` while it's
            // still called `.norte-partial` — would leave readable by
            // others a staging file with the directory's most predictable
            // name, for a file that isn't yet the one nobody asked for.
            // Atomic no-replace: a collision that appeared between
            // write() and commit() is detected by the rename ITSELF, no
            // TOCTOU window.
            match rename_noreplace(&partial, &final_path) {
                Ok(()) => {
                    reponer_modo_publicado(&file, stable);
                    Ok(())
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let kind = collision_kind_for(&final_path);
                    let _ = std::fs::remove_file(&partial);
                    Err(Error::Conflict { conflict: kind })
                }
                Err(e) if noreplace_unsupported(&e) => {
                    // FS without no-replace: M0's check→rename (best effort).
                    match std::fs::symlink_metadata(&final_path) {
                        Ok(_) => {
                            let kind = collision_kind_for(&final_path);
                            let _ = std::fs::remove_file(&partial);
                            Err(Error::Conflict { conflict: kind })
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            std::fs::rename(&partial, &final_path).map_err(|e| {
                                let _ = std::fs::remove_file(&partial);
                                map_io(&e)
                            })?;
                            reponer_modo_publicado(&file, stable);
                            Ok(())
                        }
                        Err(e) => {
                            let _ = std::fs::remove_file(&partial);
                            Err(map_io(&e))
                        }
                    }
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&partial);
                    Err(map_io(&e))
                }
            }
        })
        .await;
        // Success or a "clean" error: the closure already dealt with the
        // staging. If the closure PANICKED (Internal), let Drop try the
        // cleanup.
        if !matches!(res, Err(Error::Internal { .. })) {
            self.done = true;
        }
        res
    }

    async fn abort(mut self: Box<Self>) -> Result<(), Error> {
        self.file.take();
        self.done = true;
        let partial = self.partial.clone();
        blocking(move || match std::fs::remove_file(&partial) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(map_io(&e)),
        })
        .await
    }

    async fn keep(mut self: Box<Self>) -> Result<(), Error> {
        // Keeps the staging for a later open_resumable (ADR 0012):
        // durabilizes it (fsync) and does NOT rename or delete it. `done`
        // prevents Drop from sweeping it.
        let file = self.file.take();
        self.done = true;
        blocking(move || {
            if let Some(f) = file {
                f.sync_all().map_err(|e| map_io(&e))?;
            }
            Ok(())
        })
        .await
    }
}

impl Drop for LocalSink {
    fn drop(&mut self) {
        // ByteSink's contract: dropping without commit = best-effort
        // abort. It's a fast, synchronous unlink; the GUARANTEED cleanup
        // is abort().
        if !self.done {
            self.file.take();
            let _ = std::fs::remove_file(&self.partial);
        }
    }
}

#[cfg(test)]
mod tests {
    use norte_proto::Error;

    use super::{map_io, rename_noreplace};

    #[test]
    fn exdev_maps_to_unsupported() {
        // EXDEV on rename (issue #3): the FS can't do it — the engine
        // degrades the move to copy+delete. Io{retryable:false} would be
        // an opaque terminal error for the user.
        let e = std::io::Error::from(std::io::ErrorKind::CrossesDevices);
        assert_eq!(map_io(&e), Error::Unsupported);
    }

    #[test]
    fn enametoolong_maps_to_invalid_path() {
        // With the short staging (issue #4), a name >NAME_MAX no longer
        // blows up when opening the staging: the OS's rejection arrives at
        // the FINAL path's stat/rename. It's a path problem, not an I/O
        // one: InvalidPath.
        let e = std::io::Error::from(std::io::ErrorKind::InvalidFilename);
        assert_eq!(map_io(&e), Error::InvalidPath);
    }

    /// EILSEQ (APFS rejects non-UTF8 names) arrives as `Uncategorized`: the
    /// raw errno has to be checked. Same shift as issue #4: with the short
    /// staging the rejection happens at the commit rename, and without
    /// this mapping it would come out as an opaque `Io` (caught by macOS
    /// CI).
    #[cfg(unix)]
    #[test]
    fn eilseq_maps_to_invalid_path() {
        let e = std::io::Error::from_raw_os_error(libc::EILSEQ);
        assert_eq!(map_io(&e), Error::InvalidPath);
    }

    /// Test of `rename_noreplace`'s `unsafe` (rule 5): the no-replace
    /// contract holds on the real FS of all three CI OSes.
    #[test]
    fn rename_noreplace_never_overwrites_destination() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"source").unwrap();
        std::fs::write(&b, b"dest").unwrap();

        let err = rename_noreplace(&a, &b).expect_err("destination occupied");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&b).unwrap(), b"dest", "unchanged");
        assert_eq!(std::fs::read(&a).unwrap(), b"source", "unchanged");

        let c = dir.path().join("c");
        rename_noreplace(&a, &c).expect("free destination");
        assert_eq!(std::fs::read(&c).unwrap(), b"source");
        assert!(!a.exists());
    }

    /// Guardian of the stable name's hash (encoding-auditor H3): the value
    /// MUST be constant across Rust versions — SHA-256 guarantees this; an
    /// algorithm change would silently break cross-version resumption, so
    /// it's frozen here.
    #[test]
    fn stable_partial_name_is_frozen() {
        use norte_proto::{Scheme, Segment, VPath};
        let dst = VPath::root(Scheme::new("file").unwrap(), None)
            .join(Segment::new(b"dst.bin".to_vec()).unwrap());
        let partial = super::stable_partial_vpath(&dst).unwrap();
        assert_eq!(
            partial.file_name().unwrap().as_bytes(),
            b".norte-partial.80dcee3a35d0eff397ec041e9ee27a3c",
            "sha256(\"dst.bin\")[..16] hex — frozen (H3)"
        );
    }

    /// `is_norte_partial` (H2): recognizes the two staging shapes and
    /// NOTHING else — a user file with the prefix isn't mistaken for one.
    #[test]
    fn is_norte_partial_recognizes_only_the_known_forms() {
        use super::is_norte_partial as f;
        // Stable: prefix + 32 hex.
        assert!(f(b".norte-partial.80dcee3a35d0eff397ec041e9ee27a3c"));
        // Ephemeral: prefix + 16 hex + .<pid>-<seq>.
        assert!(f(b".norte-partial.80dcee3a35d0eff3.12345-7"));
        // NOT staging:
        assert!(!f(b".norte-partial.backup"));
        assert!(!f(b".norte-partial.notes.txt"));
        assert!(!f(b".norte-partial.")); // empty
        assert!(!f(b".norte-partial.80dcee3a35d0eff397ec041e9ee27a3")); // 31 hex
        assert!(!f(b".norte-partial.ZZZZ")); // not hex
        assert!(!f(b"other.norte-partial.80dcee3a35d0eff397ec041e9ee27a3c")); // no prefix at the start
        assert!(!f(b".norte-partial.80dcee3a35d0eff3.abc-7")); // pid not a digit
    }

    /// `SymlinkKind::Unknown` (issue #18): the kind is resolved against the
    /// REAL target relative to the link's parent; broken degrades to File;
    /// explicit kinds pass through untouched, without touching the FS.
    #[test]
    fn unknown_symlink_kind_resolves_against_the_target() {
        use norte_vfs::SymlinkKind;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("file"), b"x").unwrap();
        let link = dir.path().join("the-link");

        let kind_of = |target: &str, kind| {
            super::effective_symlink_kind(&link, std::ffi::OsStr::new(target), kind)
        };
        assert_eq!(kind_of("subdir", SymlinkKind::Unknown), SymlinkKind::Dir);
        assert_eq!(kind_of("file", SymlinkKind::Unknown), SymlinkKind::File);
        assert_eq!(
            kind_of("does-not-exist", SymlinkKind::Unknown),
            SymlinkKind::File
        );
        // Target with `..` and with a nested `/` separator: canaries for
        // pre-verbatim normalization in Windows CI (under `\\?\` the
        // kernel doesn't fold `..` nor convert `/` — the auditor's
        // finding).
        std::fs::create_dir_all(dir.path().join("inner")).unwrap();
        std::fs::create_dir_all(dir.path().join("nested").join("leaf")).unwrap();
        let inner_link = dir.path().join("inner").join("the-link");
        assert_eq!(
            super::effective_symlink_kind(
                &inner_link,
                std::ffi::OsStr::new("../subdir"),
                SymlinkKind::Unknown
            ),
            SymlinkKind::Dir,
            "relative target with .."
        );
        assert_eq!(
            kind_of("nested/leaf", SymlinkKind::Unknown),
            SymlinkKind::Dir,
            "nested target with / separator"
        );
        // ABSOLUTE target: join respects it.
        let abs = dir.path().join("subdir");
        assert_eq!(
            super::effective_symlink_kind(&link, abs.as_os_str(), SymlinkKind::Unknown),
            SymlinkKind::Dir
        );
        // Explicit: never re-resolved (does-not-exist would still be Dir).
        assert_eq!(
            kind_of("does-not-exist", SymlinkKind::Dir),
            SymlinkKind::Dir
        );
    }

    /// Node identity over the real FS (issue #16): stable, distinct
    /// between nodes, survives the rename and — where there are symlinks —
    /// `follow` resolves to the target. On volumes without identity
    /// (`Ok(None)`) the test auto-skips, just like the contract.
    #[tokio::test]
    async fn node_id_identifies_the_same_file() {
        use norte_vfs::{FollowLinks, Provider};
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a"), b"x").unwrap();
        std::fs::write(dir.path().join("b"), b"y").unwrap();
        let p = super::LocalProvider::rooted(dir.path());
        let root = super::LocalProvider::root();
        let a = root.join(norte_proto::Segment::new(b"a".to_vec()).unwrap());
        let b = root.join(norte_proto::Segment::new(b"b".to_vec()).unwrap());

        let Some(id_a) = p.node_id(&a, FollowLinks::No).await.expect("node_id a") else {
            eprintln!("skip: volume without stable identity");
            return;
        };
        let id_b = p
            .node_id(&b, FollowLinks::No)
            .await
            .expect("node_id b")
            .expect("same volume: always or never");
        assert_ne!(id_a, id_b, "distinct nodes");
        assert_eq!(
            p.node_id(&a, FollowLinks::Yes).await.unwrap().unwrap(),
            id_a,
            "follow on a normal file changes nothing"
        );

        // The rename moves the node, it doesn't recreate it.
        std::fs::rename(dir.path().join("a"), dir.path().join("c")).unwrap();
        let c = root.join(norte_proto::Segment::new(b"c".to_vec()).unwrap());
        assert_eq!(p.node_id(&c, FollowLinks::No).await.unwrap().unwrap(), id_a);

        // Nonexistent: NotFound, never a silent None.
        assert_eq!(
            p.node_id(&a, FollowLinks::No).await.unwrap_err(),
            norte_proto::Error::NotFound
        );
    }

    /// Collision by normalization (issue #8): the dirent exists in NFD
    /// (what macOS writes) and the request arrives in NFC — different
    /// bytes, identical NFC form. Labeling it `CaseCollision` would
    /// mislead the frontend.
    #[test]
    fn collision_by_normalization_is_labeled() {
        use norte_proto::ConflictKind;
        let dir = tempfile::tempdir().expect("tempdir");
        let nfd = String::from_utf8(vec![0x65, 0xCC, 0x81]).unwrap(); // e + ́
        std::fs::write(dir.path().join(&nfd), b"x").unwrap();
        let nfc = String::from_utf8(vec![0xC3, 0xA9]).unwrap(); // é
        assert_eq!(
            super::collision_kind_for(&dir.path().join(&nfc)),
            ConflictKind::Normalization
        );
        // Different case with no normalization involved: still CaseCollision.
        std::fs::write(dir.path().join("box"), b"x").unwrap();
        assert_eq!(
            super::collision_kind_for(&dir.path().join("BOX")),
            ConflictKind::CaseCollision
        );
    }

    /// A path with an interior NUL (possible in a hostile caller `base`)
    /// would truncate the wide string and rename ANOTHER path: clean
    /// rejection.
    #[cfg(windows)]
    #[test]
    fn rename_noreplace_rejects_interior_nul() {
        use std::os::windows::ffi::OsStringExt;
        let evil = std::path::PathBuf::from(std::ffi::OsString::from_wide(&[
            u16::from(b'C'),
            u16::from(b':'),
            u16::from(b'\\'),
            0,
            u16::from(b'x'),
        ]));
        let dir = tempfile::tempdir().expect("tempdir");
        let ok = dir.path().join("a");
        std::fs::write(&ok, b"x").unwrap();
        let err = rename_noreplace(&evil, &ok).expect_err("interior NUL in source");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        let err = rename_noreplace(&ok, &evil).expect_err("interior NUL in destination");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(std::fs::read(&ok).unwrap(), b"x", "nothing moved");
    }
}
