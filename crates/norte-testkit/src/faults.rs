//! Deterministic fault injection for [`MemProvider`](crate::MemProvider): the
//! copy engine and cancellation are tested without touching disk or
//! depending on randomness (spec §12). Every fault is configured BEFORE the
//! operation and fires at an exact point (op N, byte N).

use std::sync::Mutex;
use std::time::Duration;

use norte_proto::VPath;

/// Internal key: the `VPath`'s segments (raw bytes).
pub(crate) type SegPath = Vec<Vec<u8>>;

pub(crate) fn seg_path(p: &VPath) -> SegPath {
    p.segments().map(<[u8]>::to_vec).collect()
}

/// A [`MemProvider`](crate::MemProvider)'s fault configuration.
///
/// Shared via `Arc`: tests keep the handle and mutate the config while the
/// provider is in use. Everything is deterministic — no probabilities.
#[derive(Debug, Default)]
pub struct Faults {
    inner: Mutex<FaultState>,
}

#[derive(Debug, Default)]
struct FaultState {
    latency_per_op: Option<Duration>,
    fail_read_at: Option<(SegPath, usize)>,
    fail_write_at: Option<(SegPath, usize)>,
    /// This path's (byte-exact) `list` fails with `Error::Io`; the rest of
    /// the tree lists normally. For a walker that must KEEP GOING despite an
    /// unreadable subdir (fs.search).
    fail_list_at: Option<SegPath>,
    /// The `rename` whose SOURCE is this path (byte-exact) fails with
    /// `Error::Io`, WITHOUT applying its effect. For the transactional batch
    /// executor: the step that triggers the rollback.
    fail_rename_at: Option<SegPath>,
    /// `rename` OVERWRITES the destination instead of rejecting it
    /// (sftp's posix-rename, object's copy+delete). For testing the
    /// anti-clobber guards of the CALLER, which cannot be told apart from
    /// the provider's own rejection on a provider that already rejects.
    rename_clobbers: bool,
    /// How many renames are left before cancelling
    /// [`FaultState::cancel_token`] (#274): this is what lets a test land
    /// INSIDE a sequence of two.
    cancel_after_renames: Option<u64>,
    /// How many `list`s are left before cancelling
    /// [`FaultState::cancel_token`]: this is what lets a test land INSIDE a
    /// walk and neither before nor after it.
    cancel_after_lists: Option<u64>,
    /// The token that gets cancelled when one of the counters above reaches
    /// zero. It is ONE for both: a test arms whichever it needs, and arming
    /// both at once describes no specific moment.
    cancel_token: Option<tokio_util::sync::CancellationToken>,
    /// `Some(n)`: `n` operations left before disconnection.
    disconnect_after: Option<u64>,
    /// The next `n` operations fail retryable (TRANSIENT unavailability);
    /// afterward the provider recovers on its own.
    unavailable_next: u64,
    /// The next `n` MUTATIONS that get applied return a transient error
    /// AFTER applying their effect (post-effect ambiguity).
    ambiguous_next: u64,
    /// `copy_native` stays PENDING while this is armed (simulates a
    /// minutes-long S3 multipart copy): only the caller's cancellation —
    /// dropping the future — ends it. Deterministic, no global latency.
    hold_copy_native: bool,
    /// `true` since some `copy_native` ENTERED the gate: the test syncs its
    /// cancel to this signal, with no blind sleeps.
    copy_native_entered: bool,
    /// Number of `Provider::read` calls served (a counter, not a fault).
    read_calls: u64,
}

impl Faults {
    /// Fixed latency added to every operation (uses tokio's clock: compatible
    /// with `tokio::time::pause`).
    pub fn set_latency_per_op(&self, latency: Option<Duration>) {
        self.lock().latency_per_op = latency;
    }

    /// Reading `path` fails with [`Error::Io`](norte_proto::Error::Io) after
    /// delivering exactly `byte_n` bytes.
    ///
    /// The key is compared byte-exact against the requested path, WITHOUT
    /// case folding: it points the fault at the same string the operation
    /// will use.
    pub fn fail_read_at(&self, path: &VPath, byte_n: usize) {
        self.lock().fail_read_at = Some((seg_path(path), byte_n));
    }

    /// Writing to `path` fails with [`Error::Io`](norte_proto::Error::Io) as
    /// soon as the total written reaches `byte_n` bytes.
    ///
    /// Byte-exact key, no case folding (see [`Self::fail_read_at`]).
    pub fn fail_write_at(&self, path: &VPath, byte_n: usize) {
        self.lock().fail_write_at = Some((seg_path(path), byte_n));
    }

