//! Shell integration — the PURE half (design
//! `docs/superpowers/specs/2026-08-10-shell-integration-design.md`).
//!
//! Nothing here touches a terminal or spawns anything, so all of it is
//! unit-testable without a tty. This module carries `--pick` (§B): the
//! picker's byte-exact output; `cd_bytes`/`Shell` (§C): what goes in the
//! `--cd-file` and the wrapper text `norte shell-init` prints; and
//! `login_shell`/`terminal_candidates`/`next_norte_level` (§D/§E): WHICH
//! program a suspension or the GUI launches, and the marker its child
//! inherits.
//!
//! The `*_from` functions take the environment as an ARGUMENT and the thin
//! wrappers read `std::env` and call them. That split is not decoration: a
//! test that set a process-wide env var would race every other test in the
//! same binary, and `std::env::set_var` is `unsafe` since Rust 2024 anyway
//! (rule 5 — this crate forbids `unsafe`).

/// The picker's output: every path's bytes, each followed by a NUL.
///
/// A local (`file://`) path comes out in NATIVE form (`/tmp/a`), because
/// that is what the tool on the other side of the pipe will open — a
/// `norte-vfs-local::vpath_to_native` failure (not `file://`, or an
/// authority that makes it someone else's provider) falls back to the wire
/// form, which is the only lossless thing to say about a location that has
/// no native path at all.
///
/// NUL-terminated, not NUL-separated: a single result is unambiguous on its
/// own and `xargs -0` is happy either way. The bytes are the path's,
/// untouched — a name is bytes (rule 1) and a picker that lossily decodes is
/// a picker that opens the wrong file.
#[must_use]
pub fn pick_bytes(paths: &[norte_proto::VPath]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in paths {
        match norte_vfs::native::vpath_to_native(p) {
            #[cfg(unix)]
            Ok(native) => {
                use std::os::unix::ffi::OsStrExt;
                out.extend_from_slice(native.as_os_str().as_bytes());
            }
            // Windows has no byte-exact `OsStr` accessor: a picker that
            // round-trips arbitrary bytes on that platform is not part of
            // this item (design §B says so explicitly).
            #[cfg(not(unix))]
            Ok(native) => {
                out.extend_from_slice(native.to_string_lossy().into_owned().as_bytes());
            }
            Err(_) => out.extend_from_slice(p.to_wire().as_bytes()),
        }
        out.push(0);
    }
    out
}

/// What to write into the `--cd-file`, or `None` when the pane is not local.
///
/// `Some` is the directory's native bytes with a trailing NUL. `None` means
/// write nothing at all — an empty file tells the wrapper to leave the shell
/// where it is, and a norte that died mid-write can therefore never move a
/// shell to half a path. Whether a pane is local IS whether
/// `norte_vfs::native::vpath_to_native` accepts it: `file://` with no
/// authority, exactly the same test `pick_bytes` uses for its native/wire
/// split — so a `sftp://`/`s3://` pane, or a `file://` one with an
/// authority, is `None` here too.
///
/// Unlike [`pick_bytes`] there is no wire-form fallback: a shell can only
/// `cd` into a real directory on disk, and a wire form is not one.
#[must_use]
pub fn cd_bytes(dir: &norte_proto::VPath) -> Option<Vec<u8>> {
    let native = norte_vfs::native::vpath_to_native(dir).ok()?;
    #[cfg(unix)]
    let mut bytes = {
        use std::os::unix::ffi::OsStrExt;
        native.as_os_str().as_bytes().to_vec()
    };
    // Same Windows caveat as `pick_bytes`: no byte-exact `OsStr` accessor, so
    // this is lossy there. A Windows picker/cd is not part of this item.
    #[cfg(not(unix))]
    let mut bytes = native.to_string_lossy().into_owned().into_bytes();
    bytes.push(0);
    Some(bytes)
}

/// A shell we can emit a cd-on-quit wrapper for (`norte shell-init`, §C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// bash.
    Bash,
    /// zsh.
    Zsh,
    /// fish.
    Fish,
}

/// bash/zsh's wrapper, verbatim (design §C, ONE deviation from its literal
/// text — see below). Reads the cd-file with `IFS= read -r -d ''`, never
/// `$(cat …)` — command substitution cannot carry a NUL and strips trailing
/// newlines, corrupting any directory name that ends in one. Invokes
/// `command ntc`, never bare `ntc`: this function is itself bound to the name
/// `ntc`, so a bare call would recurse forever.
///
/// The design's draft named the exit-code variable `status`, which zsh
/// reserves (it aliases `$status` to `$?` itself): `local status=$?` fails
/// there with "read-only variable: status", caught by actually running zsh
/// against this wrapper rather than only reading the shell code. Renamed to
/// `rc` for both shells, so bash and zsh keep sharing one wrapper body.
const BASH_ZSH_WRAPPER: &str = "\
ntc() {
    local f
    f=\"$(mktemp \"${TMPDIR:-/tmp}/ntc-cd.XXXXXX\")\" || return 1
    command ntc --cd-file \"$f\" \"$@\"
    local rc=$?
    local dir
    IFS= read -r -d '' dir < \"$f\"
    rm -f -- \"$f\"
    if [ -n \"$dir\" ]; then
        cd -- \"$dir\" || return $?
    fi
    return $rc
}
";

