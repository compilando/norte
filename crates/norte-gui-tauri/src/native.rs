//! The NATIVE effects the host asks for: clipboard, open with the desktop,
//! terminal (task 6.5).
//!
//! **Why this lives here and not in the host, nor in the webview.** The host
//! says WHAT has to be done, with operands that come out of its semantic
//! state; this process decides HOW, with one narrow door per thing. And the
//! webview does not take part: its capabilities are listening for events and
//! nothing else (ADR 0066, decision D11), so it neither sees the paths nor
//! has anything to run with.
//!
//! **None of the three is a shell.** Each one builds a closed `argv` — the
//! program comes from a list, never from user text — and does not go through
//! an interpreter: no `sh -c`, which is where a file name with a `;` stops
//! being a name. The choice of program lives in
//! `norte_frontend::shell`/`openers`, which do no I/O and are tested without a
//! tty; here only the PATH is checked and it is launched.
//!
//! The clipboard text goes over STDIN, never in the `argv`: a name is bytes,
//! and one starting with `-` would become a flag for the helper.

use std::process::Stdio;

use norte_ui_host::dto::NativeEffect;

/// What happened to an effect. It is SAID: "copied" over an empty clipboard
/// is only discovered when the paste lands somewhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// Launched (or written) without error.
    Done,
    /// No program on the PATH knows how to do it.
    NoProgram,
    /// There was a program and it failed to start or to write.
    Failure,
}

