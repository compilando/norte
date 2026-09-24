//! The UI session on disk: one JSON per user, written whole and at once.
//!
//! The file is `<state_dir>/session.json`, next to `journal.db` and
//! `logs/` —`state_dir()` ALREADY ends in `norte`, so it isn't appended
//! again here—. It's written via temporary + `rename`, which is what makes
//! a power cut mid-dump leave the PREVIOUS session, never a half one.
//!
//! Loading has four outcomes and none is a panic: no file (first startup),
//! it loaded, it's corrupt, or it comes from a version this binary doesn't
//! know how to read. The last two are NOT clobbered lightly and are NOT
//! reported by quoting the content: a session carries the paths the reader
//! moves through, and a path doesn't go to a log over a parse error.

use std::path::{Path, PathBuf};

use norte_proto::methods::{SESSION_BODY_MAX, Session};

/// The highest body version this binary knows how to hand its frontend.
///
/// It's the ONLY number in the file the core looks at, and looking at it
/// isn't reading the body: it compares it, it doesn't interpret it. A file
/// with a higher version isn't loaded and —this is what matters— isn't
/// clobbered: losing the session a newer binary wrote can't be recovered,
/// and the price of respecting it is starting once from configuration.
///
/// It moves in lockstep with `norte_frontend::session::SCHEMA_VERSION`,
/// which is what gives the body its meaning. The two numbers live in
/// different crates because the core CANNOT depend on the frontend; that
/// they don't drift apart is checked by a test in `norte-tui`, the only
/// crate that sees both
/// (`las_dos_versiones_de_esquema_van_del_brazo`).
pub const SCHEMA_VERSION: u32 = 2;

/// The largest thing accepted to READ from disk.
///
/// `put` caps the body at [`SESSION_BODY_MAX`]; with no symmetric cap on
/// load, a multi-GB file —which any process of the same uid can leave
/// behind while nobody holds the lock— would be read whole into memory at
/// startup. The margin is for the envelope (`version`, `revision` and the
/// keys).
const MAX_FILE_BYTES: u64 = SESSION_BODY_MAX as u64 + 4096;

/// Distinguishes the temporary file of TWO dumps from the same process, so
/// `create_new` never runs into its own.
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What was found on load.
#[derive(Debug)]
pub enum LoadOutcome {
    /// No file. First startup, and not a failure.
    Fresh,
    /// The session that was there.
    Loaded(Session),
    /// There was a file and it couldn't be read. `reason` is DIAGNOSTIC
    /// —category and position—, never the content.
    Corrupt {
        /// What failed, without quoting a single byte of the file.
        reason: String,
    },
    /// The file was written by a newer binary.
    FromTheFuture {
        /// The version it carried, so it can be stated in the notice.
        version: u32,
    },
}

/// Where the session lives: `<state_dir>/session.json`.
#[must_use]
pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join("session.json")
}

/// Loads `<state_dir>`'s session.
///
/// Never fails: the four outcomes are [`LoadOutcome`], because the three
/// that aren't "loaded" have the same reasonable answer —start from
/// configuration— and only differ in what has to be told to the human.
#[must_use]
pub fn load(state_dir: &Path) -> LoadOutcome {
    let file = path(state_dir);
    // The cap BEFORE reading: the size is the inode's, not the content's,
    // so stating it doesn't quote a single byte of the file.
    if let Ok(m) = std::fs::metadata(&file)
        && m.len() > MAX_FILE_BYTES
    {
        return LoadOutcome::Corrupt {
            reason: format!(
                "takes up {} bytes and the load cap is {MAX_FILE_BYTES}",
                m.len()
            ),
        };
    }
    let raw = match std::fs::read(&file) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LoadOutcome::Fresh,
        // A file that exists and can't be read is NOT a first startup: if
        // it were treated as one, the next dump would clobber it.
        Err(e) => {
            return LoadOutcome::Corrupt {
                reason: format!("could not be read: {}", e.kind()),
            };
        }
    };
    // First ONLY the version: refusing a future one can't depend on the
    // rest of its shape fitting this binary.
    match serde_json::from_slice::<VersionOnly>(&raw) {
        Ok(VersionOnly { version }) if version > SCHEMA_VERSION => {
            return LoadOutcome::FromTheFuture { version };
        }
        Ok(_) => {}
        Err(e) => {
            return LoadOutcome::Corrupt {
                reason: diagnose(&e),
            };
        }
    }
    match serde_json::from_slice::<Session>(&raw) {
        Ok(session) => LoadOutcome::Loaded(session),
        Err(e) => LoadOutcome::Corrupt {
            reason: diagnose(&e),
        },
    }
}

/// Only the version, to decide whether the file is from the future before
/// looking at anything else.
#[derive(serde::Deserialize)]
struct VersionOnly {
    version: u32,
}

