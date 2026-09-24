//! The log panel: what is shown from the ring and how it is browsed.
//!
//! The ring (`norte_config::logring`) stores; this decides what is seen.
//! Lives in the shared crate because the window needs the same answers, and
//! a presentation decision duplicated between frontends silently drifts
//! (ADR 0077).
//!
//! # Two levels, and confusing them is the trap
//!
//! There is the level the ring **captures** and the level the panel
//! **shows**, and they are not the same. Filtering to DEBUG what was stored
//! at INFO shows nothing: the DEBUG lines do not exist. Whoever changes the
//! shown level has to raise the ring's, and that invariant lives in
//! `LogRing::raise_to` — in the type that OWNS the level — and not in a
//! return value a second frontend could ignore.
//!
//! And lowering it does NOT lower the ring's, on purpose: going to DEBUG,
//! back to WARN and asking for DEBUG again has to show what happened in
//! between. If lowering it stopped capturing, that round trip would erase
//! exactly the stretch being investigated. The cost is over-capturing for as
//! long as the session lasts, which is the cheap one of the two possible
//! mistakes.

use norte_config::logline::{LogLevel, LogLine};

/// Where the lines the panel shows come from.
///
/// This is the SAVED PREFERENCE, not what is painted. A frontend with the
/// core embedded — a single process, a single ring — has no second source to
/// show, and whoever decides to collapse `Both` into `Window` and hide the
/// selector in that case is THAT frontend: there is no way to know here
/// whether there is a daemon on the other end, and this crate must not
/// pretend there is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogSource {
    /// Only this process.
    Window,
    /// Only the daemon.
    Daemon,
    /// Both, merged by timestamp.
    #[default]
    Both,
}

/// The panel's state.
#[derive(Debug, Clone)]
pub struct LogPanel {
    /// Up to which verbosity is SHOWN.
    level: LogLevel,
    /// Which source is shown: a preference, not what is painted (see
    /// [`LogSource`]).
    source: LogSource,
    /// Text filter over module and message. Empty = everything.
    filter: String,
    /// The same, already lowercased.
    ///
    /// Precomputed because [`Self::matches`] runs once PER LINE and per
    /// frame: with the needle inline, every call allocated a new `String`
    /// for the same thing, two thousand times, ten times a second.
    filter_lc: String,
    /// How many lines are above the first visible one. `None` = stuck to
    /// the end (follows what arrives).
    scroll: Option<usize>,
    /// How many rows fit, from the previous frame.
    ///
    /// Set by whoever paints, and this was fixed after a review: key
    /// handling GUESSED ten — the height the pane opens with — while
    /// painting used the real interior, which is eight. Every page skipped
    /// two lines, and the first one, four: what neither window showed could
    /// not be read at all. Guessing the viewport silently breaks scrolling,
    /// and this tree already had the fix for the other long lists
    /// (`ui::geometry::before_frame`).
    rows: usize,
}

impl Default for LogPanel {
    fn default() -> Self {
        Self {
            // INFO: the same as what the ring captures on startup, so
            // opening the panel shows something from the very first moment.
            level: LogLevel::Info,
            source: LogSource::Both,
            filter: String::new(),
            filter_lc: String::new(),
            scroll: None,
            // One until the first frame tells the truth: never zero, so a
            // page before painting moves something instead of nothing.
            rows: 1,
        }
    }
}

impl LogPanel {
    /// The level currently being shown.
    #[must_use]
    pub const fn level(&self) -> LogLevel {
        self.level
    }

    /// Changes the level SHOWN.
    ///
    /// Raising the ring's is `LogRing::raise_to`'s job, and this was fixed
    /// after a review: this function used to return "the level the ring must
    /// capture" and trusted the caller to compare it and raise it.
    /// `#[must_use]` forces binding the value, not using it — and the second
    /// frontend would have copied a `let _ =` and ended up filtering to
    /// DEBUG some lines nobody captured. The invariant now lives in the type
    /// that owns the level.
    pub fn show_level(&mut self, l: LogLevel) {
        self.level = l;
        // Back to the end: after changing the filter, what the reader wants
        // to see is the latest match, not the spot they were looking at in a
        // different list.
        self.scroll = None;
    }

