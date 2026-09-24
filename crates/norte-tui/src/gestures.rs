//! The gestures that hand control to another program or move a whole panel:
//! openers, the command line, the editor, disconnecting, mirror and dragging
//! between panels.
//!
//! They lived in the `ntc` binary's root —a crate DIFFERENT from this lib—
//! along with three of the biggest test modules: `pane_gestures_tests` (682
//! lines), `open_tests` and `edit_tests`.
//!
//! What ties these functions together is a shape, not a topic: ALL of them
//! decide and NONE execute. `resolve_opener` leaves a `PendingOpen`,
//! `submit_command_line` and `edit_under_cursor` leave a `PendingShell`, and
//! `mirror_plan`/`pull_plan` return a [`PaneMove`] instead of navigating. The
//! terminal's owner —the event loop— is the one that launches. This is what
//! lets the DECISION be tested with no `Backend` and no event stream, and
//! those are exactly the tests that came stuck to a binary.
//!
//! [`shell_cwd`]'s rustdoc was stranded over `disconnect` in `main.rs`, two
//! doc comments in a row in front of a single function. It goes back to its
//! own without a word changed.

use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::{EntryKind, VPath};

use crate::app::{App, Trail, error_message};
use crate::mouse;
use crate::navigate::{Cd, cd_in};
use crate::suspend::run_suspended;
use crate::tty;

/// Resolves the selected file's opener (#28) and leaves in
/// `app.pending_open` what the run loop —owner of the terminal— will launch.
/// `ns.toml` rules first; with no rule for that mimetype the desktop
/// launcher is left, which is what makes F4 work with no configuration
/// written. Every failure goes to the bar —clean degradation, never a blind
/// launch—: no file (no-op), remote or inside an archive (`msg-open-remote`),
/// or missing binary (`msg-open-missing-program`, which also covers a Linux
/// with no `xdg-utils`).
pub fn resolve_opener(app: &mut App) {
    use norte_frontend::openers;
    let Some(path) = app
        .focused()
        .selected()
        .filter(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink))
        .map(|e| e.path.clone())
    else {
        return;
    };
    // Native path: ONLY local `file://`; archive/sftp/s3 → no native path.
    let Ok(native) = norte_vfs_local::vpath_to_native(&path) else {
        app.message = Some(t("msg-open-remote"));
        return;
    };
    let mime = openers::guess_mime(
        path.file_name()
            .map_or(&[][..], norte_proto::Segment::as_bytes),
    );
    let Some(opener) = app.openers.resolve(mime) else {
        // With no rule in `ns.toml` for this mimetype, the last resort is
        // left: the desktop's own launcher. Before, this was an error
        // message, which forced writing configuration just to open a PDF.
        // Goes `detached` — hands the file to its associated program and
        // returns, so suspending the TUI for it would only paint a flicker.
        let (program, argv) = openers::system_opener(&native);
        app.pending_open = Some(crate::app::PendingOpen {
            program,
            argv,
            detached: true,
            // The pane's here too (#144): `xdg-open` hands it to the
            // associated program, which can be the same editor as a
            // declared opener — inheriting norte's cwd depending on which
            // path got there would be the same surprise through a different
            // door.
            cwd: norte_vfs_local::vpath_to_native(app.focused().dir()).ok(),
        });
        return;
    };
    let program = opener.program().to_owned();
    // Probing the binary on the PATH (`program_available`) is disk I/O: NOT
    // done here (the async path) — the run loop runs it in spawn_blocking
    // alongside the launch (rule 2).
    // `%d` = the pane's directory (native); if for whatever reason it does
    // not convert, the file's own parent.
    let dir = norte_vfs_local::vpath_to_native(app.focused().dir()).unwrap_or_else(|_| {
        native
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default()
    });
    let detached = opener.detached();
    app.pending_open = Some(crate::app::PendingOpen {
        program,
        argv: opener.argv(&[&native], &dir),
        // A GRAPHICAL opener is not waited for: suspending the TUI for a
        // window that opens elsewhere would leave the reader staring at a
        // blank terminal until they close it. The rule states it, because
        // norte cannot know it on its own.
        detached,
        // The SAME `dir` that feeds `%d`: the child opens in the directory
        // the reader is looking at (#144).
        cwd: Some(dir),
    });
}

/// Probes the opener's binary on the PATH (#28) — disk I/O in
/// `spawn_blocking`, NEVER on the async executor (rule 2) — and, if it
/// exists, suspends the TUI and launches it. Returns the result's LOCALIZED
/// bar message (missing binary / launched / spawn failure).
pub async fn launch_opener(
    terminal: &mut tty::Tui,
    // Suspending the TUI hands over the WHOLE terminal: mouse capture is
    // released before and restored after ([`run_opener`]).
    capture: &mut mouse::Capture,
    pending: crate::app::PendingOpen,
) -> String {
    let crate::app::PendingOpen {
        program,
        argv,
        detached,
        cwd,
    } = pending;
    let prog = std::ffi::OsString::from(program.clone());
    let available =
        tokio::task::spawn_blocking(move || norte_frontend::openers::program_available(&prog))
            .await
            .unwrap_or(false);
    if !available {
        return ta("msg-open-missing-program", &[("program", &program)]);
    }
    if detached {
        return match spawn_detached(argv, cwd).await {
            Ok(()) => ta("msg-open-launched", &[("program", &program)]),
            Err(e) => ta(
                "msg-open-failed",
                &[("program", &program), ("error", &e.to_string())],
            ),
        };
    }
    // Invariant: `Opener::argv` always pushes the binary (`command[0]`), and
    // `parse` rejects an empty `command` — so `argv[0]` never panics, and an
    // empty argv here does NOT mean what it means in `run_suspended`
    // (showing the terminal and launching nothing), which would be a silent
    // F4.
    debug_assert!(
        !argv.is_empty(),
        "an opener's argv always carries the binary"
    );
    // The pane's directory as cwd (#144). A declared opener's `%d` already
    // travels inside the argv, so this is not for resolving paths: it is so
    // an editor saves, and a `:e` navigates, where the reader is looking —
    // what the three shell commands have done since #135 and this did not.
    //
    // Decided, not discovered: the price is that an opener writing a
    // RELATIVE path ends up writing it to the pane's directory. Accepted as
    // the lesser of the two surprises.
    match run_suspended(terminal, capture, argv, cwd, false).await {
        Ok(_) => ta("msg-open-launched", &[("program", &program)]),
        Err(e) => ta(
            "msg-open-failed",
            &[("program", &program), ("error", &e.to_string())],
        ),
    }
}