/// fish's wrapper, verbatim (design §C). `string split0` is fish's NUL-safe
/// read, the equivalent of bash/zsh's `read -d ''` above; same
/// `command ntc` rule against recursion.
const FISH_WRAPPER: &str = "\
function ntc
    set -l f (mktemp (test -n \"$TMPDIR\"; and echo $TMPDIR; or echo /tmp)/ntc-cd.XXXXXX)
    or return 1
    command ntc --cd-file $f $argv
    set -l status_code $status
    set -l dir (string split0 < $f)
    rm -f -- $f
    if test -n \"$dir[1]\"
        cd -- $dir[1]
    end
    return $status_code
end
";

impl Shell {
    /// `"bash"`/`"zsh"`/`"fish"`, else `None`. What `norte shell-init` and
    /// `norte doctor` parse; case-sensitive on purpose — a shell name is not
    /// user prose, it is one of exactly three tokens a rc file will pass
    /// verbatim.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    /// The wrapper's source, for `eval` (bash/zsh) or `source` (fish).
    #[must_use]
    pub fn wrapper(self) -> &'static str {
        match self {
            Self::Bash | Self::Zsh => BASH_ZSH_WRAPPER,
            Self::Fish => FISH_WRAPPER,
        }
    }
}

/// The environment variable a norte-launched child inherits, one higher than
/// the one norte itself was started with.
///
/// Same idea as `SHLVL`: a user who opens a shell from norte (`app.terminal`)
/// and runs `ntc` inside it has two of them, and without a marker the second
/// quit looks like the first one failing. norte never READS this back into
/// behaviour — the consumer is the user's own prompt, which is exactly where
/// the information is needed.
pub const LEVEL_VAR: &str = "NORTE_LEVEL";

/// The user's interactive shell: `$SHELL`, else `/bin/sh` (`%COMSPEC%`, else
/// `cmd.exe`, on Windows).
///
/// Reads the environment; the decision itself is [`login_shell_from`].
#[must_use]
pub fn login_shell() -> std::path::PathBuf {
    let var = if cfg!(windows) { "COMSPEC" } else { "SHELL" };
    login_shell_from(std::env::var_os(var).as_deref())
}

/// Testable core of [`login_shell`].
///
/// An ABSENT variable and an EMPTY one are the same answer, deliberately:
/// `SHELL=` is what a stripped `env -i` leaves behind, and
/// `Command::new("")` is not an error here — it is a confusing spawn failure
/// several stack frames later, with no hint that the environment was the
/// problem. The fallback is a path POSIX requires to exist.
///
/// A RELATIVE `$SHELL` gets the same answer, and that one is a security
/// decision rather than a convenience (S4 security review, MAJOR-2). The
/// child is spawned with `Command::current_dir` pointing at the directory the
/// user is BROWSING, and on unix `current_dir` is applied before the program
/// is resolved — so `SHELL=bash` with a `.` (or an empty component) anywhere
/// in `PATH` would execute a file called `bash` out of a directory whose
/// contents nobody vouched for: extract a hostile archive, walk into it,
/// press the key. `$SHELL` is conventionally an absolute path out of
/// `/etc/passwd`; refusing anything else costs nothing real and closes that
/// door for good.
///
/// The value is taken as bytes (`OsStr`), never through a lossy `String`: a
/// shell can live under a non-UTF-8 path like anything else (rule 1).
#[must_use]
pub fn login_shell_from(env_shell: Option<&std::ffi::OsStr>) -> std::path::PathBuf {
    let fallback = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };
    match env_shell {
        Some(s) if !s.is_empty() && std::path::Path::new(s).is_absolute() => {
            std::path::PathBuf::from(s)
        }
        _ => std::path::PathBuf::from(fallback),
    }
}

/// The argv that opens `file` in the user's editor (#133).
///
/// `$VISUAL` first, then `$EDITOR`, then a fallback POSIX requires to exist.
/// That order is the convention every editor-launching tool follows, and
/// getting it backwards is how a user who set `VISUAL` for their GUI editor
/// ends up in `vi`.
#[must_use]
pub fn editor_argv(file: &std::path::Path) -> Vec<std::ffi::OsString> {
    editor_argv_from(
        std::env::var_os("VISUAL").as_deref(),
        std::env::var_os("EDITOR").as_deref(),
        file,
    )
}

/// Testable core of [`editor_argv`].
///
/// **The path is its OWN argument and is never interpolated into a command
/// line.** Going through `$SHELL -c "$EDITOR <path>"` would mean quoting a
/// filename that norte treats as bytes — a name with a quote, a newline or a
/// `$` in it either breaks the line or executes part of itself (rule 1 plus
/// the obvious). Passing argv directly means the bytes reach the editor
/// exactly as they are on disk.
///
/// The editor SPEC is split on ASCII whitespace, so `EDITOR="code -w"` works.
/// The cost of that convenience is an editor whose own program path contains a
/// space, which would have to be spelled without one; the trade is worth it
/// because flags in `$EDITOR` are common and spaces in `/usr/bin` are not.
///
/// An absent or empty spec is the same answer, for the same reason
/// [`login_shell_from`] treats them alike: `EDITOR=` is what a stripped
/// environment leaves, and spawning `""` is a confusing failure several frames
/// later rather than an error anyone can read.
///
/// # A relative `$EDITOR` is normal, and that is why the guard is elsewhere
///
/// [`login_shell_from`] refuses a relative `$SHELL` outright, and this
/// function deliberately does NOT do the same (#302): `EDITOR=vim` or
/// `EDITOR="code -w"` is what everybody's shell profile says, while a relative
/// `$SHELL` is a misconfiguration. The hazard is the same either way — the
/// child is spawned with the BROWSED directory as `current_dir`, which unix
/// applies before resolving the program — so the guard sits at the launch
/// instead: `argv[0]` is resolved to an absolute path with
/// [`crate::openers::resolve_program`], which ignores the relative and empty
/// `PATH` entries that make the attack possible, and the child is never given
/// the bare name.
#[must_use]
pub fn editor_argv_from(
    visual: Option<&std::ffi::OsStr>,
    editor: Option<&std::ffi::OsStr>,
    file: &std::path::Path,
) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    #[cfg(unix)]
    fn trocea(spec: &std::ffi::OsStr) -> Vec<OsString> {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
        spec.as_bytes()
            .split(u8::is_ascii_whitespace)
            .filter(|t| !t.is_empty())
            .map(|t| OsString::from_vec(t.to_vec()))
            .collect()
    }
    #[cfg(not(unix))]
    fn trocea(spec: &std::ffi::OsStr) -> Vec<OsString> {
        spec.to_string_lossy()
            .split_ascii_whitespace()
            .map(OsString::from)
            .collect()
    }

    let elegido = [visual, editor]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty())
        .map(trocea)
        .filter(|v| !v.is_empty());
    let mut argv = elegido.unwrap_or_else(|| {
        vec![OsString::from(if cfg!(windows) {
            "notepad.exe"
        } else {
            "vi"
        })]
    });
    argv.push(file.as_os_str().to_os_string());
    argv
}

// El editor SIN fichero —un buffer vacío— vivía aquí, y era la mitad del
// `pane.edit-new` que creaba el fichero fuera de norte: lo creaba el editor al
// guardar, sin política, sin journal y sin undo. Desde #290 el fichero lo crea
// el daemon (`fs.create`) y el editor se abre sobre él, así que lo único que
// se necesita es `editor_argv`. Se retira en vez de dejarse: una función que
// solo sirve para volver a saltarse el journal es una invitación.

/// The argv that runs ONE command line through `shell`, non-interactively.
///
/// The flag is a DECISION and belongs here, not in a frontend (rule 7): POSIX
/// shells take `-c`, `cmd.exe` takes `/C` and PowerShell takes `-Command`. A
/// TUI that hardcodes `-c` gives a Windows user a usage error from `cmd.exe`
/// and a command that never ran (S4 rust review, MAJOR-1).
///
/// The line travels as ONE argument, never split: the shell owns that
/// grammar — pipes, quoting, globs — and any splitting norte did here would
/// be a second, different grammar that disagrees with the one about to parse
/// it.
#[must_use]
pub fn shell_command_argv(shell: &std::path::Path, cmd: &str) -> Vec<std::ffi::OsString> {
    vec![
        shell.as_os_str().to_os_string(),
        std::ffi::OsString::from(command_flag_for(shell)),
        std::ffi::OsString::from(cmd),
    ]
}

