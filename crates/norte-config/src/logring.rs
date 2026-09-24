//! An in-memory ring of log lines, for a frontend to display.
//!
//! The log has existed since #255 and goes to a file. That is useful for
//! investigating AFTERWARDS, and no use for what happens while you watch: a
//! connection that fails in 240ms leaves "permission denied" on screen,
//! saying nothing, while the exact reason — "could not resolve the secret",
//! "authentication rejected" — gets written to a file you have to go fetch
//! from another terminal. This is the other half: the same lines, in memory,
//! to paint where the reader already is.
//!
//! Lives in this crate and not in the TUI because the window needs exactly
//! the same thing, and because the subscriber setup already lives here.
//!
//! # The security cap is not negotiable
//!
//! [`crate::logging`] documents that `suppaftp` logs `PASS <password>` at
//! TRACE level of the `log` crate, and that is why the file's filter carries
//! a `suppaftp=info` directive that beats any `RUST_LOG`. This ring carries
//! the SAME cap, and here it matters more: its level is raised live from the
//! interface, so without the cap it would take nothing more than someone
//! requesting DEBUG in the panel for an FTP password to show up on screen.
//! The cap lives in `under_cap`, and the test that pins it is the most
//! important one in the module.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tracing::{Level, Metadata};
use tracing_subscriber::layer::{Context, Filter};

/// How many lines the ring holds if nobody says otherwise.
///
/// 2000: enough for the whole session you are debugging to fit, little
/// enough that the memory goes unnoticed. When it fills up it drops the old
/// ones and SAYS SO ([`LogRing::dropped`]): a panel that silently discards
/// lies about what there was.
pub const RING_DEFAULT: usize = 2000;

/// The targets whose level the reader may raise from the panel.
///
/// **Allowlist, and this was fixed after a review.** The first version was a
/// BLOCKLIST with a single name, `suppaftp`, because it is the one #43
/// documented as emitting `PASS <password>` at TRACE. But what protected
/// everything else was the GLOBAL filter at INFO, and this module took it out
/// of the ring's path specifically to be able to raise the level live. With
/// the blocklist, one keypress put **the entire address space** at TRACE: the
/// TUI embeds the core, so `russh`, `rustls`, `hyper`, `reqwest` and `opendal`
/// are in there, and at that level they write headers and network buffers.
///
/// The argument holds just as well the other way: nobody opens this panel to
/// read hyper frames. What you want to see is what norte does. So OUR stuff
/// gets raised, and third-party stuff stays at INFO no matter what — which is
/// what the global filter we removed used to do.
///
/// These are CRATE names, and are compared as such: see [`is_ours`].
const OURS: &[&str] = &["norte", "ntc"];

/// The target whose TRACE carries passwords (rule 10, #43). Redundant with
/// the allowlist — `suppaftp` does not start with `norte` — and stays as a
/// second belt: it is the only cap documented with a CVE behind it, and
/// losing it while refactoring the allowlist would be silent.
const TARGET_WITH_SECRETS: &str = "suppaftp";

/// Is this target OURS? By crate SEGMENT, never by raw prefix.
///
/// A `starts_with` over the whole string — which is what there was — accepted
/// `nortex` and `ntcp`: a future dependency with such a name would have
/// entered TRACE, in a ring whose level any local client can raise with a
/// keypress and which gets read on screen. And this allowlist is the ONLY cap
/// that keeps `suppaftp`'s `PASS <password>` out (#43, rule 10), so widening
/// it by accident is exactly the failure that goes unseen.
///
/// A real target's shape is `norte_core::connect`, `norte_vfs_local`, `ntc`:
/// a CRATE name with underscores, and after it the module path following
/// `::`. So it is compared against the first segment, and only counts if it
/// is the exact name (`norte`, `ntc`, the binary) or if it continues with `_`
/// (`norte_core`, `ntc_something`). `nortex` does not continue with `_` and is
/// left out, which is the point.
fn is_ours(target: &str) -> bool {
    let root = target.split("::").next().unwrap_or(target);
    OURS.iter().any(|ours| {
        root == *ours
            || root
                .strip_prefix(*ours)
                .is_some_and(|rest| rest.starts_with('_'))
    })
}

