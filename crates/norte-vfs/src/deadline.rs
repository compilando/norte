//! A deadline for a syscall that can't be cancelled.
//!
//! Lives here, and not in every crate that needs it, because the reason it
//! exists doesn't belong to anyone in particular: **a dead network mount
//! doesn't cancel**. A `statfs` over a downed NFS enters D-state and
//! doesn't come out until the mount answers or someone force-unmounts it;
//! there's no signal, no token, and `spawn_blocking` cancels nothing — it
//! only chooses who waits.

use std::time::Duration;

/// Runs `query` (blocking) on a DETACHED [`std::thread`], not on
/// [`tokio::task::spawn_blocking`], and gives up at `deadline`.
///
/// # Why a loose thread and not the pool
///
/// Tokio's blocking pool is BOUNDED (512 threads by default) and SHARED
/// with everything else in the process, including every `spawn_blocking` a
/// provider does for ordinary disk work. A query against a dead mount
/// hangs indefinitely and there's no way to cancel an in-flight syscall:
/// if that happened in the pool, a downed mount would tie up a slot for as
/// long as it stayed down, and a few of those would leave the daemon
/// without a pool for ANY other local operation.
///
/// A `std::thread` costs one system thread leaked per hung probe instead
/// of a slot of a shared resource — worse in isolation (a fresh stack,
/// never reused) and much better in aggregate, because it can't block
/// anyone else. And the `oneshot` the `timeout` walks away from lets the
/// runtime shut down without waiting for it, which a `spawn_blocking` task
/// doesn't allow: the runtime waits for it on shutdown even after its
/// caller no longer does.
///
/// `None` = didn't answer in time. What that means is up to the caller:
/// for volumes it's "this mount doesn't say its size", and for
/// `capabilities_at` it's "I keep what the provider declares". This
/// function has no opinion.
///
/// ```
/// use norte_vfs::deadline::blocking_with_deadline;
/// use std::time::Duration;
///
/// let rt = tokio::runtime::Builder::new_current_thread()
///     .enable_time()
///     .build()
///     .expect("runtime");
/// rt.block_on(async {
///     // Answers in time.
///     assert_eq!(blocking_with_deadline(|| 7, Duration::from_secs(5)).await, Some(7));
///
///     // Doesn't answer: the caller stays alive, and the thread is left
///     // alone with its syscall until the system releases it.
///     let late = blocking_with_deadline(
///         || std::thread::sleep(Duration::from_secs(30)),
///         Duration::from_millis(20),
///     )
///     .await;
///     assert!(late.is_none());
/// });
/// ```
pub async fn blocking_with_deadline<T, F>(query: F, deadline: Duration) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        // The receiver may already be gone (the deadline expired and
        // `timeout` dropped its half): `send` failing only means nobody's
        // listening anymore, not that anything went wrong.
        let _ = tx.send(query());
    });
    tokio::time::timeout(deadline, rx)
        .await
        .ok()
        .and_then(Result::ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordinary case: it answers and its answer is returned.
    #[tokio::test]
    async fn an_answer_in_time_arrives_whole() {
        assert_eq!(
            blocking_with_deadline(|| "hi", Duration::from_secs(5)).await,
            Some("hi")
        );
    }

    /// And what matters: whoever asks does NOT stay hung with the thread.
    ///
    /// The measurement is of the caller, not of the thread: the thread
    /// stays inside its syscall — there's no way to cancel it — and that's
    /// what this function is about. What's checked is that the `await`
    /// returns within the deadline and not after the block's 30 seconds.
    #[tokio::test]
    async fn the_caller_returns_within_the_deadline_even_if_the_thread_keeps_going() {
        let t0 = std::time::Instant::now();
        let r = blocking_with_deadline(
            || std::thread::sleep(Duration::from_secs(30)),
            Duration::from_millis(50),
        )
        .await;
        let dt = t0.elapsed();
        assert!(r.is_none(), "did not answer in time: the answer is `None`");
        assert!(
            dt < Duration::from_secs(5),
            "the caller returned in {dt:?}, meaning it waited on the thread"
        );
    }

    /// A panic inside the query is "did not answer", not a panic of the
    /// caller's: the thread dies with its `Sender` and the receiver reads
    /// a closed channel. Without this, a platform probe blowing up would
    /// take down whoever asked, which is the daemon's task.
    #[tokio::test]
    async fn a_panic_in_the_query_is_did_not_answer() {
        let r = blocking_with_deadline(
            || -> u8 { panic!("the probe blows up") },
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(r, None);
    }
}