    /// The source currently being shown (a preference, see [`LogSource`]).
    #[must_use]
    pub const fn source(&self) -> LogSource {
        self.source
    }

    /// Changes the source.
    pub fn set_source(&mut self, s: LogSource) {
        self.source = s;
        // Same as when changing the filter or the level: the composed list
        // changes shape, and staying at the PREVIOUS one's scroll leaves the
        // reader at a spot they did not ask for.
        self.scroll = None;
    }

    /// Cycles through the three sources and returns to the first: it is ONE
    /// control, not three.
    pub fn cycle_source(&mut self) {
        self.set_source(match self.source {
            LogSource::Window => LogSource::Daemon,
            LogSource::Daemon => LogSource::Both,
            LogSource::Both => LogSource::Window,
        });
    }

    /// The current text filter.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Changes the text filter.
    pub fn set_filter(&mut self, f: impl Into<String>) {
        self.filter = f.into();
        self.filter_lc = self.filter.to_lowercase();
        self.scroll = None;
    }

    /// Is it stuck to the end?
    #[must_use]
    pub const fn following(&self) -> bool {
        self.scroll.is_none()
    }

    /// Sticks back to the end.
    pub const fn follow(&mut self) {
        self.scroll = None;
    }

    /// How many rows fit. Called by whoever paints, once per frame.
    pub const fn set_viewport_rows(&mut self, rows: usize) {
        // Never zero: with zero, the `cap` would be the total and a page
        // would not move anything.
        self.rows = if rows == 0 { 1 } else { rows };
    }

    /// How many rows fit (the previous frame's).
    #[must_use]
    pub const fn viewport_rows(&self) -> usize {
        self.rows
    }

    /// Scrolls up `n` lines, unsticking from the end.
    ///
    /// Unsticking is half the panel: one that always jumps to the end
    /// cannot be read while something is writing, which is exactly when it
    /// is needed.
    pub fn scroll_up(&mut self, n: usize, visible: usize) {
        let cap = visible.saturating_sub(self.rows);
        let current = self.scroll.unwrap_or(cap);
        self.scroll = Some(current.saturating_sub(n));
    }

    /// Scrolls down `n` lines; on reaching the end it sticks back.
    pub fn scroll_down(&mut self, n: usize, visible: usize) {
        let cap = visible.saturating_sub(self.rows);
        let current = self.scroll.unwrap_or(cap);
        let new = current.saturating_add(n);
        self.scroll = if new >= cap { None } else { Some(new) };
    }

    /// Does this line pass both filters?
    ///
    /// The text is compared lowercased and against the module TOO, not just
    /// the message: half of a real search is "show me connect's".
    #[must_use]
    pub fn matches(&self, line: &LogLine) -> bool {
        if line.level > self.level {
            return false;
        }
        if self.filter_lc.is_empty() {
            return true;
        }
        contains_case_insensitive(&line.message, &self.filter_lc)
            || contains_case_insensitive(&line.target, &self.filter_lc)
    }

