//! uid/gid → user and group name (ADR 0145), for the `posix.owner` and
//! `posix.group` columns.
//!
//! The libc (`getpwuid_r`/`getgrgid_r`) is asked, not `/etc/passwd`: only
//! the libc sees what NSS resolves — LDAP, SSSD, systemd-homed —, and a
//! name missing there is the one missing from `ls -l`. That question can
//! go over the network, so:
//!
//! - it's called ONLY from code that's already blocking (a listing's
//!   `stat` runs in `spawn_blocking`), never from an async context;
//! - the answer is stored for [`VALIDITY`] per id, so a directory of ten
//!   thousand files owned by the same person asks once and not ten
//!   thousand times;
//! - each question runs on its own thread and is awaited for [`DEADLINE`]:
//!   a hung directory server leaves the cell blank instead of hanging the
//!   listing, which can't be cancelled while it's INSIDE the libc. After a
//!   deadline expires, no new ids are asked about for [`VALIDITY`] (the
//!   "brake"), so a dead NSS costs one deadline per minute and not one per
//!   owner;
//! - the cache does NOT hold the lock while it asks: at most two listings
//!   ask the same thing at once.
//!
//! The name is returned in BYTES: POSIX doesn't require it to be UTF-8,
//! and whoever paints it already knows how to mask foreign bytes.

use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How long an answer is worth, and how long the brake lasts after a
/// deadline expires: enough for a large listing to ask once per owner, and
/// little enough for a newly created or renamed user to show up without
/// restarting the daemon.
const VALIDITY: Duration = Duration::from_mins(1);

/// How long NSS is waited for. A local `/etc/passwd` answers in
/// microseconds and a healthy LDAP in milliseconds; 200 ms doesn't clip
/// any real answer, same as `CAPS_AT_DEADLINE` in the provider.
const DEADLINE: Duration = Duration::from_millis(200);

/// Distinct ids remembered. Past the cap, it's emptied entirely: it's a
/// cache, not a registry. A mount with more owners than this asks about
/// all of them again after emptying, which is slower but not incorrect.
const CAP: usize = 4096;

/// Ceiling of the libc's buffer. A `passwd` entry this size doesn't exist;
/// going past this is a broken NSS, and the answer is "no name".
const BUF_MAX: usize = 1 << 20;

/// Retries on `EINTR`: a signal mid-question isn't an answer, and must not
/// end up stored as "has no name".
const EINTR_RETRIES: usize = 3;

/// What the libc answers, distinguishing what can be remembered from what
/// can't.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Response {
    /// The id has this name.
    Name(Vec<u8>),
    /// The libc answered, and the id has no name: remembered.
    NoName,
    /// The libc did NOT answer (error, signal, impossible buffer): not
    /// remembered, so it's asked about again next time.
    Failure,
}

type Resolver = fn(u32) -> Response;

/// Per id: when it was learned and what was learned (`None` = has no name).
type Map = HashMap<u32, (Instant, Option<Vec<u8>>)>;

/// Cache and brake for one kind of id.
struct Cache {
    map: Mutex<Map>,
    /// Until when new ids aren't asked about (a deadline expired).
    brake: Mutex<Option<Instant>>,
}

impl Cache {
    fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            brake: Mutex::new(None),
        }
    }

    fn store(&self, id: u32, when: Instant, value: Option<Vec<u8>>) {
        // A poisoned lock just means it stops caching.
        if let Ok(mut m) = self.map.lock() {
            if m.len() >= CAP && !m.contains_key(&id) {
                m.clear();
            }
            m.insert(id, (when, value));
        }
    }
}

static USERS: LazyLock<Cache> = LazyLock::new(Cache::new);
static GROUPS: LazyLock<Cache> = LazyLock::new(Cache::new);

/// The name of user `uid`, or `None` if the system doesn't know one for it
/// or didn't answer in time.
pub(crate) fn usuario(uid: u32) -> Option<Vec<u8>> {
    query(&USERS, uid, Instant::now(), DEADLINE, resolve_user)
}

/// The name of group `gid`, or `None` if the system doesn't know one for it
/// or didn't answer in time.
pub(crate) fn grupo(gid: u32) -> Option<Vec<u8>> {
    query(&GROUPS, gid, Instant::now(), DEADLINE, resolve_group)
}

