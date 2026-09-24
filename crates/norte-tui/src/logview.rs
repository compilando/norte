//! The TUI's log panel: the kind, the keys and the remote half.
//!
//! The state (level, filter, follow-tail, the source) lives in
//! [`norte_frontend::logpanel`] because the window needs the same one, and
//! the lines come from `norte_config::logring`'s ring. What is left here is
//! what belongs to this terminal: which key does what, and how this
//! process's own lines are joined with the daemon's.
//!
//! # Why there is a remote half (#328)
//!
//! `ntc --socket <path>` talks to a daemon that is ANOTHER process: the
//! providers, the journal, the policy and the reason a connection failed are
//! on the other side of the socket, and this ring only has the terminal's
//! own lines. A panel that did not say so would look broken — someone opens
//! it right when a connection fails, does not see the line that explains it,
//! and concludes the log does not work instead of that it is looking
//! somewhere else.
//!
//! The window solved this first (`norte_ui_host::controller::logpanel`) and
//! this is the SAME answer on purpose: a decision one frontend makes and the
//! other does not silently diverge from (ADR 0077).

use norte_config::logline::{LogLevel, LogLine};
use norte_frontend::logpanel::LogSource;
use norte_i18n::{t, ta};

/// The kind that occupies a log slot.
pub const KIND: &str = "log";

/// How many lines are requested from the daemon on each round.
///
/// The daemon caps it at 1000, so this is a request and not a contract. Five
/// hundred because a round that does not fit loses NOTHING —what is left
/// over follows after the cursor and is picked up by the next round— and
/// because the panel shows at most one screen.
pub const MAX_REMOTE: u32 = 500;

/// Cap on the daemon lines kept in memory.
///
/// The local ring already has its own; this is the same care for the remote
/// one, because here lines ACCUMULATE round after round and with no cap a
/// panel left open all afternoon would grow without end.
const MAX_LINES_REMOTAS: usize = 2000;

/// What is known about the DAEMON's log.
///
/// Three values and not a `bool`, because "has not answered yet" and "said
/// it has no log" are shown differently: the first says nothing, and the
/// second is a sentence the panel has to put on screen. Collapsing them
/// would make a freshly opened panel assert an absence nobody has checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Servicio {
    /// Never answered: unknown.
    #[default]
    NoResponse,
    /// Serves its log: there is a second source of truth.
    Serves,
    /// Said it has no log to serve.
    NoRing,
}

/// The log panel's remote half (#328).
///
/// What is NOT here is the in-flight request: on this terminal that is
/// `InFlight::log_tail`, in the event loop, which is the one that launches
/// and harvests everything that goes to the backend. The window carries it
/// inside because there the actor is the only writer.
#[derive(Debug, Default)]
pub struct LogRemote {
    /// Is there a daemon to speak of?
    ///
    /// `main` sets it once, from `Backend::is_remote`, and it does not
    /// change: the backend does not switch arms during the process's
    /// lifetime. At `false` —a plain `ntc`, which is the DEFAULT startup—
    /// this panel is exactly #326's: one process, one ring, and not a word
    /// about a daemon.
    ///
    /// Without this field the panel lied, and in two ways in a row: the
    /// embedded arm rightly answers `Unsupported` to `log.tail` —its ring is
    /// the one this panel is already reading—, so the border went from "of
    /// this process (the daemon logs separately)" during the first probe to
    /// staying at "this daemon does not serve its log" afterward. There is
    /// no daemon at all. The sentence was written for the OTHER degradation
    /// —a real daemon built without the `logging` feature— and here it was
    /// being said to nobody.
    pub hay_daemon: bool,
    /// What the daemon has delivered so far, oldest to newest.
    ///
    /// It accumulates and is not re-fetched whole on every round: the probe
    /// pulls the remote ring with a cursor, so each response brings only
    /// what is new.
    pub lines: Vec<LogLine>,
    /// Where it was up to. `None` = not asked yet, which is "give me
    /// whatever there is" and is NOT the same as zero: against a ring that
    /// has already wrapped, a zero would report a false `lost` on the first
    /// probe.
    pub cursor: Option<u64>,
    /// What is known about whether it serves its log.
    pub servicio: Servicio,
    /// The level it answered having set, in wire form.
    ///
    /// Its own and not ours: it is global to all its clients and only goes
    /// up, so what was requested and what is set need not match.
    pub level: Option<String>,
    /// How many lines fell off behind this cursor. They accumulate: a
    /// silent gap lies about what happened.
    pub lost: u64,
    /// Which opening of the panel this is.
    ///
    /// Between asking and answering there is room for a close and an open,
    /// and the previous session's response has to die instead of landing
    /// —with its cursor— in the new panel.
    pub epoch: u64,
    /// A level that has to be requested from the daemon, set by the key and
    /// drained by the loop.
    ///
    /// Same pattern as `App::places_wants_drives` and for the same reason:
    /// `log.level` is I/O and `App` has no backend. The last one pressed
    /// wins — asking a ring that only goes up for two levels in a row is
    /// asking for the higher one.
    pub asks_level: Option<LogLevel>,
}