/// Consumes the host's native effects until the channel closes.
///
/// Each one runs on a blocking thread: starting a process and writing to its
/// stdin are blocking calls, and doing them on the async executor is rule 2
/// broken somewhere nobody would look.
pub async fn pump(
    mut rx: tokio::sync::broadcast::Receiver<NativeEffect>,
    host: std::sync::Arc<norte_ui_host::UiHost>,
    theme: impl Fn(&str) + Send + Sync + 'static,
    close: impl Fn() + Send + 'static,
) {
    // `Sync` and in an `Arc` so it can be sent to a blocking thread: a theme
    // that is a PATH gets read, and reading on the async executor is rule 2
    // broken.
    let theme = std::sync::Arc::new(theme);
    loop {
        match rx.recv().await {
            // CLOSE is not "run" either: destroying the window belongs to
            // this process. The host asks for it once there is nothing left
            // to ask about — `[ui] confirm_quit` decides whether to ask, and
            // whoever answers is the reader — so here it is simply obeyed.
            Ok(NativeEffect::CloseWindow) => close(),
            // The HANDOFF to the terminal (phase 9): the emulator is opened
            // and, ONLY if it started, this window closes. The first version
            // launched it and forgot about it — neither closing nor checking
            // — so the window was left behind without the session, saying
            // "handing off…" forever. If it does not start, the host is told,
            // and it stays, recovers the session and reports it: decision 4
            // of ADR 0123.
            //
            // Awaited here and not fire-and-forget: launching an emulator is
            // a `spawn`, a matter of milliseconds, and the order matters —
            // closing before knowing whether it opened is the failure the
            // terminal already had.
            Ok(NativeEffect::HandoffToTerminal { daemon }) => {
                let result = tokio::task::spawn_blocking(move || handoff(daemon))
                    .await
                    .unwrap_or(RunOutcome::Failure);
                match result {
                    RunOutcome::Done => close(),
                    other => {
                        let _ = host
                            .dispatch(norte_ui_host::UiAction::HandoffFailed {
                                no_terminal: matches!(other, RunOutcome::NoProgram),
                            })
                            .await;
                    }
                }
            }
            // The THEME is not "run" either: it is resolved again here,
            // because colors cross to the webview converted into CSS
            // variables and that conversion belongs to this process.
            //
            // A PRESET runs on the spot, on purpose: it is arithmetic over
            // colors, and sending it to another thread would add a frame of
            // delay to something the reader is watching change under the
            // cursor. A PATH (ADR 0020) gets READ, so it goes to a blocking
            // thread — with a theme on a downed mount, doing it here would
            // freeze the executor.
            Ok(NativeEffect::ThemeChanged { name }) => {
                if norte_frontend::theme::is_preset(Some(&name)) {
                    theme(&name);
                } else {
                    let theme = std::sync::Arc::clone(&theme);
                    tokio::task::spawn_blocking(move || theme(&name));
                }
            }
            // The folder picker is the only one that ANSWERS (#284): the
            // others are fire-and-forget, but for this one the host expects a
            // path back, so its answer returns through `dispatch` like any
            // other action — the same door the renderer uses.
            Ok(NativeEffect::PickDirectory { from }) => {
                let host = std::sync::Arc::clone(&host);
                tokio::task::spawn(async move {
                    let chosen = tokio::task::spawn_blocking(move || pick_directory(&from))
                        .await
                        .unwrap_or(None);
                    let _ = host
                        .dispatch(norte_ui_host::UiAction::DirectoryPicked { path: chosen })
                        .await;
                });
            }
            // A program that is AWAITED (#312) answers with what it printed,
            // through the same door as the folder picker: `dispatch`. A
            // fire-and-forget one is launched and forgotten, like everything
            // else.
            Ok(NativeEffect::RunProgram {
                title_key,
                argv,
                cwd,
                detached: false,
            }) => {
                let host = std::sync::Arc::clone(&host);
                tokio::task::spawn(async move {
                    let ran = tokio::task::spawn_blocking(move || run(&argv, cwd.as_deref()))
                        .await
                        .unwrap_or_else(|_| Ran::failure(String::new()));
                    let _ = host
                        .dispatch(norte_ui_host::UiAction::ProgramFinished {
                            title_key,
                            command: ran.command,
                            output: ran.output,
                            truncated: ran.truncated,
                            failed: ran.failed,
                        })
                        .await;
                });
            }
            // A user command (ADR 0177): run to the end on the runtime, its
            // output to the log ring, and how it ended back through `dispatch`.
            Ok(NativeEffect::RunUserCommand { launch }) => {
                let host = std::sync::Arc::clone(&host);
                tokio::task::spawn(async move {
                    let _ = host.dispatch(user_command_action(launch).await).await;
                });
            }
            Ok(effect) => {
                // Without waiting for the result: an `xdg-open` can take
                // seconds to return, and the next gesture from whoever is
                // sitting there does not wait for their PDF to open.
                tokio::task::spawn_blocking(move || execute(&effect));
            }
            // Lagged: some gesture was lost, not a piece of screen. It keeps
            // listening, which is the opposite of what the view does.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// What `pump` does with a `RunUserCommand`, minus the host: run, then build
/// the action that reports how it ended.
pub(crate) async fn user_command_action(
    launch: norte_frontend::user_commands::Launch,
) -> norte_ui_host::UiAction {
    let label = launch.label.clone();
    let refresh = launch.refresh;
    let finished = norte_frontend::user_commands::run_background(launch).await;
    norte_ui_host::UiAction::UserCommandFinished {
        label,
        finished,
        refresh,
    }
}

/// Does ONE. Blocks: called from `spawn_blocking`.
#[must_use]
pub fn execute(effect: &NativeEffect) -> RunOutcome {
    match effect {
        NativeEffect::CopyBytes { bytes, .. } => copy_bytes(bytes),
        NativeEffect::OpenPath { path } => open_path(path),
        NativeEffect::OpenTerminal { dir, configured } => terminal(dir, configured.as_deref()),
        // Phase 9: `pump` handles it, since it is the one that can close
        // the window and notify the host depending on how it turns out. This
        // is only reached if someone calls `execute` by hand, and then the
        // terminal is simply opened.
        NativeEffect::HandoffToTerminal { daemon } => handoff(*daemon),
        NativeEffect::Notify { title, body } => notify(title, body),
        // Fire-and-forget: the comparator opens its own window and this does
        // not wait. The argv arrives already resolved and interpolated from
        // the host; here it is only launched.
        NativeEffect::RunProgram {
            argv,
            cwd,
            detached: true,
            ..
        } => {
            let Some((program, args)) = argv.split_first() else {
                return RunOutcome::NoProgram;
            };
            // A part that does not decode refuses the launch, cwd included:
            // running somewhere else than asked is not a fallback.
            let (Some(program), Some(args), Some(cwd)) = (
                os_from_bridge(program),
                args.iter()
                    .map(|a| os_from_bridge(a))
                    .collect::<Option<Vec<_>>>(),
                cwd.as_deref()
                    .map_or(Some(None), |d| os_from_bridge(d).map(Some)),
            ) else {
                return RunOutcome::NoProgram;
            };
            launch(
                std::path::Path::new(&program),
                &args,
                cwd.as_deref().map(std::path::Path::new),
            )
        }
        // `pump` handles all four, and none of them launches a program
        // here: the `RunProgram` that is AWAITED has to answer, the folder
        // picker has to be ANSWERED with the path, the theme is a catalogue
        // to rebuild, and closing belongs to the event loop. There is nothing
        // to run here.
        NativeEffect::RunProgram { .. }
        | NativeEffect::PickDirectory { .. }
        | NativeEffect::ThemeChanged { .. }
        | NativeEffect::CloseWindow
        // A user command's background run (ADR 0177): `pump` runs it;
        // reached only by a hand call.
        | NativeEffect::RunUserCommand { .. } => RunOutcome::NoProgram,
    }
}

/// Fires the notification with the first program that exists (#285).
///
/// The text arrives ALREADY composed, translated, masked and bounded: nothing
/// is decided about it here, it is only delivered. A notification that
/// cannot be given is SAID — `NoProgram` — instead of swallowed: whoever
/// believes they will be notified and has no `notify-send` deserves to know,
/// once.
fn notify(title: &str, body: &str) -> RunOutcome {
    for argv in norte_frontend::shell::notify_candidates(title, body) {
        let Some((program, args)) = argv.split_first() else {
            continue;
        };
        // Resolved to an absolute path like everything else norte launches
        // (ADR 0082). There is no `current_dir` here, so #302's hole does not
        // apply; it is done the same way anyway, because the next person who
        // copies this pattern will copy it with a `cwd` set.
        let Some(path) = norte_frontend::openers::resolve_program(program) else {
            continue;
        };
        let status = std::process::Command::new(&path)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(st) if st.success() => return RunOutcome::Done,
            // It exists and it failed: the next one is not tried. Two
            // notifications for the same event is worse than none.
            Ok(_) => return RunOutcome::Failure,
            // Not on the PATH: try the next one.
            Err(_) => {}
        }
    }
    RunOutcome::NoProgram
}

/// Opens the DESKTOP's folder picker and returns what was chosen (#284).
/// `None` = closed without choosing, or there is no picker here at all.
///
/// Blocks on purpose — called from `spawn_blocking` — a picker stays open for
/// however long the reader takes to decide, which can be a minute.
///
/// The candidates are tried in order and the first one that EXISTS decides: a
/// program that is not there gives `NotFound` on launch and the next one is
/// tried, the same per-attempt probing the clipboard and the terminal do.
/// Canceling is told apart from choosing by the exit code, not by parsing the
/// text — a directory can be named like any error message.
#[must_use]
fn pick_directory(from: &norte_proto::VPath) -> Option<String> {
    // With a REMOTE pane there is no native path to open at, and that blocks
    // nothing: the picker always returns a folder on this machine, and
    // copying from an `sftp://` to a local folder is legitimate. What is lost
    // is the suggestion of where to start, not the operation.
    let native = norte_vfs::native::vpath_to_native(from).unwrap_or_else(|_| {
        std::env::var_os("HOME")
            .map_or_else(|| std::path::PathBuf::from("/"), std::path::PathBuf::from)
    });
    for argv in norte_frontend::shell::directory_picker_candidates(&native) {
        let (program, args) = argv.split_first()?;
        // Absolute path, like the rest (ADR 0082): a candidate that does not
        // resolve is not launched, and the next one is tried.
        let Some(path) = norte_frontend::openers::resolve_program(program) else {
            continue;
        };
        let output = std::process::Command::new(&path)
            .args(args)
            // No stdin: a picker reads nothing, and leaving it open is a door
            // that is not needed (the same rule 9 as the rest).
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        let Ok(output) = output else {
            // Not on the PATH: to the next one.
            continue;
        };
        if !output.status.success() {
            // It exists and it closed without choosing. The next one is NOT
            // tried: the reader already answered, and opening another picker
            // on them would be refusing to take a "no".
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned();
        return (!path.is_empty()).then_some(path);
    }
    None
}

/// Writes `bytes` to the clipboard with the first helper that exists.
///
/// The body lives in `norte_frontend::shell` since #286: the terminal needs
/// exactly the same thing, and keeping two copies of "which helper and in
/// what order" is having two answers to the same question. What this window
/// does NOT have is OSC 52 output, which needs a terminal emulator in front
/// of it.
fn copy_bytes(bytes: &[u8]) -> RunOutcome {
    match norte_frontend::shell::copy_to_clipboard(bytes) {
        norte_frontend::shell::ClipboardOutcome::Done(_) => RunOutcome::Done,
        norte_frontend::shell::ClipboardOutcome::NoHelper => RunOutcome::NoProgram,
        norte_frontend::shell::ClipboardOutcome::Failed => RunOutcome::Failure,
    }
}

/// Opens `path` with whichever application the desktop chooses.
fn open_path(path: &norte_proto::VPath) -> RunOutcome {
    let Ok(native) = norte_vfs::native::vpath_to_native(path) else {
        // The host already checks this; here is the belt: `xdg-open` is not
        // given something that is not on this disk.
        return RunOutcome::NoProgram;
    };
    let (program, argv) = norte_frontend::openers::system_opener(&native);
    let Some(path) = norte_frontend::openers::resolve_program(std::ffi::OsStr::new(&program))
    else {
        return RunOutcome::NoProgram;
    };
    launch(&path, &argv[1..], None)
}

/// Opens a terminal sitting in `dir`.
fn terminal(dir: &norte_proto::VPath, configured: Option<&[String]>) -> RunOutcome {
    // Through `shell_dir`, like the terminal: on Windows the raw native path
    // is verbatim (`\\?\`), which `cmd`'s cwd and `%d` mishandle, and a
    // reserved name must be refused, not opened somewhere else.
    let Some(native) = norte_frontend::shell::shell_dir(dir) else {
        return RunOutcome::NoProgram;
    };
    for argv in norte_frontend::shell::terminal_candidates(configured, &native) {
        let Some(program) = argv.first() else {
            continue;
        };
        let Some(path) = norte_frontend::openers::resolve_program(program) else {
            continue;
        };
        // And with the cwd set IN ADDITION to the flag: `xterm` has no flag
        // and inherits the directory, which is exactly the case the shared
        // list documents.
        return launch(&path, &argv[1..], Some(&native));
    }
    RunOutcome::NoProgram
}

/// The HANDOFF to the terminal (phase 9): opens an emulator with
/// `ntc --attach` inside.
///
/// Uses `terminal_command_candidates`, the shared list of emulators and
/// their run-this-command flag, with `ntc` as the program. `[ui] terminal`
/// does NOT apply here: its template has no way to say where a command goes.
///
/// `--daemon` travels if this window carries it, and it has to: the session
/// that was just let go of is the daemon's, and an `ntc` against its embedded
/// core would find nothing.
fn handoff(daemon: bool) -> RunOutcome {
    let Some(ntc) = norte_frontend::openers::resolve_program(std::ffi::OsStr::new("ntc")) else {
        return RunOutcome::NoProgram;
    };
    // The flags from the SAME place the terminal gets its test that it
    // accepts them (`norte_frontend::handoff`): hand-written in two binaries,
    // one side changed without the other and the handoff died silently.
    let mut command = vec![ntc.to_string_lossy().into_owned()];
    command.extend(norte_frontend::handoff::terminal_args(daemon));
    for argv in norte_frontend::shell::terminal_command_candidates(&command) {
        let Some(program) = argv.first() else {
            continue;
        };
        let Some(path) = norte_frontend::openers::resolve_program(program) else {
            continue;
        };
        return launch(&path, &argv[1..], None);
    }
    RunOutcome::NoProgram
}

/// What an awaited program left behind (#312).
struct Ran {
    /// The argv, as text, to say what ran.
    command: String,
    /// stdout and stderr, in that order, up to [`MAX_OUTPUT`].
    output: Vec<u8>,
    /// Cut off by the cap.
    truncated: bool,
    /// Did not start, or ran past the deadline.
    failed: bool,
}

impl Ran {
    fn failure(command: String) -> Self {
        Self {
            command,
            output: Vec::new(),
            truncated: false,
            failed: true,
        }
    }
}

/// A path or argument as it crossed the bridge, back to the platform's
/// string: its own bytes on unix, WTF-8 on Windows. `None` if Windows bytes
/// are not WTF-8, which nothing on this side produced.
#[cfg_attr(unix, allow(clippy::unnecessary_wraps))] // only Windows can fail
fn os_from_bridge(bytes: &[u8]) -> Option<std::ffi::OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(std::ffi::OsStr::from_bytes(bytes).to_owned())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt as _;
        norte_vfs::wtf8::decode_to_wide(bytes).map(|w| std::ffi::OsString::from_wide(&w))
    }
}

/// How much output is kept from an awaited program: the host splits it into
/// lines and bounds it again, but a `diff` of two ISOs has no reason to fill
/// this window's memory before it gets there.
const MAX_OUTPUT: usize = 1024 * 1024;

/// How long to wait for a program before calling it hung. A text comparator
/// finishes instantly; one that takes longer is waiting on someone.
const PROGRAM_DEADLINE: std::time::Duration = std::time::Duration::from_mins(1);

/// Runs and WAITS, capturing whatever it prints (#312). Blocks: called from
/// `spawn_blocking`. No shell in between: the argv arrives already resolved
/// and interpolated, and piping it through `sh -c` would mean reinterpreting
/// file names that someone else named.
fn run(argv: &[Vec<u8>], cwd: Option<&[u8]>) -> Ran {
    use std::io::Read as _;
    let command = argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let Some((program, rest)) = argv.split_first() else {
        return Ran::failure(command);
    };
    let (Some(program), Some(rest), Some(cwd)) = (
        os_from_bridge(program),
        rest.iter()
            .map(|a| os_from_bridge(a))
            .collect::<Option<Vec<_>>>(),
        cwd.map_or(Some(None), |d| os_from_bridge(d).map(Some)),
    ) else {
        return Ran::failure(command);
    };
    let mut cmd = std::process::Command::new(program);
    cmd.args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let Ok(mut child) = cmd.spawn() else {
        return Ran::failure(command);
    };
    // Both pipes are read up to the cap, then waited on with a deadline.
    // Reading first and waiting after: a child that fills its stderr pipe
    // without it being read gets stuck, and `wait` would never return.
    let mut output = Vec::new();
    let mut truncated = false;
    let mut read_pipe = |pipe: Option<&mut dyn std::io::Read>| {
        let Some(p) = pipe else {
            return;
        };
        let mut buf = Vec::new();
        let _ = p.take((MAX_OUTPUT + 1) as u64).read_to_end(&mut buf);
        if buf.len() > MAX_OUTPUT {
            buf.truncate(MAX_OUTPUT);
            truncated = true;
        }
        output.extend_from_slice(&buf);
    };
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    read_pipe(out.as_mut().map(|o| o as &mut dyn std::io::Read));
    read_pipe(err.as_mut().map(|e| e as &mut dyn std::io::Read));
    let start = std::time::Instant::now();
    let failed = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) if start.elapsed() < PROGRAM_DEADLINE => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break true;
            }
            Err(_) => break true,
        }
    };
    Ran {
        command,
        output,
        truncated,
        failed,
    }
}

