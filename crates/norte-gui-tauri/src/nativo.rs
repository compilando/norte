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
pub enum Resultado {
    /// Launched (or written) without error.
    Hecho,
    /// No program on the PATH knows how to do it.
    SinPrograma,
    /// There was a program and it failed to start or to write.
    Fallo,
}

/// Consumes the host's native effects until the channel closes.
///
/// Each one runs on a blocking thread: starting a process and writing to its
/// stdin are blocking calls, and doing them on the async executor is rule 2
/// broken somewhere nobody would look.
pub async fn bombear(
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
                    .unwrap_or(Resultado::Fallo);
                match result {
                    Resultado::Hecho => close(),
                    other => {
                        let _ = host
                            .dispatch(norte_ui_host::UiAction::HandoffFailed {
                                no_terminal: matches!(other, Resultado::SinPrograma),
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
            Ok(NativeEffect::PickDirectory { desde }) => {
                let host = std::sync::Arc::clone(&host);
                tokio::task::spawn(async move {
                    let chosen = tokio::task::spawn_blocking(move || pick_directory(&desde))
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
                        .unwrap_or_else(|_| Ran::fallo(String::new()));
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
            Ok(effect) => {
                // Without waiting for the result: an `xdg-open` can take
                // seconds to return, and the next gesture from whoever is
                // sitting there does not wait for their PDF to open.
                tokio::task::spawn_blocking(move || ejecutar(&effect));
            }
            // Lagged: some gesture was lost, not a piece of screen. It keeps
            // listening, which is the opposite of what the view does.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Does ONE. Blocks: called from `spawn_blocking`.
#[must_use]
pub fn ejecutar(efecto: &NativeEffect) -> Resultado {
    match efecto {
        NativeEffect::CopyBytes { bytes, .. } => copy_bytes(bytes),
        NativeEffect::OpenPath { path } => open_path(path),
        NativeEffect::OpenTerminal { dir } => terminal(dir),
        // Phase 9: `bombear` handles it, since it is the one that can close
        // the window and notify the host depending on how it turns out. This
        // is only reached if someone calls `ejecutar` by hand, and then the
        // terminal is simply opened.
        NativeEffect::HandoffToTerminal { daemon } => handoff(*daemon),
        NativeEffect::Notify { titulo, cuerpo } => notify(titulo, cuerpo),
        // Fire-and-forget: the comparator opens its own window and this does
        // not wait. The argv arrives already resolved and interpolated from
        // the host; here it is only launched.
        NativeEffect::RunProgram {
            argv,
            cwd,
            detached: true,
            ..
        } => {
            use std::os::unix::ffi::OsStrExt as _;
            let Some((program, args)) = argv.split_first() else {
                return Resultado::SinPrograma;
            };
            let args: Vec<std::ffi::OsString> = args
                .iter()
                .map(|a| std::ffi::OsStr::from_bytes(a).to_owned())
                .collect();
            let cwd = cwd
                .as_deref()
                .map(|d| std::path::Path::new(std::ffi::OsStr::from_bytes(d)));
            launch(
                std::path::Path::new(std::ffi::OsStr::from_bytes(program)),
                &args,
                cwd,
            )
        }
        // `bombear` handles all four, and none of them launches a program
        // here: the `RunProgram` that is AWAITED has to answer, the folder
        // picker has to be ANSWERED with the path, the theme is a catalogue
        // to rebuild, and closing belongs to the event loop. There is nothing
        // to run here.
        NativeEffect::RunProgram { .. }
        | NativeEffect::PickDirectory { .. }
        | NativeEffect::ThemeChanged { .. }
        | NativeEffect::CloseWindow => Resultado::SinPrograma,
    }
}

/// Fires the notification with the first program that exists (#285).
///
/// The text arrives ALREADY composed, translated, masked and bounded: nothing
/// is decided about it here, it is only delivered. A notification that
/// cannot be given is SAID — `SinPrograma` — instead of swallowed: whoever
/// believes they will be notified and has no `notify-send` deserves to know,
/// once.
fn notify(titulo: &str, cuerpo: &str) -> Resultado {
    for argv in norte_frontend::shell::notify_candidates(titulo, cuerpo) {
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
            Ok(st) if st.success() => return Resultado::Hecho,
            // It exists and it failed: the next one is not tried. Two
            // notifications for the same event is worse than none.
            Ok(_) => return Resultado::Fallo,
            // Not on the PATH: try the next one.
            Err(_) => {}
        }
    }
    Resultado::SinPrograma
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
fn pick_directory(desde: &norte_proto::VPath) -> Option<String> {
    // With a REMOTE pane there is no native path to open at, and that blocks
    // nothing: the picker always returns a folder on this machine, and
    // copying from an `sftp://` to a local folder is legitimate. What is lost
    // is the suggestion of where to start, not the operation.
    let native = norte_vfs::native::vpath_to_native(desde).unwrap_or_else(|_| {
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
fn copy_bytes(bytes: &[u8]) -> Resultado {
    match norte_frontend::shell::copy_to_clipboard(bytes) {
        norte_frontend::shell::ClipboardOutcome::Done(_) => Resultado::Hecho,
        norte_frontend::shell::ClipboardOutcome::NoHelper => Resultado::SinPrograma,
        norte_frontend::shell::ClipboardOutcome::Failed => Resultado::Fallo,
    }
}

/// Opens `path` with whichever application the desktop chooses.
fn open_path(path: &norte_proto::VPath) -> Resultado {
    let Ok(native) = norte_vfs::native::vpath_to_native(path) else {
        // The host already checks this; here is the belt: `xdg-open` is not
        // given something that is not on this disk.
        return Resultado::SinPrograma;
    };
    let (program, argv) = norte_frontend::openers::system_opener(&native);
    let Some(path) = norte_frontend::openers::resolve_program(std::ffi::OsStr::new(&program))
    else {
        return Resultado::SinPrograma;
    };
    launch(&path, &argv[1..], None)
}

/// Opens a terminal sitting in `dir`.
fn terminal(dir: &norte_proto::VPath) -> Resultado {
    let Ok(native) = norte_vfs::native::vpath_to_native(dir) else {
        return Resultado::SinPrograma;
    };
    for argv in norte_frontend::shell::terminal_candidates(&native) {
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
    Resultado::SinPrograma
}

/// The HANDOFF to the terminal (phase 9): opens an emulator with
/// `ntc --attach` inside.
///
/// Reuses `terminal_candidates`, the shared list of emulators and their
/// command flag, and sets `ntc` as the program to run: this way the handoff
/// opens the same emulator as `app.terminal`, and a desktop where that one
/// works needs nothing else configured for this one.
///
/// `--daemon` travels if this window carries it, and it has to: the session
/// that was just let go of is the daemon's, and an `ntc` against its embedded
/// core would find nothing.
fn handoff(daemon: bool) -> Resultado {
    let Some(ntc) = norte_frontend::openers::resolve_program(std::ffi::OsStr::new("ntc")) else {
        return Resultado::SinPrograma;
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
    Resultado::SinPrograma
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
    fn fallo(command: String) -> Self {
        Self {
            command,
            output: Vec::new(),
            truncated: false,
            failed: true,
        }
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
    use std::os::unix::ffi::OsStrExt as _;
    let command = argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let Some((program, rest)) = argv.split_first() else {
        return Ran::fallo(command);
    };
    let mut cmd = std::process::Command::new(std::ffi::OsStr::from_bytes(program));
    cmd.args(rest.iter().map(|a| std::ffi::OsStr::from_bytes(a)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(std::ffi::OsStr::from_bytes(d));
    }
    let Ok(mut child) = cmd.spawn() else {
        return Ran::fallo(command);
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
) -> Resultado {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    match cmd.spawn() {
        Ok(_) => Resultado::Hecho,
        Err(_) => Resultado::Fallo,
    }
}

#[cfg(test)]
mod tests {
    use super::{Resultado, ejecutar};
    use norte_ui_host::dto::NativeEffect;

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
            ejecutar(&NativeEffect::OpenPath {
                path: remote.clone()
            }),
            Resultado::SinPrograma
        );
        assert_eq!(
            ejecutar(&NativeEffect::OpenTerminal { dir: remote }),
            Resultado::SinPrograma
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
        let r = ejecutar(&NativeEffect::CopyBytes {
            // A name that is not UTF-8: it travels as-is over STDIN, and the
            // clipboard receives the SAME bytes that open that file.
            bytes: vec![b'/', b't', b'm', b'p', b'/', 0xFF, 0xFE],
            count: 1,
        });
        assert!(matches!(
            r,
            Resultado::Hecho | Resultado::SinPrograma | Resultado::Fallo
        ));
    }
}