/// Can this line enter the ring above INFO?
///
/// The cap lives here and not in the configurable filter on purpose: the
/// configurable one is changed from the interface, and this must not be
/// changeable from anywhere.
fn under_cap(target: &str, level: Level) -> bool {
    if level <= Level::INFO {
        // INFO and worse always pass: that is what the file logs by
        // default, and it is the level the ring starts at.
        return true;
    }
    // The second belt is still a raw `starts_with`, and that is deliberate:
    // in a BLOCKLIST, wide is safe, so a `suppaftp_something` that does not
    // exist today would already be covered. In the ALLOWLIST it is the
    // opposite, and that is why it goes by segment ([`is_ours`]).
    !target.starts_with(TARGET_WITH_SECRETS) && is_ours(target)
}

pub use crate::logline::{LogLevel, LogLine};

/// What there was after a cursor, and what that cursor missed.
///
/// Exists for [`LogRing::since`], which in turn exists so a frontend can poll
/// without repainting two thousand lines per round (see [`LogRing::pushed`]):
/// the client keeps `next` and asks from there next round. `lost` is what
/// makes that polling honest — without it, a slow client that falls behind
/// the ring would see a jump in the content with no explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tail {
    /// The lines after the cursor, oldest to newest.
    pub lines: Vec<LogLine>,
    /// The cursor for the next call.
    pub next: u64,
    /// How many lines fell off the ring before this cursor saw them.
    pub lost: u64,
}

/// From `tracing` to the type the frontend paints.
fn level_of(l: Level) -> LogLevel {
    match l {
        Level::ERROR => LogLevel::Error,
        Level::WARN => LogLevel::Warn,
        Level::INFO => LogLevel::Info,
        Level::DEBUG => LogLevel::Debug,
        _ => LogLevel::Trace,
    }
}

/// Ring state.
#[derive(Debug)]
struct Ring {
    lines: VecDeque<LogLine>,
    cap: usize,
}

/// Ring shared between the `tracing` layer and whoever paints it.
///
/// `Clone` hands out the SAME ring (it is an `Arc`): the layer writes and the
/// frontend reads with no coordination needed.
#[derive(Debug, Clone)]
pub struct LogRing {
    ring: Arc<Mutex<Ring>>,
    /// Minimum level being kept, changeable live from the interface. A `u8`
    /// and not `Level` because it has to be atomic.
    level: Arc<AtomicU8>,
    /// How many have been discarded from filling up.
    dropped: Arc<AtomicU64>,
    /// How many lines have gone IN in total, ever.
    ///
    /// A counter that only goes up, so one can ask "has anything changed?"
    /// without cloning the ring. The window needs this because its panel
    /// does not repaint per frame like the TUI's does: it has to poll, and
    /// polling with [`LogRing::snapshot`] would clone two thousand lines per
    /// round almost always just to find nothing new.
    ///
    /// The length will not do: with the ring full it stays fixed at the cap
    /// and stops moving right when the most is happening.
    pushed: Arc<AtomicU64>,
}

/// `Level` is not representable as a number in `tracing`'s public API, so it
/// is encoded here. Increasing order of verbosity.
fn level_to_u8(l: Level) -> u8 {
    match l {
        Level::ERROR => 0,
        Level::WARN => 1,
        Level::INFO => 2,
        Level::DEBUG => 3,
        _ => 4,
    }
}

fn u8_to_level(n: u8) -> Level {
    match n {
        0 => Level::ERROR,
        1 => Level::WARN,
        2 => Level::INFO,
        3 => Level::DEBUG,
        _ => Level::TRACE,
    }
}