/// Launches the command WITHOUT touching the terminal: the desktop launcher
/// (`xdg-open`/`open`/`explorer.exe`) hands the file to its associated
/// program and exits, so suspending the TUI for it would be a free flicker.
/// stdio goes to `null` — a chatty launcher cannot write over the listing.
/// The child is waited for in the background (rule 2: in `spawn_blocking`),
/// which is what buries it: without that `wait` it would stay a zombie until
/// norte itself died.
async fn spawn_detached(
    argv: Vec<std::ffi::OsString>,
    cwd: Option<std::path::PathBuf>,
) -> std::io::Result<()> {
    debug_assert!(
        !argv.is_empty(),
        "the system launcher's argv always carries the binary"
    );
    let mut child = tokio::task::spawn_blocking(move || {
        // ABSOLUTE path, resolved with norte's cwd and never the pane's
        // (#302): see the long comment in `suspend::run_suspended`, the
        // other place a child is launched with `current_dir` set.
        //
        // `split_first` and not `argv[0]`: the `debug_assert` above does not
        // exist in release, and indexing an empty argv would be a panic
        // where there is already an `io::Result` to say it with.
        let (name, rest) = argv
            .split_first()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty argv"))?;
        let program = norte_frontend::openers::resolve_program(name).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "not found in PATH (relative PATH entries are ignored)",
            )
        })?;
        let mut cmd = std::process::Command::new(&program);
        // `current_dir` only if present: explicitly passing the inherited
        // cwd is not the same as not touching it, and here there is nothing
        // better to inherit (#144).
        if let Some(dir) = &cwd {
            cmd.current_dir(dir);
        }
        cmd.args(resto)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    })
    .await
    .map_err(std::io::Error::other)??;
    tokio::task::spawn_blocking(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The Enter of [`Modal::CommandLine`](crate::app::Modal::CommandLine)
/// (#135): leaves `$SHELL -c CMD` pending and closes the prompt.
///
/// The locality guard is REPEATED here, the one from the dispatch that
/// opened the prompt is not enough: between opening it and confirming it the
/// pane may have gone off to an `sftp://`, and running the line in norte's
/// own directory is not what was asked for — it would run it somewhere the
/// user is not looking at. The prompt closes in both cases: leaving it open
/// after a rejection would invite pressing Enter again against the same
/// rejection.
///
/// The line travels as ONE argument: the shell parses it (pipes, quotes,
/// globs), never norte. Splitting it here would be inventing a grammar that
/// does not match the one the receiving shell has.
pub fn submit_command_line(app: &mut App, cmd: &str) {
    match shell_cwd(app) {
        Ok(dir) => {
            let shell = norte_frontend::shell::login_shell();
            app.pending_shell = Some(crate::app::PendingShell {
                // The flag is decided by `norte-frontend` per shell (rule
                // 7): `cmd.exe` does not understand `-c`.
                argv: norte_frontend::shell::shell_command_argv(&shell, cmd),
                cwd: Some(dir),
                wait_for_key: true,
                check_regular: None,
            });
        }
        Err(msg) => app.message = Some(msg),
    }
    app.command_line_submitted();
}

/// Where the focused panel goes when its session closes.
///
/// The decision is made by `norte-frontend` and shared by both frontends:
/// the trail backward, skipping the machine that is closing, and home when
/// nothing is left. It used to live twice —here "home, always", and in the
/// window the trail— and the same key left the panel in two different
/// places.
///
/// Decided BEFORE releasing the session, as in the window: afterward, the
/// panel's path no longer serves as a key.
fn destino_tras_desconectar(app: &App, closed: &VPath) -> VPath {
    norte_frontend::nav::regreso_tras_desconectar(closed, app.history[app.focus()].trail())
        .unwrap_or_else(norte_frontend::shell::home_vpath)
}

/// Closes the focused panel's session and takes it out of there (#140).
///
/// Both halves matter and in this order: first the session is released
/// —while the panel's path is still the remote one, which is where the key
/// comes from— and then it navigates. The other way around would require
/// remembering where it came from.
///
/// On a LOCAL panel there is nothing to close and it says so: a key that
/// answers "done" over something that did nothing teaches you not to trust
/// the message.
pub async fn disconnect(app: &mut App, backend: &Backend) {
    let dir = app.focused().dir().clone();
    if norte_vfs_local::vpath_to_native(&dir).is_ok() {
        app.message = Some(t("msg-disconnect-local"));
        return;
    }
    let destination = destino_tras_desconectar(app, &dir);
    match backend.close_connection(&dir).await {
        Ok(closed) => {
            app.message = Some(t(if closed {
                "msg-disconnect-done"
            } else {
                "msg-disconnect-none"
            }));
            // The panel cannot stay looking at a connection that just
            // closed. The navigation is requested by the run loop on the
            // next turn, like any other.
            app.pending_disconnect_dest = Some(destination);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// El editor sobre la entrada bajo el cursor (#133).
///
/// An editor opens a SYSTEM FILE: over a remote pane there is none to give
/// it —downloading it, editing it and uploading it back is a different
/// feature, with its own conflict and its own reversal— so it says so and
/// opens nothing. Same guard, and same message, as the shell and the command
/// line.
///
/// Not over a directory either: whoever wants to enter has `nav.enter`, and
/// opening an editor on a folder is teaching the editor what it does not
/// know.
/// # Errors
///
/// The ALREADY LOCALIZED message for why nothing opens, which the caller
/// sends to the bar as is: no entry under the cursor, it is a directory, or
/// the pane is remote (and then it comes from [`shell_remote_message`], with
/// the location sanitized).
pub fn edit_under_cursor(app: &App) -> Result<EditLaunch, String> {
    let Some(entry) = app.focused().selected() else {
        return Err(t("msg-edit-nothing"));
    };
    if entry.kind == norte_proto::EntryKind::Dir {
        return Err(t("msg-edit-not-a-file"));
    }
    let Ok(native) = norte_vfs_local::vpath_to_native(&entry.path) else {
        return Err(shell_remote_message(app));
    };
    // The child's cwd is the directory being looked at, same as with the
    // shell: an editor's `:w other.txt` lands where the human is, not where
    // norte started.
    let dir = norte_vfs_local::vpath_to_native(app.focused().dir()).ok();
    // `[ui] editor` outranks `$VISUAL`/`$EDITOR`: whoever writes it in
    // norte's configuration is choosing NORTE's editor, and an inherited
    // environment cannot outrank what the user said here.
    if let Some(spec) = app.editor.as_ref().filter(|s| !s.command.is_empty()) {
        let base = dir.clone().unwrap_or_else(|| {
            native
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default()
        });
        return Ok(EditLaunch::Open(crate::app::PendingOpen {
            program: spec.command[0].clone(),
            argv: norte_frontend::openers::expand_argv(&spec.command, &[&native], &base),
            detached: spec.detached,
            cwd: Some(base),
        }));
    }
    Ok(EditLaunch::Shell(crate::app::PendingShell {
        argv: norte_frontend::shell::editor_argv(&native),
        cwd: dir.and_then(|d| norte_frontend::shell::child_cwd(&d)),
        // A full-screen editor sees itself out; waiting for a key afterward
        // would be one more step between saving and returning to the panels.
        wait_for_key: false,
        // F4 over a listing row does NOT check: here the name was chosen by
        // the human from what they saw, norte did not announce it by
        // creating it, and #303's path-based `undo` does not come into play.
        // The listing→key window exists all the same, and is stated in ADR
        // 0082 as a decision and not an oversight.
        check_regular: None,
    }))
}

/// Comparing TWO files (#312), delegating to the `[ui] diff` program.
///
/// The operand is decided by the shared crate ([`norte_frontend::diffpair`]):
/// two marked in the focused panel, or one here and another in the
/// DESTINATION panel. Anything else says so instead of guessing.
///
/// Both have to have a native path: an external `diff` cannot be given an
/// `sftp://`, and downloading them to compare is a different feature. Same
/// guard —and same message— as the editor and the shell.
///
/// # Errors
///
/// The ALREADY LOCALIZED message for why nothing gets compared: there are
/// not two, one is not a file, or one is not on this system.
pub fn compare_files(app: &App) -> Result<EditLaunch, String> {
    let marked: Vec<&norte_proto::Entry> = app.focused().marked_entries();
    let here = app.focused().selected();
    let there = app
        .target_index()
        .and_then(|i| app.panes.get(i))
        .and_then(crate::app::Pane::selected);
    let (a, b) =
        norte_frontend::diffpair::pair(&marked, here, there).map_err(|e| t(e.message_key()))?;
    let (Ok(na), Ok(nb)) = (
        norte_vfs_local::vpath_to_native(&a),
        norte_vfs_local::vpath_to_native(&b),
    ) else {
        return Err(shell_remote_message(app));
    };
    let dir = norte_vfs_local::vpath_to_native(app.focused().dir()).unwrap_or_else(|_| {
        na.parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default()
    });
    // `[ui] diff` outranks it, and goes through the openers' path because it
    // can be a window (`detached`). Without it, `diff -u` through the
    // shell's path, which is the only one that knows how to WAIT FOR A KEY:
    // `diff`'s output is a few lines that finish instantly, and without that
    // wait the reader sees a flicker and returns to the panels without
    // having read anything.
    if let Some(spec) = app.diff.as_ref().filter(|s| !s.command.is_empty()) {
        return Ok(EditLaunch::Open(crate::app::PendingOpen {
            program: spec.command[0].clone(),
            argv: norte_frontend::openers::expand_argv(&spec.command, &[&na, &nb], &dir),
            detached: spec.detached,
            cwd: Some(dir),
        }));
    }
    let template: Vec<String> = norte_frontend::diffpair::DEFAULT_ARGV
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    Ok(EditLaunch::Shell(crate::app::PendingShell {
        argv: norte_frontend::openers::expand_argv(&template, &[&na, &nb], &dir),
        cwd: norte_frontend::shell::child_cwd(&dir),
        wait_for_key: true,
        check_regular: None,
    }))
}

/// How the editor gets launched: through the shell's path (suspend the
/// terminal and wait) or through the openers' (which also knows how to NOT
/// wait).
///
/// Two paths and not one because they are two different contracts:
/// `$EDITOR`'s editor is a terminal program and is always waited for, while
/// `[ui] editor`'s can be a window —and then waiting for it would leave the
/// TUI blank until the reader closes it.
#[derive(Debug)]
pub enum EditLaunch {
    /// Suspends the terminal and waits (`$VISUAL`/`$EDITOR`).
    Shell(crate::app::PendingShell),
    /// The openers' path, which honors `detached` (`[ui] editor`).
    Open(crate::app::PendingOpen),
}

/// The editor over a file the daemon JUST created (#290).
///
/// [`edit_under_cursor`]'s sibling for the other half of `pane.edit-new`: the
/// path does not come from the cursor —the listing may not have refreshed
/// yet, and the cursor could be anywhere— but from what was sent to be
/// created.
///
/// The `cwd` comes from the created file's PARENT, not the focused pane:
/// between the submit and the outcome the reader may have pressed Tab or
/// navigated, and an editor's `:w other.txt` must land where the
/// just-created file is —which is what the gesture is about— and not
/// wherever focus ended up.
///
/// `Err` with the LOCALIZED message if that path has no native form: should
/// not happen (creating is refused over a remote pane, both when opening the
/// dialog and again on confirming it), but opening an editor on something
/// that cannot be named is not an alternative. The caller SAYS SO: half the
/// gesture lost silently is exactly the kind of thing this command came to
/// remove.
///
/// The suspension that comes out of here carries the created path in
/// [`PendingShell::check_regular`](crate::app::PendingShell::check_regular):
/// #303's check is done by the run loop right against the launch, with
/// [`motivo_para_no_lanzar`], and not here.
///
/// # Errors
///
/// The LOCALIZED message, ready for the bar: the path has no native form.
pub fn edit_created(path: &VPath) -> Result<crate::app::PendingShell, String> {
    let Ok(native) = norte_vfs_local::vpath_to_native(path) else {
        return Err(t("msg-edit-created-unnamable"));
    };
    let cwd = native.parent().and_then(norte_frontend::shell::child_cwd);
    Ok(crate::app::PendingShell {
        argv: norte_frontend::shell::editor_argv(&native),
        cwd,
        wait_for_key: false,
        check_regular: Some(path.clone()),
    })
}

/// Why this suspension must NOT launch, or `None` if it should go ahead
/// (#303).
///
/// Between creating the file and opening the editor there is a window, and
/// norte opens it itself: it ANNOUNCES the name by creating it —there is
/// nothing to guess— and whoever can write in that directory sees it appear
/// (inotify), unlinks it and leaves a symlink in its place. The human would
/// then type into a file nobody showed them, and the `Created` entry's
/// `undo` works by PATH and not by identity: undoing would send to the trash
/// whatever is there NOW.
///
/// So it asks before launching, and refuses anything that is not a regular
/// file. The question goes through the BACKEND (`fs.stat`, which is `lstat`
/// — it describes the link and never its target), not with a `std::fs` call
/// in the loop: disk I/O on the executor is rule 2, and the backend can also
/// be the daemon.
///
/// **Called by the RUN LOOP right before handing over the terminal**, and
/// that spot is half the fix: asking where opening the editor is decided
/// left room, between the answer and the `exec`, for a whole `refresh_panes`
/// —both panels, seconds on a remote pane— which is the window this exists
/// to narrow.
///
/// **Narrows, does not close**: between the `stat` and the `exec` a gap is
/// left. Closing it for real would require opening the file once and handing
/// the descriptor to the child, and no editor interface here accepts that.
///
/// The three forms of "this is no longer what was created" —a link, a
/// directory, nothing— give the SAME text: saying which one would be
/// confirming to whoever planted the link that their link is in place. A
/// failure while ASKING (the daemon handed off, a timeout) has its own: calling
/// it tampering would be a false accusation, and teaching people to ignore
/// that message is what disables it on the day it is true.
pub async fn motivo_para_no_lanzar(
    backend: &norte_core::backend::Backend,
    path: Option<VPath>,
) -> Option<String> {
    let path = path?;
    match backend.stat(&path).await {
        Ok(entry) if entry.kind == norte_proto::EntryKind::File => None,
        // `NotFound` goes with the other two and not with the failure:
        // "there is nothing there" is an ANSWER, and it is also the most
        // likely outcome of an attack —unlinking and not replacing. What
        // cannot sneak in here is a `ProviderUnavailable` or a timeout.
        Ok(_) | Err(norte_proto::Error::NotFound) => Some(t("msg-edit-created-changed")),
        Err(_) => Some(t("msg-edit-created-unchecked")),
    }
}

/// The working directory that belongs to a child launched from the focused
/// pane, or the LOCALIZED message for why there is none.
///
/// Two different refusals, and saying them separately matters: the pane is
/// not local (`sftp://`, a bucket, inside an archive — there is no directory
/// on this machine), or it is but its native form cannot be given to a child
/// (Windows: a path that only exists with the `\\?\` prefix, see
/// [`norte_frontend::shell::child_cwd`]). A single message for both would
/// send the user looking for the problem in the wrong place.
///
/// Race note (S4 review, MINOR-3): this resolves a PATH, not a descriptor, so
/// between the check and the child's `chdir` someone with write permission
/// on the parent can swap the directory for a symlink. Closing it for real
/// requires `openat`/`fchdir` and no other path in norte does it; it is
/// stated in the help topic's honest limits.
/// # Errors
///
/// The ALREADY LOCALIZED message for whichever refusal applies, which are
/// the two above and are stated separately on purpose.
pub fn shell_cwd(app: &App) -> Result<std::path::PathBuf, String> {
    let Ok(native) = norte_vfs_local::vpath_to_native(app.focused().dir()) else {
        return Err(shell_remote_message(app));
    };
    norte_frontend::shell::child_cwd(&native).ok_or_else(|| {
        let (text, hostile) = norte_frontend::path_display(app.focused().dir());
        ta(
            "msg-shell-cwd-unsupported",
            &[("path", &badged(&text, hostile))],
        )
    })
}

/// The path, already sanitized, with the hostile badge OUTSIDE the
/// translation (so no locale can drop it) and BOUNDED with a middle ellipsis.
///
/// The cap is not cosmetic (S4 review, M4): these messages interpolate the
/// path IN THE MIDDLE of the sentence, and both the status bar (a one-line
/// `Paragraph`) and the GUI's flash cut off on the right with no mark — so a
/// long path takes down with it exactly the part that explains why the key
/// did nothing, and the key looks broken.
fn badged(text: &str, hostile: bool) -> String {
    let short = norte_frontend::middle_ellipsis(text, SHELL_MSG_PATH_MAX);
    if hostile {
        format!("{} {short}", crate::ui::HOSTILE_BADGE)
    } else {
        short
    }
}

/// Character budget for the path inside a shell warning: leaves plenty of
/// room for the clause that follows on an 80-column terminal.
const SHELL_MSG_PATH_MAX: usize = 48;

/// The "no shell fits here" warning (#135), with the location SANITIZED.
///
/// Sanitizing is not optional: a directory's name can carry bidi,
/// invisibles or controls, and this line is painted in the status bar —
/// where `path_display` is exactly the gate everything else coming from
/// disk passes through.
#[must_use]
pub fn shell_remote_message(app: &App) -> String {
    let (text, hostile) = norte_frontend::path_display(app.focused().dir());
    ta("msg-shell-remote", &[("path", &badged(&text, hostile))])
}

/// Where a pane gesture wants to send a pane.
///
/// The `*_plan` functions return this instead of navigating so the DECISION —
/// which pane travels, and where — is testable without a `Backend` or an
/// event stream. `None` from them means there is nothing to do, and WHY is
/// deliberately not this type's business: the dispatch arm decides whether
/// the reason deserves a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneMove {
    /// The pane that will navigate.
    pub pane: usize,
    /// Where it will go.
    pub dir: VPath,
}

/// `pane.mirror`: the UNFOCUSED pane goes where the focused one is, and the
/// focus stays put — the fastest way to line up a copy, because the
/// destination of `pane.copy` is whatever the other pane holds.
///
/// `None` when the focused pane is a virtual search listing (a list of hits
/// is not a location, so there is no origin to send) or when both panes are
/// already there — a redundant `cd` would re-list the other pane and slide
/// its listing out from under the reader's cursor for nothing.
///
/// "Already there" reads `virtual_search` as well as the directory: a results
/// pane's `dir()` is the ROOT the search walked, which is usually the very
/// directory the other pane is sitting in, and it is NOT what the reader is
/// looking at. Comparing the two alone refused the gesture in silence
/// precisely when it had the most to do — the real cd is what takes the pane
/// out of search mode.
#[must_use]
pub fn mirror_plan(app: &App) -> Option<PaneMove> {
    let from = app.focus();
    // Who "the other" is, is said by the shared ROLE, never `focus ^ 1`:
    // that is a two-panel count, and there have been three or four ever
    // since splitting became possible. With three, `^ 1` from the last one
    // clamps to the panel ITSELF (`slot_of` clamps), so the gesture used to
    // compare itself with itself and do nothing, silently — "pull only works
    // left to right".
    let to = app.target_index()?;
    if app.panes[from].virtual_search {
        return None;
    }
    let dir = app.panes[from].dir().clone();
    (app.panes[to].dir() != &dir || app.panes[to].virtual_search)
        .then_some(PaneMove { pane: to, dir })
}

/// What `nav.enter` does with whatever is under the cursor.
///
/// An enum and not three `if`s in the dispatcher because the DECISION is
/// tested without a backend and without a terminal, which is this whole
/// module's split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnterAction {
    /// A directory (or an archive, which is entered the same way): navigate.
    Cd(VPath),
    /// A file on THIS disk: hand it to its associated program, the same
    /// path as `pane.open` ([`resolve_opener`]).
    OpenExternal,
    /// A file not on this disk: the INTERNAL viewer, which is the only thing
    /// that can be done with it here — `xdg-open` cannot be given an
    /// `sftp://`.
    View(VPath),
    /// Nothing under the cursor, or something that is neither a file nor a
    /// directory.
    Nothing,
    /// The `..` row: GO UP to this directory, leaving the cursor on the one
    /// being left. It is a `cd` with memory, and that is why it is not
    /// [`Self::Cd`].
    Up(VPath),
}

/// What `nav.enter` does RIGHT NOW: navigate, open externally, or view.
///
/// Over a file it used to do NOTHING, silently: `trail::nav_enter_target`
/// only answers for directories, symlinks and archives, and the dispatcher's
/// arm with `None` stayed still. An orthodox file manager opens the file
/// with its associated program —Krusader, Total Commander and Norton do
/// that— and the internal viewer still has its own key (`pane.view`).
#[must_use]
pub fn enter_action(app: &App) -> EnterAction {
    // GOING UP is said apart from entering, even though both end in a `cd`:
    // on going up the cursor has to land on the directory being left, and
    // only whoever knows they are going up knows that. Answering
    // `Cd(parent)` as for any other folder, `nav.enter` over `..` used to
    // leave the cursor on the parent's first row while the dedicated
    // go-up key got it right: the same key with two results depending on
    // which door it was entered through.
    if app.focused().cursor_is_parent_row()
        && let Some(parent) = app.focused().parent_target().cloned()
    {
        return EnterAction::Up(parent);
    }
    if let Some(dir) = crate::trail::nav_enter_target(app) {
        return EnterAction::Cd(dir);
    }
    let Some(path) = app
        .focused()
        .selected()
        .filter(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink))
        .map(|e| e.path.clone())
    else {
        return EnterAction::Nothing;
    };
    if norte_vfs_local::vpath_to_native(&path).is_ok() {
        EnterAction::OpenExternal
    } else {
        EnterAction::View(path)
    }
}

/// `pane.mirror-target`: like [`mirror_plan`], but what travels is the
/// CURSOR's target — the folder under the cursor when it is one, and this
/// pane's own location otherwise.
///
/// It is Krusader's `Ctrl+←`/`Ctrl+→`, and the reason it is a separate verb
/// rather than a smarter `pane.mirror` is that `pane.mirror` is bound in four
/// presets as "send this location": teaching it to prefer the cursor would
/// change, in silence, what a key those readers already use does.
///
/// Which directory that is gets decided ONCE, in
/// [`norte_frontend::PaneState::target_dir`], so the window cannot answer it
/// differently (ADR 0077). The two refusals are [`mirror_plan`]'s, unchanged:
/// a virtual search listing has no location to send, and a destination that is
/// already there is left alone.
#[must_use]
pub fn mirror_target_plan(app: &App) -> Option<PaneMove> {
    let from = app.focus();
    let to = app.target_index()?;
    if app.panes[from].virtual_search {
        return None;
    }
    let dir = app.panes[from].target_dir().clone();
    (app.panes[to].dir() != &dir || app.panes[to].virtual_search)
        .then_some(PaneMove { pane: to, dir })
}

/// `pane.pull`: the FOCUSED pane goes where the other one is — the same
/// gesture as [`mirror_plan`] the other way round, with the same two reasons
/// to decline and the same reading of a virtual DESTINATION.
#[must_use]
pub fn pull_plan(app: &App) -> Option<PaneMove> {
    let to = app.focus();
    let from = app.target_index()?;
    if app.panes[from].virtual_search {
        return None;
    }
    let dir = app.panes[from].dir().clone();
    (app.panes[to].dir() != &dir || app.panes[to].virtual_search)
        .then_some(PaneMove { pane: to, dir })
}

/// Carries out what a `*_plan` decided: navigate, or explain the refusal.
///
/// `pane.mirror` and `pane.pull` differ ONLY in which pane travels and which
/// one the location is read FROM, and both of those are already settled by
/// the time the plan exists — so they share this body rather than two arms
/// that must be kept in step by hand.
///
/// `origin` is the pane the location comes from. A virtual search listing
/// there is the one refusal that deserves a message: the reader asked for
/// something that cannot be done. Both panes already being in the same place
/// stays SILENT — nothing was asked for that failed.
pub async fn run_pane_gesture(
    app: &mut App,
    backend: &Backend,
    events: &mut crate::console::Console<'_>,
    plan: Option<PaneMove>,
    origin: usize,
) -> Cd {
    let Some(m) = plan else {
        if app.panes[origin].virtual_search {
            app.message = Some(t("msg-pane-not-a-location"));
        } else if app.target_index().is_none() {
            // With three or more panels and none designated, the gesture
            // cannot guess which one is "the other" (ADR 0058 D7). And
            // staying silent is a dead key: it looks exactly the same as
            // from the last panel.
            app.message = Some(t("msg-no-target-designated"));
        }
        return Cd::Cancelled;
    };
    // ORDINARY navigation, but through `cd_in` and not the `cd` wrapper: the
    // plan says which pane travels, and in the mirror it is NOT the focused
    // one.
    cd_in(app, backend, events, m.pane, m.dir, Trail::Record).await
}

/// A fingerprint of WHICH surface owns the keyboard, sampled before and after
/// each turn of a repeated dispatch.
///
/// A count repeats the dispatch, and a dispatched command can put a modal, a
/// viewer or one of seven overlays in front of the panes. Everything left of
/// the count would then fire BEHIND it, against a pane the reader is no
/// longer looking at and cannot see change. The run loop routes a key event
/// by testing exactly these fields, so comparing them is the same question
/// the router asks.
///
/// It is COMPARED, never merely tested: `5` then `viewer.down` starts with
/// the viewer already open, and a guard that broke on "a viewer is open"
/// would stop that count after one row. Only a CHANGE means the dispatch
/// moved the keyboard.
#[must_use]
pub fn keyboard_owner(app: &App) -> u16 {
    let bits = [
        app.menu.is_some(),
        app.modal.is_some(),
        app.viewer.is_some(),
        app.help.is_some(),
        app.theme_picker.is_some(),
        app.columns_picker.is_some(),
        app.extensions.is_some(),
        app.nav_popup.is_some(),
        app.search_dialog.is_some(),
        // The diff pane (`Shift+F2`): a full keyboard owner while it is up,
        // like the viewer and unlike the live-search pane. Its rows are not
        // entries, so nothing behind it could act on what the cursor is on.
        app.compare.is_some(),
        app.palette.is_some(),
        app.settings.is_some(),
        // K3c: the shortcut editor. Like the other overlays, and unlike the
        // which-key panel below: it keeps ALL the keys while open, so a
        // dispatch that opened it behind a count would leave the rest of the
        // repetitions falling into it.
        app.shortcuts.is_some(),
        app.focused().quick_visible().is_some(),
        // K3a: the which-key panel takes no keys — the pane resolver keeps
        // them while it is up — and, unlike its neighbours, this bit is
        // CONSTANT across the comparison by construction: `Resolution::Run`
        // clears the panel before `owner_before` is sampled, and nothing
        // reachable from `dispatch` can open one (the only two writers are
        // `App::show_pending`/`clear_pending`, both of them on the key path).
        // So it can never break a `5j`, and it can never save one either. It
        // is here as a DEFENSIVE entry: the day a command opens a which-key of
        // its own (a "show me everything" key is the obvious candidate), the
        // count must notice, and the alternative is remembering to add it
        // then.
        app.which_key.is_some(),
    ];
    bits.iter()
        .enumerate()
        .fold(0u16, |acc, (i, &on)| acc | (u16::from(on) << i))
}

#[cfg(test)]
mod pane_gestures_tests {
    use super::{App, Cd, Trail, keyboard_owner, mirror_plan, mirror_target_plan, pull_plan};
    use crate::app::{Modal, Palette, Pane, TrailStep};
    use crate::jobs::on_search_dialog_key;
    use crate::keymap::Command;
    use crate::navigate::{record_step, settle_suspended_trail};
    use crate::trail::{
        Rewind, back_target, forward_target, nav_stalled, rewind_for, rewind_trail,
    };
    use crate::viewer::Viewer;
    use norte_proto::{Error, VPath};

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("test wire")
    }

    /// `App` with each pane on its dir. The panes are built WHOLE
    /// (`Pane::new`) instead of moving an existing pane: same shape the
    /// other test modules in this file use, and no new `#[cfg(test)]`
    /// setter is needed.
    fn app_en(left: &str, right: &str) -> App {
        App::new(
            Pane::new(vp(left), Vec::new()),
            Pane::new(vp(right), Vec::new()),
        )
    }

    /// Mirror: the UNFOCUSED pane goes where the focused one is, and focus
    /// does not move.
    #[test]
    fn mirror_sends_the_other_pane_and_does_not_move_focus() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.set_focus(0);
        let plan = mirror_plan(&app).expect("with two normal panes there is a plan");
        assert_eq!(plan.pane, 1, "the OTHER pane travels");
        assert_eq!(plan.dir, vp("mem:///a"), "to where the focused one is");
        assert_eq!(app.focus(), 0, "focus has not moved");
    }

    /// The mirror looks at FOCUS, not pane 0: with focus on the right, the
    /// left one travels. (Control mutation: fixing `from = 0` breaks here.)
    #[test]
    fn mirror_with_focus_on_the_right_sends_the_left_one() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.set_focus(1);
        let plan = mirror_plan(&app).expect("plan");
        assert_eq!(plan.pane, 0);
        assert_eq!(plan.dir, vp("mem:///b"));
    }

    /// `nav.enter` over a FILE opens, instead of staying still: with a
    /// native path, the associated program; without one —a remote panel—,
    /// the internal viewer, which is the only thing that can be done there.
    #[test]
    fn enter_over_a_file_opens_instead_of_doing_nothing() {
        use super::{EnterAction, enter_action};
        use norte_proto::{Entry, EntryKind};
        let entrada = |wire: &str, kind| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: vp(wire),
            kind,
            size: None,
            mtime_ms: None,
        };

        // A LOCAL file: to its associated program.
        let mut app = App::new(
            Pane::new(
                vp("file:///casa"),
                vec![
                    entrada("file:///casa/dentro", EntryKind::Dir),
                    entrada("file:///casa/bin.dat", EntryKind::File),
                ],
            ),
            Pane::new(vp("file:///otro"), Vec::new()),
        );
        app.set_focus(0);
        assert_eq!(
            enter_action(&app),
            EnterAction::Cd(vp("file:///casa/dentro")),
            "over a directory, still navigates"
        );
        app.panes[0].set_cursor(1);
        assert_eq!(enter_action(&app), EnterAction::OpenExternal);

        // The SAME file on a pane not on this disk: viewer.
        let mut remoto = App::new(
            Pane::new(
                vp("mem:///casa"),
                vec![entrada("mem:///casa/bin.dat", EntryKind::File)],
            ),
            Pane::new(vp("mem:///otro"), Vec::new()),
        );
        remoto.set_focus(0);
        assert_eq!(
            enter_action(&remoto),
            EnterAction::View(vp("mem:///casa/bin.dat")),
            "with no native path there is no associated program to hand it to"
        );

        // And with nothing under the cursor, nothing.
        let vacio = App::new(
            Pane::new(vp("file:///casa"), Vec::new()),
            Pane::new(vp("file:///otro"), Vec::new()),
        );
        assert_eq!(enter_action(&vacio), EnterAction::Nothing);
    }

    /// `nav.enter` over `..` is GOING UP, and going up is said with its own
    /// verb.
    ///
    /// It used to answer `Cd(parent)`, which is the same as entering any
    /// other folder, and that is why the cursor landed on the parent's
    /// first row instead of on the directory being left — while the
    /// dedicated go-up key got it right. The same key, two results,
    /// depending on which of the two doors it was entered through.
    #[test]
    fn enter_over_the_up_row_goes_up_not_a_plain_cd() {
        use super::{EnterAction, enter_action};
        use norte_proto::{Entry, EntryKind};
        let entrada = |wire: &str, kind| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: vp(wire),
            kind,
            size: None,
            mtime_ms: None,
        };
        let mut izq = Pane::new(
            vp("file:///casa/dentro"),
            vec![entrada("file:///casa/dentro/a.txt", EntryKind::File)],
        );
        izq.set_parent_row(true);
        let mut app = App::new(izq, Pane::new(vp("file:///otro"), Vec::new()));
        app.set_focus(0);
        assert!(app.panes[0].is_parent_row(app.panes[0].cursor()));
        assert_eq!(
            enter_action(&app),
            EnterAction::Up(vp("file:///casa")),
            "over `..`, GO UP — and whoever executes it leaves the cursor on `dentro`"
        );

        // Over a real folder it is still a normal `cd`: entering does not
        // remember where you came from, because you did not come from
        // inside.
        app.panes[0].set_cursor(1);
        assert_eq!(enter_action(&app), EnterAction::OpenExternal);
    }

    /// The TARGET's mirror sends the folder under the cursor, and over
    /// anything else —a file, the `..` row— it sends this location, which is
    /// what the usual mirror does.
    #[test]
    fn target_mirror_sends_the_folder_under_the_cursor() {
        use norte_proto::{Entry, EntryKind};
        let entrada = |wire: &str, kind| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: vp(wire),
            kind,
            size: None,
            mtime_ms: None,
        };
        let mut app = App::new(
            Pane::new(
                vp("mem:///a"),
                vec![
                    entrada("mem:///a/dentro", EntryKind::Dir),
                    entrada("mem:///a/f.txt", EntryKind::File),
                ],
            ),
            Pane::new(vp("mem:///b"), Vec::new()),
        );
        app.set_focus(0);

        let plan = mirror_target_plan(&app).expect("plan");
        assert_eq!(plan.pane, 1, "the OTHER pane travels, like the mirror");
        assert_eq!(plan.dir, vp("mem:///a/dentro"), "the cursor's folder");

        app.panes[0].set_cursor(1);
        let plan = mirror_target_plan(&app).expect("plan");
        assert_eq!(
            plan.dir,
            vp("mem:///a"),
            "over a file, this location — like `pane.mirror`"
        );

        // And `pane.mirror` does NOT change: it still sends the location
        // even with the cursor over a folder.
        app.panes[0].set_cursor(0);
        assert_eq!(mirror_plan(&app).expect("plan").dir, vp("mem:///a"));
    }

    /// With THREE panels, "the other" is not guessed: the gesture asks you
    /// to designate a target, and with one designated it goes there.
    ///
    /// It used to compute `focus ^ 1`, which is a two-panel count: from the
    /// third panel it clamped to ITSELF —`slot_of` clamps— so the plan
    /// compared itself with itself and came out `None`. On screen: the key
    /// did nothing from the last panel, which is "pull only works left to
    /// right".
    #[test]
    fn with_three_panels_the_gesture_asks_for_a_designated_target() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.set_focus(1);
        app.layout_split(norte_frontend::layout::Dir::Horizontal);
        assert_eq!(app.panes.len(), 3, "three panels");

        assert!(
            mirror_plan(&app).is_none(),
            "with no designated target it is not guessed"
        );
        assert!(pull_plan(&app).is_none());

        // With the target designated, the gesture has somewhere to go again
        // — and it is not the panel itself. `layout_set_target` CYCLES
        // through the ones without focus, so it cycles to `mem:///a`'s,
        // which is the only one whose directory differs from the one that
        // travels (the other two came from splitting).
        for _ in 0..3 {
            app.layout_set_target();
            if app.target_index() == Some(0) {
                break;
            }
        }
        assert_eq!(app.target_index(), Some(0), "the designated target is 0");
        let plan = mirror_plan(&app).expect("with a designated target there is a plan");
        assert_eq!(
            plan.pane, 0,
            "the DESIGNATED one travels, not the focused one"
        );
        assert_eq!(plan.dir, vp("mem:///b"), "the location comes from focus");
    }

    /// Pull: the FOCUSED pane goes where the other one is.
    #[test]
    fn pull_moves_the_focused_pane() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.set_focus(0);
        let plan = pull_plan(&app).expect("plan");
        assert_eq!(plan.pane, 0);
        assert_eq!(plan.dir, vp("mem:///b"));
    }

    /// Both already in the same place: a SILENT no-op, not a redundant cd
    /// that re-sorts the other pane's listing under the reader's cursor.
    #[test]
    fn same_dir_has_nothing_to_do() {
        let mut app = app_en("mem:///a", "mem:///a");
        app.set_focus(0);
        assert!(mirror_plan(&app).is_none());
        assert!(pull_plan(&app).is_none());
    }

    /// From a VIRTUAL results pane there is no location to send nor to pull
    /// from: `dir()` there is the walk's ROOT, not what the reader sees.
    #[test]
    fn a_virtual_pane_is_not_a_location() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.panes[0].virtual_search = true;
        app.set_focus(0);
        assert!(mirror_plan(&app).is_none(), "there is no origin to send");
        app.set_focus(1);
        assert!(pull_plan(&app).is_none(), "nor to pull from");
    }

    /// The virtual pane only vetoes when it is the ORIGIN. Sending it a
    /// location works fine: the real cd takes it out of search mode, which
    /// is exactly what the reader asked for.
    #[test]
    fn a_virtual_pane_can_be_a_destination() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.panes[1].virtual_search = true;
        app.set_focus(0);
        let plan = mirror_plan(&app).expect("a virtual destination does not veto");
        assert_eq!(plan.pane, 1);
        assert_eq!(plan.dir, vp("mem:///a"));
    }

    /// And the "both already in the same place" shortcut cannot stay silent
    /// in front of a VIRTUAL destination: the root the search walked is
    /// usually exactly the other pane's dir, and there `dir()` is not what
    /// the reader is looking at. With the shortcut comparing only dirs,
    /// mirroring onto a results pane rooted right there used to do NOTHING
    /// — it neither took the pane out of search mode, nor said why.
    #[test]
    fn a_virtual_destination_in_the_same_dir_still_has_something_to_do() {
        let mut app = app_en("mem:///a", "mem:///a");
        app.panes[1].virtual_search = true;

        app.set_focus(0);
        let plan = mirror_plan(&app).expect("the virtual destination is not \"already there\"");
        assert_eq!(plan.pane, 1, "the results pane travels");
        assert_eq!(
            plan.dir,
            vp("mem:///a"),
            "and the real cd takes it out of the mode"
        );

        // And the symmetric gesture: with focus ON the virtual pane,
        // `pane.pull` brings it to where the other one is — the same dir, a
        // real listing.
        app.set_focus(1);
        let plan = pull_plan(&app).expect("pulling into a virtual pane is not a no-op either");
        assert_eq!(plan.pane, 1);
        assert_eq!(plan.dir, vp("mem:///a"));
    }

    // --- nav.back / nav.forward ---

    /// Moves pane 0 to `dir` without going through a `cd` (which needs a
    /// backend): a NEW pane over that dir, the same shape the other test
    /// modules in this file use.
    fn poner_en(app: &mut App, dir: &str) {
        app.panes[0] = Pane::new(vp(dir), Vec::new());
    }

    /// The trail is walked for real: A→B→C, two steps back reaches A. The
    /// A→B→A→B oscillation that walking the MRU would give is what this
    /// test rejects.
    #[test]
    fn back_walks_the_trail_and_forward_undoes_it() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));

        let where_to = back_target(&mut app).expect("there is a trail");
        assert_eq!(where_to, vp("mem:///b"));
        poner_en(&mut app, "mem:///b");
        assert_eq!(back_target(&mut app), Some(vp("mem:///a")));
        poner_en(&mut app, "mem:///a");
        assert_eq!(back_target(&mut app), None, "the trail ran out");

        assert_eq!(forward_target(&mut app), Some(vp("mem:///b")));
    }

    /// With an empty trail the key SAYS SO: a key that stays silent is
    /// indistinguishable from a broken one. (The message is set by
    /// `walk_trail`, which needs a backend; here the half that decides there
    /// is NO destination is pinned.)
    #[test]
    fn back_with_no_trail_gives_no_target() {
        let mut app = app_en("mem:///a", "mem:///otro");
        app.set_focus(0);
        assert_eq!(back_target(&mut app), None);
        assert_eq!(forward_target(&mut app), None);
    }

    /// The property that prevents the loop: a `Replay` does not record.
    /// Checked against the trail, which is where the decision lives.
    #[test]
    fn the_trail_does_not_feed_on_itself() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///b"));
        let before = app.history[0].back_len();
        let _ = back_target(&mut app);
        assert_eq!(
            app.history[0].back_len(),
            before - 1,
            "a step back CONSUMES trail; never produces it"
        );
    }

    /// A step back from a RESULTS pane does happen (it is the key that most
    /// resembles "get me out of here"), and what it leaves on the forward
    /// branch is the REAL directory the search started from — not a list of
    /// hits, which is not a place. A virtual pane's `dir()` IS that
    /// directory.
    #[test]
    fn back_from_a_results_pane_leaves_the_real_dir_on_the_branch() {
        let mut app = app_en("mem:///b", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a")); // the reader reached B from A
        app.panes[0].begin_search(vp("mem:///b")); // Alt+F7 on B
        assert!(app.panes[0].virtual_search, "results pane");

        assert_eq!(
            back_target(&mut app),
            Some(vp("mem:///a")),
            "the step leaves the search for where the reader was before"
        );
        poner_en(&mut app, "mem:///a"); // the real cd lands (and harvests the run)
        assert_eq!(
            forward_target(&mut app),
            Some(vp("mem:///b")),
            "and forward returns to the dir the search started from, as a listing"
        );
    }

    /// What makes the test above honest, poked at the seam that could break
    /// it: the search's root IS the `dir()` of the pane that launches it. If
    /// the dialog let you type another root, `back_target` would start
    /// pointing at a place the reader has never been and would have to read
    /// `SearchRun::prev_dir` instead.
    #[test]
    fn the_search_root_is_the_dir_of_the_pane_that_launches_it() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut app = app_en("mem:///raiz", "mem:///otro");
        app.set_focus(0);
        let mut dialog = crate::app::SearchDialog::new();
        dialog.push_char('x'); // with no criteria, Enter does not launch
        app.search_dialog = Some(dialog);

        let params = on_search_dialog_key(&mut app, KeyModifiers::NONE, KeyCode::Enter)
            .expect("Enter with criteria launches the search");
        assert_eq!(
            params.root,
            *app.panes[0].dir(),
            "the walk's root is the focused pane's dir"
        );
    }

    /// The trail is PER PANE: `back_target` follows focus, not pane 0.
    #[test]
    fn the_trail_belongs_to_the_focused_pane() {
        let mut app = app_en("mem:///izq", "mem:///der");
        app.history[0].record(vp("mem:///solo-izq"));
        app.set_focus(1);
        assert_eq!(back_target(&mut app), None, "pane 1 has no trail");
        app.set_focus(0);
        assert_eq!(back_target(&mut app), Some(vp("mem:///solo-izq")));
    }

    // --- The trail's POLICY (`rewind_for`) and its effect (`rewind_trail`).
    // The tests call the SAME functions `walk_trail` calls: before, they
    // reimplemented the effect by hand (`untake_step` + `history.remove`),
    // so `walk_trail` could stop rewinding and they would stay green.

    /// A `NotFound` does not just rewind: it RETIRES the destination from
    /// the whole history. It is the only one of the three decisions that
    /// touches the MRU.
    #[test]
    fn the_policy_for_a_destination_that_does_not_exist_is_rewind_and_retire() {
        assert_eq!(
            rewind_for(&Cd::Failed(Error::NotFound)),
            Rewind::StepAndRetire
        );
    }

    /// Any OTHER failure rewinds but KEEPS the destination: a downed host
    /// or a directory you cannot read are still places, and can answer the
    /// next attempt.
    #[test]
    fn the_policy_for_another_failure_is_rewind_keeping_the_destination() {
        assert_eq!(
            rewind_for(&Cd::Failed(Error::PermissionDenied)),
            Rewind::Step
        );
    }

    /// An ABANDONED cd (Esc during a slow listing, or the event stream
    /// dying) rewinds THE SAME as a failure: nobody resumes it and the pane
    /// did not move. Without this the trail believes the reader left a
    /// directory still on screen.
    #[test]
    fn the_policy_for_an_abandoned_cd_is_to_rewind() {
        assert_eq!(rewind_for(&Cd::Cancelled), Rewind::Step);
    }

    /// And the ONLY one that does not touch the trail: the TOFU is going to
    /// resume THIS SAME navigation (the modal loads the pane and the trail
    /// mode), so rewinding would count the retry that does work twice.
    #[test]
    fn the_policy_for_a_suspended_cd_is_to_leave_the_trail_alone() {
        assert_eq!(rewind_for(&Cd::Suspended), Rewind::No);
    }

    /// A cd that DID move the pane must not rewind anything, nor must
    /// outcomes arriving from other paths (refresh, swap) that never come
    /// from a trail step.
    #[test]
    fn a_cd_that_lands_does_not_rewind() {
        assert_eq!(rewind_for(&Cd::Replaced(0)), Rewind::No);
        assert_eq!(rewind_for(&Cd::Refreshed([true, true])), Rewind::No);
        assert_eq!(rewind_for(&Cd::Swapped), Rewind::No);
    }

    // --- The COUNT's brake on the trail (`nav_stalled`, ADR 0044).

    /// A trail step that does not land stops the count DEAD. The step gets
    /// rewound (the tests above pin that), so the next turn would request
    /// the SAME listing: `20` + `nav.back` against a downed host would be
    /// twenty identical remote calls. And `Esc` during a listing IS
    /// `Cd::Cancelled`, so without this brake the very key the reader uses
    /// to try to stop it would feed the next retry.
    #[test]
    fn a_trail_step_that_does_not_land_stops_the_count() {
        for (label, outcome) in [
            ("abandoned", Cd::Cancelled),
            ("does not exist", Cd::Failed(Error::NotFound)),
            ("no permission", Cd::Failed(Error::PermissionDenied)),
        ] {
            assert!(
                nav_stalled(Command::NavBack, &outcome),
                "back {label} must stop"
            );
            assert!(
                nav_stalled(Command::NavForward, &outcome),
                "forward {label} must stop"
            );
        }
    }

    /// A step that DOES land lets the count go on: `3` + `nav.back` is three
    /// steps when all three exist.
    #[test]
    fn a_trail_step_that_lands_lets_the_count_go_on() {
        assert!(!nav_stalled(Command::NavBack, &Cd::Replaced(0)));
        assert!(!nav_stalled(
            Command::NavForward,
            &Cd::Refreshed([true, true])
        ));
        // Suspended is the TOFU: stopped by the keyboard-owner change (the
        // modal), not this brake — and rewinding here would count the retry
        // twice.
        assert!(!nav_stalled(Command::NavBack, &Cd::Suspended));
    }

    /// And the brake is ONLY for the trail's two commands. `Cd::Cancelled`
    /// is `dispatch`'s default outcome, so every command that is not a cd
    /// returns it: asking about it in general would stop `5j` on the first
    /// row.
    #[test]
    fn the_trail_brake_does_not_reach_a_non_navigating_command() {
        assert!(!nav_stalled(Command::CursorDown, &Cd::Cancelled));
        assert!(!nav_stalled(Command::ViewerDown, &Cd::Cancelled));
    }

    /// The count stops when dispatch MOVES the keyboard to another surface.
    /// A before is compared with an after precisely because the viewer can
    /// be open FROM THE START (`5` + `viewer.down`): a guard that asked "is
    /// there a viewer?" would kill that count on the first turn.
    #[test]
    fn the_keyboard_owner_changes_when_a_command_opens_something() {
        let mut app = app_en("mem:///izq", "mem:///der");
        let panels_only = keyboard_owner(&app);
        app.modal = Some(Modal::ConfirmQuit);
        assert_ne!(keyboard_owner(&app), panels_only, "a modal steps in front");
        app.modal = None;
        assert_eq!(keyboard_owner(&app), panels_only, "and closing it returns");
        app.palette = Some(Palette::new(Vec::new()));
        assert_ne!(keyboard_owner(&app), panels_only, "the palette too");
        app.palette = None;
        // And the case that forces COMPARING instead of asking: with the
        // viewer open from the start (`5` + `viewer.down`), the owner has
        // not changed between turns and the count has to go on.
        app.viewer = Some(Viewer::new(
            vp("mem:///izq/x.txt"),
            b"hola\n".to_vec(),
            false,
        ));
        let with_viewer = keyboard_owner(&app);
        assert_ne!(with_viewer, panels_only, "the viewer is another owner");
        assert_eq!(
            keyboard_owner(&app),
            with_viewer,
            "but does NOT CHANGE between two turns of the count"
        );
    }

    /// A step back that LANDS on a failed cd must not leave the trail
    /// counting a move that never happened: the reader is still where they
    /// were. It rewinds WHOLE — the destination goes back to the trail and
    /// "forward" is not left with a ghost that would return the reader to a
    /// place they never left.
    #[test]
    fn a_failed_back_step_rewinds_completely() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("there is a trail");
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 1)
        );

        let outcome = Cd::Failed(Error::PermissionDenied);
        rewind_trail(&mut app, 0, TrailStep::Back, &dir, rewind_for(&outcome));

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (2, 0),
            "the trail is left exactly as it was"
        );
        assert_eq!(
            back_target(&mut app),
            Some(dir),
            "and the same destination is still available to retry"
        );
    }

    /// Symmetric: a failed FORWARD step rewinds the same way.
    #[test]
    fn a_failed_forward_step_rewinds_completely() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///b"));
        let _ = back_target(&mut app); // trail: back=[], fwd=[c]
        poner_en(&mut app, "mem:///b");
        let dir = forward_target(&mut app).expect("there is a forward branch");
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 0)
        );

        let outcome = Cd::Failed(Error::PermissionDenied);
        rewind_trail(&mut app, 0, TrailStep::Forward, &dir, rewind_for(&outcome));

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (0, 1)
        );
        assert_eq!(forward_target(&mut app), Some(dir));
    }

    /// This review's case: `Esc` during the step's listing. The reader is
    /// still on C, so the trail has to stay as it was. With the step
    /// counted as good, the next "forward" would cd into the directory
    /// already on screen (a key that does nothing visible) and `back`
    /// would inherit a ghost that eats the next `nav.back`.
    #[test]
    fn a_step_abandoned_with_esc_leaves_no_ghost_in_the_trail() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("there is a trail");

        // The pane did NOT move: still on C (`poner_en` is not called).
        rewind_trail(
            &mut app,
            0,
            TrailStep::Back,
            &dir,
            rewind_for(&Cd::Cancelled),
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (2, 0),
            "the trail stays as it was: nobody left C"
        );
        assert_eq!(
            back_target(&mut app),
            Some(vp("mem:///b")),
            "and the same destination is still there to retry"
        );
    }

    /// The TOFU is the exception: the modal RESUMES this same navigation, so
    /// the step already taken stays taken — rewinding it would count the
    /// successful retry twice.
    #[test]
    fn a_step_suspended_by_the_tofu_keeps_the_step() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("there is a trail");

        rewind_trail(
            &mut app,
            0,
            TrailStep::Back,
            &dir,
            rewind_for(&Cd::Suspended),
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 1),
            "the step is still taken: the retry will finish it"
        );
    }

    // --- The step that outlives whoever started it: TOFU (`Modal::TrustHostKey`).
    // `walk_trail` returns `Suspended` WITHOUT rewinding because the retry
    // was going to finish the step; these three poke at who really finishes
    // it, with the same `rewind_for`/`rewind_trail` pair that runs in
    // production.

    /// The reader walked A→B→C→D, took ONE step back (already complete: on
    /// C, with D on the forward branch) and the NEXT step back —toward B—
    /// gets suspended in the TOFU modal. Returns the app and the suspended
    /// step's destination.
    ///
    /// The previous forward branch is not decoration: it is what tells a
    /// rewind of two apart. With `fwd` empty the second rewind would be a
    /// no-op and no test would see it; with the reader's branch underneath,
    /// it eats it.
    fn app_con_paso_suspendido() -> (App, VPath) {
        let mut app = app_en("mem:///d", "mem:///otro");
        app.set_focus(0);
        for dir in ["mem:///a", "mem:///b", "mem:///c"] {
            app.history[0].record(vp(dir));
        }
        // A previous `nav.back` ALREADY completed: trail back=[a,b], fwd=[d].
        let c = back_target(&mut app).expect("there is a trail");
        assert_eq!(c, vp("mem:///c"));
        poner_en(&mut app, "mem:///c");
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (2, 1)
        );

        // And NOW the step the TOFU suspends: the pane does NOT move.
        let dir = back_target(&mut app).expect("there is a trail");
        assert_eq!(dir, vp("mem:///b"));
        // What `walk_trail` does facing a `Suspended`: NOTHING, on purpose —
        // it counts on whoever answers the modal to finish the step.
        rewind_trail(
            &mut app,
            0,
            TrailStep::Back,
            &dir,
            rewind_for(&Cd::Suspended),
        );
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 2),
            "the step is taken and pending completion"
        );
        (app, dir)
    }

    /// The retry LANDS: the pane really moved, so nothing gets rewound — the
    /// step `walk_trail` took is the step that occurred.
    #[test]
    fn a_retry_that_lands_leaves_the_step_taken_and_does_not_record_it() {
        let (mut app, dir) = app_con_paso_suspendido();
        let before = (app.history[0].back_len(), app.history[0].fwd_len());
        let mru_before: Vec<VPath> = app.history[0].entries().iter().cloned().collect();
        // The modal CARRIES the interrupted navigation's trail, and the
        // retry passes it to `cd_in` as is: it is still a `Replay`.
        let trail = Trail::Replay(TrailStep::Back);

        // What the retry's cd_in does.
        record_step(
            &mut app.history[0],
            &mut app.popular,
            &vp("mem:///c"),
            &dir,
            trail,
        );
        settle_suspended_trail(&mut app, 0, &dir, trail, &Cd::Replaced(0));

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            before,
            "the step was already counted: finishing it does not count it again"
        );
        assert_eq!(
            app.history[0].entries().iter().cloned().collect::<Vec<_>>(),
            mru_before,
            "and a retry that lands is still a Replay: it does not enter the MRU"
        );
    }

    /// The retry FAILS (or the reader denies the key, or trusting fails):
    /// the navigation dies without the pane ever moving. The step comes back
    /// EXACTLY once — rewinding it twice would eat the forward branch the
    /// reader already had.
    #[test]
    fn an_abandoned_retry_rewinds_the_step_exactly_once() {
        let (mut app, dir) = app_con_paso_suspendido();
        let (back_dado, fwd_dado) = (app.history[0].back_len(), app.history[0].fwd_len());

        settle_suspended_trail(
            &mut app,
            0,
            &dir,
            Trail::Replay(TrailStep::Back),
            &Cd::Cancelled,
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (back_dado + 1, fwd_dado - 1),
            "the step comes back ONCE: one more behind, one less ahead \
             (two rewinds would give (3, 0) and eat the reader's branch)"
        );
        assert_eq!(
            back_target(&mut app),
            Some(dir),
            "and the same destination is still available to retry"
        );
        assert_eq!(
            app.history[0].fwd_len(),
            2,
            "with the forward branch the reader already had intact underneath"
        );
    }

    /// The retry runs into ANOTHER unknown key: it suspends again. Nothing
    /// gets rewound (the new modal loads the same trail and the same step,
    /// so there is still someone to finish it) and nothing gets recorded
    /// either — it is still a `Replay`.
    #[test]
    fn a_retry_that_suspends_again_neither_rewinds_nor_records() {
        let (mut app, dir) = app_con_paso_suspendido();
        let before = (app.history[0].back_len(), app.history[0].fwd_len());
        let mru_before: Vec<VPath> = app.history[0].entries().iter().cloned().collect();

        settle_suspended_trail(
            &mut app,
            0,
            &dir,
            Trail::Replay(TrailStep::Back),
            &Cd::Suspended,
        );
        // And what the retry's `cd_in` does with the trail: nothing.
        record_step(
            &mut app.history[0],
            &mut app.popular,
            &vp("mem:///c"),
            &dir,
            Trail::Replay(TrailStep::Back),
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            before,
            "the step is still pending completion, neither rewound nor duplicated"
        );
        assert_eq!(
            app.history[0].entries().iter().cloned().collect::<Vec<_>>(),
            mru_before,
            "and a Replay does not enter the MRU by being retried"
        );
    }

    /// A NORMAL cd that runs into the TOFU has no step to rewind: it did not
    /// come from the trail. `Trail::Record` says so, and `settle` touches
    /// nothing.
    #[test]
    fn a_suspended_normal_cd_has_no_step_to_rewind() {
        let (mut app, dir) = app_con_paso_suspendido();
        let before = (app.history[0].back_len(), app.history[0].fwd_len());

        settle_suspended_trail(&mut app, 0, &dir, Trail::Record, &Cd::Cancelled);

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            before,
            "a navigation that did not come from the trail owes it nothing"
        );
    }

    /// And if the destination turned out to NOT EXIST, besides rewinding it
    /// gets RETIRED from the whole history — the same treatment the popup
    /// already gives a `NotFound`. Without this `nav.back` would keep
    /// pointing at a directory that just proved it is not there, and the key
    /// could only fail.
    #[test]
    fn a_notfound_destination_disappears_from_the_whole_trail() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("there is a trail");

        let outcome = Cd::Failed(Error::NotFound);
        rewind_trail(&mut app, 0, TrailStep::Back, &dir, rewind_for(&outcome));

        assert_eq!(
            back_target(&mut app),
            Some(vp("mem:///a")),
            "back jumps to the next living one, does not retry the dead dir"
        );
        assert!(
            !app.history[0].entries().contains(&dir),
            "and it is not in the MRU the popup paints either"
        );
    }
}

