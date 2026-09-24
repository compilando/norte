//! The HANDOFF to the window (phase 9 of the WOW program): what gets
//! launched, and how.
//!
//! What is HERE is only the half this process can decide without talking to
//! anyone: which binary the window is and with what arguments it starts.
//! Dumping the screen and releasing the session belongs to the session writer
//! ([`crate::session_push::request_handoff`]), and launching it, to the loop.

use std::ffi::OsString;

/// The names the window may be installed under, in order of preference.
///
/// Two and not one because `just link-gui` leaves both: `ntc-gui` is the
/// binary's name and `norte-gui` the alias people type. Looking them up by
/// PATH — and not a compiled-in path — is what makes a norte installed any of
/// the three ways (package, `cargo install`, symlink to the development tree)
/// hand off to the one the reader actually has.
const WINDOW: &[&str] = &["ntc-gui", "norte-gui"];

/// The `argv` a handoff starts the window with.
///
/// `--attach` is what sets it apart from an ordinary launch: besides the
/// screen, it claims what this process just left MARKED in the session.
/// Without it, the window would open where you were but without what you had
/// selected, which is exactly the half that a `cd` cannot redo.
///
/// The arguments come from [`norte_frontend::handoff::window_args`], the SAME
/// place the window's own test that it accepts them draws from. They used to
/// be written here by hand and carried `--daemon`, which the window does not
/// have — it always runs with a daemon: it exited with code 2 and, with
/// `stderr` closed, said nothing. The handoff was left with a terminal that
/// closed and a window that never arrived.
#[must_use]
pub fn window_argv() -> Vec<OsString> {
    let mut argv = vec![OsString::from(program())];
    argv.extend(
        norte_frontend::handoff::window_args()
            .into_iter()
            .map(OsString::from),
    );
    argv
}

/// The first of [`WINDOW`] that is on the PATH, or just the first one.
///
/// Returning the first when none is found is not pretending it exists: the
/// launch fails, the loop says so, and the reader stays where they were. The
/// alternative — refusing here — would turn "you do not have the window
/// installed" into an `Option` the caller would have to explain twice.
fn program() -> &'static str {
    WINDOW
        .iter()
        .copied()
        .find(|p| norte_frontend::openers::program_available(std::ffi::OsStr::new(p)))
        .unwrap_or(WINDOW[0])
}

/// Launches `argv` DETACHED: this process is about to go away, so the window
/// must not be left hanging off it.
///
/// Neither `stdin` nor `stdout` nor `stderr` are inherited. The terminal
/// belongs to the TUI, and a window writing to it after the handoff returns
/// it to the shell would litter the reader's prompt with traces they never
/// asked for.
///
/// Returns the `Child` so the caller can check with [`wait_startup`] that
/// the window is STILL alive: a successful `spawn` only says the process
/// started.
///
/// # Errors
/// Whatever `spawn` gives: the binary being missing is the normal case, and
/// is reported.
pub fn spawn_window(argv: &[OsString]) -> std::io::Result<std::process::Child> {
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty argv"))?;
    std::process::Command::new(program)
        .args(rest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
}

/// How long to wait for the window to prove it is going to live.
///
/// What is worth catching is IMMEDIATE death — an unknown flag, a missing
/// library, a daemon that fails to start — which happens within tens of
/// milliseconds. A second and a half is plenty for that and goes unnoticed
/// when handing over the screen, which is a one-time gesture; what it does
/// not catch — a window that dies ten seconds later — waiting three would not
/// catch either.
pub const GRACIA: std::time::Duration = std::time::Duration::from_millis(1_500);

/// Is the window still alive past `grace`? `Err` with its exit code if it
/// died before that (`None` if a signal killed it).
///
/// It is the missing half of the handoff: without this, the terminal left as
/// soon as `spawn` said `Ok`, and a window that exited with code 2 left the
/// reader with neither of the two — the opposite of what ADR 0123 promises.
///
/// Polls with `try_wait`, which does NOT block, and sleeps with tokio's
/// clock: the event loop does not sit stalled in a system call (rule 2). The
/// `Child` released on return does not kill the process: `std` does not do
/// that on drop, and the window goes on with its life.
///
/// # Errors
/// The exit code of a window that died within `grace`.
pub async fn wait_startup(
    mut child: std::process::Child,
    grace: std::time::Duration,
) -> Result<(), Option<i32>> {
    const STEP: std::time::Duration = std::time::Duration::from_millis(100);
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Err(status.code()),
            // Unable to ask means not knowing whether it died, and the answer
            // that does not leave the reader with nothing is to assume it is
            // alive: the session is already written and released, and the
            // window will claim it if it starts.
            Err(_) => return Ok(()),
            Ok(None) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(());
        }
        tokio::time::sleep(STEP).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--attach` is ALWAYS present, and `--daemon` NEVER: the window does
    /// not have it, and an unknown flag killed it without saying so. That the
    /// window accepts this `argv` is proven by its own parser, in its crate.
    #[test]
    fn the_window_is_asked_for_attach_and_nothing_it_does_not_know() {
        let argv = window_argv();
        assert!(argv.iter().any(|a| a == "--attach"), "{argv:?}");
        assert!(!argv.iter().any(|a| a == "--daemon"), "{argv:?}");
    }

    /// An empty `argv` never reaches `spawn`: it is rejected with an error
    /// instead of indexing.
    #[test]
    fn an_empty_argv_does_not_panic() {
        assert!(spawn_window(&[]).is_err());
    }

    /// A window that DIES right after being born does not count as a handoff
    /// done.
    ///
    /// The bug that asked for this: the window exited with code 2 over a flag
    /// it did not know, and the terminal had already left because `spawn` had
    /// said `Ok` — which only means the process STARTED. The reader was left
    /// with neither of the two, exactly what ADR 0123 promises does not
    /// happen.
    #[tokio::test]
    async fn a_window_that_dies_at_birth_is_not_a_handoff() {
        let child = std::process::Command::new("sh")
            .args(["-c", "exit 2"])
            .spawn()
            .expect("sh exists");
        assert_eq!(
            wait_startup(child, std::time::Duration::from_millis(1_500)).await,
            Err(Some(2)),
            "and it says which code it exited with"
        );
    }

    /// And one that is still alive past the grace period, is one.
    #[tokio::test]
    async fn a_window_that_stays_alive_is_a_handoff() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("sleep exists");
        let pid = child.id();
        // The wait keeps the `Child`; to avoid leaving a loose `sleep`, it is
        // killed afterwards by its pid.
        let _ = &mut child;
        assert_eq!(
            wait_startup(child, std::time::Duration::from_millis(300)).await,
            Ok(())
        );
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }
}