/// The "run this one line" flag of a shell, by file name.
///
/// Matching on the file name and not the whole path so `/usr/bin/pwsh` and a
/// bare `pwsh` agree. Anything unrecognised gets the POSIX `-c`, which is the
/// right default on unix and the right guess for a POSIX-ish shell installed
/// on Windows (`bash.exe` from Git for Windows takes `-c`, not `/C`).
///
/// The last component is taken by splitting on BOTH separators rather than
/// through `Path::file_name`, which only knows the host's: a unix build
/// handed `C:\...\cmd.exe` would otherwise see the whole string as one name
/// and answer `-c`. The decision should not depend on which OS is asking.
fn command_flag_for(shell: &std::path::Path) -> &'static str {
    let full = shell.to_string_lossy().to_ascii_lowercase();
    let name = full.rsplit(['/', '\\']).next().unwrap_or(&full);
    match name.trim_end_matches(".exe") {
        "cmd" => "/C",
        "powershell" | "pwsh" => "-Command",
        _ => "-c",
    }
}

/// The directory as a child process can receive it, or `None` when it cannot.
///
/// On unix this is the path unchanged. On Windows it is the point where two
/// requirements of this repository collide (S4 encoding audit, M5):
/// `norte_vfs::native::vpath_to_native` deliberately returns a VERBATIM
/// (`\\?\`) path so that reserved names, trailing dots and spaces, and paths
/// over 260 characters survive at all — and `CreateProcessW`'s
/// `lpCurrentDirectory` does not accept that namespace, nor does `wt -d`.
/// Handing one over either fails the spawn or, worse, opens the terminal
/// somewhere else.
///
/// So the prefix is stripped when stripping is LOSSLESS, and the answer is
/// `None` when it is not — a path that only exists because of the prefix
/// cannot be a child's working directory, and saying so is better than
/// opening a shell in a directory that is not the one on screen. The caller
/// turns `None` into a message.
#[must_use]
pub fn child_cwd(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    #[cfg(not(windows))]
    {
        Some(dir.to_path_buf())
    }
    #[cfg(windows)]
    {
        let text = dir.to_str()?;
        let Some(stripped) = text.strip_prefix(VERBATIM_PREFIX) else {
            return Some(dir.to_path_buf());
        };
        // UNC in verbatim form (`\\?\UNC\server\share`) has no plain spelling
        // that means the same thing to `CreateProcessW`.
        if stripped.len() >= 260 || stripped.starts_with("UNC\\") {
            return None;
        }
        // Without the prefix, Win32 path munging eats a trailing dot or space
        // and reinterprets a reserved device name — the very things the
        // prefix was there to protect.
        for component in stripped.split('\\') {
            if component.is_empty() {
                continue;
            }
            if component.ends_with('.') || component.ends_with(' ') {
                return None;
            }
            let stem = component
                .split_once('.')
                .map_or(component, |(s, _)| s)
                .to_ascii_uppercase();
            if WIN_RESERVED.contains(&stem.as_str()) {
                return None;
            }
        }
        Some(std::path::PathBuf::from(stripped))
    }
}

/// The verbatim prefix `norte_vfs::native::vpath_to_native` puts on every
/// Windows path.
#[cfg(windows)]
const VERBATIM_PREFIX: &str = r"\\?\";

/// The device names Win32 still reinterprets in a non-verbatim path.
#[cfg(windows)]
const WIN_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The terminal emulators probed, in order, when `$TERMINAL` says nothing.
///
/// Deliberately SHORT and unix-only. It is not a compatibility list to grow
/// forever: `$TERMINAL` and `xdg-terminal-exec` are the answers a desktop is
/// supposed to give, and this is the fallback for a desktop that gives
/// neither.
#[cfg(all(unix, not(target_os = "macos")))]
const UNIX_TERMINALS: &[&str] = &[
    "ghostty",
    "kitty",
    "alacritty",
    "wezterm",
    "konsole",
    "gnome-terminal",
    "xterm",
];

/// The cwd argument a known terminal emulator needs, if it needs one.
///
/// Every candidate is also spawned with `current_dir(dir)` set, which is what
/// makes `xterm` and `$TERMINAL` land in the right place. These flags exist
/// for the emulators that do NOT honour the launching process's cwd: a
/// `gnome-terminal` or `konsole` served by an already-running instance takes
/// the SERVER's cwd, so the child would open in the user's home and look like
/// norte simply ignored the pane.
///
/// Only members of [`UNIX_TERMINALS`] appear here — a closed set whose flags
/// we can state. `$TERMINAL` is an arbitrary program and gets no flag guessed
/// at it: an unknown flag does not degrade, it stops the terminal opening at
/// all.
#[cfg(all(unix, not(target_os = "macos")))]
fn cwd_args(program: &str, dir: &std::path::Path) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    // Byte-exact concatenation: `OsString::push` never goes through `str`, so
    // a directory whose name is not UTF-8 survives into the argv (rule 1).
    let glued = |flag: &str| {
        let mut s = OsString::from(flag);
        s.push(dir.as_os_str());
        vec![s]
    };
    let separate = |flag: &str| vec![OsString::from(flag), OsString::from(dir.as_os_str())];
    match program {
        "ghostty" | "gnome-terminal" => glued("--working-directory="),
        "kitty" => glued("--directory="),
        "alacritty" => separate("--working-directory"),
        "wezterm" => vec![
            OsString::from("start"),
            OsString::from("--cwd"),
            OsString::from(dir.as_os_str()),
        ],
        "konsole" => separate("--workdir"),
        // `xterm` has no such flag and inherits the cwd, which is the case
        // `current_dir` already covers.
        _ => Vec::new(),
    }
}

/// The bytes to put on the clipboard for `paths`: one per line, no trailing
/// newline.
///
/// Same spelling rule as [`pick_bytes`] — native form where there is one,
/// because that is what the tool on the other side will open, and the wire
/// form where there is not, because that is the only true thing to say about
/// a location that is not on this disk. The separator is a newline and not a
/// NUL: this goes to a human's clipboard, and every paste target in existence
/// splits on newlines.
///
/// BYTES, and never a `String`: a name is bytes (rule 1), and a path decoded
/// with replacement characters pastes as a path that opens something else.
///
/// ```
/// use norte_frontend::shell::clipboard_bytes;
/// use norte_proto::VPath;
///
/// let a = VPath::parse("file:///tmp/a").expect("vpath");
/// let b = VPath::parse("file:///tmp/b").expect("vpath");
/// assert_eq!(clipboard_bytes(&[a, b]), b"/tmp/a\n/tmp/b");
/// ```
#[must_use]
pub fn clipboard_bytes(paths: &[norte_proto::VPath]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        match norte_vfs::native::vpath_to_native(p) {
            #[cfg(unix)]
            Ok(native) => {
                use std::os::unix::ffi::OsStrExt;
                out.extend_from_slice(native.as_os_str().as_bytes());
            }
            #[cfg(not(unix))]
            Ok(native) => out.extend_from_slice(native.to_string_lossy().as_bytes()),
            Err(_) => out.extend_from_slice(p.to_wire().as_bytes()),
        }
    }
    out
}