#[cfg(test)]
mod open_tests {
    use super::{App, resolve_opener};
    use crate::app::Pane;
    use norte_proto::{Entry, EntryKind, Segment, VPath};

    fn pane_con(nombre: &str) -> Pane {
        let dir = VPath::parse("file:///d").expect("test wire");
        let entry = Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(nombre.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        };
        Pane::new(dir, vec![entry])
    }

    /// With no rule in `ns.toml` for the mimetype, F4 falls back to the
    /// desktop launcher instead of giving up with a message. Before, this
    /// forced writing configuration just to open a PDF.
    #[test]
    fn with_no_declared_opener_falls_back_to_the_system_launcher() {
        let mut app = App::new(pane_con("informe.pdf"), pane_con("otro.txt"));
        resolve_opener(&mut app);
        let pending = app.pending_open.expect("F4 resolves something to launch");
        assert!(
            pending.detached,
            "the desktop launcher does not suspend the TUI"
        );
        assert_eq!(pending.argv.len(), 2, "binary + file, no shell");
        assert!(
            pending.argv[1].to_string_lossy().ends_with("informe.pdf"),
            "opens the file under the cursor: {:?}",
            pending.argv
        );
        assert!(app.message.is_none(), "and leaves no error in the bar");
    }

    /// A declared opener still outranks it, and THAT one does keep the
    /// terminal (it can be `bat` or an editor).
    #[test]
    fn a_declared_opener_wins_and_takes_the_terminal() {
        let mut app = App::new(pane_con("notas.txt"), pane_con("x.txt"));
        app.openers = norte_frontend::openers::OpenersConfig::parse(
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .expect("test config");
        resolve_opener(&mut app);
        let pending = app.pending_open.expect("F4 resolves the declared opener");
        assert_eq!(pending.program, "bat");
        assert!(!pending.detached);
    }

    /// #144: BOTH of F4's paths carry the pane's directory as cwd.
    ///
    /// The three shell commands have passed it since #135 and the openers
    /// did not, so an editor opened on a file from the pane used to save in
    /// norte's cwd. Left as is on purpose in the shell wave —changing it
    /// changes behavior, and an opener writing a RELATIVE path ends up
    /// writing it elsewhere— and it was DECIDED on 2026-08-14 to pass it:
    /// the surprise of saving where you are not looking is the bigger of
    /// the two.
    ///
    /// Both paths and not just the declared one: `xdg-open` hands the file
    /// to its associated program, which can be the same editor, and
    /// inheriting the cwd depending on which door was used would be the same
    /// surprise wearing a different face.
    #[test]
    fn both_of_f4s_paths_open_in_the_panes_directory() {
        for (nombre, config) in [
            ("informe.pdf", None),
            (
                "notas.txt",
                Some("[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n"),
            ),
        ] {
            let mut app = App::new(pane_con(nombre), pane_con("otro.txt"));
            if let Some(c) = config {
                app.openers =
                    norte_frontend::openers::OpenersConfig::parse(c).expect("test config");
            }
            let expected = norte_vfs_local::vpath_to_native(app.focused().dir())
                .expect("the test pane is local");
            resolve_opener(&mut app);
            let pending = app.pending_open.expect("F4 resolves something");
            assert_eq!(
                pending.cwd.as_deref(),
                Some(expected.as_path()),
                "{nombre}: the child opens where the reader is looking"
            );
        }
    }

    /// A remote file has no native path: neither a declared opener nor the
    /// system launcher can open it, and the user must be told.
    #[test]
    fn a_remote_file_launches_nothing() {
        let dir = VPath::parse("sftp://host/d").expect("test wire");
        let entry = Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"a.pdf".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        };
        let mut app = App::new(Pane::new(dir.clone(), vec![entry]), Pane::new(dir, vec![]));
        resolve_opener(&mut app);
        assert!(app.pending_open.is_none());
        assert!(app.message.is_some(), "it says so in the bar");
    }
}

