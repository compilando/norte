//! The UI session the daemon stores (L2).
//!
//! It's called `ui_session` and not `session` because the `sessions` module
//! already exists and is something else: AGENT sessions. This one is a
//! human's screen.
//!
//! The core stores it, versions it, and returns it; **it does not read it**.
//! Same decision as ADR 0058 —"a screen is a tree the core stores and does
//! not interpret"— carried to the next-door process: the body's types live
//! in `norte-frontend`, which depends on `norte-proto` and not the other
//! way around.
//!
//! The only two things this module enforces both try to protect itself: the
//! `revision` (a stale writer doesn't clobber the current one) and the
//! 1 MiB cap (a buggy client doesn't fill the disk).

pub mod disk;

use std::sync::Mutex;

use norte_proto::methods::{SESSION_BODY_MAX, Session};

/// Why a `put` was refused.
#[derive(Debug, thiserror::Error)]
pub enum PutError {
    /// The revision the client brought isn't the current one.
    #[error("stale revision; the current one is {current}")]
    Conflict {
        /// The current revision, so the client can re-read against it.
        current: u64,
    },
    /// The body exceeds [`SESSION_BODY_MAX`].
    #[error("the body takes up {bytes} bytes and the cap is {SESSION_BODY_MAX}")]
    TooLarge {
        /// Serialized bytes it brought.
        bytes: usize,
    },
    /// The session is CLOSED: this process already did its last dump.
    #[error("the session is already closed; there's nobody left to write it")]
    Sealed,
    /// The body claims to be of a schema this core doesn't know how to READ
    /// (#247).
    ///
    /// Accepting it was the worst possible outcome: the core would dump to
    /// disk a document its own load guard rejects, so from the next
    /// startup on `session.get` would answer "from the future", the
    /// session would stop having an owner, and persistence would die
    /// silently until someone deleted the file by hand. A newer `ntc`
    /// against an older `norte` —the two `SCHEMA_VERSION`s live in
    /// different crates— was all it took.
    #[error("the body is of version {version} and this core knows {known}")]
    UnknownSchema {
        /// The one the client brought.
        version: u32,
        /// The newest this core knows how to read.
        known: u32,
    },
}

/// The daemon's live session: one per process, with its revision and its
/// owner.
#[derive(Debug, Default)]
pub struct SessionStore {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    session: Session,
    /// Owning connection, if any claimed it.
    owner: Option<u64>,
    /// There are changes not yet dumped to disk.
    dirty: bool,
    /// Nothing more is accepted: the process is shutting down.
    sealed: bool,
}

impl SessionStore {
    /// A store that starts with the session that came from disk.
    #[must_use]
    pub fn new(session: Session) -> Self {
        Self {
            inner: Mutex::new(Inner {
                session,
                owner: None,
                dirty: false,
                sealed: false,
            }),
        }
    }

    /// The current session. Cloning is cheap compared to holding the lock
    /// while serializing to a socket.
    #[must_use]
    pub fn get(&self) -> Session {
        self.lock().session.clone()
    }

    /// Replaces the whole session. Returns the NEW revision.
    ///
    /// # Errors
    ///
    /// [`PutError::TooLarge`] if the body exceeds [`SESSION_BODY_MAX`], and
    /// [`PutError::Conflict`] if the revision the client brings isn't the
    /// current one. In both cases what's stored stays EXACTLY as it was:
    /// truncating a document whose schema is unknown is worse than
    /// rejecting it.
    pub fn put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> Result<u64, PutError> {
        // The cap is measured in SERIALIZED BYTES, which is what it takes
        // up on the wire and on disk. A body that doesn't even serialize
        // doesn't fit anywhere, so it counts as the worst possible case.
        let bytes = serde_json::to_vec(&body).map_or(usize::MAX, |v| v.len());
        if bytes > SESSION_BODY_MAX {
            return Err(PutError::TooLarge { bytes });
        }
        let mut g = self.lock();
        // Under the SAME lock as the mutation, and not against a separate
        // atomic token: checking outside and mutating inside leaves a
        // window —a thread preempted right in the middle— through which a
        // `put` enters AFTER the last dump and gets answered with a
        // revision that will never reach any disk. Same lesson as
        // `pin_for_task` in #205.
        if g.sealed {
            return Err(PutError::Sealed);
        }
        if g.session.revision != revision {
            return Err(PutError::Conflict {
                current: g.session.revision,
            });
        }
        // A schema this core doesn't know how to read is NOT written
        // (#247). The guard lives here and not in the handler for the same
        // reason as the size one: it's checked by whoever is going to
        // store it, which is the only one who knows what it can read back.
        // `0` is "the client didn't say", and it's rejected too: the
        // stored session is read by `disk::load`, which decides by this
        // number, and a zero would end up written next to a real body.
        if version == 0 || version > crate::ui_session::disk::SCHEMA_VERSION {
            return Err(PutError::UnknownSchema {
                version,
                known: crate::ui_session::disk::SCHEMA_VERSION,
            });
        }
        g.session.version = version;
        g.session.body = body;
        // `saturating_add`: the revision COMES IN from the file, and a file
        // is something any process of the same uid can leave lying around.
        // At the cap, still accepting `put` and no longer counting is the
        // worst that happens; adding without care used to be a panic in
        // debug —inside the mutex, poisoning it— and a wrap to zero in
        // release, which reopens exactly the stale-writer window the
        // revision exists to close.
        g.session.revision = g.session.revision.saturating_add(1);
        g.dirty = true;
        Ok(g.session.revision)
    }