    /// `path`'s `list` (byte-exact key, no case folding) fails with
    /// [`Error::Io`](norte_proto::Error::Io) `{retryable: true}`; other
    /// directories list normally. For testing that a walker (fs.search)
    /// KEEPS GOING in the face of an unreadable subdir.
    pub fn fail_list_at(&self, path: &VPath) {
        self.lock().fail_list_at = Some(seg_path(path));
    }

    /// The `rename` whose SOURCE is `path` (byte-exact key, no case folding)
    /// fails with [`Error::Io`](norte_proto::Error::Io) `{retryable: false}`
    /// and **without applying its effect**: the tree stays exactly as it
    /// was.
    ///
    /// It is the failure a transactional executor needs — step k dies and
    /// everything before it has to be undone. Non-retryable on purpose: an
    /// injected fault is not cured by retrying, and a batch that retried
    /// would only paper over the rollback the test wants to observe.
    ///
    /// The fault is NOT consumed: while it is armed, EVERY rename from that
    /// source fails — including the rollback's, which is how the "the
    /// reversal couldn't either" path gets tested. Disarm it with
    /// [`Self::clear`].
    pub fn fail_rename_at(&self, path: &VPath) {
        self.lock().fail_rename_at = Some(seg_path(path));
    }

    /// Cancels `token` right AFTER the `n`-th rename that gets applied.
    ///
    /// The only control that lets a test land INSIDE a sequence of renames
    /// and neither before nor after: it is where the state a spelling change
    /// (#274) must not leave visible lives — the file under the bypass name.
    /// A `sleep` would hit it by chance; this does not depend on the clock.
    pub fn cancel_after_renames(&self, n: u64, token: tokio_util::sync::CancellationToken) {
        let mut s = self.lock();
        s.cancel_after_renames = Some(n);
        s.cancel_token = Some(token);
    }

    /// Queried by the provider after applying a rename: decrements, and
    /// cancels the token on reaching zero.
    pub fn tick_rename(&self) {
        let mut s = self.lock();
        let Some(remaining) = s.cancel_after_renames else {
            return;
        };
        if remaining <= 1 {
            s.cancel_after_renames = None;
            if let Some(t) = s.cancel_token.take() {
                t.cancel();
            }
        } else {
            s.cancel_after_renames = Some(remaining - 1);
        }
    }

    /// Cancels `token` right AFTER the `n`-th `list` that gets served.
    ///
    /// [`Self::cancel_after_renames`]'s twin for walkers. A walk cancelled
    /// before it starts proves nothing —the body exits on its first check
    /// and leaves a blank report, indistinguishable from never having
    /// run— so to prove a walk stops CLEANLY it has to be cancelled while
    /// inside it. A `sleep` gets it right by chance; this, always and
    /// without a clock.
    pub fn cancel_after_lists(&self, n: u64, token: tokio_util::sync::CancellationToken) {
        let mut s = self.lock();
        s.cancel_after_lists = Some(n);
        s.cancel_token = Some(token);
    }

    /// Queried by the provider when serving a `list`: decrements, and
    /// cancels the token on reaching zero.
    pub fn tick_list(&self) {
        let mut s = self.lock();
        let Some(remaining) = s.cancel_after_lists else {
            return;
        };
        if remaining <= 1 {
            s.cancel_after_lists = None;
            if let Some(t) = s.cancel_token.take() {
                t.cancel();
            }
        } else {
            s.cancel_after_lists = Some(remaining - 1);
        }
    }

    /// `rename` stops rejecting a busy destination and OVERWRITES it, the
    /// way providers whose rename is not atomic really behave: sftp's
    /// posix-rename and object's copy+delete.
    ///
    /// Exists so a CALLER's anti-clobber guard can be tested. Without this, a
    /// test against `MemProvider` —which rejects on its own— still passes
    /// with the guard deleted: what it proves is the provider's contract,
    /// not the belt of whoever uses it.
    pub fn rename_clobbers(&self, clobber: bool) {
        self.lock().rename_clobbers = clobber;
    }

    /// `true` if `rename` must overwrite the destination instead of
    /// rejecting it.
    #[must_use]
    pub(crate) fn renames_clobber(&self) -> bool {
        self.lock().rename_clobbers
    }

    /// After `n` more operations, EVERY operation returns
    /// [`Error::ProviderUnavailable`](norte_proto::Error::ProviderUnavailable)
    /// with `retryable: true` (the provider "disconnected").
    pub fn disconnect_after(&self, n: u64) {
        self.lock().disconnect_after = Some(n);
    }

    /// The next `n` operations fail with
    /// [`Error::ProviderUnavailable`](norte_proto::Error::ProviderUnavailable)
    /// `{retryable: true}` and AFTERWARD the provider recovers on its own —
    /// the transient counterpart of [`Self::disconnect_after`], for testing
    /// the engine's backoff retries (ADR 0005).
    pub fn unavailable_for_next(&self, n: u64) {
        self.lock().unavailable_next = n;
    }