#[cfg(test)]
mod disconnect_tests {
    use super::*;
    use crate::app::Pane;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("test wire")
    }

    /// A panel planted on `sftp://srv/b` with the trail passed to it.
    fn app_remota(trail: &[&str]) -> App {
        let mut app = App::new(
            Pane::new(vp("sftp://srv/b"), Vec::new()),
            Pane::new(vp("file:///tmp"), Vec::new()),
        );
        let side = app.focus();
        for p in trail {
            app.history[side].record(vp(p));
        }
        app
    }

    /// The destination comes from the TRAIL, not from `$HOME`: it is the
    /// same decision the window makes, and when each frontend made it on its
    /// own the same key left the panel in two different places.
    #[test]
    fn the_destination_comes_from_the_trail_like_in_the_window() {
        let app = app_remota(&["file:///home/o", "sftp://srv/a"]);
        assert_eq!(
            destino_tras_desconectar(&app, &vp("sftp://srv/b")),
            vp("file:///home/o"),
            "skips the machine that is closing"
        );
    }

    /// With nothing foreign in the trail it falls back home, which is what
    /// the window does — and what this function ALWAYS did.
    #[test]
    fn with_no_foreign_trail_it_falls_back_home() {
        let app = app_remota(&["sftp://srv/a"]);
        assert_eq!(
            destino_tras_desconectar(&app, &vp("sftp://srv/b")),
            norte_frontend::shell::home_vpath(),
        );
    }
}