impl LogRemote {
    /// Starts from scratch, keeping what is known about the daemon.
    ///
    /// The lines and the cursor belong to THIS opening; whether the daemon
    /// serves its log or not is a fact about the daemon, and forgetting it
    /// would hide the second source every time the panel is reopened.
    ///
    /// That makes a `NoRing` verdict last as long as the process does,
    /// and it is deliberate, not an oversight: that state comes from a
    /// COMPILE-time feature of the binary on the other side of the socket
    /// (or a mount that failed on startup), so it cannot change under a
    /// live daemon. What DOES change —a daemon restarting built a different
    /// way— is a new connection, and that brings its own session. Reviewed
    /// and shelved on purpose, so it does not have to be argued again.
    pub fn restart(&mut self) {
        self.lines.clear();
        self.cursor = None;
        self.lost = 0;
        self.asks_level = None;
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Is it worth asking it again?
    ///
    /// A daemon that has already said it has no log is NOT asked again: the
    /// refusal cannot change while that daemon lives —it comes from a
    /// compile-time feature or a mount that failed on startup—, and
    /// continuing to probe would be two RPCs a second forever for an answer
    /// that cannot be different. It is asymmetric on purpose: the POSITIVE
    /// case does need to keep being asked, because the log grows.
    ///
    /// There is no version comparison anywhere, and there is none because
    /// an older daemon does not even complete `initialize`.
    ///
    /// And a daemon that does not exist, either: without `hay_daemon` it is
    /// never asked — see that field.
    #[must_use]
    pub const fn must_request(&self) -> bool {
        self.hay_daemon && !matches!(self.servicio, Servicio::NoRing)
    }
}

/// A line from the wire into the shape the panel paints.
///
/// An unrecognized level falls back to `Info` instead of dropping the line:
/// the protocol says an unknown value must be able to ARRIVE, and losing the
/// whole message for not understanding its label is worse than showing it
/// with the ordinary label.
fn wire_line(l: norte_proto::methods::LogLine) -> LogLine {
    LogLine {
        epoch_ms: l.epoch_ms,
        level: LogLevel::from_wire(&l.level).unwrap_or(LogLevel::Info),
        target: l.target,
        message: l.message,
    }
}

/// The source that is truly being shown.
///
/// The preference is stored as-is (`LogPanel::source`), but a source that
/// does not exist cannot be shown, and the panel reports what there is, not
/// what was asked for. It collapses in BOTH directions, which are the same
/// rule seen from each shore:
///
/// - with no ring on the other side (the embedded case, or a daemon without
///   the `logging` feature) everything falls back to `Window`;
/// - with no ring in THIS process —nobody mounted the layer— there is
///   nothing local to mix in, so everything falls back to `Daemon`.
///
/// With both rings absent it stays at `Window`, which is where the "no log
/// installed in this process" sentence lives: there is no log IN MEMORY to
/// read, and that is not the same as "nothing is being logged".
#[must_use]
pub fn source_efectiva(app: &crate::app::App) -> LogSource {
    match (
        app.log_remote.servicio == Servicio::Serves,
        app.log_ring.is_some(),
    ) {
        (true, true) => app.log_panel.source(),
        (true, false) => LogSource::Daemon,
        (false, _) => LogSource::Window,
    }
}

/// The local ring, already cloned. Empty if none is installed.
///
/// Separate from [`visible`] because the borrow has to be held by whoever
/// paints: `merge` returns references on purpose, and the ring already
/// cloned once in its `snapshot`.
#[must_use]
pub fn snapshot(app: &crate::app::App) -> Vec<LogLine> {
    app.log_ring
        .as_ref()
        .map(norte_config::logring::LogRing::snapshot)
        .unwrap_or_default()
}

/// What the panel shows: the two sources merged and already filtered, each
/// line with the process it came from.
#[must_use]
pub fn visible<'a>(
    app: &'a crate::app::App,
    locales: &'a [LogLine],
) -> Vec<(&'a LogLine, LogSource)> {
    norte_frontend::logpanel::merge(locales, &app.log_remote.lines, source_efectiva(app))
        .into_iter()
        .filter(|(l, _)| app.log_panel.matches(l))
        .collect()
}

/// What what is being shown is called. `None` = nothing to say.
///
/// **With no daemon there is no segment**, and the absence IS the answer: a
/// plain `ntc` has one process and one ring, so there are not two things to
/// distinguish and any sentence about the origin would be answering a
/// question nobody asked. It is the same reasoning by which the window
/// hides its selector when there is no second source, and it leaves the
/// panel exactly as #326 left it.
///
/// A daemon that has said it has no log to serve is stated HERE and not in
/// a separate sentence, and it is a difference from the window that has a
/// reason: a terminal panel's border is a single line, not a row of labels
/// that can grow, and the two sentences together —"of this process (the
/// daemon logs separately)" and "this daemon does not serve its log"— say
/// the same thing twice and do not fit. The second one wins because it
/// explains WHY there is nothing more than this.
#[must_use]
pub fn source_label(app: &crate::app::App, source: LogSource) -> Option<String> {
    if !app.log_remote.hay_daemon {
        return None;
    }
    if app.log_remote.servicio == Servicio::NoRing {
        return Some(t("log-source-unsupported"));
    }
    Some(t(match source {
        // Of THIS process, and saying so is the point: with `--socket`, the
        // daemon's part —the providers, the journal, the policy— is NOT
        // here, and that is the interesting half.
        LogSource::Window if app.log_ring.is_some() => "log-source-window",
        // It is not "nothing is being logged": the process keeps writing to
        // its file. What is missing is the in-memory ring, which is what
        // this panel reads.
        LogSource::Window => "log-no-ring",
        LogSource::Daemon => "log-source-daemon",
        LogSource::Both => "log-source-both",
    }))
}

/// Whose level was just raised. Empty = nobody's but this process's, and
/// then there is nothing to announce.
///
/// **Whenever the daemon is one of the sources being read**, not only when
/// it is the sole one: in `Both`, which is what the panel opens with,
/// pressing `t` raises a GLOBAL daemon ring, shared with all its clients,
/// that never goes back down and that closing this panel does not lower.
/// Staying quiet about it on the common path would leave that decision
/// unannounced.
///
/// It goes to the status bar and not to the panel's border, and this is
/// where the two windows part ways: the window's is a row of labels that
/// grows, and a terminal panel's border is ONE line that `ratatui` silently
/// truncates — with this sentence placed there, at 120 columns the capture
/// note (the one that states the daemon's level) no longer fit. And the
/// status bar is also the place where this terminal explains what a key
/// just did, which is exactly what this is.
///
/// By the EFFECTIVE source and not by the preference: what has truly been
/// raised is announced. The request does go by the preference —see
/// [`apply_action`]—, because it is one of the two ways to find out
/// whether that daemon knows about logging.
#[must_use]
pub fn level_notice(app: &crate::app::App) -> String {
    if source_efectiva(app) == LogSource::Window {
        String::new()
    } else {
        t("log-source-daemon-level")
    }
}

