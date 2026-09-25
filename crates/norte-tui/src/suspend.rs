//! Handing the terminal over to another program and getting it back intact.
//!
//! It used to live in the `ntc` binary's root — a crate DISTINCT from this
//! lib — so `suspension_outcome`, the only part of the suspension that can
//! be tested without a real terminal, had its tests inside the binary.
//!
//! The invariant that orders the whole module: [`suspend_terminal`] and
//! [`resume_terminal`] are PAIRED and no path exits in between. S4's review
//! found the opposite — a boundary placed after three `?`s that had already
//! touched the terminal — and the symptom was a TUI that kept painting with
//! raw mode off.
//!
//! This module does not touch `App`: the suspension is a matter between the
//! terminal and a child process.

use norte_i18n::t;

use crate::mouse;
use crate::tty;

/// Suspends the TUI (leaves the alternate screen + raw mode), runs `argv`
/// with the CONTROL TERMINAL as stdio in `cwd`, and restores on EVERY path.
///
/// Born as `run_opener` (#28) and generalized by S4 (#135). The restoration
/// structure is the same idea, with a difference S4's review flagged
/// (MAJOR-3): the "from here on you must restore" boundary was AFTER three
/// `?`s that had already touched the terminal, so a failed
/// `LeaveAlternateScreen` returned `Err` with raw mode off and the alternate
/// screen still up — and the run loop kept painting a TUI whose keys no
/// longer responded and whose text echoed into the scrollback. Now
/// [`suspend_terminal`] and [`resume_terminal`] are paired and NO path
/// exits in between.
///
/// - an EMPTY `argv` launches nothing and returns `Ok(None)`: that is
///   `app.toggle-panels`, which only shows the host terminal.
/// - `cwd` of `None` leaves norte's directory, which is what #28's openers
///   do today (their `%d` already travels INSIDE argv, so changing it here
///   would be a behavior change with a refactor as its excuse).
/// - `wait_for_key` keeps the host terminal in view until the user presses
///   something. Without it the listing comes back over the command's
///   output with no way to read it.
///
/// # The child's stdio is `/dev/tty`, not the inherited one
///
/// Since task 1 the TUI paints on the control terminal PRECISELY so stdout
/// can carry data, and since task 2 it does (`--pick` writes the chosen
/// paths, NUL-terminated). A child with inherited stdio would mix them with
/// its own: `ntc --pick | xargs -0 …` followed by F9 puts the whole shell
/// session into the pipe, and the first "path" the downstream tool reads is
/// the shell's output glued to the first path — opening the wrong file, not
/// a cosmetic defect (S4 review, H1 and MAJOR-1). Inheriting stdin is
/// equally bad the other way: `ntc < /dev/null` gave an F9 whose shell read
/// EOF and exited instantly, and it looked like the key was broken.
///
/// If the control terminal cannot be opened it is inherited, as before:
/// that is degradation, not a reason to launch nothing.
///
/// # Ctrl+C kills the child, not norte
///
/// Leaving raw mode returns `ISIG`, and the child stays in the foreground
/// process group together with norte: with no handler, the Ctrl+C used to
/// abort a `make` would kill the whole file manager (S4 review, B1).
/// Registering SIGINT/SIGQUIT in tokio installs a process handler —
/// permanent, and that is fine: in TUI mode raw mode already stops those
/// signals from being generated — so norte survives and the child, whose
/// dispositions `exec` returned to `SIG_DFL`, dies. SIGTSTP (Ctrl+Z) is NOT
/// covered: suspending norte with the terminal half handed over is a
/// different problem, and it is stated in the help topic's honest limits.
///
/// The child inherits [`norte_frontend::shell::LEVEL_VAR`] incremented.
/// norte never reads it again: the consumer is the user's own prompt, which
/// is where it is needed to know this shell came out of a norte.
/// # Errors
///
/// Whatever fails handing over the terminal, launching the child, waiting
/// for the key, or getting it back — and in that order of precedence, which
/// is the one [`suspension_outcome`] sets. Handing over the terminal is the
/// only one of the four that aborts the suspension: the other three have
/// already gone through restoration by the time they are returned.
pub async fn run_suspended(
    terminal: &mut tty::Tui,
    capture: &mut mouse::Capture,
    argv: Vec<std::ffi::OsString>,
    cwd: Option<std::path::PathBuf>,
    wait_for_key: bool,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    // Registered BEFORE handing over the terminal, and alive until the end:
    // see "Ctrl+C kills the child" above. A failure to register does not
    // stop suspending — it means going back to the old behavior, not being
    // left without the key.
    #[cfg(unix)]
    let _signals = {
        use tokio::signal::unix::{SignalKind, signal};
        (
            signal(SignalKind::interrupt()).ok(),
            signal(SignalKind::quit()).ok(),
        )
    };
    // The capture's state is read BEFORE touching anything: if the release
    // itself fails halfway, restoration has to know what to go back to (S4
    // review, MINOR-6).
    let mouse_on = capture.active();
    // ---- boundary: from here on, every path goes through `resume_terminal`.
    let yielded = suspend_terminal(terminal, capture);
    if let Err(e) = yielded {
        let _ = resume_terminal(terminal, capture, mouse_on);
        return Err(e);
    }
    let child = if argv.is_empty() {
        Ok(Ok(None))
    } else {
        let level = norte_frontend::shell::next_norte_level();
        let stdio = || {
            tty::open_controlling_terminal()
                .and_then(|f| f.try_clone())
                .map_or_else(
                    |_| std::process::Stdio::inherit(),
                    std::process::Stdio::from,
                )
        };
        tokio::task::spawn_blocking(move || {
            // The program is resolved to an ABSOLUTE path here, with
            // norte's cwd still set, and the raw name is never handed to
            // `Command` (#302). On unix `current_dir` is applied BEFORE
            // resolving the program, so an `$EDITOR=vim` with a `.` (or an
            // empty component) in `PATH` would run a file called `vim`
            // inside the directory the reader is BROWSING: extract a
            // hostile archive, enter it and press F4. `resolve_program`
            // skips `PATH`'s relative entries for that exact reason.
            //
            // And if it is not found, NOTHING is launched: falling back to
            // the raw name would hand the search back to `execvp` with the
            // cwd already changed, which is exactly the hole. An editor
            // that is not installed gave an ENOENT anyway; what changes is
            // that now the message says which program.
            //
            // `split_first` and not `argv[0]`: the `is_empty` above covers
            // it today, but an index is a panic and there is already an
            // `io::Result` here.
            let (name, rest) = argv.split_first().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty argv")
            })?;
            let program = norte_frontend::openers::resolve_program(name).ok_or_else(|| {
                // `msg-shell-failed` already NAMES the program, so this
                // detail only says what the caller does not know: that it
                // was not found, and where it was looked for.
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "not found in PATH (relative PATH entries are ignored)",
                )
            })?;
            let mut cmd = std::process::Command::new(&program);
            cmd.args(rest)
                .env(norte_frontend::shell::LEVEL_VAR, level)
                .stdin(stdio())
                .stdout(stdio())
                .stderr(stdio());
            if let Some(dir) = cwd {
                cmd.current_dir(dir);
            }
            cmd.status().map(Some)
        })
        .await
    };
    // The wait goes AFTER the child and BEFORE restoring: it is the window
    // in which the command's output stays on screen. Its own failure must
    // not skip restoration, so it is stored and propagated with the rest.
    let waited = if wait_for_key {
        wait_for_any_key(terminal.backend_mut()).await
    } else {
        Ok(())
    };
    // Whatever the user typed WHILE the child was running is still in
    // crossterm's buffer, and without draining it the run loop would
    // dispatch it right after as COMMANDS against a listing that just
    // changed (S4 review, MINOR-2): the rest of a multi-line paste is the
    // case that hurts.
    drain_type_ahead().await;
    let restored = resume_terminal(terminal, capture, mouse_on);
    suspension_outcome(child, waited, restored)
}

