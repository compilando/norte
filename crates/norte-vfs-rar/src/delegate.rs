//! What external program reads the RAR, how it is found and **how it is
//! bounded**.
//!
//! Rule 9 lives here: the delegate is given a path, a name and a pipe,
//! never the user's filesystem. Every hardening in [`Delegate::command`]
//! carries weight, and none of it is decorative.

use futures::stream::StreamExt;
use norte_proto::Error;
use norte_vfs::ByteStream;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Delegation's own failures, with the detail
/// [`norte_proto::Error`] cannot carry over the wire.
///
/// The conversion to the protocol error is deliberately poor —
/// `Unsupported` — because the wire does not carry prose; the sentence
/// lives here, for the log and for `norte doctor`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RarError {
    /// No RAR reader is installed.
    ///
    /// The message NAMES what to install on purpose: a `.rar` that opens
    /// and shows nothing teaches the user nothing.
    #[error("no RAR reader found: install `7z` (p7zip) or `unrar` and try again")]
    NoDelegate,
    /// The executable did not start (does not exist, is not executable, no
    /// permissions).
    #[error("could not run the RAR reader `{program}`: {source}")]
    Spawn {
        /// Path of the executable that was attempted.
        program: String,
        /// The operating system's failure.
        source: std::io::Error,
    },
    /// The child went past the wall-clock deadline and was KILLED. Not an
    /// `Io` worth retrying: doing the same thing again just hangs again.
    #[error("the RAR reader took longer than {}s and was killed", .0.as_secs())]
    Timeout(Duration),
    /// The entry's name, treated as the pattern the delegate would apply,
    /// also matches ANOTHER entry in the archive.
    ///
    /// Refused instead of guessed: the wrong entry's stream looks exactly
    /// like the right one's.
    #[error("the entry name would match more than one entry as a pattern; refusing to guess")]
    AmbiguousForDelegate,
    /// The child ended badly. `stderr` comes trimmed: it is diagnostic, not
    /// a channel.
    #[error("the RAR reader failed (exit {code}): {stderr}")]
    Failed {
        /// Exit code, or `-1` if it died by signal.
        code: i32,
        /// First lines of `stderr`, lossy — for the log only.
        stderr: String,
    },
}

impl From<RarError> for Error {
    /// The wire does not carry prose: all of this collapses into a handful
    /// of categories, and the sentence stays in this side's log.
    fn from(e: RarError) -> Self {
        match e {
            // Neither is fixed by retrying, and both have a sentence the
            // log does carry.
            RarError::NoDelegate | RarError::AmbiguousForDelegate => Self::Unsupported,
            RarError::Spawn { .. } | RarError::Timeout(_) => {
                Self::ProviderUnavailable { retryable: false }
            }
            // The delegate answers and says no: it is the container that
            // fails, not the I/O.
            RarError::Failed { .. } => Self::Corrupt,
        }
    }
}

/// How many children can be alive at once across the whole process.
///
/// A pane listing a directory with forty `.rar`s cannot turn into forty
/// processes: the semaphore is what keeps delegation's cost bounded.
const MAX_CHILDREN: usize = 4;

static CHILDREN: Semaphore = Semaphore::const_new(MAX_CHILDREN);

/// Wall-clock deadline per list invocation.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(30);

/// The child's working directory: an EMPTY one, the process's own.
///
/// Never the user's tree. A delegate that decides to write relative paths
/// —`7z e` without `-so`, a future version, a misplaced flag— writes here,
/// where there is nothing to overwrite. If it cannot be created, the child
/// runs without an explicit `current_dir` rather than failing the whole
/// read; that case can now only happen with a broken system temp.
///
/// # Created EXCLUSIVELY, and not with a guessable name
///
/// It used to be `temp_dir().join(format!("norte-rar-{pid}"))` with
/// `create_dir_all`, and that is two bad things together (ADR 0082 security
/// review): the pid is guessable —or pre-created in bulk—, and
/// `create_dir_all` SUCCEEDS if the directory already exists, without
/// checking owner or mode. On a `/tmp` anyone can write to, the delegate's
/// cwd could be someone else's directory with whatever that someone wanted
/// inside. `TempDir` creates with a random name, exclusively, at 0700.
///
/// The `TempDir` is deliberately leaked inside the `OnceLock`: it lives as
/// long as the process, and deleting it while a child has it as cwd would
/// be worse than leaving it.
fn sandbox_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<tempfile::TempDir>> = OnceLock::new();
    DIR.get_or_init(|| tempfile::Builder::new().prefix("norte-rar-").tempdir().ok())
        .as_ref()
        .map(tempfile::TempDir::path)
}