/// Which ring is keeping MORE than what is shown, and which one.
///
/// Only when it is over-capturing: saying "capturing info" over a panel
/// that shows info would be noise, and noise is what makes the line that
/// does matter stop being read.
///
/// With a single source the sentence does not name the ring —there is no
/// other one to confuse it with—; with both, each part says whose it is.
/// That the daemon's level appears here is what makes the whole rule
/// legible: its own is global to its clients and only goes up, so it can be
/// far above what this panel shows, and that gap is exactly what this
/// sentence exists to not stay quiet about.
#[must_use]
pub fn capture_note(app: &crate::app::App, source: LogSource) -> String {
    let shown = app.log_panel.level();
    let local = app
        .log_ring
        .as_ref()
        .map(norte_config::logring::LogRing::level)
        .filter(|cap| *cap > shown);
    let remote = app
        .log_remote
        .level
        .as_deref()
        .and_then(LogLevel::from_wire)
        .filter(|cap| *cap > shown);
    let phrase = |key, cap: LogLevel| ta(key, &[("level", cap.label().trim())]);
    let parts: Vec<String> = match source {
        LogSource::Window => local
            .map(|c| phrase("log-capturing", c))
            .into_iter()
            .collect(),
        LogSource::Daemon => remote
            .map(|c| phrase("log-capturing-daemon", c))
            .into_iter()
            .collect(),
        LogSource::Both => local
            .map(|c| phrase("log-capturing-window", c))
            .into_iter()
            .chain(remote.map(|c| phrase("log-capturing-daemon", c)))
            .collect(),
    };
    parts.join(" · ")
}

/// The lines that have been lost, by ring and SAYING which one.
///
/// Two numbers and not one, because they do not mean the same thing and do
/// not have the same lifetime: the local ring's counts what it has evicted
/// since the process started and is never reset; the daemon's counts what
/// THIS opening of the panel lost, and goes back to zero on reopening it.
/// Adding them would give a number that is neither of the two things.
///
/// Each ring is mentioned only if it is being read: warning about a gap in
/// a log that is not on screen is an alarm about nothing.
#[must_use]
pub fn discard_note(app: &crate::app::App, source: LogSource) -> String {
    let mut parts: Vec<String> = Vec::new();
    let local = app
        .log_ring
        .as_ref()
        .map_or(0, norte_config::logring::LogRing::dropped);
    if local > 0 && source != LogSource::Daemon {
        parts.push(ta(
            // Without naming the ring when it is the only one being read.
            if source == LogSource::Window {
                "log-dropped"
            } else {
                "log-dropped-window"
            },
            &[("n", &local.to_string())],
        ));
    }
    if app.log_remote.lost > 0 && source != LogSource::Window {
        parts.push(ta(
            "log-missed-daemon",
            &[("n", &app.log_remote.lost.to_string())],
        ));
    }
    parts.join(" · ")
}

/// Lands what the daemon answered to `log.tail` (#328).
pub fn land_tail(
    app: &mut crate::app::App,
    epoch: u64,
    res: Result<norte_proto::methods::LogTailResult, norte_proto::Error>,
) {
    if epoch != app.log_remote.epoch {
        // From a previous opening: neither its lines nor its cursor are
        // valid anymore.
        return;
    }
    match res {
        Ok(r) => {
            app.log_remote.servicio = Servicio::Serves;
            app.log_remote.level = Some(r.level);
            app.log_remote.cursor = Some(r.next);
            app.log_remote.lost = app.log_remote.lost.saturating_add(r.lost);
            app.log_remote
                .lines
                .extend(r.lines.into_iter().map(wire_line));
            // The cap is applied from the front: what is old is what gets
            // dropped, same as in the ring, and it counts as lost — which is
            // what keeps the trim from leaving a silent gap.
            let overflow = app.log_remote.lines.len().saturating_sub(MAX_LINES_REMOTAS);
            if overflow > 0 {
                app.log_remote.lines.drain(..overflow);
                app.log_remote.lost = app
                    .log_remote
                    .lost
                    .saturating_add(overflow.try_into().unwrap_or(u64::MAX));
            }
        }
        // The ONLY reachable degradation: a daemon of the same version
        // without the `logging` feature (or the embedded arm, which has no
        // second source to offer). See `LogRemote::must_request`.
        Err(norte_proto::Error::Unsupported) => app.log_remote.servicio = Servicio::NoRing,
        // Any failure —the connection dropped, the daemon is busy— is NOT
        // "this daemon has no log": saying so would accuse something that
        // fixes itself on the very next round of a permanent lack. It stays
        // quiet and retries.
        Err(_) => {}
    }
}

/// Lands the level the daemon has truly set (#328).
pub fn land_level(app: &mut crate::app::App, epoch: u64, res: Result<String, norte_proto::Error>) {
    if epoch != app.log_remote.epoch {
        return;
    }
    match res {
        Ok(level) => {
            app.log_remote.servicio = Servicio::Serves;
            app.log_remote.level = Some(level);
        }
        Err(norte_proto::Error::Unsupported) => app.log_remote.servicio = Servicio::NoRing,
        Err(_) => {}
    }
}

/// How many rows a page advances.
///
/// The HEIGHT is no longer guessed here —whoever paints sets it, per frame
/// (`LogPanel::set_viewport_rows`)—; this is only how far `PageDown` jumps.
const PAGE: isize = 10;

/// What a key asks the panel for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogAction {
    /// Show up to this level.
    Level(norte_config::logline::LogLevel),
    /// Scroll `n` lines up or down.
    Scroll(isize),
    /// Go back to sticking to the end.
    Follow,
    /// Cycle the source: this process, the daemon, both (#328).
    Source,
    /// Start typing a filter.
    StartFilter,
    /// Return the keyboard.
    Leave,
}