/// Launches and LETS GO: the window does not wait for a PDF to open.
fn launch(
    program: &std::path::Path,
    args: &[std::ffi::OsString],
    cwd: Option<&std::path::Path>,
) -> RunOutcome {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    // Its own process group, as the terminal's launches: a Ctrl+C or a
    // hangup aimed at the window does not take the launched program along.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    match cmd.spawn() {
        Ok(mut child) => {
            // Reaped off the window's threads: dropped, it stays a zombie
            // until the window exits. A reaper that cannot start costs one.
            let _ = std::thread::Builder::new()
                .name("norte-reap".into())
                .spawn(move || {
                    let _ = child.wait();
                });
            RunOutcome::Done
        }
        Err(_) => RunOutcome::Failure,
    }
}

#[cfg(test)]
mod tests {
    use super::{RunOutcome, execute};
    use norte_ui_host::dto::NativeEffect;

    #[cfg(unix)]
    #[tokio::test]
    async fn a_user_command_runs_and_becomes_an_action() {
        let d = tempfile::tempdir().unwrap();
        let launch = norte_frontend::user_commands::Launch {
            id: "t".into(),
            label: "T".into(),
            program: "sh".into(),
            args: vec!["-c".into(), "exit 4".into()],
            cwd: d.path().to_path_buf(),
            run_in: norte_frontend::user_commands::RunIn::Background,
            refresh: true,
            confirm: false,
            count: 0,
        };
        let a = super::user_command_action(launch).await;
        assert!(matches!(
            a,
            norte_ui_host::UiAction::UserCommandFinished {
                finished: norte_frontend::user_commands::Finished::Failed { code: 4 },
                refresh: true,
                ..
            }
        ));
    }

