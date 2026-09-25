//! How a search ended, and which sentence matches it.
//!
//! The outcomes are said differently because they mean different things
//! **about the disk**, not about the interface: "no more" is an answer,
//! "I stopped it" is half an answer and "it broke" is not an answer at all.
//! Collapsing them leaves the screen asserting a directory does not contain
//! what was searched for when what actually happened is that nobody got
//! around to looking.
//!
//! It lives here because both frontends used to decide it on their own and
//! **with different precedence** (ADR 0077): a search the reader stopped
//! right at the cap said "cancelled" in the terminal and "there is more" in
//! the window. Now there is one precedence, and changing it changes both.

use norte_proto::TaskState;

/// How it ended, or that it is still going.
///
/// It is not `TaskState`: that is wire-level and has states that say nothing
/// to a search (`Pending`, `Paused`, `Unknown`). This is what has to be told
/// to the reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Still running, or has not started yet. Both count as "waiting".
    Running,
    /// Finished going through everything there was.
    Done,
    /// The reader stopped it. What was found stands; what is left was never
    /// looked at.
    Cancelled,
    /// It broke, and with which error CATEGORY (already translated).
    ///
    /// Persistent: a failure never degrades to "done" on the next keystroke.
    /// That cost a review round in the terminal and is inherited here
    /// already fixed.
    ///
    /// Stores the text, not the `Error`, because `ui.lang` does NOT change on
    /// the fly (`norte_i18n::force` runs once per process; the list of keys
    /// out of the host's hot-reload reach says so by name). The day the
    /// language can be changed without restarting, this has to switch to
    /// storing the error and translating at paint time.
    Failed(String),
}

/// What happened to a search task, if anything did.
///
/// `None` = not an outcome: `Running` and `Pending` are "waiting", and
/// `Unknown` — a state from a newer protocol — is too, because
/// [`TaskState::is_terminal`] deliberately counts it as non-terminal: facing
/// something it does not understand, the client keeps listening.
///
/// A `_ => Done` instead of this is what made a queued or paused search
/// announce itself as finished with no results.
///
/// ```
/// use norte_frontend::search_status::{Outcome, outcome_of};
/// use norte_proto::TaskState;
///
/// assert_eq!(outcome_of(&TaskState::Completed, |_| unreachable!()), Some(Outcome::Done));
/// assert_eq!(outcome_of(&TaskState::Cancelled, |_| unreachable!()), Some(Outcome::Cancelled));
/// // Neither queued nor paused is an outcome.
/// assert_eq!(outcome_of(&TaskState::Pending, |_| unreachable!()), None);
/// assert_eq!(outcome_of(&TaskState::Paused, |_| unreachable!()), None);
/// assert_eq!(outcome_of(&TaskState::Running, |_| unreachable!()), None);
/// ```
#[must_use]
pub fn outcome_of(
    state: &TaskState,
    category: impl FnOnce(&norte_proto::Error) -> String,
) -> Option<Outcome> {
    match state {
        TaskState::Completed => Some(Outcome::Done),
        TaskState::Cancelled => Some(Outcome::Cancelled),
        TaskState::Failed { error } => Some(Outcome::Failed(category(error))),
        // `Pending`, `Paused`, `Running` and `Unknown`: nothing to announce.
        // And `Unknown` is the one that matters — a state from a newer
        // protocol is NOT terminal (`TaskState::is_terminal` excludes it on
        // purpose: facing something it does not understand, the client keeps
        // listening), so falling here is correct and not an oversight. The
        // wildcard is needed because `TaskState` is `#[non_exhaustive]`.
        _ => None,
    }
}

/// The Fluent key for the status sentence.
///
/// **The precedence is the decision**, and it is the terminal's
/// (`norte_tui::jobs::search::finalize_search_state`): cancelled beats
/// truncated. Both say "this is not all", but only one says WHY, and "there
/// is more" about a search the reader stopped also asserts that the crawl
/// reached the cap — which, after a cancellation, is exactly what is not
/// known.
///
/// `at_cap` only counts on one that FINISHED: while it runs, reaching the cap
/// is not an outcome, and announcing it as one put a terminal sentence next
/// to a spinner.
///
/// ```
/// use norte_frontend::search_status::{Outcome, status_key};
///
/// assert_eq!(status_key(&Outcome::Running, false), "search-status-running");
/// assert_eq!(status_key(&Outcome::Running, true), "search-status-running");
/// assert_eq!(status_key(&Outcome::Done, true), "search-status-truncated");
/// assert_eq!(status_key(&Outcome::Done, false), "search-status-done");
/// // Cancelled beats the cap: only it says why what is missing is missing.
/// assert_eq!(status_key(&Outcome::Cancelled, true), "search-status-cancelled");
/// assert_eq!(
///     status_key(&Outcome::Failed("permission".to_owned()), true),
///     "search-status-failed",
/// );
/// ```
#[must_use]
pub fn status_key(outcome: &Outcome, at_cap: bool) -> &'static str {
    match outcome {
        Outcome::Cancelled => "search-status-cancelled",
        Outcome::Failed(_) => "search-status-failed",
        Outcome::Done if at_cap => "search-status-truncated",
        Outcome::Done => "search-status-done",
        Outcome::Running => "search-status-running",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pair that disagreed between frontends: stopping the search right
    /// at the cap. The terminal said "cancelled" and the window "there is
    /// more".
    #[test]
    fn cancelled_at_the_cap_says_cancelled() {
        assert_eq!(
            status_key(&Outcome::Cancelled, true),
            "search-status-cancelled"
        );
    }

    /// And a LIVE one that reaches the cap keeps saying it is running: a
    /// terminal sentence next to a spinner reads as already finished.
    #[test]
    fn live_at_the_cap_keeps_saying_it_is_running() {
        assert_eq!(status_key(&Outcome::Running, true), "search-status-running");
    }

    /// A state this client does not understand is NOT an outcome: the wire
    /// says so (`is_terminal` excludes `Unknown`) and this has to say the
    /// same, or a newer daemon makes the window announce finished searches
    /// that are still running.
    #[test]
    fn an_unknown_state_ends_nothing() {
        assert_eq!(outcome_of(&TaskState::Unknown, |_| String::new()), None);
        for s in [TaskState::Pending, TaskState::Paused, TaskState::Running] {
            assert_eq!(outcome_of(&s, |_| String::new()), None, "{s:?}");
        }
    }

    /// And the three that are outcomes are told apart, with the cause inside
    /// the failure.
    #[test]
    fn the_three_outcomes_are_told_apart() {
        assert_eq!(
            outcome_of(&TaskState::Completed, |_| String::new()),
            Some(Outcome::Done)
        );
        assert_eq!(
            outcome_of(&TaskState::Cancelled, |_| String::new()),
            Some(Outcome::Cancelled)
        );
        assert_eq!(
            outcome_of(
                &TaskState::Failed {
                    error: norte_proto::Error::PermissionDenied
                },
                |_| "no permission".to_owned()
            ),
            Some(Outcome::Failed("no permission".to_owned()))
        );
    }
}