/// Translates a log panel key.
///
/// An explicit `match` and NOT the keymap: these keys only exist while the
/// panel holds the keyboard, they are single letters, and putting them in
/// the keymap would force the seven presets to declare six shortcuts that
/// mean nothing outside here. It is the same criterion as the connections
/// picker and the layout one, and it is also why `s` (the source, #328)
/// does not appear in any preset: it is not a catalogue command, it is a key
/// of this panel, like `e`, `w`, `i`, `d`, `t` and `/`.
#[must_use]
pub fn key(
    code: crossterm::event::KeyCode,
    mods: crossterm::event::KeyModifiers,
) -> Option<LogAction> {
    use crossterm::event::{KeyCode, KeyModifiers};
    use norte_config::logline::LogLevel;
    if !(mods.is_empty() || mods == KeyModifiers::SHIFT) {
        return None;
    }
    Some(match code {
        KeyCode::Char('e') => LogAction::Level(LogLevel::Error),
        KeyCode::Char('w') => LogAction::Level(LogLevel::Warn),
        KeyCode::Char('i') => LogAction::Level(LogLevel::Info),
        KeyCode::Char('d') => LogAction::Level(LogLevel::Debug),
        KeyCode::Char('t') => LogAction::Level(LogLevel::Trace),
        // The initial of "source", and the one loose letter left free among
        // the five level ones.
        KeyCode::Char('s') => LogAction::Source,
        KeyCode::Char('/') => LogAction::StartFilter,
        KeyCode::Up => LogAction::Scroll(-1),
        KeyCode::Down => LogAction::Scroll(1),
        KeyCode::PageUp => LogAction::Scroll(-PAGE),
        KeyCode::PageDown => LogAction::Scroll(PAGE),
        // `End` is "go back to the very end", which is different from
        // scrolling down a lot: after a new filter the list changes length
        // and scrolling down blind does not land right.
        KeyCode::End => LogAction::Follow,
        // And `Home`, to the start of what is left: whoever has `End` looks
        // for it. Not `isize::MIN`, which would overflow on negation — the
        // scroll is clamped only against the cap.
        KeyCode::Home => LogAction::Scroll(isize::MIN + 1),
        KeyCode::Esc => LogAction::Leave,
        _ => return None,
    })
}

/// Applies a key to `app`'s log panel.
///
/// Raising the PANEL's level also raises the RING's when needed: without
/// that, asking for DEBUG would filter down to DEBUG some lines that were
/// stored at INFO, i.e. it would show exactly nothing and look broken.
/// Lowering it does not lower the ring's — see the note on
/// [`norte_frontend::logpanel`].
pub fn apply(
    app: &mut crate::app::App,
    resolver: &mut crate::keymap::Resolver,
    mods: crossterm::event::KeyModifiers,
    code: crossterm::event::KeyCode,
) {
    use crossterm::event::{KeyCode, KeyModifiers};
    // `Ctrl+C` BEFORE anything else, even with the filter field open: it is
    // the emergency exit, and every other handler in this tree checks it
    // first. It used to come after, and typing a filter left the reader with
    // no way to quit the program.
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    // With the filter field open, the rest of the keys belong to it:
    // otherwise a `d` in the middle of a word would change the level instead
    // of being typed.
    if app.log_filter_input.is_some() {
        edit_filter(app, mods, code);
        return;
    }
    let Some(action) = key(code, mods) else {
        // What this panel does NOT own follows its path through the keymap,
        // and this is not a detail: without it `layout.log` itself would die
        // here and the panel could not be closed with the same key that
        // opened it. A panel that keeps ALL the keys hijacks the keyboard
        // instead of taking it.
        pass_to_keymap(app, resolver, mods, code);
        return;
    };
    apply_action(app, action);
}

/// What each panel action does.
///
/// Separate from [`apply`] so it can be tested without setting up a key
/// resolver: what these lines decide —when the ring's level rises and when
/// it does not— is the panel's invariant, not a key's translation.
pub fn apply_action(app: &mut crate::app::App, action: LogAction) {
    match action {
        LogAction::Level(l) => {
            app.log_panel.show_level(l);
            // And the ring captures AT LEAST that: filtering down to DEBUG
            // what was stored at INFO would show nothing and look broken.
            // `raise_to` never lowers — see its rustdoc.
            if let Some(ring) = app.log_ring.as_ref() {
                ring.raise_to(l);
            }
            // And, if the daemon is one of the sources, it is asked TOO
            // (#328): its ring is its own, and without raising it the lines
            // being requested never come to exist on the other side.
            //
            // By the PREFERENCE and not by the effective source: whoever has
            // chosen to read the daemon is asking for its level even if it
            // has not answered yet, and the response to this call is
            // precisely one of the two ways to find out whether it knows
            // about logging.
            //
            // And only if there is someone to ask: with no daemon, or with
            // one that has already said it has no ring, this would be dead
            // state nobody drains.
            if app.log_panel.source() != LogSource::Window && app.log_remote.must_request() {
                app.log_remote.asks_level = Some(l);
            }
            // And it is STATED, because that ring does not belong to this
            // process: it is global to all the daemon's clients and never
            // goes back down. See [`level_notice`] for why it goes to the
            // bar and not the border.
            let notice = level_notice(app);
            if !notice.is_empty() {
                app.message = Some(notice);
            }
        }
        // With no second source it does nothing: cycling through three views
        // of the same ring would be a control that promises something that
        // does not exist, and changing the preference underneath would leave
        // the reader with a source they did not ask for on the day there
        // really is a daemon. It is the same thing the window does, where
        // the selector simply is not painted.
        LogAction::Source => {
            if app.log_remote.servicio == Servicio::Serves {
                app.log_panel.cycle_source();
            }
        }
        LogAction::Scroll(n) => {
            // The visible count is taken only here: doing it for every key
            // would also walk the whole ring on a level change or on
            // opening the filter, neither of which scrolls anything.
            //
            // Over the MERGED list, which is what is seen: counting only the
            // local ones would leave the cap short and a page would not
            // reach the end.
            let local = snapshot(app);
            let count = visible(app, &local).len();
            if n < 0 {
                app.log_panel.scroll_up(n.unsigned_abs(), count);
            } else {
                app.log_panel
                    .scroll_down(usize::try_from(n).unwrap_or(0), count);
            }
        }
        LogAction::Follow => app.log_panel.follow(),
        // The filter is typed in the same field as the rest of the TUI's
        // single-line inputs; `/` is what opens it.
        // It opens with what was already being filtered, not blank: refining
        // a filter is the normal case, and retyping it whole is not.
        LogAction::StartFilter => {
            app.log_filter_input = Some(app.log_panel.filter().to_string());
        }
        // Releases the keys WITHOUT closing the panel: closing something the
        // reader only wanted to stop operating is the wrong response, and
        // closing it is already what `alt+l` does again.
        LogAction::Leave => app.return_keys_to_panes(),
    }
}