impl LogRing {
    /// A ring of `cap` lines, keeping from INFO.
    ///
    /// Starts at INFO and not DEBUG because the cost of a verbose level is
    /// paid even when nobody is watching: whoever opens the panel raises it
    /// and asks for it.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            ring: Arc::new(Mutex::new(Ring {
                lines: VecDeque::with_capacity(cap.min(RING_DEFAULT)),
                cap: cap.max(1),
            })),
            level: Arc::new(AtomicU8::new(level_to_u8(Level::INFO))),
            dropped: Arc::new(AtomicU64::new(0)),
            pushed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The level being kept right now.
    ///
    /// In [`LogLevel`] and not `tracing::Level` because whoever asks and
    /// changes it is a frontend, and a presentation frontend does not
    /// compile `tracing` (see [`crate::logline`]).
    #[must_use]
    pub fn level(&self) -> LogLevel {
        level_of(u8_to_level(self.level.load(Ordering::Relaxed)))
    }

    /// Raises the level to `l` if needed, and NEVER lowers it.
    ///
    /// The invariant lives here and not in the caller, and that was fixed
    /// after a review: it used to be documented on `LogPanel::show_level`,
    /// which returned the minimum level to capture and trusted the caller to
    /// compare and raise it. `#[must_use]` forces you to BIND the value, not
    /// do something with it — and as soon as the window became the second
    /// caller, it would have copied a `let _ =` and its panel would have
    /// filtered out DEBUG lines that nobody captured.
    ///
    /// Does not lower on purpose: going to DEBUG, back to WARN, and asking
    /// for DEBUG again must show what happened in between. Whoever really
    /// wants to lower it uses [`Self::set_level`], and today only closing the
    /// panel does.
    pub fn raise_to(&self, l: LogLevel) {
        if self.level() < l {
            self.set_level(l);
        }
    }

    /// Sets the level LIVE, up or down.
    ///
    /// What was already discarded does not come back: raising to DEBUG shows
    /// the DEBUGs from now on, not the earlier ones. Whoever paints it has to
    /// say so, because a panel that fills up halfway after asking for more
    /// detail looks broken.
    ///
    /// For the normal path — "show me more" — use [`Self::raise_to`].
    /// Lowering is a separate decision, and today only closing the panel
    /// makes it: without it, one keypress would leave the process capturing
    /// TRACE for the rest of the session.
    pub fn set_level(&self, l: LogLevel) {
        let tracing_level = match l {
            LogLevel::Error => Level::ERROR,
            LogLevel::Warn => Level::WARN,
            LogLevel::Info => Level::INFO,
            LogLevel::Debug => Level::DEBUG,
            LogLevel::Trace => Level::TRACE,
        };
        self.level
            .store(level_to_u8(tracing_level), Ordering::Relaxed);
        // The static level `tracing` caches per callsite comes from
        // `max_level_hint`, so changing it without invalidating that cache
        // would leave `debug!`s cut off by the cheap shortcut even though the
        // ring already wants them. The two go together or neither works.
        tracing::callsite::rebuild_interest_cache();
    }

    /// How many lines have been dropped for lack of room.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// How many lines have gone in in total, to detect changes cheaply.
    ///
    /// Only goes up. Comparing two reads says whether there is anything new
    /// without taking the ring's lock or cloning anything.
    #[must_use]
    pub fn pushed(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }

    /// How many lines fit in here.
    ///
    /// This is the DEEPEST history that can be requested, and that is why it
    /// travels in `log.tail`'s response (ADR 0092): whoever paints the log
    /// can say "this is everything there is" instead of implying there is
    /// more. An UPPER cap, not a promise — whoever serves the ring also trims
    /// what it delivers per round, so a single call with this size can come
    /// back short.
    ///
    /// It is neither [`Self::pushed`] nor the current length: both of those
    /// move, and this is the only one of the three that says where the
    /// bottom is.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner).cap
    }

    /// A copy of the lines, oldest to newest.
    ///
    /// A copy, not a borrow: the lock cannot stay held while a frame is
    /// painted, because whoever writes is any runtime thread, and blocking it
    /// to paint would turn the panel into a brake.
    #[must_use]
    pub fn snapshot(&self) -> Vec<LogLine> {
        let r = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        r.lines.iter().cloned().collect()
    }

    /// What there is after `cursor`, up to `max` lines.
    ///
    /// `cursor` is not an index into `lines`: it is the position of
    /// [`Self::pushed`] the last time whoever is asking looked. That is what
    /// makes it possible to say how much was lost — an index into the
    /// `VecDeque` stops meaning anything as soon as an old line falls off the
    /// other end.
    ///
    /// The arithmetic: `pushed` only goes up and `lines.len()` is what
    /// survives of it, so the oldest line remaining has position
    /// `base = pushed - len`. A cursor below `base` lost `base - cursor`
    /// lines, and that is exactly what [`Tail::lost`] counts — the
    /// alternative, staying silent about it, is the same lie
    /// [`Self::dropped`] exists to avoid. A cursor ABOVE `pushed` — a daemon
    /// that restarted under a client that kept its earlier cursor — is
    /// neither a panic nor a gap: it is treated as if it were `pushed`, with
    /// nothing new and nothing lost, because there is no way to know what was
    /// there and claiming a gap would be lying in the other direction.
    ///
    /// `base`, `pushed` and the copy of `lines` are read under the SAME lock:
    /// reading `pushed` outside it would let a concurrent writer slip lines
    /// in between one read and the other, and `lost` would come out wrong —
    /// intermittently, which in this module is a bug, not noise.
    ///
    /// # Examples
    ///
    /// ```
    /// use norte_config::logring::{LogRing, ring_layer};
    /// use tracing_subscriber::prelude::*;
    ///
    /// let ring = LogRing::new(10);
    /// let sub = tracing_subscriber::registry().with(ring_layer(&ring));
    /// tracing::subscriber::with_default(sub, || {
    ///     tracing::info!("connecting");
    /// });
    ///
    /// let tail = ring.since(0, 10);
    /// assert_eq!(tail.lines.len(), 1);
    /// assert_eq!(tail.next, 1, "the next call asks from here");
    /// assert_eq!(tail.lost, 0, "nothing was lost: the cursor was not stale");
    /// ```
    #[must_use]
    pub fn since(&self, cursor: u64, max: usize) -> Tail {
        // `pushed` and `r.lines` under the SAME lock (see the rustdoc):
        // reading them separately would leave a window for `push` to slip a
        // line in between the two reads and throw `base` off.
        let r = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        let pushed = self.pushed.load(Ordering::Relaxed);
        // Invariant: `pushed` never decreases and `lines.len()` is what
        // survived of it, so `pushed >= lines.len()` always — the
        // subtraction cannot underflow.
        let base = pushed - r.lines.len() as u64;
        let cursor = cursor.min(pushed);
        let lost = base.saturating_sub(cursor);
        // `cursor` is already capped at `pushed`, and `base <= pushed`, so
        // `cursor.max(base) >= base` always — this subtraction cannot
        // underflow either.
        let start = cursor.max(base) - base;
        // `start` does not always fit in `usize` on a 32-bit target; the
        // `unwrap_or(usize::MAX)` is safe because the real vector never
        // exceeds `usize::MAX` elements, so a `start` that does not fit is
        // already greater than `r.lines.len()` — skipping it entirely gives
        // the same empty list that skipping the real `start` would have.
        let lines: Vec<LogLine> = r
            .lines
            .iter()
            .skip(usize::try_from(start).unwrap_or(usize::MAX))
            .take(max)
            .cloned()
            .collect();
        let next = base + start + lines.len() as u64;
        Tail { lines, next, lost }
    }

    /// How many lines of level `l` or worse does the ring hold?
    ///
    /// Without cloning anything, which is the point: the panel bar asks this
    /// on EVERY frame to put the figure on the log button, and answering it
    /// with [`Self::snapshot`] cloned two thousand lines — with their two
    /// `String`s each — ten times a second, fighting the writing thread for
    /// the lock. Counting is the same pass as asking whether there is any:
    /// the ring is bounded.
    #[must_use]
    pub fn count_at_or_above(&self, l: LogLevel) -> usize {
        let r = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        r.lines.iter().filter(|line| line.level <= l).count()
    }

    /// Pushes a line, dropping the oldest one if it does not fit.
    fn push(&self, line: LogLine) {
        // `into_inner`, not discarding: inside there is a `VecDeque` of
        // data, with no invariant a panic could have broken halfway. Before,
        // a poisoned lock left the panel showing "nothing to show" forever —
        // exactly the lie this module says it does not want to tell.
        let mut r = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        if r.lines.len() == r.cap {
            r.lines.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        r.lines.push_back(line);
        self.pushed.fetch_add(1, Ordering::Relaxed);
    }
}