    /// A launched program gets its own process group, away from the
    /// window's signals (Linux: the group is read from `/proc`).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_launch_runs_in_its_own_process_group() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("pgid");
        let script = format!(
            "cut -d' ' -f5 /proc/$$/stat > '{}.t' && mv '{0}.t' '{0}'",
            out.display()
        );
        let outcome = super::launch(
            std::path::Path::new("sh"),
            &["-c".into(), script.into()],
            None,
        );
        assert_eq!(outcome, RunOutcome::Done);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let pgid = loop {
            if let Ok(s) = std::fs::read_to_string(&out) {
                break s;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the child never wrote"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let ours = std::fs::read_to_string("/proc/self/stat").unwrap();
        let ours = ours.split(' ').nth(4).unwrap();
        assert_ne!(pgid.trim(), ours, "its own group");
    }

    /// A location that is NOT on this disk is not handed to the desktop.
    ///
    /// The host already checks this and says so; this is the belt on the
    /// other side: `xdg-open` is not given an `sftp://`, and a terminal is
    /// not given a directory it cannot sit in. Without this guard, the
    /// conversion would fail silently and the user would see "opening…" over
    /// nothing.
    #[test]
    fn what_is_not_on_this_disk_does_not_launch() {
        let remote = norte_proto::VPath::parse("sftp://maquina/casa/x").expect("vpath");
        assert_eq!(
            execute(&NativeEffect::OpenPath {
                path: remote.clone()
            }),
            RunOutcome::NoProgram
        );
        assert_eq!(
            execute(&NativeEffect::OpenTerminal {
                dir: remote,
                configured: None
            }),
            RunOutcome::NoProgram
        );
    }

    /// The clipboard is tried with the system's helpers, and when there is
    /// none it is SAID instead of claiming it copied.
    ///
    /// It does not assert which one wins: on the CI machine there may be
    /// none, and on a human's there may be two. What is pinned is that the
    /// result is one of the three and never a panic over bytes that are not
    /// UTF-8.
    #[test]
    fn copying_bytes_does_not_decode_or_panic() {
        let r = execute(&NativeEffect::CopyBytes {
            // A name that is not UTF-8: it travels as-is over STDIN, and the
            // clipboard receives the SAME bytes that open that file.
            bytes: vec![b'/', b't', b'm', b'p', b'/', 0xFF, 0xFE],
            count: 1,
        });
        assert!(matches!(
            r,
            RunOutcome::Done | RunOutcome::NoProgram | RunOutcome::Failure
        ));
    }
}
