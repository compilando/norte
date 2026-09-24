//! What the reader is waiting for, and since when.
//!
//! The TUI already knew how to cancel a slow navigation (`Esc`) and did not
//! know how to SAY it was waiting: during a `connect` to a remote bucket the
//! screen stayed on the last frame, indistinguishable from a hang. This is
//! the data the surfaces paint, and it lives here — not in the TUI — because
//! the window needs exactly the same thing, and a presentation decision
//! duplicated between frontends diverges silently (ADR 0077).
//!
//! Two rules the type enforces, not whoever paints:
//!
//! - **Nothing before the threshold.** A local `cd` takes milliseconds;
//!   showing a flicker on every one turns the indicator into noise, and noise
//!   stops being looked at. [`Busy::visible`] answers for all of them.
//! - **Indeterminate, and honest.** There is no percentage here because in a
//!   `connect` there is none: the phase is a network that answers or does
//!   not. The repository already has the opinion written in `modal.rs` — "a
//!   'checking…' label that never advances is a lie shaped like a spinner" —
//!   the corollary is that a BAR that makes up progress is worse.

use std::time::Duration;

use norte_proto::VPath;

/// How long something has to last to deserve an indicator.
///
/// 250 ms: below it, the operation finishes before the eye registers it and
/// all that is seen is a flicker; above it, it appears well before anyone has
/// time to think the program has hung.
pub const THRESHOLD: Duration = Duration::from_millis(250);

/// Spinner frames, the same ones cargo uses.
///
/// Braille and not `|/-\`: it is what a Rust tooling user already recognizes
/// as "working", and this TUI already depends on Unicode box drawing, so it
/// adds no font requirement that was not already there.
const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// How often the spinner advances.
///
/// Public because whoever waits has to wake up at EXACTLY this rate: with two
/// 80 ms constants in two modules, changing one skips or repeats frames and
/// nothing turns red.
pub const FRAME_EVERY: Duration = Duration::from_millis(80);

/// What kind of work is being waited on. CLOSED vocabulary: every variant has
/// its Fluent key and [`BusyKind::key`] is total, so a job in flight can never
/// end up without a label to tell the reader.
/// There are variants only for waits that BLOCK repainting. Search, compare,
/// sync and the model's plan are deliberately NOT here: they live in
/// `jobs::inflight`, harvested by the main loop, and that loop already
/// repaints every 100 ms — giving them a variant would suggest they are
/// missing wiring here when in fact they do not need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BusyKind {
    /// Establishing a remote connection (the case that uncovered this).
    Connecting,
    /// Listing a directory.
    Listing,
    /// Fetching a file to open it in the viewer.
    Opening,
}

impl BusyKind {
    /// The verb's Fluent key. Total by construction.
    ///
    /// ```
    /// use norte_frontend::busy::BusyKind;
    /// assert_eq!(BusyKind::Connecting.key(), "busy-connecting");
    /// ```
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Connecting => "busy-connecting",
            Self::Listing => "busy-listing",
            Self::Opening => "busy-opening",
        }
    }

    /// All the variants, for the tests that require totality.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Connecting, Self::Listing, Self::Opening]
    }
}

/// A job in flight, exactly as painted.
///
/// It does not carry an `Instant`: it carries the ELAPSED time, which is what
/// both questions need and the only thing a test can fix without a clock.
/// Whoever builds it (the event loop, which does have the starting
/// `Instant`) does the subtraction once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Busy {
    /// What is being done.
    pub kind: BusyKind,
    /// About what: the RAW `VPath`, not an already-rendered text.
    ///
    /// This was fixed after the encoding audit, and the reason applies to any
    /// path that crosses a shared type: rendering here left out of reach the
    /// three things only whoever paints knows — the altered-name badge, the
    /// pane's encoding reinterpretation (#98/F2) and the available width —
    /// and all three were lost at once. What was seen was a masked path
    /// WITHOUT the mark that says it was masked, on the one surface that also
    /// offers to cancel.
    pub target: Option<VPath>,
    /// The pane it affects, if it affects one. `None` = a session-level job.
    pub pane: Option<usize>,
    /// Since it started.
    pub elapsed: Duration,
}

impl Busy {
    /// A newly-born job (`elapsed` zero): still invisible.
    #[must_use]
    pub fn new(kind: BusyKind, target: Option<VPath>, pane: Option<usize>) -> Self {
        Self {
            kind,
            target,
            pane,
            elapsed: Duration::ZERO,
        }
    }