/// Serde's error, reported WITHOUT its message: `serde_json`'s `Display`
/// quotes the value that didn't fit ("invalid type: string "…""), and that
/// value comes from the session file, which carries paths. Category and
/// position are enough to diagnose and leak nothing.
fn diagnose(e: &serde_json::Error) -> String {
    let what = match e.classify() {
        serde_json::error::Category::Io => "i/o",
        serde_json::error::Category::Syntax => "malformed JSON",
        serde_json::error::Category::Data => "unexpected shape",
        serde_json::error::Category::Eof => "ends too soon",
    };
    format!("{what} at line {} column {}", e.line(), e.column())
}

/// Writes the session to `<state_dir>`, whole and at once.
///
/// Temporary + `rename`: a cut in the middle leaves the PREVIOUS session
/// intact, which is the only acceptable alternative to the new one.
/// `sync_all` goes BEFORE the rename because renaming a file whose data is
/// still in the page cache is exactly how an empty file gets published.
///
/// # Errors
///
/// Any I/O failure creating the directory, writing the temporary, or
/// renaming it. The caller WARNS and continues: not being able to save the
/// screen isn't a reason to bring down the session that produced it.
pub fn write(state_dir: &Path, session: &Session) -> std::io::Result<()> {
    ensure_dir(state_dir)?;
    let file = path(state_dir);
    // The temporary is in the SAME directory on purpose: `rename` is only
    // atomic within one filesystem. And it carries the pid in its name
    // because a fixed name is a file that could ALREADY be there: `mode`
    // only applies on CREATE, so an inherited `.tmp` would be published
    // with whatever permissions —or link target— someone else left on it.
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = file.with_extension(format!("json.{}.{seq}.tmp", std::process::id()));
    // The file IS the wire's document, no envelope: the same shape
    // `session.get` returns. A separate file format would be a second
    // thing to version for no gain — the only thing that needs knowing
    // about this file is which body version it carries, and that already
    // travels inside it.
    let bytes = serde_json::to_vec(session).map_err(std::io::Error::other)?;
    // A half-written temporary doesn't stay as a souvenir: the next dump
    // with this pid would run into it and `create_new` would fail forever.
    if let Err(e) = write_private(&tmp, &bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &file) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // And the `rename` gets synced too: the temporary's data was on disk,
    // but the NEW name lives in the directory, and a power cut without
    // this leaves the old name pointing at what was there before.
    sync_dir(state_dir);
    Ok(())
}

/// Syncs the directory so the `rename` survives a power cut.
///
/// Silent on purpose: a directory `fsync` that can't be done —some network
/// filesystems— doesn't invalidate a dump that's already written and
/// renamed.
fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// What was recovered from disk, and whether it can be written over.
///
/// The second field is the half that matters: "not read" and "not
/// clobbered" are different decisions, and the first without the second is
/// exactly how an old binary eats a new one's session —it starts empty,
/// the client writes against revision 0, and the next dump publishes that
/// emptiness over the file it didn't know how to read—.
#[derive(Debug)]
pub struct Restored {
    /// The recovered session, or an empty one if there was none readable.
    pub session: Session,
    /// Whether this process can dump over the file that was there.
    pub writable: bool,
}

/// `state_dir`'s session, reporting out loud whatever isn't "loaded".
///
/// Three of the four outcomes start from configuration and differ only in
/// what has to be told to the human, so that choice isn't the caller's:
/// all it owns is where the state lives. It's shared by the daemon and the
/// embedded frontend, which would otherwise say two different things about
/// the same file.
///
/// The FOURTH —a file from the future— also forbids writing, and THAT does
/// have to bubble up to whoever decides if there's a writer: ADR 0059's
/// promise isn't "not read", it's "not lost".
///
/// The notices are half of what these outcomes are worth: without them,
/// "the screen came up blank" is indistinguishable from "it was never
/// saved".
///
/// Synchronous: both callers invoke it inside a `spawn_blocking` (rule 2).
#[must_use]
pub fn load_or_default(state_dir: &Path) -> Restored {
    match load(state_dir) {
        LoadOutcome::Loaded(session) => Restored {
            session,
            writable: true,
        },
        LoadOutcome::Fresh => Restored {
            session: Session::default(),
            writable: true,
        },
        // An UNREADABLE file DOES get clobbered, and that's the opposite of
        // an exception: whatever it carried is already lost, and never
        // writing again would leave the reader with no session forever
        // over a power cut from a month ago.
        LoadOutcome::Corrupt { reason } => {
            tracing::warn!(%reason, "UI session unreadable: starting from configuration");
            Restored {
                session: Session::default(),
                writable: true,
            }
        }
        LoadOutcome::FromTheFuture { version } => {
            tracing::warn!(
                version,
                known = SCHEMA_VERSION,
                "UI session from a newer version: not read and NOT written"
            );
            Restored {
                session: Session::default(),
                writable: false,
            }
        }
    }
}

