//! TOFU (trust-on-first-use) store for the PROJECT `./.norte/init.lua`
//! (ADR 0026, M4 Lua): unlike the user's `init.lua` (their own config,
//! executed without asking), this file comes with SOMEONE ELSE'S repo —
//! running it blindly is RCE. A pattern copied from
//! `norte-connect::known_hosts` (host key TOFU) and `norte-connect::secret`
//! (atomic 0600 write): first contact → [`TrustDecision::Unknown`], the
//! frontend asks, the answer is persisted by (path, hash).
//!
//! Identity key: (EXACT bytes of the path, sha256 of the content bytes). A
//! file that CHANGES content at the same path does not inherit the old
//! version's trust — it is asked again, just like a host that changes its
//! key.
//!
//! Rule 1 (file names = bytes): the path is compared via
//! `OsStr::as_encoded_bytes()`, NEVER by its `String` form — two paths that
//! only differ in non-UTF8 bytes never collide, and vice versa: the `path`
//! field carried in the TOML is ONLY for human inspection of the file, the
//! identity match uses `path_hex`.
//!
//! Known limitation (CLAUDE.md's recurring traps): this store does NOT
//! normalize NFC/NFD — it compares bytes as they arrive. On macOS, if the
//! CALLER obtains the path through two different routes (one NFC, another
//! NFD after going through HFS+/APFS), `check`/`record` would see them as
//! DIFFERENT paths. The caller (T8) must ALWAYS use the same form (the one
//! `std::fs::canonicalize` returns) for check and record of the same
//! script.
//!
//! SYNCHRONOUS I/O (small file, plain TOML): the caller (task 7 of this
//! plan) wraps it in `spawn_blocking` when using it from an async context
//! (rule 2 — this rule is met by the CALLER, not in here).

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Result of checking a script against the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    /// (path, hash) matches an approved entry: load without asking.
    Trusted,
    /// (path, hash) matches EXACTLY a denied entry: do NOT load, do NOT
    /// ask again (avoids hammering the user with the same script).
    Denied,
    /// The path has a denied entry, but with a DIFFERENT hash: the content
    /// changed since the rejection. The caller (T8) treats this as a
    /// SILENT deny (a notice on the status bar), NEVER an automatic modal
    /// — if we reopened the modal on every edit of an already-rejected
    /// script, the user would end up approving out of fatigue. Asking
    /// again requires an explicit action (e.g. deleting the entry, out of
    /// T6's scope).
    DeniedPathChanged,
    /// No entry for this path, or the path was APPROVED but with a
    /// DIFFERENT hash (in that case it IS asked again: approving a script
    /// is not a blank check for any future version).
    Unknown,
}

/// Persisted entry: one decision per path (the most recent replaces any
/// previous one for the same path — `record` is upsert-by-path).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// Path in "lossy" form — ONLY so a human can eyeball the TOML. NEVER
    /// used for the identity match (rule 1): that is `path_hex`.
    path: String,
    /// EXACT bytes of the path (`OsStr::as_encoded_bytes()`) in hex — the
    /// real identity key, with no loss and no UTF-8 assumption.
    path_hex: String,
    /// hex sha256 of the evaluated content bytes.
    hash: String,
    /// `true` = approved, `false` = denied.
    allow: bool,
    /// Informational timestamp (epoch seconds; no new time dependency).
    date_epoch: u64,
}

/// Plain TOML file: a list of entries under the `entry` key.
#[derive(Debug, Default, Serialize, Deserialize)]
struct FileFormat {
    #[serde(default, rename = "entry")]
    entries: Vec<Entry>,
}

/// TOFU store for the project's `init.lua`.
///
/// All operations are SYNCHRONOUS I/O (small file, touched at
/// startup/reload): the async caller must wrap them in `spawn_blocking`
/// (rule 2 — the responsibility is the caller's, not this type's).
#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    entries: Vec<Entry>,
}

