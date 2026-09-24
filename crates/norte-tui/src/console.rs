//! The live terminal, in BOTH directions: what the reader types and what gets
//! painted for them.
//!
//! Only the first half used to exist. A long wait — a `cd` to a remote
//! bucket, which can take seconds — ran its own `select!` reading keys so it
//! could be cancelled, and during all that time nobody repainted: the screen
//! stayed on the last frame, which is indistinguishable from a hang. The fix
//! is not an indicator, it is that whoever waits can paint; the indicator
//! comes after ([`norte_frontend::busy`]).
//!
//! They travel together and not as two parameters, for the same reason
//! `jobs::inflight` bundled seventeen of `run`'s variables: whoever waits
//! needs exactly these two, always both, and separating them makes every
//! function along the way born with one more parameter.

use crossterm::event::EventStream;

use crate::app::App;

/// The event stream plus something to repaint with while waiting.
pub struct Console<'a> {
    /// What the reader types. Public: `select!` uses it directly — a
    /// `next_event(&mut self)` method would hold `&mut self` for the whole
    /// future, and the repainting arm, in the same `select!`, would not
    /// compile.
    pub events: &'a mut EventStream,
    paint: Paint<'a>,
    /// A repaint failure has already been warned about on this console.
    failure_warned: bool,
}

/// What to paint with, if painting is possible at all.
enum Paint<'a> {
    /// The event loop, which owns the terminal.
    Terminal(&'a mut crate::tty::Tui),
    /// Nothing to paint with. EXPLICIT, and not an `Option` someone forgot to
    /// fill in: a test with no terminal and a context that truly cannot paint
    /// must say so, not look like an oversight.
    Detached,
}

impl<'a> Console<'a> {
    /// The loop's console: reads and paints.
    pub fn new(events: &'a mut EventStream, terminal: &'a mut crate::tty::Tui) -> Self {
        Self {
            events,
            paint: Paint::Terminal(terminal),
            failure_warned: false,
        }
    }

    /// A console that only reads (tests, and any context with no terminal).
    pub fn detached(events: &'a mut EventStream) -> Self {
        Self {
            events,
            paint: Paint::Detached,
            failure_warned: false,
        }
    }

    /// The terminal, for whoever needs it for something other than
    /// repainting: launching an editor, suspending, measuring the screen.
    ///
    /// Exists because the terminal has ONE owner and it is now this console:
    /// two `&mut`s at once do not compile, and a second `terminal` parameter
    /// alongside the console would be exactly the duplication this type came
    /// to avoid. `None` on a detached console — and there, whoever was about
    /// to launch something launches nothing: that is correct, not a
    /// degradation (with no terminal there is nothing to hand over).
    pub fn terminal(&mut self) -> Option<&mut crate::tty::Tui> {
        match &mut self.paint {
            Paint::Terminal(t) => Some(t),
            Paint::Detached => None,
        }
    }

    /// Repaints with the current state, if there is something to paint with.
    ///
    /// A one-off EXEMPTION from rule 2, the same one the event loop's `draw`
    /// has and for the same reason (ratatui's official async pattern): the
    /// draw writes the control terminal synchronously. There is ONE
    /// difference here worth having written down: the loop draws when
    /// nothing is in flight, and this draws while the ONLY cancellation path
    /// is pending. If the terminal stalls writing (an XOFF, a remote pty with
    /// a full buffer), the `select!` does not advance and `Esc` stops
    /// responding for as long as the stall lasts. It is accepted because the
    /// alternative — not repainting — is the failure this exists to fix, and
    /// because the loop runs that same exposure every turn.
    ///
    /// Best-effort ON PURPOSE: a draw failure must not change a navigation's
    /// return type (nor turn "could not paint the spinner" into "the
    /// navigation failed"), and nothing is lost — the loop's `draw`, which is
    /// fatal, tries again as soon as the wait ends.
    pub fn repaint(&mut self, app: &App) {
        if let Paint::Terminal(term) = &mut self.paint
            && let Err(e) = term.draw(|f| crate::ui::draw(f, app))
        {
            // Once per wait, not twelve times a second: a terminal broken for
            // ten minutes is 7500 identical lines burying what actually
            // matters in the log.
            if !self.failure_warned {
                self.failure_warned = true;
                tracing::warn!(error = %e, "could not repaint during a wait");
            }
        }
    }
}

/// How a painted wait ended.
pub enum Waited<T> {
    /// The work finished.
    Done(T),
    /// The reader pressed `Esc`.
    Cancelled,
    /// The reader pressed `Ctrl+C`: cancel AND quit.
    Quit,
}

/// Waits on `fut`, repainting the spinner and allowing cancellation.
///
/// This is the pattern for EVERY long wait in the TUI, and it is here instead
/// of repeated because repeating it was the bug: when only navigation had it,
/// refreshing panes and opening the viewer kept freezing the screen exactly
/// the same way, with a console in hand that already knew how to paint.
/// Whoever adds the fourth wait inherits the spinner by using this.
///
/// `Esc` and `Ctrl+C` are FIXED here, they do not go through the keymap: they
/// are the emergency exit and must not be remappable to something that does
/// not exist. Every other key is discarded while the wait lasts.
///
/// The caller sets `app.busy` BEFORE and clears it AFTER; this only keeps it
/// updated with the elapsed time.
pub async fn wait_painting<T>(
    console: &mut Console<'_>,
    app: &mut App,
    started: std::time::Instant,
    fut: impl Future<Output = T>,
) -> Waited<T> {
    use futures::StreamExt as _;

    tokio::pin!(fut);
    // At the spinner's pace and no other: with two different constants, the
    // frame skips or repeats and nobody notices.
    let mut tick = tokio::time::interval(norte_frontend::busy::FRAME_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            res = &mut fut => return Waited::Done(res),
            _ = tick.tick() => {
                if let Some(b) = &mut app.busy {
                    b.elapsed = started.elapsed();
                    // Before the threshold there is nothing new to show:
                    // repainting an identical frame is work the reader pays
                    // for.
                    if b.visible() {
                        console.repaint(app);
                    }
                }
            }
            maybe = console.events.next() => {
                match maybe {
                    Some(Ok(crossterm::event::Event::Key(key)))
                        if key.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        use crossterm::event::{KeyCode, KeyModifiers};
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                                return Waited::Quit;
                            }
                            (KeyCode::Esc, _) => return Waited::Cancelled,
                            _ => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    // The event stream ended or broke: there is nobody left
                    // to cancel or to continue, so the wait is abandoned.
                    Some(Err(_)) | None => return Waited::Cancelled,
                }
            }
        }
    }
}