    /// The visible lines, in order, and the index the window of `height`
    /// rows starts at.
    ///
    /// Returns indices over the filtered set and not over the ring: the
    /// caller paints one chunk, and what gets cut is what is seen, not what
    /// there is.
    #[must_use]
    pub fn view<'a>(&self, lines: &'a [LogLine], height: usize) -> (Vec<&'a LogLine>, usize) {
        let visible: Vec<&LogLine> = lines.iter().filter(|l| self.matches(l)).collect();
        let start = self.window_start(visible.len(), height);
        (visible, start)
    }

    /// The index the window of `height` rows starts at, over an already
    /// filtered list of `total` elements.
    ///
    /// [`Self::view`]'s other half, exposed separately because whoever
    /// merges two sources ([`merge`]) no longer has a `&[LogLine]` to hand
    /// it: it has `(line, source)` pairs. Without this, that caller had to
    /// materialize the merge into its own `Vec<LogLine>` — cloning what
    /// `merge` borrows on purpose — just to ask about the scroll offset
    /// again.
    ///
    /// ```
    /// use norte_frontend::logpanel::LogPanel;
    /// let mut p = LogPanel::default();
    /// p.set_viewport_rows(10);
    /// // Stuck to the end: the window starts where the last ten fit.
    /// assert_eq!(p.window_start(25, 10), 15);
    /// // And never above the cap, even if the list shrinks under it.
    /// assert_eq!(p.window_start(4, 10), 0);
    /// ```
    #[must_use]
    pub fn window_start(&self, total: usize, height: usize) -> usize {
        let cap = total.saturating_sub(height);
        self.scroll.map_or(cap, |s| s.min(cap))
    }

    /// How many of the ring's lines pass the filters.
    ///
    /// Without building the vector: it is the only thing scrolling needs,
    /// and doing it with [`Self::view`] cloned references from the 2000 on
    /// every keystroke, including the ones that scroll nothing.
    #[must_use]
    pub fn visible_count(&self, lines: &[LogLine]) -> usize {
        lines.iter().filter(|l| self.matches(l)).count()
    }
}

/// Merges two lists already sorted by `epoch_ms`, marking each line's
/// origin. Stable: on a tie, the local one first — two processes on the
/// same machine share a clock, so equal timestamps are the normal case, not
/// the rare one, and a list that reorders itself between frames cannot be
/// read.
///
/// Returns BORROWED lines on purpose: the ring already cloned once in its
/// `snapshot`, and the panel paints at most one screen; cloning two thousand
/// lines again per frame is exactly the cost the window's projection was
/// written to avoid.
///
/// The level filter and the text one are NOT applied here: they go after,
/// on the result, so that a daemon line does not sneak in just for coming
/// from outside.
#[must_use]
pub fn merge<'a>(
    local: &'a [LogLine],
    remote: &'a [LogLine],
    s: LogSource,
) -> Vec<(&'a LogLine, LogSource)> {
    match s {
        LogSource::Window => local.iter().map(|l| (l, LogSource::Window)).collect(),
        LogSource::Daemon => remote.iter().map(|l| (l, LogSource::Daemon)).collect(),
        LogSource::Both => {
            let mut out = Vec::with_capacity(local.len() + remote.len());
            let mut i = 0;
            let mut j = 0;
            while i < local.len() && j < remote.len() {
                if remote[j].epoch_ms < local[i].epoch_ms {
                    out.push((&remote[j], LogSource::Daemon));
                    j += 1;
                } else {
                    // Same timestamp: local first, on purpose.
                    out.push((&local[i], LogSource::Window));
                    i += 1;
                }
            }
            out.extend(local[i..].iter().map(|l| (l, LogSource::Window)));
            out.extend(remote[j..].iter().map(|l| (l, LogSource::Daemon)));
            out
        }
    }
}