#[cfg(test)]
mod edit_tests {
    use super::*;
    use crate::app::Pane;

    fn app_local() -> App {
        let d = VPath::parse("file:///tmp").expect("test wire");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// #133: with nothing under the cursor there is nothing to edit, and it
    /// says so.
    #[test]
    fn editing_nothing_says_so() {
        let app = app_local();
        assert!(edit_under_cursor(&app).is_err());
    }

    /// A FOLDER is not edited: `nav.enter` is there to enter it, and opening
    /// an editor on a directory is teaching the editor what it does not
    /// know.
    #[test]
    fn a_folder_is_not_edited() {
        let mut app = app_local();
        app.panes[0].begin_listing(
            VPath::parse("file:///tmp").expect("wire"),
            vec![norte_proto::Entry {
                path: VPath::parse("file:///tmp/sub").expect("wire"),
                kind: norte_proto::EntryKind::Dir,
                size: None,
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            }],
            false,
            None,
        );
        let err = edit_under_cursor(&app).expect_err("a folder, no");
        assert!(!err.is_empty());
    }

    /// A REMOTE pane has no system file to give the editor, so it says so
    /// instead of opening anything.
    #[test]
    fn a_remote_pane_is_not_edited() {
        let d = VPath::parse("sftp://host/casa").expect("wire");
        let mut app = App::new(
            Pane::new(d.clone(), Vec::new()),
            Pane::new(d.clone(), Vec::new()),
        );
        app.panes[0].begin_listing(
            d.clone(),
            vec![norte_proto::Entry {
                path: VPath::parse("sftp://host/casa/a.txt").expect("wire"),
                kind: norte_proto::EntryKind::File,
                size: Some(1),
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            }],
            false,
            None,
        );
        assert!(edit_under_cursor(&app).is_err());
    }

    /// An embedded backend with the local provider: `motivo_para_no_lanzar`
    /// asks `fs.stat` (#303), so its tests need real disk.
    fn backend_local() -> norte_core::backend::Backend {
        let engine = norte_core::Engine::new();
        engine.register_provider(std::sync::Arc::new(
            norte_vfs_local::LocalProvider::os_root(),
        ));
        norte_core::backend::Backend::Embedded(std::sync::Arc::new(engine))
    }

    /// #290: the other half of `pane.edit-new` opens the editor over the
    /// path that was SENT TO BE CREATED, not over whatever is under the
    /// cursor: when the task finishes, the listing may not have refreshed
    /// yet.
    #[test]
    fn the_created_files_editor_targets_the_requested_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fichero = dir.path().join("notas.txt");
        std::fs::write(&fichero, b"").expect("create");
        let creado = norte_vfs::native::vpath_from_native(&fichero).expect("native");

        let pending = edit_created(&creado).expect("local");
        assert_eq!(
            pending.check_regular.as_ref(),
            Some(&creado),
            "the suspension carries the path to check on launch"
        );
        assert_eq!(pending.argv.len(), 2, "program and path, no shell line");
        assert_eq!(
            pending.argv[1],
            fichero.clone().into_os_string(),
            "the path travels as its own argument"
        );
        assert!(!pending.wait_for_key, "an editor sees itself out");
        // The cwd is the created file's PARENT, not the focused pane:
        // between the submit and the outcome the reader may have gone
        // elsewhere.
        assert_eq!(
            pending.cwd.as_deref(),
            Some(dir.path()),
            "an editor's `:w other.txt` lands where the just-created file is"
        );
    }