/// The external program acting as the RAR reader.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Delegate {
    /// `7z` or `7zz` (p7zip). Preferred: preserves the name's raw bytes.
    SevenZip(PathBuf),
    /// `unrar`.
    Unrar(PathBuf),
}

/// The executables that get probed, **in order of preference**.
///
/// The order is measured, not chosen by taste: `unrar` TRUNCATES a non-UTF8
/// name in its listing (`cp437-\xa4\xa5.txt` comes out as `cp437-`, no
/// extension), and `7z -slt` delivers it whole. A provider that loses a
/// file's extension is not acceptable while there is an alternative.
const CANDIDATES: [&str; 3] = ["7z", "7zz", "unrar"];

impl Delegate {
    /// Probes `PATH` for a reader: `7z`, `7zz`, `unrar`.
    ///
    /// Touches the filesystem (one `is_file` per candidate and `PATH`
    /// directory), so it is called ONCE outside the async path — when
    /// building the provider —, never per operation.
    ///
    /// # Errors
    ///
    /// [`RarError::NoDelegate`] if none is installed.
    pub fn discover() -> Result<Self, RarError> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut found = Vec::new();
        for dir in std::env::split_paths(&path) {
            // Only ABSOLUTE `PATH` entries. A relative one —or the empty
            // one, which means "the current directory"— would resolve the
            // delegate against a directory nobody vouched for, and the
            // child is also launched with `current_dir` set: opening a
            // `.rar` would execute whatever `7z` was sitting there (ADR
            // 0082 security review, same rule as
            // `openers::resolve_program`).
            if !dir.is_absolute() {
                continue;
            }
            for exe in CANDIDATES {
                let candidate = dir.join(exe);
                if candidate.is_file() {
                    found.push((exe, candidate));
                }
            }
        }
        Self::discover_in(&found)
    }

    /// The pure half of [`discover`](Self::discover): picks among
    /// already-resolved candidates, honoring the preference order and not
    /// the arrival order.
    ///
    /// # Errors
    ///
    /// [`RarError::NoDelegate`] if the list comes empty or carries no known
    /// name.
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use norte_vfs_rar::Delegate;
    ///
    /// let chosen = Delegate::discover_in(&[
    ///     ("unrar", PathBuf::from("/usr/bin/unrar")),
    ///     ("7z", PathBuf::from("/usr/bin/7z")),
    /// ])
    /// .expect("there are candidates");
    /// assert!(matches!(chosen, Delegate::SevenZip(_)));
    /// ```
    pub fn discover_in(candidates: &[(&str, PathBuf)]) -> Result<Self, RarError> {
        for exe in CANDIDATES {
            if let Some((_, path)) = candidates.iter().find(|(name, _)| *name == exe) {
                return Ok(match exe {
                    "unrar" => Self::Unrar(path.clone()),
                    _ => Self::SevenZip(path.clone()),
                });
            }
        }
        Err(RarError::NoDelegate)
    }

    /// The delegate PINNED by configuration (`[archive] rar_delegate`).
    ///
    /// The dialect is decided by the executable's name —`unrar` speaks
    /// `vt`/`p`, anything else is treated as `7z`—, and a binary that does
    /// not exist does not fail here but when used, with an error that NAMES
    /// it: pinning a broken path and not finding out until a `.rar` is
    /// opened is worse than finding out by opening a `.rar`.
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use norte_vfs_rar::Delegate;
    ///
    /// assert!(matches!(
    ///     Delegate::pinned(PathBuf::from("/opt/bin/unrar")),
    ///     Delegate::Unrar(_)
    /// ));
    /// ```
    #[must_use]
    pub fn pinned(program: PathBuf) -> Self {
        let name = program.file_name().unwrap_or_default().to_string_lossy();
        if name.contains("unrar") {
            Self::Unrar(program)
        } else {
            Self::SevenZip(program)
        }
    }

    /// The absolute path of the chosen executable.
    #[must_use]
    pub fn program(&self) -> &Path {
        match self {
            Self::SevenZip(p) | Self::Unrar(p) => p,
        }
    }

    /// `argv` for the LISTING. Every argument goes after `--` and the
    /// password is empty on the command line itself: a `stdin` prompt cannot
    /// happen if nobody is going to ask.
    #[must_use]
    pub fn list_argv(&self, archive: &Path) -> Vec<OsString> {
        let mut argv: Vec<OsString> = match self {
            Self::SevenZip(_) => ["l", "-slt", "-p", "-bd", "-y", "--"],
            Self::Unrar(_) => ["vt", "-p-", "-idc", "-y", "--", ""],
        }
        .iter()
        .filter(|a| !a.is_empty())
        .map(OsString::from)
        .collect();
        argv.push(archive.as_os_str().to_os_string());
        argv
    }

    /// `argv` for READING ONE entry to `stdout`.
    ///
    /// The name travels in **bytes**, without going through `String`: a name
    /// that is not UTF-8 is still a name (rule 1). Whether the delegate
    /// treats that name as a pattern is the provider's problem, which
    /// refuses before getting here.
    #[must_use]
    pub fn read_argv(&self, archive: &Path, entry: &[u8]) -> Vec<OsString> {
        let mut argv: Vec<OsString> = match self {
            Self::SevenZip(_) => ["e", "-so", "-bd", "-y", "-p", "--"],
            Self::Unrar(_) => ["p", "-inul", "-p-", "-y", "--", ""],
        }
        .iter()
        .filter(|a| !a.is_empty())
        .map(OsString::from)
        .collect();
        argv.push(archive.as_os_str().to_os_string());
        argv.push(os_from_bytes(entry));
        argv
    }

    /// Builds the child process with rule 9 applied. Every line carries
    /// weight:
    ///
    /// - `stdin` to `null`: a password prompt cannot hang the daemon,
    ///   because there is nobody to ask;
    /// - `stderr` captured: the delegate's messages do not contaminate the
    ///   data stream nor the process log;
    /// - `current_dir` in an empty directory: never the user's tree;
    /// - `env_clear`: the child inherits neither credentials nor
    ///   `LD_PRELOAD`;
    /// - `kill_on_drop`: dropping the future kills the child, which is what
    ///   makes cancelling mean something.
    fn command(&self, argv: &[OsString]) -> tokio::process::Command {
        // The path is made ABSOLUTE here, with norte's cwd still in place.
        // `discover` already only looks at absolute `PATH` entries, but
        // `[archive] rar_delegate` accepts whatever the reader writes, and on
        // unix `current_dir` is applied BEFORE resolving the program: a
        // relative `7z` would be resolved by the child against the sandbox,
        // not against where the reader thought (ADR 0082 security review).
        let program = if self.program().is_absolute() {
            self.program().to_path_buf()
        } else {
            std::env::current_dir()
                .map_or_else(|_| self.program().to_path_buf(), |c| c.join(self.program()))
        };
        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .kill_on_drop(true);
        if let Some(dir) = sandbox_dir() {
            cmd.current_dir(dir);
        }
        cmd
    }

    /// Launches the child, waits for its COMPLETE output and returns it as
    /// bytes.
    ///
    /// For the listing, which is small and must be parsed whole. Once
    /// `timeout` passes the child dies and the error says so.
    ///
    /// # Errors
    ///
    /// [`RarError::Spawn`] if the executable does not start,
    /// [`RarError::Timeout`] if the deadline runs out, [`RarError::Failed`]
    /// if it exits with a non-zero status.
    ///
    /// # Panics
    ///
    /// If the children semaphore were closed, which this crate never does.
    pub async fn run_capture(
        &self,
        argv: &[OsString],
        timeout: Duration,
    ) -> Result<Vec<u8>, RarError> {
        let _permit = CHILDREN
            .acquire()
            .await
            .expect("the semaphore never closes");
        let child = self
            .command(argv)
            .spawn()
            .map_err(|source| RarError::Spawn {
                program: self.program().display().to_string(),
                source,
            })?;
        // The child lives INSIDE the future: if the timeout drops it,
        // `kill_on_drop` kills it. There is no path that leaves an orphaned
        // process.
        let out = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| RarError::Timeout(timeout))?
            .map_err(|source| RarError::Spawn {
                program: self.program().display().to_string(),
                source,
            })?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(RarError::Failed {
                code: out.status.code().unwrap_or(-1),
                stderr: first_lines(&out.stderr),
            })
        }
    }

    /// Launches the child and returns its `stdout` as a stream, without
    /// accumulating it.
    ///
    /// Cancelling the token kills the child (rule 3): the stream ends in
    /// [`Error::Cancelled`] and the semaphore permit is released.
    ///
    /// # Errors
    ///
    /// [`RarError::Spawn`] if the executable does not start.
    ///
    /// # Panics
    ///
    /// If the children semaphore were closed, which this crate never does,
    /// or if `stdout` did not come as a pipe having been requested as such.
    pub async fn run_stream(
        &self,
        argv: &[OsString],
        cancel: CancellationToken,
    ) -> Result<ByteStream, RarError> {
        // The permit is FORGOTTEN here and returned by hand when the task
        // below finishes: the stream outlives this function, so it cannot be
        // tied to a guard scoped to it.
        CHILDREN
            .acquire()
            .await
            .expect("the semaphore never closes")
            .forget();
        let mut child = self.command(argv).spawn().map_err(|source| {
            CHILDREN.add_permits(1);
            RarError::Spawn {
                program: self.program().display().to_string(),
                source,
            }
        })?;
        let mut stdout = child.stdout.take().expect("stdout requested as a pipe");
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, Error>>(4);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let read = tokio::select! {
                    () = cancel.cancelled() => {
                        let _ = tx.send(Err(Error::Cancelled)).await;
                        break;
                    }
                    r = stdout.read(&mut buf) => r,
                };
                match read {
                    Ok(0) => {
                        // EOF: the verdict comes from the exit status, not
                        // the silence. An encrypted `.rar` gives zero bytes
                        // and an error.
                        match child.wait().await {
                            Ok(st) if st.success() => {}
                            Ok(_) | Err(_) => {
                                let _ = tx.send(Err(Error::Corrupt)).await;
                            }
                        }
                        break;
                    }
                    Ok(n) => {
                        if tx
                            .send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                            .await
                            .is_err()
                        {
                            break; // consumer left: `kill_on_drop` finishes it off
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(Err(Error::Io { retryable: false })).await;
                        break;
                    }
                }
            }
            drop(child); // kill_on_drop: neither cancelled nor broken leaves a live process
            CHILDREN.add_permits(1);
        });
        Ok(tokio_stream::wrappers::ReceiverStream::new(rx).boxed())
    }
}