/// The right to WRITE this `state_dir`'s session, while it lives.
///
/// Released by its `Drop`, and the OS releases it just the same if the
/// process dies outright: a core that crashes doesn't leave the file
/// locked forever.
#[derive(Debug)]
pub struct SessionLock {
    /// The locked descriptor. The lock IS this handle: closing it releases it.
    _file: std::fs::File,
}

/// Tries to take the right to write `state_dir`'s session.
///
/// `Ok(None)` = another process has it; whoever doesn't get it runs
/// UNOWNED —loads the screen, uses it, and doesn't write—. It's `try_lock`
/// and not `lock` on purpose: waiting would hang a startup behind a core
/// that's alive and has no intention of letting go.
///
/// The lock is over a sibling `session.json.lock`, and NEVER over the file
/// itself, for the reason `lock_config_file` in `norte-config` already
/// documents: the writer replaces the file via `rename`, so a lock on it
/// would be a lock on an inode that stops being the file the moment anyone
/// writes.
///
/// # Errors
///
/// I/O failures creating the directory or opening the lock file.
pub fn lock(state_dir: &Path) -> std::io::Result<Option<SessionLock>> {
    ensure_dir(state_dir)?;
    let mut name = path(state_dir).into_os_string();
    name.push(".lock");
    // NOT truncated (same reason as in `norte-config`): `CREATE_ALWAYS`
    // over a lockfile another process holds can fail on Windows instead of
    // reaching the lock attempt, which is exactly the contention this
    // exists to resolve.
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(false);
    // Empty and with no secrets, but next to a 0600 file: a 0644 lockfile
    // only tells whoever's looking that there's a session here.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let handle = opts.open(PathBuf::from(name))?;
    match handle.try_lock() {
        Ok(()) => Ok(Some(SessionLock { _file: handle })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Creates `<state_dir>` with owner-only permissions, and NARROWS one that
/// was already open.
///
/// `DirBuilder::mode` only applies on create, so a state directory
/// inherited from an older version —or created by another subsystem with a
/// different umask— would stay at 0755 with the session inside. Same hole
/// `create_dir_locked` patches in `logging`.
fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        if let Ok(md) = std::fs::metadata(dir)
            && md.permissions().mode() & 0o077 != 0
        {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Writes `bytes` to `file`, creating it NEW at 0600 and syncing it before
/// returning: the caller is about to rename it right after.
///
/// `create_new` and not `create`: `open`'s mode only rules when the file is
/// CREATED, so opening one that was already there publishes whatever
/// permissions —or link— whoever left it there set. With `O_NOFOLLOW` on
/// top, a symlink where the temporary should be is an error, not a write to
/// wherever it points.
fn write_private(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    // The mode goes in `open`, not in a later `set_permissions`: between
    // creating at 0644 and adjusting it there's a window where another
    // system user can open it, and what's inside are the reader's paths.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
        // rustix and not libc, per rule 5: the constant is the same and
        // comes with no `unsafe` (see this crate's Cargo.toml).
        if let Ok(nofollow) = i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()) {
            opts.custom_flags(nofollow);
        }
    }
    let mut f = opts.open(file)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(v: u32, rev: u64) -> Session {
        Session {
            version: v,
            revision: rev,
            body: serde_json::json!({ "a": 1 }),
        }
    }

    /// No file means no session, and that is NOT a failure: it's a first
    /// startup.
    #[test]
    fn no_file_is_a_first_startup() {
        let d = tempfile::tempdir().expect("tmp");
        assert!(matches!(load(d.path()), LoadOutcome::Fresh));
    }

    /// Writing and reading back returns the same session, revision
    /// included.
    #[test]
    fn round_trip_through_disk() {
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &session(1, 9)).expect("writes");
        let LoadOutcome::Loaded(s) = load(d.path()) else {
            panic!("loaded")
        };
        assert_eq!(s.revision, 9);
        assert_eq!(s.version, 1);
    }

    /// A kind no binary declares survives the trip through disk: the body
    /// is opaque here too.
    #[test]
    fn an_unknown_kind_survives_disk() {
        let d = tempfile::tempdir().expect("tmp");
        let body = serde_json::json!({ "layouts": { "default": {
            "kind": "kind-from-another-binary", "params": { "x": [1, 2] } } } });
        write(
            d.path(),
            &Session {
                version: 1,
                revision: 1,
                body: body.clone(),
            },
        )
        .expect("writes");
        let LoadOutcome::Loaded(s) = load(d.path()) else {
            panic!("loaded")
        };
        assert_eq!(s.body, body);
    }

    /// A corrupt file is NOT a blank screen: it's a diagnosis and a start
    /// from config. And the diagnosis doesn't quote the CONTENT: a session
    /// file carries paths, and a path doesn't go to a log over a parse
    /// error.
    #[test]
    fn a_corrupt_file_is_a_diagnosis_not_a_blank_screen() {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(path(d.path()).parent().expect("parent")).expect("mkdir");
        std::fs::write(path(d.path()), b"{ this is not json").expect("writes");
        let LoadOutcome::Corrupt { reason } = load(d.path()) else {
            panic!("corrupt")
        };
        assert!(!reason.is_empty());
        assert!(!reason.contains("this is not json"), "{reason}");
    }

    /// A session of a FUTURE version isn't clobbered. Losing a new session
    /// against an old binary can't be recovered.
    #[test]
    fn a_future_version_is_not_clobbered() {
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &session(SCHEMA_VERSION + 1, 1)).expect("writes");
        let LoadOutcome::FromTheFuture { version } = load(d.path()) else {
            panic!("from the future")
        };
        assert_eq!(version, SCHEMA_VERSION + 1);
        // And what makes "not clobbered" true and not just a phrase: the
        // outcome BUBBLES UP to whoever decides if there's a writer.
        let r = load_or_default(d.path());
        assert!(!r.writable, "a file from the future is not written");
        assert_eq!(r.session.revision, 0, "nor is it read");
    }

    /// An UNREADABLE file DOES get clobbered: what it carried is already
    /// lost, and never writing again would leave the reader with no
    /// session forever.
    #[test]
    fn a_corrupt_file_does_get_clobbered() {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::write(path(d.path()), b"{ this is not json").expect("writes");
        assert!(load_or_default(d.path()).writable);
    }

    /// A giant file isn't read into memory: it's refused by size, and the
    /// diagnosis says bytes, which belong to the inode, not the content.
    #[test]
    fn a_giant_file_does_not_load() {
        let d = tempfile::tempdir().expect("tmp");
        let big = vec![b'x'; usize::try_from(MAX_FILE_BYTES).expect("fits") + 1];
        std::fs::write(path(d.path()), &big).expect("writes");
        let LoadOutcome::Corrupt { reason } = load(d.path()) else {
            panic!("by size")
        };
        assert!(reason.contains("bytes"), "{reason}");
    }

    /// The state directory gets NARROWED even if it already existed open:
    /// the session lives inside and `mode` only rules on create.
    #[cfg(unix)]
    #[test]
    fn an_inherited_directory_gets_narrowed() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tempfile::tempdir().expect("tmp");
        let dir = d.path().join("state");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        write(&dir, &session(1, 1)).expect("writes");
        let mode = std::fs::metadata(&dir).expect("stat").permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");
    }

    /// A `.tmp` that was already there —from another user, or pointing
    /// elsewhere— isn't reused: the name carries pid and sequence, and
    /// `open` is `create_new`.
    #[test]
    fn an_inherited_temporary_is_not_reused() {
        let d = tempfile::tempdir().expect("tmp");
        let old = path(d.path()).with_extension("json.tmp");
        std::fs::write(&old, b"from someone else").expect("writes");
        write(d.path(), &session(1, 1)).expect("writes anyway");
        let LoadOutcome::Loaded(s) = load(d.path()) else {
            panic!("loaded")
        };
        assert_eq!(s.revision, 1);
        assert_eq!(
            std::fs::read(&old).expect("still there"),
            b"from someone else"
        );
    }

    /// The write is atomic: it leaves no `.tmp` behind.
    #[test]
    fn writing_leaves_no_temporaries() {
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &session(1, 1)).expect("writes");
        write(d.path(), &session(1, 2)).expect("rewrites");
        let dir = path(d.path()).parent().expect("parent").to_owned();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("reads")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp") || n.ends_with('~'))
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
    }

    /// A second core over the same state does NOT write: it clones and runs
    /// unowned. And once the first one leaves, the next one DOES take it —
    /// which is what keeps a handover from leaving the successor unable to
    /// save anything.
    #[test]
    fn a_second_core_does_not_write_over_someone_elses_state() {
        let d = tempfile::tempdir().expect("tmp");
        let one = lock(d.path()).expect("lock").expect("free");
        assert!(
            lock(d.path()).expect("lock").is_none(),
            "the second one doesn't take it"
        );
        drop(one);
        assert!(
            lock(d.path()).expect("lock").is_some(),
            "once released, the next one does"
        );
    }

    /// Not just anyone can read the file: the session carries the paths the
    /// reader moves through.
    #[cfg(unix)]
    #[test]
    fn the_file_belongs_only_to_the_owner() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tempfile::tempdir().expect("tmp");
        write(d.path(), &session(1, 1)).expect("writes");
        let mode = std::fs::metadata(path(d.path()))
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");
    }
}