/// HANDS the terminal over to the persistent subshell until the reader asks
/// for it back with the same chord (#142).
///
/// Returns the directory the shell ended up in, if it announced one and it
/// is different: the panel follows it, which is half of why a subshell is
/// not a scrollback.
///
/// # How the keys are split up
///
/// There is ONE single terminal reader — crossterm's, the one the TUI
/// already uses — and keys are TRANSLATED into the bytes a shell expects
/// ([`crate::subshell::key_to_bytes`]). A thread reading `/dev/tty` raw
/// would have been more faithful and would have left that thread blocked
/// inside a `read` when the shell is released, eating the reader's next
/// key: the one that already belonged to the panels.
///
/// Raw mode STAYS ON for as long as it lasts: without it the terminal
/// cooks the line and the shell sees no key until Enter — no line editing,
/// no Ctrl+C, no history.
///
/// # Blocks
///
/// SYNCHRONOUS on purpose, and it has to be called from
/// [`tokio::task::block_in_place`]: the loop below stays inside for the
/// whole shell session — minutes, if the reader left a `make` running —
/// doing blocking I/O over the terminal. An `async fn` that never yields
/// would be rule 2 with a different signature, and without
/// `block_in_place` it would take down the executor's thread: background
/// tasks (the paginated drainers, the watcher) would stop making progress
/// while the shell is in front.
///
/// # Errors
/// Whatever fails handing over or getting back the terminal, or writing the
/// shell's output.
// POSIX, like the whole `subshell` module (see `lib.rs`).
#[cfg(unix)]
pub fn attach_subshell(
    terminal: &mut tty::Tui,
    capture: &mut mouse::Capture,
    sub: &mut crate::subshell::Subshell,
    dir: &std::path::Path,
    chord: norte_frontend::keymap::Chord,
) -> std::io::Result<Option<std::path::PathBuf>> {
    use crossterm::event::{Event, poll, read};
    use std::io::Write as _;

    let mouse_on = capture.active();
    // The control terminal's handle is opened BEFORE handing anything over
    // (`run_suspended` does the same, and for the same reason): between
    // `suspend_terminal` and `resume_terminal` there is no exiting, and a
    // `?` here left the alternate screen closed, the mouse released and raw
    // mode on, with the loop repainting over the reader's scrollback.
    let mut out = tty::open_controlling_terminal()?;
    if let Err(e) = suspend_terminal(terminal, capture) {
        let _ = resume_terminal(terminal, capture, mouse_on);
        return Err(e);
    }
    // Raw mode AGAIN, since `suspend_terminal` turns it off: here no
    // program that keeps the terminal is launched, the keys are handed
    // over by hand.
    let raw = crossterm::terminal::enable_raw_mode();
    // The size may have changed with the panels in front, and the shell
    // never found out: its full-screen programs would paint over a
    // geometry that no longer exists until someone resized WHILE inside.
    if let Ok(size) = crossterm::terminal::size() {
        sub.resize(size);
    }
    // The starting point is where the PANEL IS, not where the shell was: on
    // entry it is sent there, so comparing against its previous position
    // gave "it changed" — and a panel re-listing to where it already
    // was — on every Ctrl+O.
    let before = Some(dir.to_path_buf());
    // The shell FOLLOWS the panel on entry. It is the other half of the
    // following — the way back is done by the caller with what this
    // returns. It CAN REFUSE (a half-typed line, a `vim` in front): see
    // `Subshell::ir_a`.
    let _ = sub.ir_a(dir);
    let result = (|| -> std::io::Result<()> {
        loop {
            // The short deadline is what makes the shell's output appear
            // while nobody is typing: without it, a `make` would not be
            // seen making progress until the next key.
            if poll(std::time::Duration::from_millis(20))? {
                match read()? {
                    // The chord is compared in CANONICAL form (`Chord`), not
                    // as a raw event: the one bound to `app.toggle-panels`
                    // comes from the keymap, and two different crossterm
                    // events — `KeyEventKind`, the `shift` a `Char` already
                    // carries inside — are the same chord. Comparing
                    // events, the exit key depended on whether the terminal
                    // sends repeats.
                    Event::Key(k)
                        if k.kind == crossterm::event::KeyEventKind::Press
                            && crate::keymap::chord_from_crossterm(k.modifiers, k.code)
                                == Some(chord) =>
                    {
                        return Ok(());
                    }
                    Event::Key(k) => {
                        if k.kind == crossterm::event::KeyEventKind::Press
                            && let Some(bytes) = crate::subshell::key_to_bytes(&k)
                        {
                            let _ = sub.write_key(&bytes);
                        }
                    }
                    // The shell has to know the new size or it paints over
                    // a screen that does not exist.
                    Event::Resize(w, h) => sub.resize((w, h)),
                    // A paste DOES arrive, even though the mouse does not:
                    // the "a shell does not ask for it" argument falls
                    // apart the moment the shell has a `vim` in front,
                    // which DID ask for it — and the paste was lost whole,
                    // with no error and leaving no half behind.
                    Event::Paste(text) => {
                        let _ = sub.write(text.as_bytes());
                    }
                    _ => {}
                }
            }
            let pending = sub.drain();
            if !pending.is_empty() {
                out.write_all(&pending)?;
                out.flush()?;
            }
            if sub.dead() {
                return Ok(());
            }
        }
    })();
    // Restoration ALWAYS happens, like in `run_suspended`: the loop's error
    // is propagated behind it.
    if raw.is_ok() {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    let back = resume_terminal(terminal, capture, mouse_on);
    result?;
    back?;
    // Only if it CHANGED: returning the same directory would make every
    // Ctrl+O re-list the panel for nothing.
    //
    // The comparison NORMALIZES, even though what is returned is the real
    // bytes (the macOS pitfall, CLAUDE.md). `$PWD` is the string the shell
    // received in the `cd`, not a re-read from disk: on macOS a reader who
    // types `cd ~/Documentos/café` leaves a `$PWD` in NFC while the
    // `VPath` norte pulled from that same directory's `readdir` is in NFD.
    // Byte for byte they never match, so every Ctrl+O re-listed — and left
    // the panel with a `VPath` that neither the history, nor the
    // favorites, nor the marks recognize as the previous one.
    let now = sub.cwd();
    let key = |p: &Option<std::path::PathBuf>| {
        use std::os::unix::ffi::OsStrExt as _;
        p.as_ref().map(|p| {
            norte_encoding::name_key(p.as_os_str().as_bytes(), norte_encoding::FoldMode::None)
                .into_owned()
        })
    };
    Ok(if key(&now) == key(&before) { None } else { now })
}

/// Hands over the terminal: releases the mouse, bracketed paste, leaves raw
/// mode and the alternate screen, in that order.
///
/// The capture is released FIRST: the program coming next did not ask for
/// it, and inheriting it feeds it every pointer movement through stdin as
/// if they were keys. Bracketed paste follows the SAME argument (#143): the
/// child did not ask for it either, and inheriting it would hand it every
/// paste wrapped in `\e[200~`/`\e[201~` instead of plain text — `less` or an
/// external editor would read those markers as if the user had typed them.
/// Synchronous write to the control terminal (`terminal.backend_mut()`,
/// never stdout — see `tty.rs`), the same one-off exemption from rule 2 as
/// the rest of the suspension.
///
/// # Errors
///
/// Whatever crossterm returns releasing the capture, leaving raw mode, or
/// leaving the alternate screen. A failure here does NOT excuse restoring:
/// the terminal may have been left half handed-over, and that is exactly
/// the state [`resume_terminal`] has to undo.
pub fn suspend_terminal(
    terminal: &mut tty::Tui,
    capture: &mut mouse::Capture,
) -> std::io::Result<()> {
    use crossterm::event::DisableBracketedPaste;
    use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
    mouse::release_for_suspend(capture, terminal.backend_mut())?;
    // Kitty's keyboard protocol (`[ui] alt_menu`), for the same reason as
    // the capture: the shell did not ask for it and would read escapes
    // instead of letters.
    crate::alt_menu::yield_(terminal.backend_mut())?;
    // T4 (phase 5 WOW), moment 3 of 4: if the viewer had an image placed,
    // it is erased BEFORE releasing the terminal — the program coming next
    // did not ask for it either, and without erasing it it would be left
    // floating over its screen. Best-effort (never `?`): a failure here
    // must not prevent handing over the terminal, which is what this
    // moment exists to guarantee. On the way back ([`resume_terminal`])
    // there is no need to place it back by hand: `app.viewer_imagen` stays
    // alive, and the first frame the run loop paints after resuming places
    // it again on its own (the same mechanism that closes the viewer or
    // moves it to another file).
    crate::kitty_graphics::delete_placed(terminal.backend_mut());
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        // The CURSOR is also given back, and it was not (#142): `ratatui`
        // hides it on every frame that sets no position, and this TUI sets
        // none. The alternate screen does NOT save that state, so the
        // program behind it inherited an invisible cursor. With the old
        // scrollback it went unnoticed; in a shell where you TYPE it is
        // the first thing noticed. The next `draw` hides it again on its
        // own.
        crossterm::cursor::Show,
        LeaveAlternateScreen
    )?;
    Ok(())
}