/// `true` if this location has a native path — i.e. it is on THIS filesystem.
///
/// What it gates: an `xdg-open` cannot be handed an `sftp://`, and a terminal
/// has nowhere to sit inside one. Answering "no" is what lets the caller say
/// so instead of opening something else.
///
/// ```
/// use norte_frontend::shell::is_local;
/// use norte_proto::VPath;
///
/// assert!(is_local(&VPath::parse("file:///tmp").expect("vpath")));
/// assert!(!is_local(&VPath::parse("sftp://host/tmp").expect("vpath")));
/// ```
#[must_use]
pub fn is_local(path: &norte_proto::VPath) -> bool {
    norte_vfs::native::vpath_to_native(path).is_ok()
}

/// Los programas de aviso del ESCRITORIO que sabemos invocar, en orden
/// (#285).
///
/// Misma forma que [`directory_picker_candidates`] y por lo mismo: una lista,
/// sin sondear el `PATH`, y quien ejecuta prueba en orden.
///
/// El texto va como ARGUMENTO y nunca dentro de una línea de comandos. Aquí
/// eso importa el doble: el cuerpo lleva un nombre de fichero, y un nombre con
/// una comilla o un `$` o rompe la línea o ejecuta parte de sí mismo.
///
/// ```
/// use norte_frontend::shell::notify_candidates;
/// let cands = notify_candidates("norte", "hecho");
/// for argv in &cands {
///     assert!(argv.len() >= 3, "programa, título y cuerpo van separados");
/// }
/// ```
#[must_use]
pub fn notify_candidates(titulo: &str, cuerpo: &str) -> Vec<Vec<std::ffi::OsString>> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        vec![
            vec![
                "notify-send".into(),
                // Que el aviso se pueda cerrar y no se apile: norte manda uno
                // por evento, no un flujo.
                "--app-name=norte".into(),
                titulo.into(),
                cuerpo.into(),
            ],
            vec!["kdialog".into(), "--passivepopup".into(), {
                let mut s = std::ffi::OsString::from(titulo);
                s.push("\n");
                s.push(cuerpo);
                s
            }],
        ]
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = (titulo, cuerpo);
        Vec::new()
    }
}

/// Los selectores de carpeta del ESCRITORIO que sabemos invocar, en orden
/// (#284).
///
/// Una LISTA y no una elección, por lo mismo que [`terminal_candidates`]:
/// elegir pide sondear el `PATH` y esta función no hace I/O. Vacía significa
/// que aquí no hay ninguno, y eso se DICE en vez de tragárselo — «no pasó
/// nada» sobre un selector que nunca se abrió es lo que deja a alguien
/// esperando una ventana.
///
/// Cada candidato imprime la carpeta elegida por `stdout` y sale con código
/// distinto de cero si se cierra sin elegir, que es lo que hace que cancelar
/// y elegir se distingan sin analizar texto.
///
/// El conjunto es CERRADO —zenity, kdialog, yad— porque los argumentos
/// difieren en cada uno, y adivinar los de un programa desconocido es cómo se
/// acaba abriendo un selector de FICHEROS donde se pedía uno de carpetas.
///
/// ```
/// use norte_frontend::shell::directory_picker_candidates;
/// let cands = directory_picker_candidates(std::path::Path::new("/tmp"));
/// // En una máquina sin ninguno, la lista está vacía y quien llama lo dice.
/// for argv in &cands {
///     assert!(!argv.is_empty(), "un candidato sin programa no se lanza");
/// }
/// ```
#[must_use]
pub fn directory_picker_candidates(desde: &std::path::Path) -> Vec<Vec<std::ffi::OsString>> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let d = desde.as_os_str().to_os_string();
        // La `/` final es lo que hace que zenity y yad ENTREN en el
        // directorio, en vez de dejarlo señalado desde el padre.
        let arranque = || {
            let mut s = std::ffi::OsString::from("--filename=");
            s.push(&d);
            s.push("/");
            s
        };
        vec![
            vec![
                "zenity".into(),
                "--file-selection".into(),
                "--directory".into(),
                arranque(),
            ],
            vec!["kdialog".into(), "--getexistingdirectory".into(), d.clone()],
            vec![
                "yad".into(),
                "--file".into(),
                "--directory".into(),
                arranque(),
            ],
        ]
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = desde;
        Vec::new()
    }
}

/// El `VPath` de una ruta NATIVA que llega de fuera — el selector de carpetas
/// del escritorio (#284) —, o `None` si no se puede nombrar.
///
/// Va aquí y no en el host porque quien recibe esa ruta es un frontend, y la
/// conversión tiene una regla que no se puede reinventar: **los bytes son los
/// bytes**. Un nombre de fichero no es UTF-8 garantizado (regla 1), y una ruta
/// que se decodifique con pérdida por el camino abre otro fichero.
///
/// ```
/// use norte_frontend::shell::vpath_de_ruta_nativa;
/// assert!(vpath_de_ruta_nativa("/tmp").is_some());
/// // Una ruta relativa no nombra nada sin un «desde», así que se rehúsa.
/// assert!(vpath_de_ruta_nativa("tmp").is_none());
/// ```
#[must_use]
pub fn vpath_de_ruta_nativa(nativa: &str) -> Option<norte_proto::VPath> {
    let p = std::path::Path::new(nativa);
    if !p.is_absolute() {
        return None;
    }
    norte_vfs::native::vpath_from_native(p).ok()
}

/// El directorio del usuario como `VPath`, o la raíz local si el entorno no
/// lo dice: el destino de última instancia de un panel que se queda sin
/// sitio (`pane.disconnect`, [`crate::nav::regreso_tras_desconectar`]).
///
/// La raíz y no un error: un destino que no existe dejaría el panel mirando
/// una conexión cerrada, que es lo único inaceptable ahí.
///
/// Toma el `Path` ENTERO, sin pasar por `to_str()`: un `$HOME` que no sea
/// UTF-8 es un home perfectamente válido (regla 1), y decodificarlo con
/// pérdida mandaba al usuario a `/` sin decir por qué.
///
/// **Se llama desde contexto async y se acepta a sabiendas**: sin `$HOME`,
/// `home_dir` cae a `getpwuid_r`, que puede acabar en NSS (`/etc/passwd`, o
/// LDAP en una máquina con directorio de red). No va a `spawn_blocking` porque
/// el caso es el de una sesión sin `$HOME` —donde ya nada del entorno es
/// normal— y envolverlo obligaría a hacer async una decisión que los dos
/// frontends toman en medio de pintar. Si alguna vez cuelga, es aquí.
///
/// ```
/// use norte_frontend::shell::home_vpath;
/// // Siempre nombra algo: con `$HOME` o sin él.
/// assert_eq!(home_vpath().scheme(), "file");
/// ```
#[must_use]
pub fn home_vpath() -> norte_proto::VPath {
    std::env::home_dir()
        .and_then(|h| norte_vfs::native::vpath_from_native(&h).ok())
        .unwrap_or_else(|| {
            norte_proto::VPath::parse("file:///")
                .unwrap_or_else(|_| unreachable!("`file:///` parsea"))
        })
}