/// The cache, the brake and the deadline. `now`, `deadline` and `resolve`
/// come in from outside so they can be tested without sleeping and without
/// depending on the machine's users.
fn query(
    cache: &'static Cache,
    id: u32,
    now: Instant,
    deadline: Duration,
    resolve: Resolver,
) -> Option<Vec<u8>> {
    if let Ok(m) = cache.map.lock()
        && let Some((when, value)) = m.get(&id)
        && now.saturating_duration_since(*when) < VALIDITY
    {
        return value.clone();
    }
    if let Ok(f) = cache.brake.lock()
        && f.is_some_and(|until| now < until)
    {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("norte-nss".to_owned())
        .spawn(move || {
            let r = resolve(id);
            // The thread stores what it learns EVEN IF whoever asked is no
            // longer waiting: a late answer serves the next listing.
            match &r {
                Response::Name(n) => cache.store(id, now, Some(n.clone())),
                Response::NoName => cache.store(id, now, None),
                Response::Failure => {}
            }
            let _ = tx.send(r);
        });
    if spawned.is_err() {
        // No threads means no bounded question; better blank than unbounded.
        return None;
    }
    match rx.recv_timeout(deadline) {
        Ok(Response::Name(n)) => Some(n),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            if let Ok(mut f) = cache.brake.lock() {
                *f = Some(now + VALIDITY);
            }
            None
        }
        Ok(Response::NoName | Response::Failure)
        | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
    }
}