    /// And over something with no native form nothing opens: creating is
    /// refused on a remote pane, so this should not happen — and if it does,
    /// an editor over something that cannot be named is not the way out.
    #[test]
    fn with_no_native_form_no_editor_opens() {
        let remoto = VPath::parse("sftp://srv/notas.txt").expect("wire");
        assert!(edit_created(&remoto).is_err());
    }

    /// #303: between creating the file and opening the editor, someone who
    /// can write in that directory unlinks it and leaves a SYMLINK with the
    /// same name. Nothing gets launched: `fs.stat` is `lstat` and describes
    /// the link, not its target, so the link is seen for what it is.
    ///
    /// It is the only defense there is here, and it narrows without closing
    /// — that is why the question is asked by the run loop right against the
    /// `exec` and not where the gesture is resolved, which used to leave a
    /// whole `refresh_panes` inside the window.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_planted_between_creating_and_launching_is_not_edited() {
        let dir = tempfile::tempdir().expect("tempdir");
        let secreto = dir.path().join("secreto");
        std::fs::write(&secreto, b"de otro").expect("create");
        let creado_nativo = dir.path().join("notas.txt");
        // What norte created, already unlinked and replaced by the link.
        std::os::unix::fs::symlink(&secreto, &creado_nativo).expect("symlink");
        let creado = norte_vfs::native::vpath_from_native(&creado_nativo).expect("native");