    /// The next `n` point mutations (`mkdir`/`remove`/`rename`/`symlink`)
    /// that reach APPLICATION return
    /// [`Error::ProviderUnavailable`](norte_proto::Error::ProviderUnavailable)
    /// `{retryable: true}` AFTER applying their effect — the "timeout after
    /// commit" of a remote provider (issue #17): the caller cannot know
    /// whether the mutation happened. Reads and mutations that fail for
    /// another reason do NOT consume the counter.
    pub fn ambiguous_mutations(&self, n: u64) {
        self.lock().ambiguous_next = n;
    }

    /// Arms (or disarms) `copy_native`'s gate: armed, the native copy stays
    /// PENDING indefinitely — the deterministic equivalent of a
    /// minutes-long S3 multipart copy (#51). The caller escapes by
    /// cancelling (dropping the future) or disarming the gate (`false` /
    /// [`Self::clear`], observed within ≤20ms); other operations are
    /// unaffected.
    pub fn hold_copy_native(&self, hold: bool) {
        self.lock().hold_copy_native = hold;
    }

    /// `true` if some `copy_native` has already ENTERED the gate: the test
    /// waits for this signal before cancelling — deterministic, no blind
    /// sleeps.
    #[must_use]
    pub fn copy_native_entered(&self) -> bool {
        self.lock().copy_native_entered
    }

    pub(crate) async fn copy_native_gate(&self) {
        // Cheap poll: compatible with `tokio::time::pause` and does not hold
        // the lock across the await.
        self.lock().copy_native_entered = true;
        while self.lock().hold_copy_native {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Number of `Provider::read` calls served (not bytes or chunks):
    /// observability for coalescing/cache tests (#61). Also counts reads
    /// that then fail from an injected fault; does not count ones rejected
    /// by `op_gate` (disconnection).
    #[must_use]
    pub fn read_calls(&self) -> u64 {
        self.lock().read_calls
    }

    pub(crate) fn count_read(&self) {
        self.lock().read_calls += 1;
    }

    /// Clears the whole fault configuration. Also resets
    /// [`Self::read_calls`]'s counter (via `FaultState::default()`).
    pub fn clear(&self) {
        *self.lock() = FaultState::default();
    }

    /// Every operation's entry gate: applies latency and disconnection.
    /// Returns `Err` if the provider is already "disconnected".
    pub(crate) async fn op_gate(&self) -> Result<(), norte_proto::Error> {
        let latency = {
            let mut st = self.lock();
            if st.unavailable_next > 0 {
                st.unavailable_next -= 1;
                return Err(norte_proto::Error::ProviderUnavailable { retryable: true });
            }
            if let Some(remaining) = st.disconnect_after {
                if remaining == 0 {
                    return Err(norte_proto::Error::ProviderUnavailable { retryable: true });
                }
                st.disconnect_after = Some(remaining - 1);
            }
            st.latency_per_op
        };
        if let Some(d) = latency {
            tokio::time::sleep(d).await;
        }
        Ok(())
    }

    /// Consumes one ambiguous-mutation charge, if armed. Called by every Mem
    /// mutation RIGHT AFTER applying its effect.
    pub(crate) fn take_ambiguous(&self) -> bool {
        let mut st = self.lock();
        if st.ambiguous_next > 0 {
            st.ambiguous_next -= 1;
            true
        } else {
            false
        }
    }

    /// Snapshot of the read fault for `path`, if it applies.
    pub(crate) fn read_fault_for(&self, key: &SegPath) -> Option<usize> {
        let st = self.lock();
        match &st.fail_read_at {
            Some((p, n)) if p == key => Some(*n),
            _ => None,
        }
    }

    /// `true` if `key`'s `list` must fail (byte-exact injected fault).
    pub(crate) fn list_fails_for(&self, key: &SegPath) -> bool {
        self.lock().fail_list_at.as_ref() == Some(key)
    }

    /// `true` if the `rename` FROM `key` must fail (byte-exact injected
    /// fault) before touching anything.
    pub(crate) fn rename_fails_from(&self, key: &SegPath) -> bool {
        self.lock().fail_rename_at.as_ref() == Some(key)
    }

    /// Snapshot of the write fault for `path`, if it applies.
    pub(crate) fn write_fault_for(&self, key: &SegPath) -> Option<usize> {
        let st = self.lock();
        match &st.fail_write_at {
            Some((p, n)) if p == key => Some(*n),
            _ => None,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FaultState> {
        // Invariant: nobody panics with the lock held; poisoning is impossible.
        self.inner.lock().expect("faults lock is sound")
    }
}