/// The ring's filter: its configurable level PLUS the security cap.
///
/// It is a PER-LAYER `Filter`, not the registry's global filter, and that is
/// the whole point: with a global filter at INFO, `DEBUG`s are not emitted
/// and no panel can show them afterwards — filtering, in the window, what was
/// never logged is impossible. With the per-layer filter, the file keeps its
/// level and the ring has its own.
pub struct RingFilter(LogRing);

impl<S> Filter<S> for RingFilter {
    fn enabled(&self, meta: &Metadata<'_>, _: &Context<'_, S>) -> bool {
        under_cap(meta.target(), *meta.level())
            && level_to_u8(*meta.level()) <= self.0.level.load(Ordering::Relaxed)
    }

    /// The static ceiling the whole process sees.
    ///
    /// Without this, `Filtered` returns `None` = "no ceiling", and then the
    /// whole process's maximum level becomes TRACE: every `debug!` from every
    /// crate — including `hyper` and `russh` during a transfer — stops being
    /// cut off by the cheap check and walks the filter chain. It is a silent
    /// performance regression that the move to per-layer filters brought, and
    /// it goes hand in hand with [`LogRing::set_level`]'s cache invalidation.
    fn max_level_hint(&self) -> Option<tracing_subscriber::filter::LevelFilter> {
        Some(match self.0.level() {
            LogLevel::Error => tracing_subscriber::filter::LevelFilter::ERROR,
            LogLevel::Warn => tracing_subscriber::filter::LevelFilter::WARN,
            LogLevel::Info => tracing_subscriber::filter::LevelFilter::INFO,
            LogLevel::Debug => tracing_subscriber::filter::LevelFilter::DEBUG,
            LogLevel::Trace => tracing_subscriber::filter::LevelFilter::TRACE,
        })
    }