/// Every argv worth trying, in order, to put text on the system clipboard —
/// for a frontend that has no terminal to ask (the GUI, task 6.5).
///
/// A LIST, like [`terminal_candidates`], and for the same reason: choosing
/// needs a PATH probe and this function does no I/O. Empty means nothing
/// plausible exists here, which the caller REPORTS rather than swallowing —
/// "copied" over an empty clipboard is the kind of lie that is only found out
/// when the paste goes somewhere else.
///
/// The text always goes on the helper's STDIN, never in the argv. Two
/// reasons, and the second is the one that matters: a path is BYTES (rule 1)
/// and an argv is not a good place for arbitrary ones, and a path that starts
/// with `-` would otherwise be read as a flag by whichever helper is
/// installed.
///
/// Reads the environment; the decision is [`clipboard_candidates_from`].
#[must_use]
pub fn clipboard_candidates() -> Vec<Vec<std::ffi::OsString>> {
    clipboard_candidates_from(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os("DISPLAY").as_deref(),
    )
}

/// Testable core of [`clipboard_candidates`].
///
/// On unix the session type decides the ORDER and not the membership: a
/// Wayland session usually still has `xclip` working through `XWayland`, and a
/// user who has one and not the other should not be told there is no
/// clipboard. What the environment buys is trying the native one first.
///
/// macOS is `pbcopy` and Windows is `clip.exe`; both ship with the system, so
/// there is nothing to choose between.
///
/// ```
/// use norte_frontend::shell::clipboard_candidates_from;
/// use std::ffi::OsStr;
///
/// let wayland = clipboard_candidates_from(Some(OsStr::new("wayland-0")), None);
/// # #[cfg(all(unix, not(target_os = "macos")))]
/// assert_eq!(wayland.first().and_then(|a| a.first()), Some(&"wl-copy".into()));
/// // Nothing declared: the list is still offered — a helper can be there
/// // without either variable, and probing is the caller's job anyway.
/// assert!(!clipboard_candidates_from(None, None).is_empty());
/// ```
#[must_use]
pub fn clipboard_candidates_from(
    wayland: Option<&std::ffi::OsStr>,
    x11: Option<&std::ffi::OsStr>,
) -> Vec<Vec<std::ffi::OsString>> {
    use std::ffi::OsString;
    let argv = |parts: &[&str]| -> Vec<OsString> { parts.iter().map(OsString::from).collect() };
    if cfg!(target_os = "macos") {
        return vec![argv(&["pbcopy"])];
    }
    if cfg!(target_os = "windows") {
        return vec![argv(&["clip.exe"])];
    }
    let wl = vec![argv(&["wl-copy"])];
    let x = vec![
        argv(&["xclip", "-selection", "clipboard"]),
        argv(&["xsel", "--clipboard", "--input"]),
    ];
    // Wayland primero solo si la sesión lo declara; si no, X11 primero. Los
    // dos conjuntos se ofrecen siempre: XWayland es lo normal, y decirle a
    // quien tiene `xclip` que no hay portapapeles sería falso.
    let wayland_primero = wayland.is_some_and(|v| !v.is_empty());
    let x11_primero = !wayland_primero && x11.is_some_and(|v| !v.is_empty());
    let mut out = Vec::new();
    if wayland_primero || !x11_primero {
        out.extend(wl.clone());
        out.extend(x.clone());
    } else {
        out.extend(x);
        out.extend(wl);
    }
    out
}

/// Every argv worth trying, in order, to open a terminal emulator sitting in
/// `dir` — for a frontend that cannot suspend (the GUI, §E).
///
/// A LIST and not one answer because choosing needs a PATH probe, and this
/// function does no I/O (rule 2: the caller probes with
/// [`crate::openers::program_available`] off the UI thread). Empty means
/// nothing plausible exists on this platform, which the caller reports rather
/// than swallowing.
///
/// Reads the environment; the decision itself is
/// [`terminal_candidates_from`].
#[must_use]
pub fn terminal_candidates(dir: &std::path::Path) -> Vec<Vec<std::ffi::OsString>> {
    terminal_candidates_from(std::env::var_os("TERMINAL").as_deref(), dir)
}

/// Testable core of [`terminal_candidates`].
///
/// Order on unix: `$TERMINAL` (the user's explicit answer, which beats every
/// probe), then `xdg-terminal-exec` (the desktop's own answer), then the
/// crate-private `UNIX_TERMINALS` probe list — a closed set, because the cwd
/// flag differs per emulator and guessing one is how you launch a terminal in
/// the wrong directory. macOS is `open -a Terminal <dir>`, which is the
/// desktop's answer and the only one. Windows is `wt` then `cmd`.
#[must_use]
pub fn terminal_candidates_from(
    env_terminal: Option<&std::ffi::OsStr>,
    dir: &std::path::Path,
) -> Vec<Vec<std::ffi::OsString>> {
    use std::ffi::OsString;
    let mut out: Vec<Vec<OsString>> = Vec::new();
    // `$TERMINAL` first on every platform: an explicit answer is never
    // overruled by a probe. Empty is treated as unset, like `$SHELL`.
    if let Some(t) = env_terminal.filter(|t| !t.is_empty()) {
        let mut argv = vec![OsString::from(t)];
        // `TERMINAL=gnome-terminal` is an ordinary setting, and it needs the
        // same cwd flag the probe list would have given it (S4 rust review,
        // m1): without it the user hits exactly the server-owned-cwd bug
        // `cwd_args` exists to prevent, because their configuration took the
        // one branch that skipped it. Matched on the FILE NAME, so
        // `/usr/bin/konsole` counts; anything not on the closed list still
        // gets nothing guessed at it.
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if let Some(known) = std::path::Path::new(t)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .filter(|n| UNIX_TERMINALS.contains(n))
            {
                argv.extend(cwd_args(known, dir));
            }
        }
        out.push(argv);
    }
    #[cfg(target_os = "macos")]
    {
        out.push(vec![
            OsString::from("open"),
            OsString::from("-a"),
            OsString::from("Terminal"),
            OsString::from(dir.as_os_str()),
        ]);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        out.push(vec![OsString::from("xdg-terminal-exec")]);
        for program in UNIX_TERMINALS {
            let mut argv = vec![OsString::from(*program)];
            argv.extend(cwd_args(program, dir));
            out.push(argv);
        }
    }
    #[cfg(windows)]
    {
        out.push(vec![
            OsString::from("wt"),
            OsString::from("-d"),
            OsString::from(dir.as_os_str()),
        ]);
        out.push(vec![OsString::from("cmd")]);
    }
    // `dir` is genuinely unused on a platform with no branch above; naming it
    // keeps the signature stable rather than cfg-ing the parameter itself.
    let _ = dir;
    out
}

/// The FIRST candidate of [`terminal_candidates`], with no PATH probe.
///
/// `None` when the platform offers none. This is the documented "what would
/// we run, absent any probe" accessor and nothing in norte calls it: the GUI
/// uses the full list, because reporting "nothing found" honestly means
/// naming everything that was tried (S4 rust review, m2 — kept deliberately,
/// as the named first-choice question, rather than left as an accident).
#[must_use]
pub fn terminal_argv(dir: &std::path::Path) -> Option<Vec<std::ffi::OsString>> {
    terminal_candidates(dir).into_iter().next()
}