impl TrustStore {
    /// Opens the store at `path`. An ABSENT file = empty store (first use
    /// of the binary on this machine); any other read or parse error is
    /// propagated (fail-closed: a corrupt file must not silently degrade
    /// to "everything unknown" if it is actually tampering — same
    /// criterion as `KnownHostsStore`, see
    /// `crates/norte-connect/src/known_hosts.rs`).
    ///
    /// # Errors
    ///
    /// If `path` exists but cannot be read, or its content is not a valid
    /// `lua-trust.toml` (this includes non-UTF8 bytes: `read_to_string`
    /// rejects them and the error is propagated, not treated as
    /// "does not exist").
    pub fn open(path: PathBuf) -> std::io::Result<Self> {
        let entries = match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let parsed: FileFormat = toml::from_str(&raw).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("corrupt lua-trust.toml: {e}"),
                    )
                })?;
                parsed.entries
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, entries })
    }

    /// Checks `script_path` (ALREADY CANONICAL — resolved by the caller
    /// with `std::fs::canonicalize`; this store does not canonicalize) and
    /// the hash of `content` against the store.
    ///
    /// Anti-TOCTOU: the hash is computed OVER THE PASSED-IN BYTES, without
    /// re-reading the file. The caller must read the script EXACTLY ONCE,
    /// call `check(bytes)` and, if the result authorizes the load,
    /// evaluate THOSE SAME bytes — never touch disk again between the
    /// check and the eval.
    ///
    /// CALLER requirement (this type cannot enforce it, since it only sees
    /// path+content bytes): before invoking `check` one must verify that
    /// `.norte` is a REAL directory and that `init.lua` is a REGULAR file
    /// (`symlink_metadata`, or opening with `O_NOFOLLOW`) — without that
    /// check, a symlink pointing at an already-trusted project would
    /// execute, in a different (potentially hostile) context, the content
    /// the user approved for ANOTHER location. Implemented in T8.
    #[must_use]
    pub fn check(&self, script_path: &Path, content: &[u8]) -> TrustDecision {
        let path_hex = path_hex(script_path);
        let hash = hash_hex(content);
        match self.entries.iter().find(|e| e.path_hex == path_hex) {
            Some(e) if e.hash == hash && e.allow => TrustDecision::Trusted,
            Some(e) if e.hash == hash => TrustDecision::Denied,
            Some(e) if !e.allow => TrustDecision::DeniedPathChanged,
            _ => TrustDecision::Unknown,
        }
    }

    /// Records the user's decision for `(script_path, content)`. Replaces
    /// any previous entry for the SAME `script_path` (one live entry per
    /// path — the old entry, if it had a different hash, stops applying).
    ///
    /// ATOMIC write (temp file in the SAME directory + rename) with 0600
    /// permissions on Unix WHEN CREATED (copied from `write_secret_file` in
    /// `crates/norte-connect/src/secret.rs`). Creates the parent directory
    /// with 0700 if missing (copied from `Journal::open`,
    /// `crates/norte-core/src/journal.rs:283-286`).
    ///
    /// If persistence fails, the in-memory mutation is REVERTED — the
    /// state in RAM never diverges from disk (otherwise a later `check`
    /// would lie that something got recorded when it actually does not
    /// survive a restart).
    ///
    /// # Errors
    ///
    /// If creating the parent directory, writing the temp file, or the
    /// final atomic rename fails.
    pub fn record(
        &mut self,
        script_path: &Path,
        content: &[u8],
        allow: bool,
    ) -> std::io::Result<()> {
        let previous = self.entries.clone();
        let path_hex = path_hex(script_path);
        let hash = hash_hex(content);
        let date_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.entries.retain(|e| e.path_hex != path_hex);
        self.entries.push(Entry {
            path: script_path.to_string_lossy().into_owned(),
            path_hex,
            hash,
            allow,
            date_epoch,
        });

        if let Err(e) = self.persist() {
            self.entries = previous;
            return Err(e);
        }
        Ok(())
    }

    /// Dumps `self.entries` to disk atomically. Separated from `record` so
    /// that the rollback on error is a simple `self.entries = previous` in
    /// the caller (above).
    fn persist(&self) -> std::io::Result<()> {
        ensure_parent_dir_0700(&self.path)?;
        let serialized = toml::to_string(&FileFormat {
            entries: self.entries.clone(),
        })
        .map_err(|e| std::io::Error::other(format!("serializing lua-trust.toml: {e}")))?;
        write_atomic_0600(&self.path, serialized.as_bytes())
    }
}