    /// The first connection to claim it keeps it; the rest get `false` and
    /// run unowned. Claiming twice from the same connection isn't an
    /// error.
    pub fn claim(&self, conn: u64) -> bool {
        let mut g = self.lock();
        match g.owner {
            None => {
                g.owner = Some(conn);
                true
            }
            Some(actual) => actual == conn,
        }
    }

    /// Releases ownership, if it's this connection's. Releasing someone
    /// else's does nothing: a connection doesn't evict another by
    /// disconnecting.
    pub fn release(&self, conn: u64) {
        let mut g = self.lock();
        if g.owner == Some(conn) {
            g.owner = None;
        }
    }

    /// Which connection is in charge, if any.
    #[must_use]
    pub fn owner(&self) -> Option<u64> {
        self.lock().owner
    }

    /// There are undumped changes.
    #[must_use]
    pub fn dirty(&self) -> bool {
        self.lock().dirty
    }

    /// What the disk writer consumes: returns the session ONCE per change
    /// and clears the flag. With no changes, `None` — and the writer
    /// doesn't touch the disk, which is what makes waking up every second
    /// cheap.
    #[must_use]
    pub fn take_dirty(&self) -> Option<Session> {
        let mut g = self.lock();
        if !g.dirty {
            return None;
        }
        g.dirty = false;
        Some(g.session.clone())
    }

    /// Closes the session: from here on, no `put` gets in.
    ///
    /// Called by shutdown RIGHT BEFORE the last dump. Whatever arrives
    /// after gets an honest refusal instead of an `Ok(revision)` over a
    /// file nobody is going to write anymore —and whose lock is about to be
    /// released—.
    pub fn seal(&self) {
        self.lock().sealed = true;
    }

    /// Is it closed?
    #[must_use]
    pub fn sealed(&self) -> bool {
        self.lock().sealed
    }

    /// Adopts the document that's ON DISK upon belatedly getting write
    /// rights.
    ///
    /// It takes the body AND the revision, not just the number. While this
    /// process ran unowned, whoever held the lock kept saving: their
    /// document is the current one, and keeping only their revision would
    /// mean answering the client with its OWN body under the other one's
    /// number — and the first write after the handover would clobber,
    /// with no conflict and no warning, everything the other had saved.
    /// What the client wants to keep from that document is its call, since
    /// it's the only one that knows how to read it.
    ///
    /// A lower revision isn't adopted: the number doesn't go backward.
    pub fn adopt_from_disk(&self, session: Session) {
        let mut g = self.lock();
        if session.revision >= g.session.revision {
            g.session = session;
            // What was adopted is ALREADY on disk: marking it dirty would
            // just rewrite it identically, and the handover's first dump
            // would be a copy.
            g.dirty = false;
        }
    }

    /// Raises the revision to whatever is already ON DISK, if it's higher.
    ///
    /// This is for one specific case: a process that started with no write
    /// rights and gets them later (the window that had them closed).
    /// While it ran unowned, the other one kept raising the file's
    /// revision, and dumping ours as-is would renumber it BACKWARD —"the
    /// core raises it on every accepted put" would stop being true for
    /// whoever reads the file afterward.
    ///
    /// The body is NOT touched: the screen being saved is this window's,
    /// which is the one still alive. What the client has to do with what
    /// the other one saved —keeping the gaps only it had— is the client's
    /// call, since it's the only one that knows how to read the body.
    pub fn adopt_revision(&self, revision: u64) {
        let mut g = self.lock();
        if revision > g.session.revision {
            g.session.revision = revision;
        }
    }

    /// Marks dirty again whatever [`Self::take_dirty`] took and couldn't be
    /// written.
    ///
    /// Without this, a dump failure —a full disk, a momentary `EIO`— is
    /// never retried: the flag was already clean, so the next tick sees
    /// nothing to do and the session is lost until the human moves
    /// something again. The price of retrying is one `open` per second for
    /// as long as the failure lasts.
    pub fn mark_dirty(&self) {
        self.lock().dirty = true;
    }