/// Testable core of [`terminal_argv`].
#[must_use]
pub fn terminal_argv_from(
    env_terminal: Option<&std::ffi::OsStr>,
    dir: &std::path::Path,
) -> Option<Vec<std::ffi::OsString>> {
    terminal_candidates_from(env_terminal, dir)
        .into_iter()
        .next()
}

/// The value of [`LEVEL_VAR`] a child should get.
///
/// Reads the environment; the decision itself is [`next_norte_level_from`].
#[must_use]
pub fn next_norte_level() -> String {
    next_norte_level_from(std::env::var_os(LEVEL_VAR).as_deref())
}

/// Testable core of [`next_norte_level`].
///
/// Anything that is not a plain decimal `u32` counts as zero, so the child
/// gets `"1"`. That is the only safe reading: the variable is inherited from
/// whatever launched norte, so it is UNTRUSTED input — a negative number, a
/// 4 GiB string, `1; rm -rf /`, or a non-UTF-8 byte sequence must all produce
/// a plain small decimal rather than propagating. Saturating at
/// [`u32::MAX`] rather than wrapping: a counter that goes back to zero after
/// enough nesting is a counter that lies, and an overflow would panic in
/// debug.
#[must_use]
pub fn next_norte_level_from(current: Option<&std::ffi::OsStr>) -> String {
    let n: u32 = current
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    n.saturating_add(1).to_string()
}

#[cfg(test)]
mod tests {
    /// #133: `$VISUAL` manda sobre `$EDITOR`, y la ruta va SIEMPRE como su
    /// propio argumento — jamás interpolada en una línea de comandos, que es
    /// como un nombre con una comilla acaba ejecutando parte de sí mismo.
    #[test]
    fn el_editor_sale_de_visual_luego_de_editor_y_la_ruta_va_aparte() {
        use std::ffi::OsStr;
        let f = std::path::Path::new("/tmp/a b.txt");
        assert_eq!(
            editor_argv_from(Some(OsStr::new("hx")), Some(OsStr::new("nano")), f),
            [OsStr::new("hx"), OsStr::new("/tmp/a b.txt")]
        );
        assert_eq!(
            editor_argv_from(None, Some(OsStr::new("nano")), f),
            [OsStr::new("nano"), OsStr::new("/tmp/a b.txt")]
        );
    }

    /// Un spec con banderas se trocea: `EDITOR="code -w"` es lo normal.
    #[test]
    fn un_editor_con_banderas_se_trocea() {
        use std::ffi::OsStr;
        let f = std::path::Path::new("/x");
        assert_eq!(
            editor_argv_from(Some(OsStr::new("code  -w")), None, f),
            [OsStr::new("code"), OsStr::new("-w"), OsStr::new("/x")]
        );
    }

    /// Ausente y VACÍO son la misma respuesta, como en `login_shell_from`:
    /// `EDITOR=` es lo que deja un entorno pelado, y lanzar `""` es un fallo
    /// confuso tres marcos más abajo en vez de un error que alguien pueda leer.
    #[test]
    fn sin_editor_hay_un_fallback_que_existe() {
        use std::ffi::OsStr;
        let f = std::path::Path::new("/x");
        let esperado: &str = if cfg!(windows) { "notepad.exe" } else { "vi" };
        assert_eq!(
            editor_argv_from(None, None, f),
            [OsStr::new(esperado), OsStr::new("/x")]
        );
        assert_eq!(
            editor_argv_from(Some(OsStr::new("")), Some(OsStr::new("   ")), f),
            [OsStr::new(esperado), OsStr::new("/x")]
        );
    }