        assert_eq!(
            motivo_para_no_lanzar(&backend_local(), Some(creado)).await,
            Some(t("msg-edit-created-changed")),
            "a link is not the file that was created"
        );
    }

    /// And the same if a DIRECTORY is in its place, or if there is nothing
    /// left at all: the three causes give the same refusal, on purpose.
    #[tokio::test]
    async fn neither_a_directory_nor_a_gap_gets_launched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let carpeta = dir.path().join("notas.txt");
        std::fs::create_dir(&carpeta).expect("mkdir");
        let como_dir = norte_vfs::native::vpath_from_native(&carpeta).expect("native");
        let ausente =
            norte_vfs::native::vpath_from_native(&dir.path().join("no-esta")).expect("native");

        let backend = backend_local();
        assert_eq!(
            motivo_para_no_lanzar(&backend, Some(como_dir)).await,
            Some(t("msg-edit-created-changed"))
        );
        // And the one that is no longer there does NOT count as "could not
        // ask": a provider's `NotFound` is an answer, not a failure to ask.
        assert_eq!(
            motivo_para_no_lanzar(&backend, Some(ausente)).await,
            Some(t("msg-edit-created-changed"))
        );
    }

    /// The file that is still there launches, and a suspension with nothing
    /// to check —the shell, the command line— does not even ask.
    #[tokio::test]
    async fn what_is_still_the_file_launches_and_what_asks_nothing_does_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fichero = dir.path().join("notas.txt");
        std::fs::write(&fichero, b"").expect("create");
        let creado = norte_vfs::native::vpath_from_native(&fichero).expect("native");

        let backend = backend_local();
        assert_eq!(motivo_para_no_lanzar(&backend, Some(creado)).await, None);
        assert_eq!(motivo_para_no_lanzar(&backend, None).await, None);
    }

    /// And over a local file the editor's argv comes out with the path
    /// SEPARATE.
    #[test]
    fn over_a_local_file_the_editor_comes_out_with_the_path_separate() {
        let mut app = app_local();
        app.panes[0].begin_listing(
            VPath::parse("file:///tmp").expect("wire"),
            vec![norte_proto::Entry {
                path: VPath::parse("file:///tmp/a.txt").expect("wire"),
                kind: norte_proto::EntryKind::File,
                size: Some(1),
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            }],
            false,
            None,
        );
        let super::EditLaunch::Shell(pending) = edit_under_cursor(&app).expect("local and a file")
        else {
            panic!("with no `[ui] editor`, the environment's takes over, through the shell's path")
        };
        assert_eq!(pending.argv.len(), 2, "program and path, no shell line");
        assert_eq!(
            pending.argv[1],
            std::ffi::OsString::from("/tmp/a.txt"),
            "the path travels as its own argument"
        );
        assert!(!pending.wait_for_key, "an editor sees itself out");
    }

    /// `[ui] editor` outranks `$VISUAL`/`$EDITOR`, expands its field codes
    /// and, if it is a window, launches WITHOUT suspending the terminal.
    #[test]
    fn the_configured_editor_wins_and_may_not_suspend() {
        let mut app = app_local();
        app.panes[0].begin_listing(
            VPath::parse("file:///tmp").expect("wire"),
            vec![norte_proto::Entry {
                path: VPath::parse("file:///tmp/a.txt").expect("wire"),
                kind: norte_proto::EntryKind::File,
                size: Some(1),
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            }],
            false,
            None,
        );
        app.editor = Some(crate::app::EditorSpec {
            command: vec!["zed".to_owned(), "%d".to_owned(), "%f".to_owned()],
            detached: true,
        });

        let super::EditLaunch::Open(pending) = edit_under_cursor(&app).expect("local and a file")
        else {
            panic!("with `[ui] editor` it goes through the openers' path")
        };
        assert_eq!(pending.program, "zed");
        assert_eq!(
            pending.argv,
            vec![
                std::ffi::OsString::from("zed"),
                std::ffi::OsString::from("/tmp"),
                std::ffi::OsString::from("/tmp/a.txt"),
            ],
            "`%d` the pane's directory, `%f` the file, each its own argument"
        );
        assert!(pending.detached, "a window does not suspend the terminal");

        // And without the flag, it is waited for like any terminal program.
        app.editor = Some(crate::app::EditorSpec {
            command: vec!["micro".to_owned(), "%f".to_owned()],
            detached: false,
        });
        let super::EditLaunch::Open(pending) = edit_under_cursor(&app).expect("local and a file")
        else {
            panic!("still the configured editor")
        };
        assert!(!pending.detached);
    }
}