/// Gets the terminal back: alternate screen, raw mode, bracketed paste,
/// mouse capture EXACTLY as it was (if the user had it off, `[ui] mouse =
/// false`, coming back from a shell does not turn it on) and a clean
/// repaint.
///
/// Bracketed paste, unlike the mouse, has no `[ui]` to turn it off: it
/// ALWAYS comes back, just like raw mode — norte requests it the moment it
/// has the terminal (`tty::init`), with no user condition in between (#143).
/// # Errors
///
/// Whatever crossterm returns going back to the alternate screen, requesting
/// raw mode, or restoring the capture, and whatever the backend returns
/// clearing. Untranslated: the caller propagates it behind the child's
/// result.
pub fn resume_terminal(
    terminal: &mut tty::Tui,
    capture: &mut mouse::Capture,
    mouse_on: bool,
) -> std::io::Result<()> {
    use crossterm::event::EnableBracketedPaste;
    use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
    use ratatui::backend::Backend as _;
    crossterm::execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableBracketedPaste
    )?;
    enable_raw_mode()?;
    mouse::restore_after_suspend(capture, mouse_on, terminal.backend_mut())?;
    crate::alt_menu::recover(terminal.backend_mut())?;
    // NOT `Terminal::clear()`, and this is not a style preference: in
    // ratatui 0.30 that function asks for the cursor's position
    // (`get_cursor_position` → `crossterm::cursor::position`), which emits
    // the DSR `ESC [ 6 n` over **stdout** — the process's stdout, not our
    // backend's writer. Under `--pick` stdout is the caller's data pipe,
    // so coming back from a suspension injected `\x1b[6n` in front of the
    // NUL-terminated stream's first path. S4's end-to-end verification
    // caught it, not the suite: it is exactly the failure task 1 existed
    // to prevent, entering through a door task 1 does not control.
    //
    // Clearing via the BACKEND writes to `/dev/tty` like everything else,
    // and two `swap_buffers` leave BOTH buffers blank, which is what forces
    // a full repaint on the next draw (only one would leave the previous
    // one with its content from before suspending, and the diff would eat
    // almost everything).
    terminal.backend_mut().clear()?;
    terminal.swap_buffers();
    terminal.swap_buffers();
    Ok(())
}