/// Exact bytes of `path` (without assuming UTF-8 — rule 1) in hex.
fn path_hex(path: &Path) -> String {
    bytes_to_hex(path.as_os_str().as_encoded_bytes())
}

/// Lowercase hex sha256 of `content`.
fn hash_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    bytes_to_hex(hasher.finalize().as_slice())
}

/// Lowercase hex of any byte string.
fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Creates `path`'s parent directory with 0700 on Unix if missing (copied
/// from `Journal::open`, `crates/norte-core/src/journal.rs:283-286`).
/// No-op if `path` has no parent or the parent already exists.
fn ensure_parent_dir_0700(path: &Path) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(parent)
}

/// Writes `data` to `path` atomically (tmp in the SAME dir + rename) with
/// 0600 permissions on Unix when created. Copied from `write_secret_file`
/// in `crates/norte-connect/src/secret.rs`.
fn write_atomic_0600(path: &Path, data: &[u8]) -> std::io::Result<()> {
    // The tmp name carries the PID: two processes writing to the SAME
    // store at once (two embedded frontends pointing at the same store, or
    // parallel tests) must not clobber each other's temp file before the
    // rename — a fixed, shared name would corrupt the content of whoever
    // loses the `truncate` race.
    let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    {
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_asks_approved_loads_denied_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::open(dir.path().join("lua-trust.toml")).unwrap();
        let p = Path::new("/repo/.norte/init.lua");
        let content = b"norte.command('x', function() end)";
        assert_eq!(store.check(p, content), TrustDecision::Unknown);
        store.record(p, content, true).unwrap();
        assert_eq!(store.check(p, content), TrustDecision::Trusted);
        // Different content = different hash = ask again.
        assert_eq!(store.check(p, b"otro"), TrustDecision::Unknown);
        // Denied persists (do not ask again until it changes).
        store.record(p, b"otro", false).unwrap();
        assert_eq!(store.check(p, b"otro"), TrustDecision::Denied);
    }

    #[test]
    fn the_store_reopens_what_was_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        let script = Path::new("/x/.norte/init.lua");
        TrustStore::open(p.clone())
            .unwrap()
            .record(script, b"c", true)
            .unwrap();
        let store = TrustStore::open(p).unwrap();
        assert_eq!(store.check(script, b"c"), TrustDecision::Trusted);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_born_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        TrustStore::open(p.clone())
            .unwrap()
            .record(Path::new("/x"), b"c", true)
            .unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// MEDIUM-2 (security review): two DIFFERENT non-UTF8 paths must not
    /// collide in the identity match — if `check`/`record` degraded to
    /// comparing by `String` (lossy), invalid bytes would be replaced by
    /// `U+FFFD` and different paths could become indistinguishable.
    #[cfg(unix)]
    #[test]
    fn distinct_non_utf8_paths_do_not_collide() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::open(dir.path().join("lua-trust.toml")).unwrap();
        let a = Path::new(OsStr::from_bytes(b"/repo/\xFF\xFEa/.norte/init.lua"));
        let b = Path::new(OsStr::from_bytes(b"/repo/\xFF\xFEb/.norte/init.lua"));
        let content = b"same content";
        store.record(a, content, true).unwrap();
        assert_eq!(store.check(a, content), TrustDecision::Trusted);
        // `b` was never recorded: still Unknown despite sharing content and
        // almost all path bytes with `a`.
        assert_eq!(store.check(b, content), TrustDecision::Unknown);
    }

    /// MEDIUM-2: a non-UTF8 path survives a disk round-trip (the TOML
    /// stores `path_hex`, it does not depend on `path` being
    /// representable).
    #[cfg(unix)]
    #[test]
    fn non_utf8_path_survives_a_disk_round_trip() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        let script = Path::new(OsStr::from_bytes(b"/repo/\xFF\xFE/.norte/init.lua"));
        let content = b"c";
        TrustStore::open(p.clone())
            .unwrap()
            .record(script, content, true)
            .unwrap();
        let store = TrustStore::open(p).unwrap();
        assert_eq!(store.check(script, content), TrustDecision::Trusted);
    }

    /// MEDIUM-3: an ALREADY-denied script that changes content is a SILENT
    /// deny (`DeniedPathChanged`), NEVER back to `Unknown` — otherwise
    /// every edit of a rejected script would reopen the modal until the
    /// user approves out of fatigue.
    #[test]
    fn denied_that_changes_content_is_deniedpathchanged_not_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::open(dir.path().join("lua-trust.toml")).unwrap();
        let p = Path::new("/repo/.norte/init.lua");
        store.record(p, b"bad", false).unwrap();
        assert_eq!(store.check(p, b"bad"), TrustDecision::Denied);
        assert_eq!(store.check(p, b"changed"), TrustDecision::DeniedPathChanged);
    }

    /// An approved script that changes content stays `Unknown` (unchanged
    /// by this review: approving a script is not a blank check for any
    /// future version).
    #[test]
    fn approved_that_changes_content_stays_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::open(dir.path().join("lua-trust.toml")).unwrap();
        let p = Path::new("/repo/.norte/init.lua");
        store.record(p, b"good", true).unwrap();
        assert_eq!(store.check(p, b"other-version"), TrustDecision::Unknown);
    }

    /// LOW-5: an absent file = empty store, everything is `Unknown`.
    #[test]
    fn absent_is_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("does-not-exist.toml");
        let store = TrustStore::open(p).unwrap();
        assert_eq!(
            store.check(Path::new("/no/matter"), b"x"),
            TrustDecision::Unknown
        );
    }

    /// LOW-5: a corrupt file (broken TOML, and also non-UTF8 bytes) is an
    /// ERROR, it does not silently degrade to an empty store (fail-closed:
    /// it could be the trace of tampering, not a benign absence).
    #[test]
    fn corrupt_file_is_err_not_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        std::fs::write(&p, b"not valid toml \xFF\xFE [[[").unwrap();
        assert!(TrustStore::open(p).is_err());
    }

    /// LOW-5 (anti-injection pin): a `script_path` with embedded TOML
    /// syntax (quotes, newlines, a fake `[[entry]]` table) must not
    /// corrupt the file nor create a second phantom entry — since the
    /// field is serialized via `serde`/`toml` (not by manual string
    /// interpolation), escaping is automatic.
    #[test]
    fn hostile_path_with_embedded_toml_syntax_survives_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lua-trust.toml");
        let hostile = Path::new("/tmp/\"quotes\"\n[[entry]]\npath = \"injected\"\n/init.lua");
        let content = b"c";
        let mut store = TrustStore::open(p.clone()).unwrap();
        store.record(hostile, content, true).unwrap();
        let reopened = TrustStore::open(p).unwrap();
        assert_eq!(reopened.check(hostile, content), TrustDecision::Trusted);
        assert_eq!(reopened.entries.len(), 1);
    }

    /// If persistence fails, the entry must NOT remain "trusted" in memory
    /// without backing on disk (otherwise a process restart would see
    /// `Unknown` for something an earlier `check`, in the same process,
    /// had reported as `Trusted` — a transient lie).
    #[test]
    fn failed_write_does_not_let_memory_diverge_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        // Open with a valid path (open() must not see the problem)...
        let mut store = TrustStore::open(dir.path().join("lua-trust.toml")).unwrap();
        // ...and THEN break the "parent directory" by turning it into a
        // file: creating the dir fails, and with it `record` as a whole
        // must fail (access to the private `path` field, legal from the
        // test submodule).
        let blocker = dir.path().join("not-a-dir");
        std::fs::write(&blocker, b"I am a file, not a directory").unwrap();
        store.path = blocker.join("subdir").join("lua-trust.toml");
        let p = Path::new("/x/init.lua");
        assert!(store.record(p, b"c", true).is_err());
        assert_eq!(store.check(p, b"c"), TrustDecision::Unknown);
    }
}