    /// Un nombre que NO es UTF-8 llega al editor byte a byte (regla 1).
    #[cfg(unix)]
    #[test]
    fn un_nombre_no_utf8_llega_intacto_al_editor() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;
        let crudo = OsStr::from_bytes(b"/tmp/raro\xff.txt");
        let argv = editor_argv_from(Some(OsStr::new("nano")), None, std::path::Path::new(crudo));
        assert_eq!(argv[1].as_bytes(), b"/tmp/raro\xff.txt");
    }

    use super::*;
    use norte_proto::VPath;

    /// NUL-TERMINATED, not NUL-separated: one result is unambiguous and
    /// `xargs -0` is happy either way. The bytes are the path's, untouched —
    /// a name is bytes (rule 1) and a picker that lossily decodes is a picker
    /// that opens the wrong file.
    #[test]
    fn pick_bytes_terminates_every_path_with_nul() {
        let a = VPath::parse("file:///tmp/a").unwrap();
        let b = VPath::parse("file:///tmp/b").unwrap();
        assert_eq!(pick_bytes(&[a, b]), b"/tmp/a\0/tmp/b\0".to_vec());
    }

    /// A remote path has no native form, so what comes out is the wire form —
    /// which is what the tool asked norte to pick.
    #[test]
    fn pick_bytes_of_a_remote_path_is_its_wire_form() {
        let p = VPath::parse("sftp://host/x").unwrap();
        assert_eq!(pick_bytes(&[p]), b"sftp://host/x\0".to_vec());
    }

    /// Nothing selected is an empty output, never a stray NUL: a consumer
    /// that reads one empty record would open the current directory.
    #[test]
    fn pick_bytes_of_nothing_is_nothing() {
        assert!(pick_bytes(&[]).is_empty());
    }

    /// A directory is bytes and the file is NUL-delimited, so a name ending
    /// in a newline survives — `$(...)` would eat it, which is why the
    /// wrapper reads a file instead.
    #[test]
    fn cd_bytes_are_the_raw_directory_plus_a_nul() {
        let p = VPath::parse("file:///tmp/we%0Aird").unwrap();
        assert_eq!(cd_bytes(&p), Some(b"/tmp/we\nird\0".to_vec()));
    }

    /// Not `file://` writes NOTHING: there is no local cwd that corresponds
    /// to `sftp://host/x`, and an empty file is the wrapper's signal to leave
    /// the shell where it is.
    #[test]
    fn cd_bytes_of_a_remote_pane_are_nothing() {
        assert_eq!(cd_bytes(&VPath::parse("sftp://host/x").unwrap()), None);
        assert_eq!(cd_bytes(&VPath::parse("s3://b/k").unwrap()), None);
    }

    /// The wrapper must call the BINARY, not itself. A function named `ntc`
    /// that runs `ntc` is infinite recursion, and it is the one way this can
    /// fail catastrophically — so it is pinned for all three shells.
    #[test]
    fn every_wrapper_calls_the_binary_not_the_function() {
        for sh in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let w = sh.wrapper();
            assert!(w.contains("command ntc"), "{sh:?} recurses: {w}");
        }
    }

    /// No wrapper may read the cd-file with command substitution: it cannot
    /// carry a NUL and it strips trailing newlines.
    #[test]
    fn no_wrapper_uses_command_substitution_on_the_cd_file() {
        for sh in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let w = sh.wrapper();
            assert!(!w.contains("$(cat"), "{sh:?} uses $(cat …)");
            assert!(!w.contains("(cat "), "{sh:?} uses (cat …)");
        }
    }

    /// `$SHELL` wins; without it, something that certainly exists. Never an
    /// empty argv — `Command::new("")` is a confusing spawn error much later.
    #[test]
    fn login_shell_falls_back_to_a_real_shell() {
        assert_eq!(
            login_shell_from(Some("/bin/zsh".as_ref())),
            std::path::PathBuf::from("/bin/zsh")
        );
        assert!(!login_shell_from(None).as_os_str().is_empty());
        assert!(
            !login_shell_from(Some("".as_ref())).as_os_str().is_empty(),
            "`SHELL=` is what `env -i` leaves behind, and it is not a shell"
        );
    }

    /// A RELATIVE `$SHELL` is refused (S4 security review, MAJOR-2): the
    /// child is spawned with the BROWSED directory as cwd, and on unix that
    /// cwd is applied before the program is resolved — so `SHELL=bash` plus a
    /// `.` in `PATH` runs a `bash` out of whatever the user just walked into.
    #[test]
    fn a_relative_login_shell_is_refused_not_resolved_against_the_browsed_dir() {
        let fallback = login_shell_from(None);
        for relativo in ["bash", "./bash", "../bin/bash", "bin/sh"] {
            assert_eq!(
                login_shell_from(Some(relativo.as_ref())),
                fallback,
                "{relativo:?} must not become a program looked up next to the user's files"
            );
        }
        assert_ne!(
            login_shell_from(Some("/bin/zsh".as_ref())),
            fallback,
            "an absolute one is still honoured"
        );
    }

    /// The one-shot flag is a per-shell DECISION, and it lives here rather
    /// than in a frontend: `cmd.exe -c` is a usage error and a command that
    /// never ran.
    #[test]
    fn the_one_shot_flag_follows_the_shell() {
        let flag = |p: &str| {
            shell_command_argv(std::path::Path::new(p), "echo hi")[1]
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(flag("/bin/sh"), "-c");
        assert_eq!(flag("/usr/bin/zsh"), "-c");
        assert_eq!(flag(r"C:\Windows\System32\cmd.exe"), "/C");
        assert_eq!(flag("CMD.EXE"), "/C");
        assert_eq!(flag("/usr/bin/pwsh"), "-Command");
        assert_eq!(
            flag("/usr/bin/bash.exe"),
            "-c",
            "a POSIX shell on Windows is still a POSIX shell"
        );
        // And the line is ONE argument, whatever it contains.
        let a = shell_command_argv(std::path::Path::new("/bin/sh"), "ls | wc -l && echo 'a b'");
        assert_eq!(a.len(), 3);
        assert_eq!(a[2], std::ffi::OsString::from("ls | wc -l && echo 'a b'"));
    }

    /// On unix a directory is handed to a child unchanged; the interesting
    /// half of [`child_cwd`] is Windows-only and tested there.
    #[cfg(not(windows))]
    #[test]
    fn a_unix_directory_reaches_the_child_unchanged() {
        let d = std::path::Path::new("/tmp/x");
        assert_eq!(child_cwd(d).as_deref(), Some(d));
    }

    /// Windows: the verbatim prefix is stripped when that is lossless, and
    /// REFUSED when the path only exists because of it (S4 encoding audit,
    /// M5) — `CreateProcessW` and `wt -d` do not speak that namespace, and a
    /// silently munged path opens the terminal somewhere else.
    #[cfg(windows)]
    #[test]
    fn a_windows_verbatim_path_is_stripped_or_refused_never_munged() {
        use std::path::{Path, PathBuf};
        assert_eq!(
            child_cwd(Path::new(r"\\?\C:\proj")),
            Some(PathBuf::from(r"C:\proj"))
        );
        // Trailing dot/space and reserved names: Win32 would rewrite them.
        assert_eq!(child_cwd(Path::new(r"\\?\C:\proj\build.")), None);
        assert_eq!(child_cwd(Path::new(r"\\?\C:\proj\build ")), None);
        assert_eq!(child_cwd(Path::new(r"\\?\C:\CON")), None);
        assert_eq!(child_cwd(Path::new(r"\\?\C:\con.txt")), None);
        // Over MAX_PATH, and verbatim UNC.
        let largo = format!(r"\\?\C:\{}", "x".repeat(300));
        assert_eq!(child_cwd(Path::new(&largo)), None);
        assert_eq!(child_cwd(Path::new(r"\\?\UNC\server\share")), None);
    }

    /// A shell path is BYTES like any other path (rule 1): a `$SHELL` that is
    /// not UTF-8 must come out unchanged, not lossily decoded into a program
    /// that does not exist.
    #[cfg(unix)]
    #[test]
    fn login_shell_keeps_non_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"/opt/sh\xFF/bash");
        assert_eq!(
            login_shell_from(Some(raw)).as_os_str().as_bytes(),
            b"/opt/sh\xFF/bash"
        );
    }

    /// The GUI has no host terminal to suspend, so it launches one.
    /// `$TERMINAL` is the user's explicit answer and beats every probe.
    #[test]
    fn terminal_argv_prefers_the_configured_terminal() {
        let dir = std::path::Path::new("/tmp/x");
        let argv = terminal_argv_from(Some("kitty".as_ref()), dir).expect("configured");
        assert_eq!(argv[0], std::ffi::OsString::from("kitty"));
        // An arbitrary `$TERMINAL` gets no flag guessed at it — the cwd
        // travels through `current_dir`. (One we KNOW does get its flag; see
        // `a_configured_terminal_we_know_still_gets_its_cwd_flag`.)
        let argv = terminal_argv_from(Some("myterm".as_ref()), dir).expect("configured");
        assert_eq!(argv, vec![std::ffi::OsString::from("myterm")]);
    }

    /// A `$TERMINAL` that NAMES one of the emulators we know still gets its
    /// cwd flag (S4 rust review, m1): otherwise the one branch a user's own
    /// configuration takes is the branch that skips the fix.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_configured_terminal_we_know_still_gets_its_cwd_flag() {
        let dir = std::path::Path::new("/tmp/x");
        let argv = terminal_argv_from(Some("gnome-terminal".as_ref()), dir).expect("configured");
        assert_eq!(
            argv[1],
            std::ffi::OsString::from("--working-directory=/tmp/x"),
            "a gnome-terminal served by a running instance takes the SERVER's cwd"
        );
        // Full path, same answer.
        let argv = terminal_argv_from(Some("/usr/bin/konsole".as_ref()), dir).expect("configured");
        assert_eq!(argv[1], std::ffi::OsString::from("--workdir"));
        // And something we do not know still gets nothing guessed at it.
        let argv = terminal_argv_from(Some("myterm".as_ref()), dir).expect("configured");
        assert_eq!(argv.len(), 1);
    }

    /// An empty `$TERMINAL` is an unset one (same rule as `$SHELL`), so the
    /// probe list still gets its turn instead of the list starting with `""`.
    #[test]
    fn an_empty_terminal_variable_does_not_become_a_candidate() {
        let dir = std::path::Path::new("/tmp/x");
        for c in terminal_candidates_from(Some("".as_ref()), dir) {
            assert!(!c[0].is_empty(), "an empty program name is not a candidate");
        }
    }

    /// Every candidate is a non-empty argv whose `[0]` is the program to
    /// probe on the PATH, and the list is ordered `$TERMINAL` first.
    #[test]
    fn the_candidate_list_is_ordered_and_never_carries_an_empty_argv() {
        let dir = std::path::Path::new("/tmp/x");
        let all = terminal_candidates_from(Some("myterm".as_ref()), dir);
        assert_eq!(all[0], vec![std::ffi::OsString::from("myterm")]);
        assert!(all.iter().all(|c| !c.is_empty()));
        assert!(
            all.len() > 1,
            "an unset `$TERMINAL` must still leave something to try"
        );
    }

    /// The emulators that ignore the launching process's cwd are handed the
    /// directory explicitly, BYTE-EXACTLY: a name that is not UTF-8 reaches
    /// the argv unchanged rather than through a lossy `String` (rule 1).
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_known_emulator_gets_the_directory_byte_for_byte() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let dir = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/w\xFFird".to_vec()));
        let all = terminal_candidates_from(None, &dir);
        let konsole = all
            .iter()
            .find(|c| c[0] == "konsole")
            .expect("konsole is on the probe list");
        assert_eq!(konsole[1], std::ffi::OsString::from("--workdir"));
        assert_eq!(konsole[2].as_bytes(), b"/tmp/w\xFFird");
        let gnome = all
            .iter()
            .find(|c| c[0] == "gnome-terminal")
            .expect("gnome-terminal is on the probe list");
        assert_eq!(gnome[1].as_bytes(), b"--working-directory=/tmp/w\xFFird");
    }

    /// `NORTE_LEVEL` is INHERITED, so it is untrusted input. Nothing a parent
    /// process can put in it may produce anything but a small decimal.
    #[test]
    fn the_level_marker_survives_a_hostile_value() {
        assert_eq!(next_norte_level_from(None), "1");
        assert_eq!(next_norte_level_from(Some("".as_ref())), "1");
        assert_eq!(next_norte_level_from(Some("2".as_ref())), "3");
        assert_eq!(next_norte_level_from(Some("-1".as_ref())), "1");
        assert_eq!(next_norte_level_from(Some("1e9".as_ref())), "1");
        assert_eq!(next_norte_level_from(Some(" 2 ".as_ref())), "1");
        assert_eq!(next_norte_level_from(Some("1; rm -rf /".as_ref())), "1");
        assert_eq!(
            next_norte_level_from(Some("99999999999999999999".as_ref())),
            "1",
            "beyond u32 does not parse, so it is not a level"
        );
        assert_eq!(
            next_norte_level_from(Some(u32::MAX.to_string().as_ref())),
            u32::MAX.to_string(),
            "saturates rather than wrapping to zero or panicking in debug"
        );
    }

    /// Non-UTF-8 in the marker is not a level either — and must not panic.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_level_marker_restarts_the_count() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"\xFF\xFE");
        assert_eq!(next_norte_level_from(Some(raw)), "1");
    }

    /// `Shell::parse` accepts exactly the three names `shell-init`/`doctor`
    /// document, and nothing else — not a capitalised variant, not a path.
    #[test]
    fn shell_parse_is_exact_and_closed() {
        assert_eq!(Shell::parse("bash"), Some(Shell::Bash));
        assert_eq!(Shell::parse("zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::parse("fish"), Some(Shell::Fish));
        assert_eq!(Shell::parse("Bash"), None);
        assert_eq!(Shell::parse("sh"), None);
        assert_eq!(Shell::parse(""), None);
    }
}

/// Cómo acabó un intento de escribir en el portapapeles del sistema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardOutcome {
    /// Escrito, con el helper que lo hizo (para poder DECIR cuál fue).
    Done(String),
    /// No hay ningún helper instalado. NO es un fallo: en una sesión por SSH
    /// es lo normal, y un terminal todavía tiene OSC 52 ([`osc52`]).
    NoHelper,
    /// Había helper y falló al escribir.
    Failed,
}