    /// The lock, recovered from poisoning: a panic on another thread while
    /// cloning a session isn't a reason to bring down the daemon, and the
    /// invariant it protects is "one field consistent with another", not
    /// memory.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(n: usize) -> serde_json::Value {
        serde_json::json!({ "filler": "x".repeat(n) })
    }

    /// A session never written is revision 0 with no schema: a client that
    /// starts against a clean daemon can't tell "there is none" from "it
    /// failed".
    #[test]
    fn a_new_session_is_revision_zero() {
        let s = SessionStore::default();
        let g = s.get();
        assert_eq!(g.revision, 0);
        assert_eq!(g.version, 0, "no schema until someone writes one");
    }

    /// A body of a schema this core doesn't know how to READ isn't written
    /// (#247).
    ///
    /// Accepting it was the worst possible outcome: the core would dump a
    /// document its own load guard rejects, so from the next startup on the
    /// session would stay "from the future" forever, with no owner and no
    /// persistence, until someone deleted the file by hand. A newer `ntc`
    /// against an older `norte` was enough: the two `SCHEMA_VERSION`s live
    /// in different crates.
    #[test]
    fn a_schema_this_core_cannot_read_is_not_written() {
        let s = SessionStore::default();
        assert!(matches!(
            s.put(disk::SCHEMA_VERSION + 1, 0, body(1)),
            Err(PutError::UnknownSchema { .. })
        ));
        assert_eq!(s.get().revision, 0, "and it doesn't count as a write");
        assert!(
            s.take_dirty().is_none(),
            "nor does it leave anything to dump"
        );
        // Zero is "the client didn't say", and the stored session is read
        // BY that number: writing it next to a real body would leave a
        // file nobody knows how to interpret.
        assert!(matches!(
            s.put(0, 0, body(1)),
            Err(PutError::UnknownSchema { .. })
        ));
        // And the version this core knows how to read DOES get in.
        assert_eq!(s.put(disk::SCHEMA_VERSION, 0, body(1)).ok(), Some(1));
    }

    /// Every accepted `put` raises the revision, and the one it returns is
    /// what the client has to bring next time.
    #[test]
    fn every_put_raises_the_revision() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        assert_eq!(s.put(1, 0, body(1)).expect("first put"), 1);
        assert_eq!(s.put(1, 1, body(1)).expect("second put"), 2);
        assert_eq!(s.get().revision, 2);
    }

    /// A stale revision is `Conflict` WITH the current one: the client
    /// re-reads without having to ask again what to check against.
    #[test]
    fn a_stale_revision_is_a_conflict_and_does_not_write() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.put(1, 0, body(1)).expect("first put");
        let e = s.put(1, 0, body(2)).expect_err("stale");
        assert!(matches!(e, PutError::Conflict { current: 1 }), "{e:?}");
        assert_eq!(s.get().body, body(1), "what's stored isn't touched");
    }

    /// Over the cap: `TooLarge`, and what's stored STAYS PUT.
    #[test]
    fn over_the_cap_is_rejected_not_truncated() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.put(1, 0, body(10)).expect("fits");
        let e = s
            .put(1, 1, body(SESSION_BODY_MAX + 1))
            .expect_err("does not fit");
        assert!(matches!(e, PutError::TooLarge { .. }), "{e:?}");
        assert_eq!(s.get().revision, 1, "the stored session stays");
    }

    /// The cap is measured over serialized BYTES, not the number of keys
    /// nor the depth.
    #[test]
    fn the_cap_is_measured_in_serialized_bytes() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        let just_under = serde_json::json!({ "x": "y".repeat(SESSION_BODY_MAX - 12) });
        let bytes = serde_json::to_vec(&just_under).expect("serializes").len();
        assert!(bytes <= SESSION_BODY_MAX, "{bytes}");
        s.put(1, 0, just_under).expect("just under fits");
    }

    /// The cap is a `<=`, and it's checked at the exact byte: a body of
    /// exactly [`SESSION_BODY_MAX`] fits, and one byte more doesn't.
    #[test]
    fn the_exact_cap_fits_and_one_more_does_not() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        // `{"x":"…"}` is 8 bytes of envelope.
        let exact = serde_json::json!({ "x": "y".repeat(SESSION_BODY_MAX - 8) });
        assert_eq!(
            serde_json::to_vec(&exact).expect("serializes").len(),
            SESSION_BODY_MAX,
            "the fixture has to measure the EXACT cap"
        );
        s.put(1, 0, exact).expect("the exact cap fits");
        let over = serde_json::json!({ "x": "y".repeat(SESSION_BODY_MAX - 7) });
        let e = s.put(1, 1, over).expect_err("one more does not");
        assert!(matches!(e, PutError::TooLarge { .. }), "{e:?}");
    }

    /// **#233**: with the session closed, a `put` no longer gets in — and
    /// closure is checked UNDER THE SAME LOCK as the mutation.
    ///
    /// Against a separate atomic token, the whole window was left open: a
    /// thread preempted between "is it shutting down?" and the store's
    /// lock would write AFTER the last dump, and get answered with a
    /// revision that would never reach any disk.
    #[test]
    fn once_the_session_is_closed_a_put_does_not_get_in() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.put(1, 0, body(1)).expect("gets in before closing");
        assert!(!s.sealed());
        s.seal();
        assert!(s.sealed());
        let e = s.put(1, 1, body(2)).expect_err("not after");
        assert!(matches!(e, PutError::Sealed), "{e:?}");
        assert_eq!(s.get().body, body(1), "and what's stored stays");
    }

    /// Belatedly getting write rights adopts the WHOLE document, not just
    /// its number.
    ///
    /// Keeping only the revision and not the body meant the first write
    /// after the handover would fit with no conflict and clobber, with no
    /// warning, everything the other window had saved while this one ran
    /// unowned.
    #[test]
    fn adopting_from_disk_takes_the_body_and_not_just_the_revision() {
        let s = SessionStore::default();
        s.adopt_from_disk(Session {
            version: 1,
            revision: 42,
            body: body(3),
        });
        let g = s.get();
        assert_eq!(g.revision, 42);
        assert_eq!(g.body, body(3), "the other window's body");
        assert!(!s.dirty(), "what was adopted is already on disk");
        // And it doesn't go backward: an older file doesn't dethrone the
        // current one.
        s.adopt_from_disk(Session {
            version: 1,
            revision: 7,
            body: body(9),
        });
        assert_eq!(s.get().revision, 42);
    }

    /// A failed dump is marked dirty again: without this the flag was
    /// already clean and the next tick wouldn't retry ANYTHING.
    #[test]
    fn a_failed_dump_gets_marked_dirty_again() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.put(1, 0, body(1)).expect("put");
        let taken = s.take_dirty().expect("there's something to write");
        assert!(!s.dirty());
        // Here the writer fails (full disk, EIO…).
        s.mark_dirty();
        assert!(s.dirty(), "the next tick retries it");
        assert_eq!(s.take_dirty().expect("again").body, taken.body);
    }

    /// Two writers at once on the same store: revisions come out
    /// consecutive and none is lost. The mutex is what guarantees it, and
    /// this is what pins it down.
    #[test]
    fn two_threads_do_not_clobber_the_revision() {
        let s = std::sync::Arc::new(SessionStore::default());
        assert!(s.claim(1));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let s = std::sync::Arc::clone(&s);
                std::thread::spawn(move || {
                    let mut done = 0_u32;
                    for _ in 0..50 {
                        let rev = s.get().revision;
                        if s.put(1, rev, body(1)).is_ok() {
                            done += 1;
                        }
                    }
                    done
                })
            })
            .collect();
        let accepted: u32 = threads.into_iter().map(|h| h.join().expect("thread")).sum();
        assert_eq!(
            u64::from(accepted),
            s.get().revision,
            "one revision per accepted put, not one more"
        );
    }

    /// The owner is the FIRST one to claim it; releasing it frees it for
    /// the next one. Never two writers over one state.
    #[test]
    fn only_the_owner_writes() {
        let s = SessionStore::default();
        assert!(s.claim(1), "the first one keeps it");
        assert!(!s.claim(2), "the second runs unowned");
        assert!(
            s.claim(1),
            "claiming twice from the same one isn't an error"
        );
        assert_eq!(s.owner(), Some(1));
        s.put(1, 0, body(1)).expect("the owner writes");
        s.release(1);
        assert_eq!(s.owner(), None);
        assert!(
            s.claim(2),
            "once the owner leaves, the next one can take it"
        );
    }

    /// Releasing an ownership you don't hold doesn't take it from anyone.
    #[test]
    fn releasing_someone_elses_does_nothing() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        s.release(2);
        assert_eq!(s.owner(), Some(1), "2 cannot evict 1");
    }

    /// `take_dirty` returns the session ONCE per change, and nothing if
    /// nothing has changed since the last one.
    #[test]
    fn dirty_is_consumed_exactly_once() {
        let s = SessionStore::default();
        assert!(s.claim(1));
        assert!(s.take_dirty().is_none(), "nothing to write at startup");
        s.put(1, 0, body(1)).expect("put");
        assert!(s.dirty());
        assert!(s.take_dirty().is_some());
        assert!(!s.dirty(), "consumed");
        assert!(s.take_dirty().is_none());
    }

    /// A session that comes from disk starts clean: loading it isn't a
    /// change that needs writing back.
    #[test]
    fn a_session_loaded_from_disk_is_not_born_dirty() {
        let s = SessionStore::new(Session {
            version: 1,
            revision: 9,
            body: body(1),
        });
        assert_eq!(s.get().revision, 9);
        assert!(!s.dirty());
    }
}