    fn callsite_enabled(&self, meta: &'static Metadata<'static>) -> tracing::subscriber::Interest {
        // `sometimes`, not `always`/`never`: the level changes live, so this
        // callsite's answer cannot be cached. It costs one comparison per
        // event, and that is what makes raising to DEBUG without restarting
        // possible.
        if under_cap(meta.target(), *meta.level()) {
            tracing::subscriber::Interest::sometimes()
        } else {
            tracing::subscriber::Interest::never()
        }
    }
}

/// The layer that writes into the ring.
pub struct RingLayer(LogRing);

impl<S> tracing_subscriber::Layer<S> for RingLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut visitor = Flattener::default();
        event.record(&mut visitor);
        let meta = event.metadata();
        self.0.push(LogLine {
            epoch_ms: now_ms(),
            level: level_of(*meta.level()),
            target: meta.target().to_string(),
            message: visitor.text(),
        });
    }
}

/// The ring's layer, ALREADY with its cap in place.
///
/// Returns a composed layer, not the pair (layer, filter): with the pair, the
/// rustdoc promised the layer could not be installed without its cap, and the
/// type did not enforce it — dropping the filter was all it took. A rule 10
/// that depends on the caller not making a mistake is not a rule.
#[must_use]
pub fn ring_layer<S>(ring: &LogRing) -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use tracing_subscriber::Layer as _;
    RingLayer(ring.clone()).with_filter(RingFilter(ring.clone()))
}

/// Flattens an event's message and fields into one line.
///
/// The `message` field goes first and unnamed (it is the sentence); the rest
/// follows as `key=value`, which is the same thing the file's format does, so
/// the two surfaces say the same thing.
#[derive(Default)]
struct Flattener {
    message: String,
    fields: String,
}

/// Cap on a stored line.
///
/// The ring bounded LINES, not bytes, and the message has no cap: there are
/// places that format a foreign value with `Debug` — an AI provider's error,
/// say, which is text a remote server decides and travels in a WARN, within
/// what is captured by default. Two thousand of those are hundreds of
/// megabytes resident, and cloned every frame. It gets cut and SAYS SO.
const MAX_LINE: usize = 2048;

/// What gets appended to a cut line.
const CUT_MARKER: &str = "… (cut)";

impl Flattener {
    fn text(self) -> String {
        let whole = if self.fields.is_empty() {
            self.message
        } else if self.message.is_empty() {
            self.fields
        } else {
            format!("{} {}", self.message, self.fields)
        };
        cut(whole)
    }
}

/// Cuts to [`MAX_LINE`] at a CHARACTER boundary, and marks it.
///
/// By character, not by byte: cutting mid-UTF-8-sequence would give an
/// invalid `String` (panic) or, worse, bytes the terminal's masking would no
/// longer recognize for what they were.
fn cut(mut s: String) -> String {
    if s.len() <= MAX_LINE {
        return s;
    }
    let boundary = (0..=MAX_LINE)
        .rev()
        .find(|i| s.is_char_boundary(*i))
        .unwrap_or(0);
    s.truncate(boundary);
    s.push_str(CUT_MARKER);
    s
}

impl tracing::field::Visit for Flattener {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            let _ = write!(self.fields, "{}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write as _;
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            let _ = write!(self.fields, "{}={value}", field.name());
        }
    }
}