/// Escribe `bytes` en el portapapeles con el primer helper que EXISTA.
///
/// Compartido por los dos frontends (#286): la ventana no tiene terminal al
/// que pedírselo, y el terminal sí — pero cuando hay `wl-copy` o `xclip`
/// delante, usarlo es mejor que OSC 52, porque el helper CONTESTA y la
/// secuencia de escape no.
///
/// El texto va siempre por STDIN y nunca en el argv: una ruta es BYTES
/// (regla 1), y una que empiece por `-` la leería como flag el helper que
/// toque.
#[must_use]
pub fn copy_to_clipboard(bytes: &[u8]) -> ClipboardOutcome {
    use std::io::Write as _;
    for argv in clipboard_candidates() {
        let Some(programa) = argv.first() else {
            continue;
        };
        let Some(ruta) = crate::openers::resolve_program(programa) else {
            continue;
        };
        let hijo = std::process::Command::new(&ruta)
            .args(&argv[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(mut hijo) = hijo else {
            continue;
        };
        // El texto por STDIN y el stdin CERRADO después: `wl-copy` y `xclip`
        // se quedan de dueños de la selección hasta que el flujo acaba, y sin
        // cerrarlo el portapapeles queda a medias para siempre.
        let escrito = hijo
            .stdin
            .take()
            .map(|mut w| w.write_all(bytes).and_then(|()| w.flush()));
        return match escrito {
            Some(Ok(())) => ClipboardOutcome::Done(programa.to_string_lossy().into_owned()),
            _ => ClipboardOutcome::Failed,
        };
    }
    ClipboardOutcome::NoHelper
}

/// La secuencia OSC 52 que pone `bytes` en el portapapeles del TERMINAL.
///
/// Es la salida que la ventana gráfica no tiene y el terminal sí, y la única
/// que funciona por SSH sin instalar nada al otro lado: quien recibe la
/// secuencia es el emulador que el humano está mirando, no la máquina donde
/// corre norte.
///
/// **Un terminal que no la soporte la ignora en silencio**, y no hay forma de
/// preguntárselo. Por eso el llamante la usa como ÚLTIMO recurso y dice por
/// qué camino fue: «copiado» sobre un portapapeles vacío es la clase de
/// mentira que se descubre al pegar en otro sitio.
///
/// ```
/// use norte_frontend::shell::osc52;
/// assert_eq!(osc52(b"hola"), b"\x1b]52;c;aG9sYQ==\x07".to_vec());
/// ```
#[must_use]
pub fn osc52(bytes: &[u8]) -> Vec<u8> {
    use base64::Engine as _;
    let payload = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(b"\x1b]52;c;");
    out.extend_from_slice(payload.as_bytes());
    out.push(0x07);
    out
}