/// `getpwuid_r` with a buffer that grows while the libc says `ERANGE`.
#[allow(unsafe_code)]
fn resolve_user(uid: u32) -> Response {
    let mut buf: Vec<libc::c_char> = vec![0; 1024];
    let mut interruptions = 0;
    loop {
        // SAFETY: `libc::passwd` is a POD of pointers and integers: all
        // zeros (null pointers) is a valid value, and the libc fills it in
        // before it's read.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut res: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `pwd`, `res` and `buf` are owned and live until the end
        // of the iteration; `buf`'s REAL length is passed, and
        // `getpwuid_r` is reentrant: it only writes into them.
        let rc = unsafe {
            libc::getpwuid_r(uid, &raw mut pwd, buf.as_mut_ptr(), buf.len(), &raw mut res)
        };
        if rc == libc::ERANGE && buf.len() < BUF_MAX {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc == libc::EINTR && interruptions < EINTR_RETRIES {
            interruptions += 1;
            continue;
        }
        if rc != 0 {
            return Response::Failure;
        }
        if res.is_null() || pwd.pw_name.is_null() {
            return Response::NoName;
        }
        // SAFETY: with `rc == 0` and `res` non-null, `pw_name` points at a
        // NUL-terminated string INSIDE `buf`, which is still alive; it's
        // copied before dropping it.
        let name = unsafe { CStr::from_ptr(pwd.pw_name) };
        return Response::Name(name.to_bytes().to_vec());
    }
}

/// `getgrgid_r`, same as [`resolve_user`].
#[allow(unsafe_code)]
fn resolve_group(gid: u32) -> Response {
    let mut buf: Vec<libc::c_char> = vec![0; 1024];
    let mut interruptions = 0;
    loop {
        // SAFETY: `libc::group` is a POD of pointers and integers: all
        // zeros is a valid value, and the libc fills it in before it's
        // read.
        let mut grp: libc::group = unsafe { std::mem::zeroed() };
        let mut res: *mut libc::group = std::ptr::null_mut();
        // SAFETY: `grp`, `res` and `buf` are owned and live until the end
        // of the iteration; `buf`'s REAL length is passed, and
        // `getgrgid_r` is reentrant: it only writes into them.
        let rc = unsafe {
            libc::getgrgid_r(gid, &raw mut grp, buf.as_mut_ptr(), buf.len(), &raw mut res)
        };
        if rc == libc::ERANGE && buf.len() < BUF_MAX {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc == libc::EINTR && interruptions < EINTR_RETRIES {
            interruptions += 1;
            continue;
        }
        if rc != 0 {
            return Response::Failure;
        }
        if res.is_null() || grp.gr_name.is_null() {
            return Response::NoName;
        }
        // SAFETY: with `rc == 0` and `res` non-null, `gr_name` points at a
        // NUL-terminated string INSIDE `buf`, which is still alive; it's
        // copied before dropping it.
        let name = unsafe { CStr::from_ptr(grp.gr_name) };
        return Response::Name(name.to_bytes().to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A cache of its own per test: the globals are shared across test
    /// threads and would dirty the count.
    fn cache() -> &'static Cache {
        Box::leak(Box::new(Cache::new()))
    }

    /// uid 0 has a name on every unix; which name isn't fixed (a container
    /// may call it something else), only that it arrives with no C NUL.
    #[test]
    fn uid_zero_has_a_name_and_no_nul() {
        let Response::Name(n) = resolve_user(0) else {
            panic!("uid 0 with a name");
        };
        assert!(!n.is_empty());
        assert!(!n.contains(&0), "the C NUL doesn't sneak in: {n:?}");
    }

    #[test]
    fn gid_zero_has_a_name_and_no_nul() {
        let Response::Name(n) = resolve_group(0) else {
            panic!("gid 0 with a name");
        };
        assert!(!n.is_empty());
        assert!(!n.contains(&0), "the C NUL doesn't sneak in: {n:?}");
    }

    /// An id nobody owns is "no name" — which IS remembered —, not a
    /// failure nor a panic.
    #[test]
    fn an_unowned_id_is_nameless() {
        assert_eq!(resolve_user(u32::MAX - 7), Response::NoName);
        assert_eq!(resolve_group(u32::MAX - 7), Response::NoName);
    }

    /// Within validity it isn't asked about again; past it, it is.
    #[test]
    fn the_cache_respects_validity() {
        static QUESTIONS: AtomicUsize = AtomicUsize::new(0);
        fn alice(_: u32) -> Response {
            QUESTIONS.fetch_add(1, Ordering::SeqCst);
            Response::Name(b"alice".to_vec())
        }
        let c = cache();
        let t0 = Instant::now();
        let deadline = Duration::from_secs(10);
        assert_eq!(query(c, 7, t0, deadline, alice), Some(b"alice".to_vec()));
        assert_eq!(
            query(c, 7, t0 + VALIDITY / 2, deadline, alice),
            Some(b"alice".to_vec())
        );
        assert_eq!(QUESTIONS.load(Ordering::SeqCst), 1, "from the cache");
        let _ = query(c, 7, t0 + VALIDITY, deadline, alice);
        assert_eq!(QUESTIONS.load(Ordering::SeqCst), 2, "past it, it's asked");
    }

    /// A "has no name" is also remembered: an orphan uid repeated across
    /// ten thousand files isn't ten thousand trips to NSS.
    #[test]
    fn the_cache_also_remembers_absence() {
        static QUESTIONS: AtomicUsize = AtomicUsize::new(0);
        fn nobody(_: u32) -> Response {
            QUESTIONS.fetch_add(1, Ordering::SeqCst);
            Response::NoName
        }
        let c = cache();
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(query(c, 9, t0, Duration::from_secs(10), nobody), None);
        }
        assert_eq!(QUESTIONS.load(Ordering::SeqCst), 1);
    }

    /// A FAILURE (a signal, an NSS that returned an error) isn't
    /// remembered: it's asked about again next time, instead of leaving a
    /// real owner blank for a minute.
    #[test]
    fn a_failure_is_not_remembered() {
        static QUESTIONS: AtomicUsize = AtomicUsize::new(0);
        fn fails(_: u32) -> Response {
            QUESTIONS.fetch_add(1, Ordering::SeqCst);
            Response::Failure
        }
        let c = cache();
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(query(c, 5, t0, Duration::from_secs(10), fails), None);
        }
        assert_eq!(QUESTIONS.load(Ordering::SeqCst), 3);
    }

    /// A hung NSS leaves the cell blank after the deadline, and during
    /// validity it isn't asked about new ids again. The question blocks on
    /// a lock the test releases at the end: no sleeping at all.
    #[test]
    fn a_hung_nss_times_out_and_brakes() {
        static QUESTIONS: AtomicUsize = AtomicUsize::new(0);
        static GATE: Mutex<()> = Mutex::new(());
        fn stuck(_: u32) -> Response {
            QUESTIONS.fetch_add(1, Ordering::SeqCst);
            let _held = GATE.lock();
            Response::Name(b"late".to_vec())
        }
        let held = GATE.lock().expect("gate");
        let c = cache();
        let t0 = Instant::now();
        let deadline = Duration::from_millis(1);
        assert_eq!(
            query(c, 1, t0, deadline, stuck),
            None,
            "the deadline expires"
        );
        // The hung thread counts its question when it starts, which may be
        // after the deadline expires: wait for THAT condition, not a clock.
        let limit = Instant::now() + Duration::from_secs(30);
        while QUESTIONS.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < limit, "the NSS thread never started");
            std::thread::yield_now();
        }
        assert_eq!(query(c, 2, t0, deadline, stuck), None, "braked");
        assert_eq!(
            QUESTIONS.load(Ordering::SeqCst),
            1,
            "with the brake on, no other question is launched"
        );
        drop(held);
        // Past the brake it's asked about again (and now it answers).
        let after = t0 + VALIDITY;
        assert_eq!(
            query(c, 2, after, Duration::from_secs(10), stuck),
            Some(b"late".to_vec())
        );
    }

    /// Past the cap, the cache empties instead of growing without end.
    #[test]
    fn the_cache_does_not_grow_without_a_cap() {
        let c = cache();
        let t0 = Instant::now();
        for id in 0..=u32::try_from(CAP).expect("fits") {
            c.store(id, t0, None);
        }
        assert!(c.map.lock().expect("lock").len() <= CAP);
    }
}