/// Milliseconds since the epoch. A clock running backward gives 0, not a
/// panic: a log line with a weird time is better than a crashed frontend.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(level: Level, target: &str, msg: &str) -> LogLine {
        LogLine {
            epoch_ms: 0,
            level: level_of(level),
            target: target.to_string(),
            message: msg.to_string(),
        }
    }

    /// The log button's figure: it counts that level OR WORSE, and nothing
    /// else. A `WARN` counts toward `Warn`, an `ERROR` too, an `INFO` does
    /// not.
    #[test]
    fn counts_lines_of_a_level_or_worse() {
        let ring = LogRing::new(8);
        assert_eq!(ring.count_at_or_above(LogLevel::Warn), 0);
        ring.push(line(Level::INFO, "a", "hello"));
        ring.push(line(Level::WARN, "a", "watch out"));
        ring.push(line(Level::ERROR, "a", "bad"));
        assert_eq!(ring.count_at_or_above(LogLevel::Warn), 2);
        assert_eq!(ring.count_at_or_above(LogLevel::Error), 1);
    }

    /// THE test of the module (rule 10, #43): `suppaftp` logs `PASS
    /// <password>` at TRACE, and this ring's level is raised from the
    /// INTERFACE. Without the cap, asking for DEBUG in the panel would put an
    /// FTP password on screen.
    #[test]
    fn the_suppaftp_cap_is_not_lifted_even_by_asking_for_trace() {
        for level in [Level::TRACE, Level::DEBUG] {
            assert!(
                !under_cap("suppaftp", level),
                "{level} of suppaftp entered the ring"
            );
            assert!(
                !under_cap("suppaftp::command", level),
                "a child module of suppaftp snuck in at {level}"
            );
        }
        // What does pass: its INFO and worse, and OUR stuff at any level.
        assert!(under_cap("suppaftp", Level::INFO));
        assert!(under_cap("suppaftp", Level::WARN));
        assert!(under_cap("norte_core::connect", Level::TRACE));
    }

    /// THIRD-PARTY stuff does not rise above INFO no matter how much the
    /// reader asks for TRACE, and this is the fix for a BLOCKER: the TUI
    /// embeds the core, so `russh`, `rustls`, `hyper` and `opendal` are in the
    /// same process, and at that level they write headers and network
    /// buffers. Before, the global filter at INFO covered for it; this
    /// module took it out of the way to raise the level live, and without an
    /// allowlist a single keypress opened all of it.
    #[test]
    fn third_party_stuff_does_not_rise_above_info_even_if_trace_is_requested() {
        for target in [
            "russh::client",
            "russh_sftp::protocol",
            "rustls::conn",
            "hyper::proto::h1",
            "reqwest::async_impl",
            "opendal::services::s3",
            "h2::codec",
        ] {
            for level in [Level::DEBUG, Level::TRACE] {
                assert!(
                    !under_cap(target, level),
                    "\"{target}\" entered the ring at {level}"
                );
            }
            // Its warnings and errors DO: they are what explains a failure.
            assert!(under_cap(target, Level::INFO), "{target}");
            assert!(under_cap(target, Level::WARN), "{target}");
            assert!(under_cap(target, Level::ERROR), "{target}");
        }
    }

    /// And our stuff does rise, which is what the panel exists for.
    #[test]
    fn our_stuff_rises_up_to_trace() {
        for target in [
            "norte_core::connect",
            "norte_tui::navigate",
            "norte_vfs_local",
            "ntc",
        ] {
            assert!(
                under_cap(target, Level::TRACE),
                "\"{target}\" is ours and could not rise"
            );
        }
        // And the bare binary, with and without a module path after it:
        // `norte` is a real crate, not just a prefix.
        for target in ["norte", "norte::daemon", "ntc::app"] {
            assert!(
                under_cap(target, Level::TRACE),
                "\"{target}\" is ours and could not rise"
            );
        }
    }

    /// The allowlist matches by crate SEGMENT, not by raw prefix.
    ///
    /// With `starts_with` over the whole string, a future dependency named
    /// `nortex` or `ntcp` would have entered TRACE in a ring any local client
    /// raises with a keypress and reads on screen. This list is the ONLY cap
    /// keeping `suppaftp`'s `PASS <password>` out (#43, rule 10), so the
    /// negative cases are here so the next refactor does not widen it
    /// silently.
    #[test]
    fn a_crate_that_only_starts_the_same_is_not_ours() {
        for target in [
            "nortex",
            "nortex::x",
            "nortexyz::client",
            "ntcp",
            "ntcp::session",
            "norteño",
        ] {
            for level in [Level::DEBUG, Level::TRACE] {
                assert!(
                    !under_cap(target, level),
                    "\"{target}\" is not ours and entered the ring at {level}"
                );
            }
            // And its INFO and worse still get in, like any third party's:
            // they are what explains a failure.
            assert!(under_cap(target, Level::INFO), "{target}");
        }
    }

    /// The ring drops the old ones and COUNTS it: a panel that silently
    /// discards lies about what there was.
    #[test]
    fn filling_up_drops_the_old_ones_and_says_so() {
        let r = LogRing::new(3);
        for i in 0..5 {
            r.push(line(Level::INFO, "t", &format!("line {i}")));
        }
        let v = r.snapshot();
        assert_eq!(v.len(), 3, "the ring grew past its cap");
        assert_eq!(v[0].message, "line 2", "did not drop the OLDEST ones");
        assert_eq!(v[2].message, "line 4", "lost the most recent one");
        assert_eq!(r.dropped(), 2);
    }

    /// The level changes live and the ring says so: that is what lets you
    /// ask for DEBUG from the panel without restarting.
    #[test]
    fn level_changes_live() {
        let r = LogRing::new(10);
        assert_eq!(r.level(), LogLevel::Info, "starts at INFO, not DEBUG");
        r.set_level(LogLevel::Debug);
        assert_eq!(r.level(), LogLevel::Debug);
        // And the clone shares the same state: the layer and the painter are
        // two hands on the same ring.
        let other = r.clone();
        other.set_level(LogLevel::Warn);
        assert_eq!(r.level(), LogLevel::Warn);
    }

    /// A `cap` of zero is not a ring that keeps nothing: it is a
    /// configuration error that would leave the panel empty forever with no
    /// explanation.
    #[test]
    fn a_cap_of_zero_is_corrected_to_one() {
        let r = LogRing::new(0);
        r.push(line(Level::INFO, "t", "something"));
        assert_eq!(r.snapshot().len(), 1);
    }

    /// The layer, really installed, collects what passes the filter and
    /// NOTHING else.
    ///
    /// Does not use the global subscriber (which is once-per-process and
    /// shared by every test): a local one is set up with `with_default`,
    /// same as the `logging` suite does.
    #[test]
    fn the_installed_layer_collects_and_respects_the_cap() {
        use tracing_subscriber::prelude::*;

        let r = LogRing::new(50);
        let sub = tracing_subscriber::registry().with(ring_layer(&r));

        tracing::subscriber::with_default(sub, || {
            tracing::info!(scheme = "s3", "connecting");
            tracing::debug!("this does not fit yet");
            // The password the cap exists to keep out (#43).
            tracing::trace!(target: "suppaftp", "PASS hunter2");
        });

        let v = r.snapshot();
        assert_eq!(v.len(), 1, "something that should not have got in: {v:?}");
        assert_eq!(v[0].level, LogLevel::Info);
        assert_eq!(v[0].message, "connecting scheme=s3");
        assert!(v[0].target.starts_with("norte_config"), "{}", v[0].target);

        // Now DEBUG is requested from the interface: the DEBUGs get in and
        // the password STAYS out, which is the whole point of the cap.
        r.set_level(LogLevel::Debug);
        let sub = tracing_subscriber::registry().with(ring_layer(&r));
        tracing::subscriber::with_default(sub, || {
            tracing::debug!("now this one");
            tracing::trace!(target: "suppaftp::command", "PASS hunter2");
            tracing::debug!(target: "suppaftp", "PASS hunter2");
        });
        let v = r.snapshot();
        assert!(
            v.iter().any(|l| l.message == "now this one"),
            "raising the level did not bring the DEBUGs: {v:?}"
        );
        assert!(
            !v.iter().any(|l| l.message.contains("hunter2")),
            "A PASSWORD ENTERED THE RING: {v:?}"
        );
    }

    /// The cap holds up over the REAL path, which is not the one the other
    /// tests exercised.
    ///
    /// `suppaftp` does not emit `tracing` events: it emits `log::trace!`. The
    /// `tracing-log` bridge dispatches that record with the static target
    /// `"log"` and leaves the real one as a field, so a
    /// `tracing::trace!(target: "suppaftp", …)` — what the other tests
    /// exercised — does NOT walk the same path. The cap survives because the
    /// bridge consults `enabled` first, with the real metadata; that is a
    /// third-party implementation detail rule 10 depends on, and that is why
    /// it is pinned here.
    ///
    /// Corollary for whoever comes next: a defensive check on `meta.target()`
    /// INSIDE `on_event` would catch nothing, because by then the target is
    /// already `"log"`. It would be theater.
    #[test]
    fn the_password_does_not_get_in_through_the_log_bridge_either() {
        use tracing_subscriber::prelude::*;

        // The bridge is a process global; installing it twice is an error and
        // is harmless (another test in the binary may have done it already).
        let _ = tracing_log::LogTracer::init();
        log::set_max_level(log::LevelFilter::Trace);

        let r = LogRing::new(50);
        r.set_level(LogLevel::Trace);
        let sub = tracing_subscriber::registry().with(ring_layer(&r));
        tracing::subscriber::with_default(sub, || {
            log::trace!(target: "suppaftp", "PASS hunter2");
            log::trace!(target: "suppaftp::command", "PASS hunter2");
            // And any third party at the same level: it does not get in
            // either.
            log::trace!(target: "russh::session", "session_write_encrypted, buf = [1, 2, 3]");
            // What does pass: a third party's warning, which explains
            // failures.
            log::warn!(target: "russh::session", "reconnecting");
        });

        let v = r.snapshot();
        assert!(
            !v.iter().any(|l| l.message.contains("hunter2")),
            "A PASSWORD ENTERED THE RING THROUGH THE BRIDGE: {v:?}"
        );
        assert!(
            !v.iter().any(|l| l.message.contains("session_write")),
            "a third party's TRACE entered the ring: {v:?}"
        );
        assert!(
            v.iter().any(|l| l.message.contains("reconnecting")),
            "a third party's warning DOES have to get in: {v:?}"
        );
    }

    /// The normal case: you ask from where you left off and get what is new.
    #[test]
    fn from_a_cursor_only_the_new_ones_arrive() {
        let ring = LogRing::new(10);
        for i in 0..4 {
            ring.push(line(Level::INFO, "norte_core", &format!("l{i}")));
        }
        let t = ring.since(2, 100);
        assert_eq!(t.lines.len(), 2);
        assert_eq!(t.lines[0].message, "l2");
        assert_eq!(t.next, 4);
        assert_eq!(t.lost, 0);
    }

    /// A cursor from before the overflow SAYS how many it lost. A silent gap
    /// lies about what there was, which is why `dropped` exists.
    #[test]
    fn a_stale_cursor_says_how_many_it_lost() {
        let ring = LogRing::new(3);
        for i in 0..7 {
            ring.push(line(Level::INFO, "norte_core", &format!("l{i}")));
        }
        // The ring holds l4,l5,l6: base = 7 - 3 = 4.
        let t = ring.since(1, 100);
        assert_eq!(t.lost, 3, "l1, l2 and l3 were lost");
        assert_eq!(t.lines.len(), 3);
        assert_eq!(t.lines[0].message, "l4");
        assert_eq!(t.next, 7);
    }

    /// `max` caps the response and the cursor advances by ONLY what was
    /// delivered: asking again continues where it was cut off, missing
    /// nothing.
    #[test]
    fn max_caps_and_the_cursor_does_not_get_ahead() {
        let ring = LogRing::new(10);
        for i in 0..5 {
            ring.push(line(Level::INFO, "norte_core", &format!("l{i}")));
        }
        let t = ring.since(0, 2);
        assert_eq!(t.lines.len(), 2);
        assert_eq!(t.next, 2);
        let t2 = ring.since(t.next, 2);
        assert_eq!(t2.lines[0].message, "l2");
    }

    /// Capacity is what was asked for and does NOT move with what comes in:
    /// it is what a remote reader needs to know where the bottom of the
    /// history is (ADR 0092).
    #[test]
    fn capacity_says_the_bottom_not_the_occupancy() {
        let ring = LogRing::new(3);
        assert_eq!(ring.capacity(), 3, "empty already knows how much it holds");
        for i in 0..7 {
            ring.push(line(Level::INFO, "norte_core", &format!("l{i}")));
        }
        assert_eq!(ring.capacity(), 3, "full and overflowed, the same");
        // A ring of zero lines does not exist: `new` bumps it to one, and
        // capacity has to say what there is, not what was asked for.
        assert_eq!(LogRing::new(0).capacity(), 1);
    }

    /// A cursor from the future — a restarted daemon under a client that kept
    /// its own — is neither a panic nor a gap: nothing is new and nothing was
    /// lost.
    #[test]
    fn a_cursor_from_the_future_invents_nothing() {
        let ring = LogRing::new(10);
        ring.push(line(Level::INFO, "norte_core", "l0"));
        let t = ring.since(99, 100);
        assert!(t.lines.is_empty());
        assert_eq!(t.next, 1);
        assert_eq!(t.lost, 0);
    }

    /// The message goes first and the fields after, as in the file: the two
    /// surfaces have to say the same thing for one to serve as a reference
    /// for the other.
    #[test]
    fn the_flattener_puts_the_message_first() {
        let with = |message: &str, fields: &str| {
            Flattener {
                message: message.to_string(),
                fields: fields.to_string(),
            }
            .text()
        };
        assert_eq!(
            with("connecting", "scheme=s3 host=a-bucket"),
            "connecting scheme=s3 host=a-bucket"
        );
        // Without fields, just the sentence; without a sentence, just the
        // fields.
        assert_eq!(with("hello", ""), "hello");
        assert_eq!(with("", "a=1"), "a=1");
    }
}