/// A name in raw bytes to `OsString`, without going through `String`.
#[cfg(unix)]
fn os_from_bytes(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(bytes).to_os_string()
}

/// On Windows `argv` is UTF-16 and there is no way to pass arbitrary bytes:
/// the lossy conversion is the operating system's, not a decision of ours.
#[cfg(not(unix))]
fn os_from_bytes(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

/// `stderr`'s first lines in lossy, bounded: it is diagnostics for the log,
/// not a data channel.
fn first_lines(stderr: &[u8]) -> String {
    let cut = stderr.len().min(512);
    String::from_utf8_lossy(&stderr[..cut])
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_argv_carries_separator_and_no_password() {
        let d = Delegate::SevenZip(PathBuf::from("/usr/bin/7z"));
        let argv = d.list_argv(Path::new("/tmp/a.rar"));
        assert!(
            argv.contains(&OsString::from("--")),
            "every argument goes after `--`"
        );
        assert!(
            argv.iter().any(|a| a == "-p"),
            "empty password: never a stdin prompt"
        );
        assert_eq!(argv.last().unwrap(), "/tmp/a.rar");
    }

    #[test]
    fn unrar_listing_argv_also_silences_the_password() {
        let d = Delegate::Unrar(PathBuf::from("/usr/bin/unrar"));
        let argv = d.list_argv(Path::new("/tmp/a.rar"));
        assert!(argv.iter().any(|a| a == "-p-"), "unrar: empty password");
        let sep = argv.iter().position(|a| a == "--").expect("has separator");
        assert_eq!(sep, argv.len() - 2, "the archive goes AFTER the separator");
    }

    #[test]
    fn read_argv_passes_the_name_in_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let d = Delegate::SevenZip(PathBuf::from("/usr/bin/7z"));
        let argv = d.read_argv(Path::new("/tmp/a.rar"), b"cp437-\xa4\xa5.txt");
        assert_eq!(argv.last().unwrap().as_bytes(), b"cp437-\xa4\xa5.txt");
        assert!(
            argv.iter().any(|a| a == "-so"),
            "content comes out via stdout"
        );
    }

    /// The property is "does not hang". With `stdin` OPEN this test takes
    /// forever; with `stdin` set to null, the child dies on its own.
    #[tokio::test]
    async fn the_child_never_waits_on_stdin() {
        let d = Delegate::Unrar(PathBuf::from("/bin/cat")); // cat reads stdin until EOF
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            d.run_capture(&[], Duration::from_secs(30)),
        )
        .await;
        assert!(out.is_ok(), "stdin open: the child was left waiting");
    }

    /// A child that does not finish gets KILLED, and the error says so
    /// instead of staying silent.
    #[tokio::test]
    async fn the_wall_deadline_kills_the_child() {
        let d = Delegate::Unrar(PathBuf::from("/bin/sleep"));
        let err = d
            .run_capture(&[OsString::from("30")], Duration::from_millis(200))
            .await
            .expect_err("30s does not fit in 200ms");
        assert!(matches!(err, RarError::Timeout(_)), "{err}");
        assert_eq!(
            Error::from(err),
            Error::ProviderUnavailable { retryable: false }
        );
    }

    #[tokio::test]
    async fn cancelling_kills_the_child() {
        let token = CancellationToken::new();
        let d = Delegate::Unrar(PathBuf::from("/bin/sleep"));
        let stream = d
            .run_stream(&[OsString::from("30")], token.clone())
            .await
            .expect("sleep starts");
        token.cancel();
        let items = tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("the child survived cancellation");
        assert_eq!(
            items.last().and_then(|r| r.as_ref().err().cloned()),
            Some(Error::Cancelled),
            "the stream ends SAYING it was cancelled"
        );
    }

    /// An executable that does not exist is neither a panic nor an empty
    /// listing: it is an error that NAMES the program.
    #[tokio::test]
    async fn a_missing_executable_names_the_program() {
        let d = Delegate::SevenZip(PathBuf::from("/nonexistent/7z"));
        let err = d
            .run_capture(&[], Duration::from_secs(5))
            .await
            .expect_err("does not exist");
        assert!(err.to_string().contains("/nonexistent/7z"), "{err}");
        assert!(matches!(err, RarError::Spawn { .. }));
    }

    /// A child that exits with a non-zero status is `Corrupt`: the delegate
    /// responds and says that container is no good.
    #[tokio::test]
    async fn a_non_zero_status_is_corrupt() {
        let d = Delegate::SevenZip(PathBuf::from("/bin/false"));
        let err = d
            .run_capture(&[], Duration::from_secs(5))
            .await
            .expect_err("false always fails");
        assert!(matches!(err, RarError::Failed { .. }), "{err}");
        assert_eq!(Error::from(err), Error::Corrupt);
    }

    /// The child does NOT inherit the environment: no credentials in
    /// variables, no `LD_PRELOAD`. `env` prints whatever it has, and it must
    /// have nothing — `PATH` is set in any test environment.
    #[tokio::test]
    async fn the_child_does_not_inherit_the_environment() {
        assert!(
            std::env::var_os("PATH").is_some(),
            "the parent DOES have an environment"
        );
        let d = Delegate::SevenZip(PathBuf::from("/usr/bin/env"));
        let out = d
            .run_capture(&[], Duration::from_secs(5))
            .await
            .expect("env starts");
        let text = String::from_utf8_lossy(&out);
        assert!(text.trim().is_empty(), "inherited environment: {text}");
    }

    /// The child runs in an EMPTY directory of its own, never in the user's
    /// tree: `pwd` says so.
    #[tokio::test]
    async fn the_child_runs_outside_the_users_tree() {
        let d = Delegate::SevenZip(PathBuf::from("/bin/pwd"));
        let out = d
            .run_capture(&[], Duration::from_secs(5))
            .await
            .expect("pwd starts");
        let cwd = String::from_utf8_lossy(&out).trim().to_string();
        assert_eq!(
            Some(std::path::Path::new(&cwd)),
            sandbox_dir(),
            "the child does not run where the user is"
        );
        assert_eq!(
            std::fs::read_dir(&cwd).unwrap().count(),
            0,
            "and the directory is empty"
        );
    }

    #[test]
    fn without_a_delegate_the_error_names_the_executable() {
        let err = Delegate::discover_in(&[]).expect_err("no candidates fails");
        let msg = err.to_string();
        assert!(
            msg.contains("7z") && msg.contains("unrar"),
            "the error must say WHAT to install: {msg}"
        );
    }

    #[test]
    fn seven_zip_is_preferred_over_unrar() {
        // Order is measured, not taste: unrar TRUNCATES a non-UTF8 name in the listing.
        let found = Delegate::discover_in(&[
            ("unrar", PathBuf::from("/usr/bin/unrar")),
            ("7z", PathBuf::from("/usr/bin/7z")),
        ])
        .expect("there are candidates");
        assert!(matches!(found, Delegate::SevenZip(_)), "7z beats unrar");
    }

    #[test]
    fn seven_zip_zip_also_counts_and_goes_before_unrar() {
        let found = Delegate::discover_in(&[
            ("unrar", PathBuf::from("/usr/bin/unrar")),
            ("7zz", PathBuf::from("/opt/7zz")),
        ])
        .expect("there are candidates");
        assert_eq!(found, Delegate::SevenZip(PathBuf::from("/opt/7zz")));
    }

    #[test]
    fn a_pinned_delegate_picks_its_dialect_by_its_name() {
        assert_eq!(
            Delegate::pinned(PathBuf::from("/usr/local/bin/7zz")),
            Delegate::SevenZip(PathBuf::from("/usr/local/bin/7zz"))
        );
        assert_eq!(
            Delegate::pinned(PathBuf::from("/opt/unrar")),
            Delegate::Unrar(PathBuf::from("/opt/unrar"))
        );
        // A name that says nothing is treated as 7z: it is the dialect that
        // preserves raw bytes, i.e. the one that loses least if we guess
        // halfway right.
        assert_eq!(
            Delegate::pinned(PathBuf::from("/opt/lector")),
            Delegate::SevenZip(PathBuf::from("/opt/lector"))
        );
    }

    #[test]
    fn only_unrar_is_accepted() {
        let found = Delegate::discover_in(&[("unrar", PathBuf::from("/usr/bin/unrar"))])
            .expect("unrar works");
        assert_eq!(found, Delegate::Unrar(PathBuf::from("/usr/bin/unrar")));
    }
}