    /// Is it painted yet?
    ///
    /// The answer lives HERE and not in each surface: two places deciding on
    /// their own end up with the header spinning and the bar silent.
    ///
    /// ```
    /// use norte_frontend::busy::{Busy, BusyKind, THRESHOLD};
    /// let mut b = Busy::new(BusyKind::Connecting, None, Some(0));
    /// assert!(!b.visible(), "newly born, not shown yet");
    /// b.elapsed = THRESHOLD;
    /// assert!(b.visible());
    /// ```
    #[must_use]
    pub fn visible(&self) -> bool {
        self.elapsed >= THRESHOLD
    }

    /// The spinner's frame for the elapsed time.
    ///
    /// Derived from TIME and not a repaint counter: that way both surfaces
    /// spin in step even if one repaints more often than the other, and a
    /// test can fix it without depending on how many repaints there were.
    ///
    /// ```
    /// use norte_frontend::busy::{Busy, BusyKind, FRAME_EVERY};
    /// let mut b = Busy::new(BusyKind::Listing, None, Some(0));
    /// let first = b.frame();
    /// b.elapsed = FRAME_EVERY;
    /// assert_ne!(first, b.frame(), "one frame later it has turned");
    /// ```
    #[must_use]
    pub fn frame(&self) -> char {
        let n = self.elapsed.as_millis() / FRAME_EVERY.as_millis();
        FRAMES[usize::try_from(n % FRAMES.len() as u128).unwrap_or(0)]
    }

    /// Does it affect `pane`? False for a session-level job: a pane's header
    /// must not spin over something that is not happening to it.
    #[must_use]
    pub fn affects(&self, pane: usize) -> bool {
        self.pane == Some(pane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(elapsed: Duration) -> Busy {
        Busy {
            elapsed,
            ..Busy::new(BusyKind::Connecting, None, Some(1))
        }
    }

    /// The threshold is the halfway value: without it, every local `cd` would
    /// paint a flicker and the indicator would stop being looked at exactly
    /// when it matters.
    #[test]
    fn nothing_before_the_threshold_and_something_after() {
        assert!(!with(Duration::ZERO).visible());
        assert!(
            !with(
                THRESHOLD
                    .checked_sub(Duration::from_millis(1))
                    .expect("the threshold is greater than 1 ms")
            )
            .visible()
        );
        assert!(with(THRESHOLD).visible(), "the threshold is inclusive");
        assert!(with(Duration::from_secs(3)).visible());
    }

    /// The frame comes from TIME, so two surfaces painting at different rates
    /// show the same one, and it truly advances (it does not stay pinned,
    /// which is the lie this module exists not to tell).
    #[test]
    fn the_spinner_advances_with_time_and_wraps_around() {
        let f = |ms| with(Duration::from_millis(ms)).frame();
        assert_eq!(f(0), FRAMES[0]);
        assert_eq!(f(79), FRAMES[0], "inside the same frame it does not jump");
        assert_eq!(f(80), FRAMES[1]);
        assert_ne!(f(0), f(240), "in three frames it has changed");
        // Wraps around without going out of bounds: the duration is unbounded.
        assert_eq!(f(80 * FRAMES.len() as u64), FRAMES[0]);
        assert_eq!(with(Duration::from_hours(24)).frame(), FRAMES[0]);
    }

    /// No kind of work can be left without a label: a spinner with no verb
    /// says "waiting" and does not say for what, which is half the complaint.
    #[test]
    fn every_kind_has_its_key_and_none_repeats() {
        let keys: Vec<_> = BusyKind::all().iter().map(|k| k.key()).collect();
        for (i, a) in keys.iter().enumerate() {
            assert!(!a.is_empty());
            for b in &keys[i + 1..] {
                assert_ne!(a, b, "two kinds with the same key: {a}");
            }
        }
    }

    /// A session-level job (no pane) does not spin ANY pane's header: it
    /// would mark as busy a pane nothing is happening to.
    #[test]
    fn a_job_with_no_pane_marks_none() {
        let global = Busy::new(BusyKind::Listing, None, None);
        assert!(!global.affects(0));
        assert!(!global.affects(1));
        let ones = Busy::new(BusyKind::Connecting, None, Some(1));
        assert!(ones.affects(1));
        assert!(!ones.affects(0));
    }
}