/// What a suspension returns when more than one thing could have failed.
///
/// Extracted (S4 review, M6) because it is the ONLY part of `run_suspended`
/// that can be tested with no terminal, and it is where the rule lives: the
/// child's result rules — it is the answer to what the user asked for —
/// and the wait's and the restoration's failures are propagated behind it
/// in that order. A broken join turns into an I/O error because to the
/// caller it is indistinguishable from the child never getting to run.
/// # Errors
///
/// Does not fail on its own: returns the first of the three that carries an
/// error, in the order child → wait → restoration. A `JoinError` from the
/// child turns into `io::Error::other` because to the caller it is
/// indistinguishable from the child never getting to run.
pub fn suspension_outcome(
    child: Result<std::io::Result<Option<std::process::ExitStatus>>, tokio::task::JoinError>,
    waited: std::io::Result<()>,
    restored: std::io::Result<()>,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    // The child's failure is returned BEFORE the other two. `run_opener`
    // said this same thing in its comment and did the opposite (`restored?`
    // came out first), which was never noticed because restoring almost
    // never fails; writing the test made the contradiction show up on its
    // own. The child wins because it is the answer to what the user asked
    // for: "that shell does not exist" is actionable and "could not go
    // back to the alternate screen" says nothing about the key that was
    // pressed.
    let status = child.map_err(std::io::Error::other)??;
    waited?;
    restored?;
    Ok(status)
}