/// Does `haystack` contain `needle` (which ALREADY arrives lowercased),
/// case-insensitively and without allocating?
///
/// `haystack.to_lowercase().contains(..)` copied the whole message per line
/// and per frame. This compares window by window over the original.
fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    // By lowercased CHARACTERS and not by bytes: `char::to_lowercase` can
    // yield more than one (the Turkish `İ`), and comparing raw bytes would
    // fail as soon as the message carried accents. And with no `collect`:
    // collecting into two `Vec`s to compare windows would allocate TWICE
    // per line, which is worse than the `to_lowercase` this was written to
    // remove.
    for (i, _) in haystack.char_indices() {
        let mut h = haystack[i..].chars().flat_map(char::to_lowercase);
        let mut n = needle.chars();
        loop {
            match (n.next(), h.next()) {
                // The needle ran out with no mismatch: it is there.
                (None, _) => return true,
                // The haystack ran out before the needle: it does not fit,
                // and it would not fit from a later position either.
                (Some(_), None) => return false,
                (Some(nc), Some(hc)) if nc == hc => {}
                _ => break,
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(level: LogLevel, target: &str, msg: &str) -> LogLine {
        LogLine {
            epoch_ms: 0,
            level,
            target: target.into(),
            message: msg.into(),
        }
    }

    fn corpus() -> Vec<LogLine> {
        vec![
            l(LogLevel::Info, "norte_core::connect", "connecting"),
            l(LogLevel::Warn, "norte_core::connect", "connection failure"),
            l(LogLevel::Debug, "norte_tui::fill", "page 2"),
            l(LogLevel::Error, "norte_core::journal", "could not anchor"),
        ]
    }

    /// The level filter lets through what is LESS verbose, not only the
    /// same: asking for WARN and losing the ERROR ones would show less the
    /// worse things get.
    #[test]
    fn the_level_lets_through_the_more_severe() {
        let mut p = LogPanel::default();
        p.show_level(LogLevel::Warn);
        let v: Vec<_> = corpus().into_iter().filter(|x| p.matches(x)).collect();
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.iter().all(|x| x.level <= LogLevel::Warn));
    }

    /// The text also searches the MODULE: "show me connect's" is half of a
    /// real search, and without this you would have to know the messages by
    /// heart.
    #[test]
    fn the_text_searches_the_module_and_the_message() {
        let mut p = LogPanel::default();
        p.show_level(LogLevel::Trace);
        p.set_filter("connect");
        assert_eq!(corpus().iter().filter(|x| p.matches(x)).count(), 2);
        p.set_filter("ANCHOR"); // case-insensitive
        assert_eq!(corpus().iter().filter(|x| p.matches(x)).count(), 1);
        p.set_filter("");
        assert_eq!(corpus().iter().filter(|x| p.matches(x)).count(), 4);
    }

    /// Asking for more detail returns the level the ring has to capture:
    /// filtering to DEBUG what was stored at INFO shows nothing.
    #[test]
    fn asking_for_debug_says_what_must_be_captured() {
        let mut p = LogPanel::default();
        p.show_level(LogLevel::Debug);
        assert_eq!(p.level(), LogLevel::Debug);
    }

    /// Starts stuck to the end; scrolling up unsticks it; scrolling down to
    /// the end sticks it back. A panel that always jumps to the end cannot
    /// be read while something is writing, which is when it is needed.
    #[test]
    fn following_the_end_lets_go_on_scroll_up_and_comes_back_on_scroll_down() {
        let mut p = LogPanel::default();
        assert!(p.following());
        p.set_viewport_rows(4);
        p.scroll_up(1, 10);
        assert!(!p.following(), "scrolling up did not let go of following");
        p.scroll_down(99, 10);
        assert!(p.following(), "reaching the end did not stick it back");
    }

    /// The window is cut over what is FILTERED: with four lines, a filter
    /// that leaves two and a height of one, the last of the two is seen.
    #[test]
    fn the_window_is_cut_over_the_filtered_set() {
        let mut p = LogPanel::default();
        p.show_level(LogLevel::Warn);
        let lines = corpus();
        let (visible, start) = p.view(&lines, 1);
        assert_eq!(visible.len(), 2);
        assert_eq!(start, 1, "stuck to the end, it starts at the last one");
        assert_eq!(visible[start].message, "could not anchor");
    }

    /// The height is set by whoever paints, and pages match IT.
    ///
    /// This is the fix for a real bug: key handling guessed ten rows and
    /// painting used eight, so every page skipped two lines, and the first
    /// one, four. With 100 lines and 8 rows, stuck to the end, 92 through 99
    /// are seen; a page up has to show 84 through 91 — with no gaps between
    /// the two windows.
    #[test]
    fn a_page_does_not_skip_any_line() {
        let mut p = LogPanel::default();
        p.set_viewport_rows(8);
        let lines: Vec<LogLine> = (0..100)
            .map(|i| l(LogLevel::Info, "t", &format!("line {i}")))
            .collect();

        let (_, start) = p.view(&lines, 8);
        assert_eq!(start, 92, "stuck to the end it starts at 92");

        p.scroll_up(8, 100);
        let (_, start) = p.view(&lines, 8);
        assert_eq!(
            start, 84,
            "the previous page has to start right where the one below ends"
        );
    }

    /// With the needle precomputed, searching is still case-insensitive and
    /// still works with accents — which is what would break if raw bytes
    /// were compared instead of characters.
    #[test]
    fn the_non_allocating_filter_still_finds_accents() {
        let mut p = LogPanel::default();
        p.set_filter("CONEXIÓN");
        assert!(p.matches(&l(LogLevel::Warn, "t", "fallo de conexión remota")));
        p.set_filter("ó");
        assert!(p.matches(&l(LogLevel::Warn, "t", "CONEXIÓN")));
        p.set_filter("zzz");
        assert!(!p.matches(&l(LogLevel::Warn, "t", "conexión")));
        // A needle longer than the haystack cannot "be found".
        p.set_filter("larguísima aguja");
        assert!(!p.matches(&l(LogLevel::Warn, "t", "corto")));
    }

    /// Changing the filter goes back to the end: staying at the PREVIOUS
    /// list's scroll leaves the reader at a spot they did not ask for.
    #[test]
    fn changing_the_filter_goes_back_to_the_end() {
        let mut p = LogPanel::default();
        p.set_viewport_rows(3);
        p.scroll_up(2, 10);
        assert!(!p.following());
        p.set_filter("x");
        assert!(p.following());
        p.set_viewport_rows(3);
        p.scroll_up(2, 10);
        p.show_level(LogLevel::Error);
        assert!(p.following());
    }

    /// A line with a timestamp, for the merge tests. Different from [`l`]
    /// (which pins `epoch_ms` to 0 and asks for level and module) because
    /// `merge` only cares about the timestamp and the message.
    fn le(ms: i64, msg: &str) -> LogLine {
        LogLine {
            epoch_ms: ms,
            level: LogLevel::Info,
            target: "t".into(),
            message: msg.into(),
        }
    }

    /// The merge respects the clock, and on a tie it does not waver: local
    /// first.
    #[test]
    fn the_merge_sorts_by_timestamp_and_is_stable() {
        let local = vec![le(10, "ventana-a"), le(30, "ventana-b")];
        let remote = vec![le(10, "daemon-a"), le(20, "daemon-b")];
        let m = merge(&local, &remote, LogSource::Both);
        let ms: Vec<_> = m.iter().map(|(l, _)| l.message.as_str()).collect();
        assert_eq!(ms, ["ventana-a", "daemon-a", "daemon-b", "ventana-b"]);
        assert_eq!(m[1].1, LogSource::Daemon);
    }

    /// Choosing a single source does NOT merge: it shows that one and
    /// nothing else.
    #[test]
    fn a_single_source_does_not_bring_the_other() {
        let local = vec![le(10, "ventana")];
        let remote = vec![le(20, "daemon")];
        assert_eq!(merge(&local, &remote, LogSource::Window).len(), 1);
        assert_eq!(
            merge(&local, &remote, LogSource::Daemon)[0].0.message,
            "daemon"
        );
    }

    /// The cycle goes through all three and comes back: it is ONE control,
    /// not three.
    #[test]
    fn the_source_cycle_comes_full_circle() {
        let mut p = LogPanel::default();
        assert_eq!(p.source(), LogSource::Both);
        p.cycle_source();
        p.cycle_source();
        p.cycle_source();
        assert_eq!(p.source(), LogSource::Both);
    }

    /// The level filter and the text one keep applying AFTER merging: a
    /// daemon line that does not pass the filter does not sneak in just for
    /// coming from outside.
    #[test]
    fn the_filter_also_governs_the_remote_side() {
        let mut p = LogPanel::default();
        p.show_level(LogLevel::Error);
        let remote = vec![LogLine {
            epoch_ms: 1,
            level: LogLevel::Debug,
            target: "norte_core".into(),
            message: "noise".into(),
        }];
        let m = merge(&[], &remote, LogSource::Both);
        assert!(!p.matches(m[0].0));
    }
}
