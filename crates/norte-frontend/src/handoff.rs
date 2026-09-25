//! The HANDOFF arguments between frontends (phase 9, ADR 0123), in ONE place.
//!
//! It exists because of a bug that only showed up with a human watching: the
//! terminal launched the window as `ntc-gui --attach --daemon`, and the window
//! knew neither flag. Its parser rejects the unknown — good: a misspelled flag
//! that gets ignored is an option the user believes they set — exited with
//! code 2, and since the handoff closes its `stderr` so as not to dirty the
//! prompt, it died without saying anything. The terminal was already gone.
//!
//! The `argv` was built in one binary and parsed in ANOTHER, with nothing
//! tying them together. Now both sides pull it from here, and each binary has
//! a test that parses what the other builds with ITS real parser: it is the
//! only thing that would have caught the failure without opening a window.

/// The flag that tells a handoff apart from an ordinary launch.
///
/// With it, the incoming frontend also claims the MARKED state the other left
/// in the session. Without it, a launch is a launch, and marks from a
/// half-finished handoff do not come back to life the next day.
pub const ATTACH: &str = "--attach";

/// The arguments the terminal launches the WINDOW with.
///
/// Only [`ATTACH`], and the absence is half the fix: the window ALWAYS goes
/// with a daemon — it starts its own if there is none — so it has no
/// `--daemon`, and passing it was an unknown flag that killed it.
#[must_use]
pub fn window_args() -> Vec<String> {
    vec![ATTACH.to_owned()]
}

/// The arguments the window launches the TERMINAL (`ntc`) with.
///
/// [`ATTACH`] and `--daemon` if whoever hands off carries one: the session
/// that just let go is the daemon's, and an `ntc` against its embedded core
/// would find nothing. The window always passes `true`, because that is its
/// only mode.
#[must_use]
pub fn terminal_args(daemon: bool) -> Vec<String> {
    let mut args = vec![ATTACH.to_owned()];
    if daemon {
        args.push("--daemon".to_owned());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window does NOT receive `--daemon`: it does not have one, and an
    /// unknown flag killed it without a word. The test that it accepts the
    /// flag lives in its own crate, with its real parser; this one fixes that
    /// it never sneaks back in.
    #[test]
    fn window_is_only_asked_for_attach() {
        assert_eq!(window_args(), vec!["--attach".to_owned()]);
    }

    /// The terminal does: without `--daemon` it would run against its
    /// embedded core.
    #[test]
    fn terminal_is_passed_the_daemon() {
        assert_eq!(
            terminal_args(true),
            vec!["--attach".to_owned(), "--daemon".to_owned()]
        );
        assert_eq!(terminal_args(false), vec!["--attach".to_owned()]);
    }
}