/// Swallows whatever the user typed while the terminal did not belong to
/// norte.
///
/// It is not courtesy: without this, the rest of a multi-line paste (or
/// any type-ahead) reaches the run loop as keystrokes and gets dispatched
/// as COMMANDS against a listing the child just changed. Bounded to
/// [`TYPE_AHEAD_MAX`] events so a resize storm does not turn it into a
/// loop.
async fn drain_type_ahead() {
    let _ = tokio::task::spawn_blocking(|| {
        for _ in 0..TYPE_AHEAD_MAX {
            match crossterm::event::poll(std::time::Duration::ZERO) {
                Ok(true) => {
                    if crossterm::event::read().is_err() {
                        return;
                    }
                }
                _ => return,
            }
        }
    })
    .await;
}

/// Cap on the events [`drain_type_ahead`] discards in one go.
const TYPE_AHEAD_MAX: usize = 4096;

/// Paints the notice and blocks until the next keypress, with the terminal
/// already out of TUI mode.
///
/// Read via `crossterm::event::read`, not a raw byte from the tty, because
/// a raw byte splits escape sequences: an arrow key delivers `ESC [ A` and
/// keeping only the `ESC` leaves `[ A` in the buffer, which the TUI will
/// read right after as two keys the user never pressed. `read` parses the
/// whole event. Raw mode is turned on so ANY key counts and an Enter is not
/// needed (in canonical mode the terminal delivers nothing until the
/// newline).
///
/// # Why it does not race the run loop's `EventStream`
///
/// Not because they share crossterm's internal source mutex — that only
/// serializes — but because the reader thread `EventStream` raises when
/// polled is NOT alive here: it ends before delivering an event, and every
/// writer of `pending_shell` is a key already dispatched, so the run loop
/// is stopped in `recv()` while this runs. It is an INCIDENTAL invariant,
/// and it is worth knowing: the first path that leaves a suspension
/// pending without coming from a key (a timer, a dispatch from Lua, a
/// plugin action) reintroduces the race and would eat the user's keys to
/// replay them later. For the same reason, this wait must never be wrapped
/// in a `select!` with a timeout: `spawn_blocking` is not cancelable and
/// would keep the reader's lock.
///
/// A read error (no stdin, a dead terminal) exits without further ado: the
/// wait is courtesy and must not turn into a hang.
async fn wait_for_any_key(out: &mut impl std::io::Write) -> std::io::Result<()> {
    write_resume_prologue(out)?;
    crossterm::terminal::enable_raw_mode()?;
    let read = tokio::task::spawn_blocking(|| {
        loop {
            match crossterm::event::read() {
                Ok(crossterm::event::Event::Key(k))
                    if k.kind == crossterm::event::KeyEventKind::Press =>
                {
                    return;
                }
                // Resize/Mouse/Focus and repeats do not count as "a key":
                // keep waiting.
                Ok(_) => {}
                // With no stdin there is no key to wait for; exit instead
                // of spinning.
                Err(_) => return,
            }
        }
    })
    .await;
    // Raw mode stays on on purpose: `resume_terminal` requests it again
    // right after and `enable_raw_mode` is idempotent.
    read.map_err(std::io::Error::other)
}