/// Resolves through the keymap what this panel does not claim, and
/// dispatches it the same way as the process panel (`App::processes_command`,
/// which already handles the `layout.*` ones).
fn pass_to_keymap(
    app: &mut crate::app::App,
    resolver: &mut crate::keymap::Resolver,
    mods: crossterm::event::KeyModifiers,
    code: crossterm::event::KeyCode,
) {
    use crate::keymap::Resolution;
    let Some(chord) = crate::keymap::chord_from_crossterm(mods, code) else {
        return; // key not modeled by the keymap: ignore
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command, .. } => command,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    app.log_command(&cmd);
}

/// Keys while the filter is being typed.
///
/// `Esc` cancels and leaves the PREVIOUS filter, it does not clear it:
/// cancel means "leave it as it was", and in a log panel clearing the filter
/// by accident dumps a thousand lines over whatever you were reading.
fn edit_filter(
    app: &mut crate::app::App,
    mods: crossterm::event::KeyModifiers,
    code: crossterm::event::KeyCode,
) {
    use crossterm::event::{KeyCode, KeyModifiers};
    let Some(text) = app.log_filter_input.as_mut() else {
        return;
    };
    match code {
        KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => text.push(c),
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Enter => {
            let text = app.log_filter_input.take().unwrap_or_default();
            app.log_panel.set_filter(text);
        }
        KeyCode::Esc => app.log_filter_input = None,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use norte_config::logline::LogLevel;

    /// The five level letters are there and are the English level's
    /// initials, which is how they are named in the log itself.
    #[test]
    fn every_level_has_its_letter() {
        let expected = [
            ('e', LogLevel::Error),
            ('w', LogLevel::Warn),
            ('i', LogLevel::Info),
            ('d', LogLevel::Debug),
            ('t', LogLevel::Trace),
        ];
        for (c, level) in expected {
            assert_eq!(
                key(KeyCode::Char(c), KeyModifiers::empty()),
                Some(LogAction::Level(level)),
                "key '{c}' does not request {level:?}"
            );
        }
    }

    /// A key with Ctrl is NOT this panel's: `ctrl+c` quits the program and
    /// `ctrl+…` are global shortcuts. Swallowing them here would be hijacking
    /// them.
    #[test]
    fn control_shortcuts_are_not_kept() {
        assert_eq!(key(KeyCode::Char('c'), KeyModifiers::CONTROL), None);
        assert_eq!(key(KeyCode::Char('d'), KeyModifiers::CONTROL), None);
    }

    /// A chord with a modifier is NOT claimed by this panel, and that is
    /// where the bug was: `alt+l` is the command that opens and closes the
    /// log, and while the panel held the keyboard it swallowed it whole —
    /// meaning the same key that opened it did not close it. `key` returning
    /// `None` is what sends the key to the keymap; if it ever claims an
    /// `alt+…`, this test fails.
    #[test]
    fn chords_with_a_modifier_go_their_own_way() {
        for (code, mods) in [
            (KeyCode::Char('l'), KeyModifiers::ALT),
            (KeyCode::Char('j'), KeyModifiers::ALT),
            (KeyCode::F(9), KeyModifiers::empty()),
        ] {
            assert_eq!(
                key(code, mods),
                None,
                "{code:?}+{mods:?} was kept by the panel instead of let through"
            );
        }
    }

    /// The panel RAISES the ring's level and NEVER lowers it, and closing
    /// the panel is the only thing that returns it to where it was.
    ///
    /// All three halves matter. Without raising it, filtering down to DEBUG
    /// what was stored at INFO shows nothing and looks broken. Without the
    /// "never lowers", going to DEBUG, back to WARN and asking for DEBUG
    /// again would erase exactly the stretch you were investigating. And
    /// without lowering it on close, a single press of `t` leaves the
    /// process capturing TRACE for the rest of the session, with its cost,
    /// long after nobody is looking.
    #[test]
    fn the_ring_level_rises_never_lowers_and_resets_on_close() {
        use norte_config::logring::LogRing;
        let mut app = crate::app::testutil::app_two_panes();
        let ring = LogRing::new(10);
        app.log_ring = Some(ring.clone());
        app.toggle_log(); // opens and takes the keyboard

        apply_action(&mut app, LogAction::Level(LogLevel::Debug));
        assert_eq!(
            ring.level(),
            LogLevel::Debug,
            "asking for DEBUG did not raise it"
        );
        apply_action(&mut app, LogAction::Level(LogLevel::Warn));
        assert_eq!(
            ring.level(),
            LogLevel::Debug,
            "lowering what is SHOWN must not stop capturing"
        );
        assert_eq!(app.log_panel.level(), LogLevel::Warn);

        app.toggle_log(); // closes
        assert_eq!(
            ring.level(),
            LogLevel::Warn,
            "closing the panel must return the ring to what was being shown"
        );
    }

    /// The log's allowlist is NOT the process one's: there `dialog.confirm`
    /// cancels the task under the cursor, and here there is nothing to
    /// confirm. An `Enter` that cancels a copy from a log viewer is exactly
    /// the accident an allowlist exists to prevent.
    #[test]
    fn confirm_is_inert_in_the_log_and_its_own_key_closes_it() {
        let mut app = crate::app::testutil::app_two_panes();
        app.toggle_log();
        assert!(app.log_slot().is_some(), "did not open");

        // Inert: it neither closes the panel nor changes who owns the
        // keyboard.
        app.log_command("dialog.confirm");
        assert!(app.log_slot().is_some());
        assert_eq!(app.key_owner(), crate::app::KeyOwner::Log);

        // And its own key does: the same key that opened it closes it from
        // inside.
        app.log_command("layout.log");
        assert!(app.log_slot().is_none(), "did not close from inside");
    }

    /// `End` is not "scroll down a lot": after changing the filter the list
    /// changes length, and going back to the end has to be a command, not a
    /// bet.
    #[test]
    fn the_end_is_its_own_command() {
        assert_eq!(
            key(KeyCode::End, KeyModifiers::empty()),
            Some(LogAction::Follow)
        );
        assert_eq!(
            key(KeyCode::PageDown, KeyModifiers::empty()),
            Some(LogAction::Scroll(PAGE))
        );
    }

    // --- The remote half (#328) --------------------------------------------

    /// A line from the wire, already in presentation form.
    fn wire(epoch_ms: i64, level: &str, msg: &str) -> norte_proto::methods::LogLine {
        norte_proto::methods::LogLine {
            epoch_ms,
            level: level.to_owned(),
            target: "norte_core::daemon".to_owned(),
            message: msg.to_owned(),
        }
    }

    /// A `log.tail` response with just enough.
    fn tail(
        lines: Vec<norte_proto::methods::LogLine>,
        next: u64,
        lost: u64,
    ) -> norte_proto::methods::LogTailResult {
        norte_proto::methods::LogTailResult {
            lines,
            next,
            lost,
            level: "info".to_owned(),
            capacity: 2000,
        }
    }

    /// Puts lines into the ring through where they truly enter: the
    /// `tracing` layer.
    ///
    /// `LogRing::push` is private on purpose, and must stay that way — the
    /// filter the layer goes through is where `suppaftp`'s cap lives, since
    /// it logs `PASS <password>` at TRACE level. A shortcut for tests that
    /// skipped that cap would be testing a path that does not exist.
    fn with_lines(ring: &norte_config::logring::LogRing, f: impl FnOnce()) {
        use tracing_subscriber::layer::SubscriberExt as _;
        let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(ring));
        tracing::subscriber::with_default(s, f);
    }

    /// An app with the panel open, a local ring with one line and the daemon
    /// answering another. It is `ntc --socket`'s setup: two processes, two
    /// rings.
    ///
    /// The daemon's line is dated ONE millisecond after the local one, read
    /// from the ring itself: the clock sets the time on logging, so making
    /// up a small `epoch_ms` here would put the daemon in 1970 and the merge
    /// would come out backward for a reason that has nothing to do with what
    /// the test is looking at.
    fn app_with_both_sources() -> crate::app::App {
        use norte_config::logring::LogRing;
        let mut app = crate::app::testutil::app_two_panes();
        let ring = LogRing::new(10);
        with_lines(&ring, || tracing::info!("from this terminal"));
        let local_ms = ring.snapshot()[0].epoch_ms;
        app.log_ring = Some(ring);
        // What `main` sets from `Backend::is_remote`: there is a second
        // process.
        app.log_remote.hay_daemon = true;
        app.toggle_log();
        let epoch = app.log_remote.epoch;
        land_tail(
            &mut app,
            epoch,
            Ok(tail(
                vec![wire(local_ms + 1, "info", "from the daemon")],
                7,
                0,
            )),
        );
        app
    }

    /// With a separate daemon, the panel shows BOTH sources, in time order
    /// and knowing which one each line is from.
    ///
    /// It is #328's gap and the same answer as the window's (ADR 0077): with
    /// `--socket`, the providers, the journal, the policy and the reason a
    /// connection failed are in the other process. Fixing it in a single
    /// frontend is what makes the two silently diverge.
    #[test]
    fn la_vista_mezcla_la_terminal_y_el_daemon() {
        let app = app_with_both_sources();
        let local = snapshot(&app);
        let rows = visible(&app, &local);
        let texts: Vec<&str> = rows.iter().map(|(l, _)| l.message.as_str()).collect();
        assert_eq!(
            texts,
            ["from this terminal", "from the daemon"],
            "the merge did not reach the rows"
        );
        assert_eq!(rows[0].1, LogSource::Window);
        assert_eq!(rows[1].1, LogSource::Daemon);
    }

    /// The EFFECTIVE source is not the stored preference: a source that
    /// does not exist cannot be shown, and the panel reports what there is,
    /// not what was asked for. It collapses in both directions.
    #[test]
    fn the_effective_source_collapses_toward_the_ring_that_exists() {
        let mut app = app_with_both_sources();
        assert_eq!(source_efectiva(&app), LogSource::Both, "with both rings");

        app.log_ring = None;
        assert_eq!(
            source_efectiva(&app),
            LogSource::Daemon,
            "with no local ring there is nothing from this terminal to mix in"
        );

        app.log_remote.servicio = Servicio::NoRing;
        assert_eq!(
            source_efectiva(&app),
            LogSource::Window,
            "with no log on the other side the daemon's cannot be shown"
        );
    }

    /// The level that gets SET is always the one being shown; the daemon's
    /// is stated in the capture note, which is the place that already means
    /// "more is being collected than is shown".
    ///
    /// Setting the daemon's was the worst bug in the window's first attempt:
    /// the filter is still the panel's, so with the daemon at `trace` and
    /// the panel at `info` the header said `trace` while every `debug` line
    /// crossed the socket and was silently dropped.
    #[test]
    fn the_daemon_level_goes_in_the_capture_and_not_in_the_level() {
        let mut app = app_with_both_sources();
        app.log_remote.level = Some("trace".to_owned());
        assert_eq!(
            app.log_panel.level(),
            LogLevel::Info,
            "the panel's level is moved by the keys, not the daemon"
        );
        let note = capture_note(&app, source_efectiva(&app));
        assert!(
            note.contains(LogLevel::Trace.label().trim()),
            "the capture note does not state the daemon's level: {note:?}"
        );
        assert!(
            note.contains(&norte_i18n::ta(
                "log-capturing-daemon",
                &[("level", LogLevel::Trace.label().trim())]
            )),
            "the capture note does not say WHOSE level that is: {note:?}"
        );
    }

    /// Raising the daemon's ring is announced WHENEVER the daemon is one of
    /// the sources, not only when it is the sole one: in `Both` —which is
    /// how the panel opens— pressing `t` raises a GLOBAL daemon ring that
    /// never goes back down, and staying quiet about it would leave that
    /// decision unannounced.
    #[test]
    fn raising_the_daemons_ring_is_announced_in_the_mix_too() {
        let mut app = app_with_both_sources();
        for source in [LogSource::Both, LogSource::Daemon] {
            app.log_panel.set_source(source);
            app.message = None;
            apply_action(&mut app, LogAction::Level(LogLevel::Trace));
            assert_eq!(
                app.message.as_deref(),
                Some(norte_i18n::t("log-source-daemon-level").as_str()),
                "not announced with source {source:?}"
            );
        }
        app.log_panel.set_source(LogSource::Window);
        app.message = None;
        apply_action(&mut app, LogAction::Level(LogLevel::Warn));
        assert_eq!(
            app.message, None,
            "reading only this terminal, there is no other ring to raise"
        );
    }

    /// That the daemon does NOT serve its log is stated in the source
    /// label, which is what occupies the border: otherwise the reader
    /// would believe the interesting half simply is not happening.
    #[test]
    fn a_daemon_without_a_log_is_shown_by_the_source_label() {
        let mut app = app_with_both_sources();
        assert_eq!(
            source_label(&app, LogSource::Both),
            Some(norte_i18n::t("log-source-both"))
        );
        app.log_remote.servicio = Servicio::NoRing;
        assert_eq!(
            source_label(&app, source_efectiva(&app)),
            Some(norte_i18n::t("log-source-unsupported")),
            "a daemon with no log has to be stated"
        );
    }

    /// With no daemon —a plain `ntc`, which is the default startup—
    /// nothing is asked and nobody is named.
    ///
    /// It is the fault the review found: the embedded arm rightly answers
    /// `Unsupported` to `log.tail` —its ring is the one this panel already
    /// reads—, and the panel read that as a fact about a daemon. With an
    /// `ntc` with no daemon at all, the border went from "of this process
    /// (the daemon logs separately)" to staying at "this daemon does not
    /// serve its log". The correct answer is not a third sentence: it is
    /// the segment's absence, which is how it was in #326.
    #[test]
    fn without_a_daemon_nothing_is_polled_nor_is_anyone_named() {
        let mut app = crate::app::testutil::app_two_panes();
        app.log_ring = Some(norte_config::logring::LogRing::new(10));
        app.toggle_log();
        assert!(!app.log_remote.hay_daemon, "by default there is no daemon");
        assert!(
            !app.log_remote.must_request(),
            "it was about to probe a daemon that does not exist"
        );
        for source in [LogSource::Window, LogSource::Daemon, LogSource::Both] {
            assert_eq!(
                source_label(&app, source),
                None,
                "an origin was named with a single ring ({source:?})"
            );
        }
        // And the effective source cannot be anything else: `servicio`
        // never reaches `Serves` because nobody asks.
        assert_eq!(source_efectiva(&app), LogSource::Window);
        // Nor is anyone's ring announced when the level is raised.
        apply_action(&mut app, LogAction::Level(LogLevel::Trace));
        assert_eq!(app.message, None);
        assert_eq!(app.log_remote.asks_level, None);
    }

    /// The probe checks whether the panel is VISIBLE, not whether it
    /// exists: one hidden behind a tab that is not the active one is still
    /// in the tree, and probing it is two RPCs a second for the whole
    /// session for something nobody has in front of them.
    ///
    /// The panel bar still counts the hidden slot as open —that is #329 and
    /// is not touched here—; what this test pins down is that the network
    /// cost does not depend on that bug.
    #[test]
    fn a_panel_behind_a_tab_is_not_polled() {
        use norte_frontend::layout::{KindId, Node};
        let mut app = app_with_both_sources();
        let id = app.log_slot().expect("the panel is open");
        assert_eq!(app.log_slot_visible(), Some(id), "visible before hiding it");

        // The same slot, now in the NON-active tab of a tab group.
        let other = app
            .layout
            .slot_ids()
            .into_iter()
            .find(|s| *s != id)
            .expect("there are more slots than the log");
        app.layout = Node::Tabs {
            children: vec![
                Node::slot(other, KindId::new("pane")),
                Node::slot(id, KindId::new(KIND)),
            ],
            active: 0,
        };
        assert_eq!(
            app.log_slot(),
            Some(id),
            "still exists, which is what `log_slot` answers"
        );
        assert_eq!(
            app.log_slot_visible(),
            None,
            "a slot behind another tab is not on screen"
        );
    }

    /// Two counters and NEVER their sum: the local ring's counts what has
    /// been evicted since the process started, the daemon's what THIS
    /// opening of the panel lost. Adding them would give a number that is
    /// neither of the two things.
    #[test]
    fn the_two_counters_are_reported_separately() {
        use norte_config::logring::LogRing;
        let mut app = crate::app::testutil::app_two_panes();
        let ring = LogRing::new(1);
        with_lines(&ring, || {
            for _ in 0..4 {
                tracing::info!("x");
            }
        });
        app.log_ring = Some(ring);
        app.toggle_log();
        let epoch = app.log_remote.epoch;
        land_tail(&mut app, epoch, Ok(tail(Vec::new(), 9, 5)));
        let note = discard_note(&app, LogSource::Both);
        assert!(note.contains('3'), "missing the 3 local ones: {note:?}");
        assert!(note.contains('5'), "missing the daemon's 5: {note:?}");
        assert!(!note.contains('8'), "the counters were added: {note:?}");
        // And a ring that is not being read gets no warning: an alarm
        // about a gap that is not on screen is an alarm about nothing.
        assert!(!discard_note(&app, LogSource::Window).contains('5'));
        assert!(!discard_note(&app, LogSource::Daemon).contains('3'));
    }

    /// The cursor chains (`None` the first time, never zero) and losses
    /// ACCUMULATE: a silent gap lies about what happened.
    #[test]
    fn the_cursor_chains_and_the_losses_accumulate() {
        let mut app = crate::app::testutil::app_two_panes();
        app.toggle_log();
        assert_eq!(
            app.log_remote.cursor, None,
            "zero is not requested the first time"
        );
        let epoch = app.log_remote.epoch;
        land_tail(&mut app, epoch, Ok(tail(vec![wire(1, "warn", "a")], 4, 2)));
        assert_eq!(app.log_remote.cursor, Some(4));
        land_tail(&mut app, epoch, Ok(tail(vec![wire(2, "warn", "b")], 9, 3)));
        assert_eq!(app.log_remote.cursor, Some(9));
        assert_eq!(app.log_remote.lost, 5, "the losses did not accumulate");
        assert_eq!(
            app.log_remote.lines.len(),
            2,
            "the round does not bring only what is new"
        );
    }

    /// A daemon that says it has no log is not asked again: the refusal
    /// comes from a compile-time feature and cannot change while that
    /// daemon lives. ANY OTHER failure is not that, and it retries.
    #[test]
    fn a_daemon_without_a_log_stops_being_polled_and_a_failure_does_not() {
        let mut app = crate::app::testutil::app_two_panes();
        app.log_remote.hay_daemon = true;
        app.toggle_log();
        let epoch = app.log_remote.epoch;
        land_tail(
            &mut app,
            epoch,
            Err(norte_proto::Error::Io { retryable: true }),
        );
        assert_eq!(
            app.log_remote.servicio,
            Servicio::NoResponse,
            "a transient failure is not a permanent lack"
        );
        assert!(
            app.log_remote.must_request(),
            "a failure does not turn off the probe"
        );

        land_tail(&mut app, epoch, Err(norte_proto::Error::Unsupported));
        assert_eq!(app.log_remote.servicio, Servicio::NoRing);
        assert!(
            !app.log_remote.must_request(),
            "continuing to ask would be two RPCs a second forever"
        );
    }

    /// Between asking and answering there is room for a close and an open,
    /// and the previous session's response has to DIE: landing its cursor
    /// in the new panel would leave a made-up `lost` and lines from another
    /// read.
    #[test]
    fn the_response_of_an_earlier_open_does_not_land() {
        let mut app = crate::app::testutil::app_two_panes();
        app.toggle_log();
        let old = app.log_remote.epoch;
        app.toggle_log(); // closes: resets and changes epoch
        app.toggle_log(); // reopens
        land_tail(
            &mut app,
            old,
            Ok(tail(vec![wire(1, "info", "from the other time")], 99, 7)),
        );
        assert!(app.log_remote.lines.is_empty(), "an old response landed");
        assert_eq!(app.log_remote.cursor, None);
        assert_eq!(app.log_remote.lost, 0);
    }

    /// Closing the panel releases the daemon's lines but does NOT forget
    /// that it serves them: that there is a second source is a fact about
    /// the daemon, not about this opening, and forgetting it would hide the
    /// `s` key for half a second every time.
    #[test]
    fn closing_releases_the_lines_but_not_what_is_known_about_the_daemon() {
        let mut app = app_with_both_sources();
        assert_eq!(app.log_remote.servicio, Servicio::Serves);
        app.toggle_log();
        assert!(app.log_remote.lines.is_empty());
        assert_eq!(app.log_remote.cursor, None);
        assert_eq!(
            app.log_remote.servicio,
            Servicio::Serves,
            "forgetting that it serves would hide the control on reopen"
        );
    }

    /// `s` cycles the source, and only when there is a second one to offer:
    /// cycling through three views of the SAME ring would be a control that
    /// promises something that does not exist.
    #[test]
    fn the_source_key_only_means_something_with_a_daemon() {
        assert_eq!(
            key(KeyCode::Char('s'), KeyModifiers::empty()),
            Some(LogAction::Source)
        );
        let mut app = crate::app::testutil::app_two_panes();
        app.toggle_log();
        apply_action(&mut app, LogAction::Source);
        assert_eq!(
            app.log_panel.source(),
            LogSource::Both,
            "with no daemon, the preference is not touched"
        );

        let mut app = app_with_both_sources();
        apply_action(&mut app, LogAction::Source);
        assert_eq!(app.log_panel.source(), LogSource::Window);
        apply_action(&mut app, LogAction::Source);
        assert_eq!(app.log_panel.source(), LogSource::Daemon);
        apply_action(&mut app, LogAction::Source);
        assert_eq!(
            app.log_panel.source(),
            LogSource::Both,
            "does not go back to the start"
        );
    }

    /// Asking for more detail asks the daemon TOO when it is one of the
    /// sources: its ring is its own, and without raising it the lines being
    /// requested never come to exist on the other side. By the PREFERENCE
    /// and not by the effective source — whoever chose to read the daemon is
    /// asking for its level even if it has not answered yet. Whether there
    /// IS a daemon, though, does matter: without it the request would be
    /// dead state nobody drains.
    #[test]
    fn raising_the_level_also_asks_the_daemon() {
        let mut app = crate::app::testutil::app_two_panes();
        app.log_remote.hay_daemon = true;
        app.toggle_log();
        assert_eq!(
            app.log_remote.servicio,
            Servicio::NoResponse,
            "has not answered yet, and it is asked anyway"
        );
        apply_action(&mut app, LogAction::Level(LogLevel::Debug));
        assert_eq!(
            app.log_remote.asks_level,
            Some(LogLevel::Debug),
            "the daemon was not asked"
        );

        app.log_remote.asks_level = None;
        app.log_panel.set_source(LogSource::Window);
        apply_action(&mut app, LogAction::Level(LogLevel::Trace));
        assert_eq!(
            app.log_remote.asks_level, None,
            "reading only this terminal, there is no reason to raise anyone's global ring"
        );
    }

    /// Scrolling counts over the MERGED list: counting only the local ones
    /// leaves the cap short and a page does not reach the end.
    #[test]
    fn the_scroll_counts_both_sources() {
        let mut app = app_with_both_sources();
        app.log_panel.set_viewport_rows(1);
        apply_action(&mut app, LogAction::Scroll(-1));
        assert!(
            !app.log_panel.following(),
            "scrolling up did not detach from the end"
        );
        let local = snapshot(&app);
        assert_eq!(
            app.log_panel.window_start(visible(&app, &local).len(), 1),
            0,
            "with two lines and one row, scrolling up one leaves the first on top"
        );
        apply_action(&mut app, LogAction::Scroll(1));
        assert!(
            app.log_panel.following(),
            "scrolling down to the end did not reattach it: the cap counted only the local ring"
        );
    }

    /// The remote line cap is applied from the FRONT and what is trimmed
    /// counts as lost: a panel left open all afternoon cannot grow without
    /// end, and the trim cannot leave a silent gap.
    #[test]
    fn the_remote_cap_discards_the_old_and_counts_it() {
        let mut app = crate::app::testutil::app_two_panes();
        app.toggle_log();
        let epoch = app.log_remote.epoch;
        let many: Vec<_> = (0..i64::try_from(MAX_LINES_REMOTAS).unwrap() + 3)
            .map(|i| wire(i, "info", "x"))
            .collect();
        land_tail(&mut app, epoch, Ok(tail(many, 1, 0)));
        assert_eq!(app.log_remote.lines.len(), MAX_LINES_REMOTAS);
        assert_eq!(app.log_remote.lost, 3, "the trim stayed silent");
        assert_eq!(
            app.log_remote.lines[0].epoch_ms, 3,
            "the new ones were dropped instead of the old ones"
        );
    }
}
