//! The terminal the TUI actually paints on.
//!
//! Everything up to this module used `ratatui::init()`/`ratatui::restore()`,
//! which are hard-wired to stdout. That is fine as long as stdout is nothing
//! but the terminal — but `--pick` (shell integration, task 2 of this plan)
//! makes stdout carry DATA: the selection, NUL-terminated, meant to be piped
//! into another tool. A TUI that keeps painting escape sequences there would
//! corrupt that output the moment someone actually pipes it, and even without
//! `--pick`, `ntc > log` today fills `log` with escape sequences instead of
//! refusing to run.
//!
//! The fix is to paint on the CONTROLLING terminal instead — `/dev/tty` on
//! unix, `CONOUT$` on Windows — which exists whenever the process has one,
//! independent of what stdin/stdout are redirected to, and leaves stdout free
//! for real data.

use std::fs::{File, OpenOptions};
use std::io;

use crossterm::event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// The terminal this process is attached to, as a writer.
pub type TtyOut = File;

/// The TUI terminal. Replaces `ratatui::DefaultTerminal`, which is hard-wired
/// to stdout.
pub type Tui = Terminal<CrosstermBackend<TtyOut>>;

/// Path to the controlling terminal device.
#[cfg(unix)]
const TTY_PATH: &str = "/dev/tty";
#[cfg(windows)]
const TTY_PATH: &str = "CONOUT$";

/// Opens the CONTROLLING terminal for writing: `/dev/tty` on unix, `CONOUT$`
/// on Windows. `Err` when there is none — cron, both ends piped, a detached
/// service.
///
/// `read(true)` matters on unix even though nothing here reads from it: some
/// terminals refuse a write-only open of `/dev/tty`.
///
/// # Errors
/// The underlying I/O error from opening the device, wrapped so its message
/// names what is missing rather than reading like a bare `ENXIO`.
pub fn open_controlling_terminal() -> io::Result<TtyOut> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(TTY_PATH)
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("no controlling terminal ({TTY_PATH}): {e}"),
            )
        })
}

/// Raw mode + alternate screen + bracketed paste on the given handle, plus a
/// panic hook that undoes all three (and mouse capture, in case one was ever
/// armed) before deferring to whatever hook was already installed. Mirrors
/// what `ratatui::init` did for stdout.
///
/// Bracketed paste is terminal state exactly like raw mode (#143): every
/// place that hands the terminal back — this pair, the panic hook below, and
/// `run_suspended`'s `suspend_terminal`/`resume_terminal` in `main.rs` — has
/// to enable and disable it in step, or a crash (or a suspended shell) leaves
/// the user's OWN shell reading `\e[200~`/`\e[201~` markers around every
/// paste. `mouse::Capture` is the precedent for this class of bug.
///
/// The hook cannot borrow `out` — it moves into the returned [`Tui`], and the
/// hook must outlive this call — so on panic it opens a FRESH handle to the
/// same controlling terminal instead. That open can itself fail (the
/// terminal could be gone by the time the panic happens); the hook swallows
/// that error rather than propagating it, because a panic hook that panics
/// leaves the user with nothing printed at all.
///
/// # Errors
/// Any I/O error from entering raw mode, the alternate screen, or bracketed
/// paste.
pub fn init(mut out: TtyOut) -> io::Result<Tui> {
    install_panic_hook();
    enable_raw_mode()?;
    execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(out);
    Terminal::new(backend)
}

/// Undoes [`init`]. Called once, on the way out.
///
/// # Errors
/// Any I/O error from leaving raw mode, the alternate screen, or bracketed
/// paste.
pub fn restore(term: &mut Tui) -> io::Result<()> {
    // The kitty keyboard protocol (`[ui] alt_menu`) is a stack on the
    // terminal, not part of the alternate screen: leaving it pushed would
    // hand the user's shell escape codes instead of letters.
    //
    // Every step runs even if an earlier one failed: a write error on the
    // protocol must not leave the user in raw mode on the alternate screen.
    // The first error is the one reported.
    let protocolo = crate::alt_menu::set(false, || true, term.backend_mut());
    // Disabling raw mode first, same order as `ratatui::try_restore`: it has
    // more side effects than leaving the alternate screen buffer.
    let raw = disable_raw_mode();
    let pantalla = execute!(
        term.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
    protocolo.and(raw).and(pantalla)
}

/// Wraps whatever panic hook is already installed in one that restores the
/// terminal FIRST, so a panic's backtrace lands on a normal terminal instead
/// of one still in raw mode with the alternate screen up (or the mouse still
/// captured, swallowing the very clicks a developer might make to scroll
/// back and read it, or bracketed paste still armed, wrapping the next paste
/// into that shell in `\e[200~`/`\e[201~` markers instead of plain text).
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        if let Ok(mut out) = open_controlling_terminal() {
            let _ = crate::alt_menu::soltar_en_panico(&mut out);
            let _ = execute!(
                out,
                DisableBracketedPaste,
                LeaveAlternateScreen,
                DisableMouseCapture
            );
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No controlling terminal is an ERROR with a message, never a crash and
    /// never a screenful of escapes into somebody's pipe. Under nextest the
    /// test process has no tty, which is exactly the case being pinned.
    #[test]
    fn opening_without_a_controlling_terminal_is_an_error() {
        // Only meaningful where the harness really has no tty; when it does
        // (a developer running under a terminal that leaks it), the open
        // succeeds and there is nothing to assert.
        if let Err(e) = open_controlling_terminal() {
            let m = e.to_string();
            assert!(
                m.contains("terminal"),
                "the error must name what is missing: {m}"
            );
        }
    }
}