/// Returns the terminal to a known state and writes the "press a key"
/// notice.
///
/// The child just had the whole terminal and may have left it in any state
/// of its own: SGR active, the G1 line-drawing character set selected,
/// autowrap off (S4 review, L4). ratatui's `clear()` and repaint restore
/// attributes PER CELL, but not the character set selection nor DECAWM —
/// so the notice would come out in reversed red and with box-drawing
/// glyphs, and the listing itself behind it. Emitted, in this order: SGR
/// reset, US-ASCII in G0, autowrap on.
///
/// The notice carries `\r\n` because the raw mode that comes right after no
/// longer translates `\n`, and without the carriage return the next line
/// comes out staggered.
/// # Errors
///
/// Whatever `out` returns writing the prologue or flushing.
pub fn write_resume_prologue(out: &mut impl std::io::Write) -> std::io::Result<()> {
    write!(
        out,
        "\x1b[0m\x1b(B\x1b[?7h\r\n{}\r\n",
        t("msg-shell-press-key")
    )?;
    out.flush()
}

#[cfg(test)]
mod suspend_tests {
    use super::run_suspended;
    use crate::app::{App, Modal, Pane};
    use crate::gestures::{shell_remote_message, submit_command_line};
    use norte_proto::VPath;

    fn app_at(wire: &str) -> App {
        let d = VPath::parse(wire).expect("test wire");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// The control terminal for the two tests that really suspend, or
    /// `None` with a notice — never a silent skip (the style of the
    /// wasm/MinIO skips).
    ///
    /// Three conditions, and all three are needed:
    ///
    /// - `NORTE_TTY_TESTS`: with no opt-in it does not run. Putting the
    ///   developer's terminal into the alternate screen and raw mode in the
    ///   middle of the gate is worse than not having the test.
    /// - a `/dev/tty` that opens: without it there is nothing to suspend.
    /// - a **stdin** that is a terminal. This is not overzealousness:
    ///   `Terminal::clear` (ratatui 0.30) asks for the cursor's position
    ///   with a DSR and WAITS for the answer OVER STDIN. Under nextest
    ///   stdin is `/dev/null` even if the process runs inside tmux, so the
    ///   answer never arrives and the suspension fails after two seconds
    ///   over something that is not a product bug. Checking it here is
    ///   what stops that artifact from being read as a failure.
    fn tty_for_test() -> Option<crate::tty::TtyOut> {
        use std::io::IsTerminal as _;
        if std::env::var_os("NORTE_TTY_TESTS").is_none() {
            eprintln!("skip: NORTE_TTY_TESTS unset (this one drives a real terminal)");
            return None;
        }
        if !std::io::stdin().is_terminal() {
            eprintln!("skip: stdin is not a terminal (the DSR of `clear` would never be answered)");
            return None;
        }
        match crate::tty::open_controlling_terminal() {
            Ok(out) => Some(out),
            Err(e) => {
                eprintln!("skip: no controlling terminal ({e})");
                None
            }
        }
    }

    /// A remote pane does NOT open a shell: the refusal is that the
    /// conversion to a native path fails, and the message NAMES the pane
    /// so it does not look like the key is broken.
    #[test]
    fn a_remote_pane_has_nowhere_to_put_a_shell() {
        let app = app_at("sftp://host/x");
        assert!(
            norte_vfs_local::vpath_to_native(app.focused().dir()).is_err(),
            "if this conversion ever worked, the arm would open a shell in \
             the wrong place with nothing said"
        );
        // `path_display` is the one that decides the shape ("⟨sftp
        // host⟩/x"); what this test pins is that the pane IS NAMED, not
        // the format.
        let msg = shell_remote_message(&app);
        assert!(msg.contains("sftp") && msg.contains("host"), "{msg}");
    }

    /// A directory's name can carry bidi/invisibles, and this line is
    /// painted in the bar: it comes out SANITIZED and with a badge, never
    /// raw.
    #[test]
    fn the_notice_sanitizes_a_hostile_directory() {
        // An RLO inside the name: the classic for reversing what is read.
        let app = app_at("sftp://host/a%E2%80%AEb");
        let msg = shell_remote_message(&app);
        assert!(
            !msg.contains('\u{202E}'),
            "the bidi override never reaches the bar: {msg:?}"
        );
        assert!(
            msg.contains(crate::ui::HOSTILE_BADGE),
            "and it is marked as hostile: {msg:?}"
        );
    }

    /// `pane.command-line`'s prompt returns the line AS IS (a leading
    /// space is the `HISTCONTROL=ignorespace` convention, not garbage to
    /// trim) and a blank line launches nothing.
    #[test]
    fn the_command_line_does_not_trim_and_rejects_the_empty() {
        let mut app = app_at("file:///tmp");
        app.open_command_line();
        for c in " make test".chars() {
            app.command_line_push(c);
        }
        assert_eq!(app.command_line_confirm().as_deref(), Some(" make test"));
        let mut app = app_at("file:///tmp");
        app.open_command_line();
        for c in "   ".chars() {
            app.command_line_push(c);
        }
        assert!(app.command_line_confirm().is_none());
        assert!(
            matches!(app.modal, Some(Modal::CommandLine { error: Some(_), .. })),
            "and the diagnostic stays under the field"
        );
    }

    /// The command line's Enter arms `$SHELL -c CMD` with the WHOLE line
    /// as a single argument and the pane's dir as cwd.
    #[test]
    fn the_lines_enter_arms_shell_dash_c() {
        let mut app = app_at("file:///tmp");
        submit_command_line(&mut app, "ls | wc -l");
        let p = app.pending_shell.expect("leaves the suspension pending");
        assert_eq!(p.argv.len(), 3, "binary, -c and the line: {:?}", p.argv);
        assert_eq!(p.argv[1], std::ffi::OsString::from("-c"));
        assert_eq!(
            p.argv[2],
            std::ffi::OsString::from("ls | wc -l"),
            "the line is not chopped up: the shell parses it"
        );
        assert_eq!(p.cwd, Some(std::path::PathBuf::from("/tmp")));
        assert!(p.wait_for_key, "the output has to be readable");
        assert!(app.modal.is_none(), "and the prompt closes");
    }

    /// The pane moved to a remote between opening the prompt and confirming
    /// it: NOTHING gets executed (running it in norte's dir would be doing
    /// it where the user is not looking), it warns, and the prompt closes
    /// anyway.
    #[test]
    fn a_confirmed_line_over_a_remote_pane_executes_nothing() {
        let mut app = app_at("sftp://host/x");
        submit_command_line(&mut app, "rm -rf .");
        assert!(app.pending_shell.is_none(), "nothing to execute");
        assert!(app.message.is_some(), "and it says why");
        assert!(app.modal.is_none());
    }

    /// The rule for which error wins when several things fail, tested WITH
    /// NO terminal (S4 review, M6). The effect — who touches the screen —
    /// needs a tty; the POLICY does not, and that is where what can go
    /// wrong lives.
    #[test]
    fn the_childs_result_beats_the_wait_and_the_restoration() {
        use std::io::{Error, ErrorKind};
        let ok_status = || {
            // A real `ExitStatus` with nothing launched: a trivial child's.
            std::process::Command::new("true")
                .status()
                .expect("`true` exists on any unix")
        };
        // All fine: the child's status comes out.
        let r = super::suspension_outcome(Ok(Ok(Some(ok_status()))), Ok(()), Ok(()));
        assert!(r.expect("ok").is_some());

        // No child (empty argv) is not an error either.
        assert!(
            super::suspension_outcome(Ok(Ok(None)), Ok(()), Ok(()))
                .expect("ok")
                .is_none()
        );

        // The child's error BEATS the wait's and the restoration's: it is
        // the answer to what the user asked for.
        let e = super::suspension_outcome(
            Ok(Err(Error::new(ErrorKind::NotFound, "no shell"))),
            Err(Error::other("wait")),
            Err(Error::other("restore")),
        )
        .expect_err("the child failed");
        assert_eq!(e.kind(), ErrorKind::NotFound, "{e}");

        // With no child failure, the wait goes ahead of the restoration.
        let e = super::suspension_outcome(
            Ok(Ok(None)),
            Err(Error::other("wait")),
            Err(Error::other("restore")),
        )
        .expect_err("the wait failed");
        assert!(e.to_string().contains("wait"), "{e}");

        // And a failure ONLY in the restoration is propagated: leaving the
        // terminal half-done is never swallowed.
        let e = super::suspension_outcome(Ok(Ok(None)), Ok(()), Err(Error::other("restore")))
            .expect_err("the restoration failed");
        assert!(e.to_string().contains("restore"), "{e}");
    }

    /// The notice preceding "press a key" RETURNS the terminal to a known
    /// state before writing anything (S4 review, L4): the child may have
    /// left SGR active, the G1 line-drawing set selected, or autowrap off,
    /// and `clear()` restores per-cell attributes but not those three
    /// things. Checkable with no tty because the prologue writes to any
    /// `Write`.
    #[test]
    fn the_prologue_resets_the_terminal_before_the_notice() {
        let mut out: Vec<u8> = Vec::new();
        super::write_resume_prologue(&mut out).expect("writes into a Vec");
        let s = String::from_utf8(out).expect("UTF-8");
        assert!(s.starts_with("\x1b[0m"), "SGR reset first: {s:?}");
        assert!(s.contains("\x1b(B"), "US-ASCII in G0: {s:?}");
        assert!(s.contains("\x1b[?7h"), "autowrap on: {s:?}");
        assert!(
            s.contains("\r\n"),
            "with a carriage return: the raw mode coming next no longer translates \\n"
        );
    }

    /// The suspension restores the terminal on EVERY path, including the
    /// one that is easy to forget: a child that FAILS. That the function
    /// returns — with the child's status inside — is the proof the failure
    /// did not short-circuit the restoration.
    ///
    /// Needs a REAL control terminal, so it is opt-in (`NORTE_TTY_TESTS=1`):
    /// running it in the gate would put the developer's terminal into the
    /// alternate screen and raw mode in the middle of the suite. Skips
    /// with a notice, in the style of the wasm/MinIO skips, never in
    /// silence.
    #[tokio::test]
    async fn a_child_that_fails_does_not_skip_the_restoration() {
        let Some(out) = tty_for_test() else { return };
        let mut term = crate::tty::init(out).expect("init");
        let mut capture = crate::mouse::Capture::default();
        let status = run_suspended(
            &mut term,
            &mut capture,
            vec![std::ffi::OsString::from("false")],
            None,
            false,
        )
        .await
        .expect("the suspension returns");
        let _ = crate::tty::restore(&mut term);
        let status = status.expect("a non-empty argv has a status");
        assert!(
            !status.success(),
            "`false` fails, and that is what is propagated"
        );
    }

    /// An EMPTY argv launches nothing and is not an error: it is
    /// `app.toggle-panels`.
    #[tokio::test]
    async fn an_empty_argv_launches_nothing() {
        let Some(out) = tty_for_test() else { return };
        let mut term = crate::tty::init(out).expect("init");
        let mut capture = crate::mouse::Capture::default();
        let status = run_suspended(&mut term, &mut capture, Vec::new(), None, false)
            .await
            .expect("the suspension returns");
        let _ = crate::tty::restore(&mut term);
        assert!(
            status.is_none(),
            "there was no child, so there is no status"
        );
    }
}