#[cfg(test)]
mod compare_files_tests {
    use super::{App, EditLaunch, compare_files};
    use crate::app::Pane;
    use norte_proto::{Entry, EntryKind, Segment, VPath};

    fn entrada(dir: &VPath, nombre: &str, kind: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(nombre.as_bytes().to_vec()).expect("segmento")),
            kind,
            size: Some(1),
            mtime_ms: None,
        }
    }

    fn app_local() -> App {
        let dir = VPath::parse("file:///d").expect("wire");
        App::new(
            Pane::new(
                dir.clone(),
                vec![
                    entrada(&dir, "a.txt", EntryKind::File),
                    entrada(&dir, "b.txt", EntryKind::File),
                ],
            ),
            Pane::new(dir.clone(), vec![entrada(&dir, "c.txt", EntryKind::File)]),
        )
    }

    /// Marks the entry whose base name is `nombre`, whatever its index is
    /// after sorting (directories come first, and the `..` row in front).
    fn marcar(app: &mut App, pane: usize, nombre: &str) {
        let i = app.panes[pane]
            .entries()
            .iter()
            .position(|e| {
                e.path
                    .file_name()
                    .is_some_and(|s| s.as_bytes() == nombre.as_bytes())
            })
            .expect("the entry is in the listing");
        app.panes[pane].set_mark(i, true);
    }

    /// With no `[ui] diff`, the comparator is `diff -u`, goes through the
    /// shell's path and **waits for a key**: its output is a few lines that
    /// finish instantly, and without the wait the reader would see a
    /// flicker.
    #[test]
    fn with_nothing_configured_it_is_diff_dash_u_and_waits_for_a_key() {
        let app = app_local();
        let EditLaunch::Shell(pendiente) = compare_files(&app).expect("one from each panel") else {
            panic!("with no `[ui] diff` it goes through the shell");
        };
        assert!(pendiente.wait_for_key, "the output stays on screen");
        assert_eq!(
            pendiente.argv,
            vec![
                std::ffi::OsString::from("diff"),
                std::ffi::OsString::from("-u"),
                std::ffi::OsString::from("/d/a.txt"),
                std::ffi::OsString::from("/d/c.txt"),
            ],
            "`%F` are the TWO files, each its own argument"
        );
    }

    /// `[ui] diff` outranks it, and it can be a window — then it does not
    /// suspend.
    #[test]
    fn the_configured_comparator_wins_and_can_be_a_window() {
        let mut app = app_local();
        app.diff = Some(crate::app::EditorSpec {
            command: vec!["meld".to_owned(), "%F".to_owned()],
            detached: true,
        });
        let EditLaunch::Open(pendiente) = compare_files(&app).expect("two files") else {
            panic!("with `[ui] diff` it goes through the openers' path");
        };
        assert!(pendiente.detached, "a window does not suspend the terminal");
        assert_eq!(pendiente.program, "meld");
        assert_eq!(pendiente.argv.len(), 3, "binary + the two files");
    }

    /// Two MARKED in the focused panel outrank the other one's cursor: it is
    /// the usual operand.
    #[test]
    fn two_marked_outrank_the_other_panels_cursor() {
        let mut app = app_local();
        let dir = VPath::parse("file:///d").expect("wire");
        let _ = &dir;
        marcar(&mut app, 0, "a.txt");
        marcar(&mut app, 0, "b.txt");
        let EditLaunch::Shell(pendiente) = compare_files(&app).expect("two marked") else {
            panic!("with no `[ui] diff` it goes through the shell");
        };
        assert_eq!(
            pendiente.argv[3],
            std::ffi::OsString::from("/d/b.txt"),
            "the second is the other MARKED one, not the opposite panel's"
        );
    }

    /// A folder means comparing directories, and it says so instead of
    /// comparing what nobody chose.
    #[test]
    fn a_folder_is_not_compared_as_a_file() {
        let dir = VPath::parse("file:///d").expect("wire");
        let mut app = App::new(
            Pane::new(
                dir.clone(),
                vec![
                    entrada(&dir, "a.txt", EntryKind::File),
                    entrada(&dir, "sub", EntryKind::Dir),
                ],
            ),
            Pane::new(dir.clone(), vec![entrada(&dir, "c.txt", EntryKind::File)]),
        );
        marcar(&mut app, 0, "a.txt");
        marcar(&mut app, 0, "sub");
        assert!(compare_files(&app).is_err());
    }

    /// An external `diff` cannot be given an `sftp://`: it says so, like
    /// opening and editing do.
    #[test]
    fn a_remote_pane_says_so_instead_of_trying() {
        let remoto = VPath::parse("sftp://srv/d").expect("wire");
        let local = VPath::parse("file:///d").expect("wire");
        let app = App::new(
            Pane::new(
                remoto.clone(),
                vec![entrada(&remoto, "a.txt", EntryKind::File)],
            ),
            Pane::new(
                local.clone(),
                vec![entrada(&local, "c.txt", EntryKind::File)],
            ),
        );
        assert!(compare_files(&app).is_err());
    }
}
