//! The persistent SUBSHELL: the pty and the long-lived child (#142).
//!
//! The other half —the prompt marker, cwd parsing and the `cd` in bytes—
//! lives in [`norte_frontend::subshell`], with no I/O and with its tests.
//!
//! # What changes from what there was
//!
//! `app.toggle-panels` used to hand over the terminal and show the
//! SCROLLBACK from wherever norte started, until a key was pressed. That is
//! not a shell: it remembers nothing, nothing can be typed into it, and the
//! panel's directory does not matter to it. What Midnight Commander does —
//! and what shows immediately— is having a LIVE shell behind the panels.
//!
//! # The four decisions this holds
//!
//! **It starts LAZILY**, on the first `Ctrl+O`. A shell per norte session
//! that nobody is going to use is a process, a pty and someone's
//! `.bashrc` running just in case.
//!
//! **The child inherits the pty and NOT norte's terminal.** That is why it
//! can stay alive while the panels are painted: nobody shares the tty.
//! While it is attached, this module copies bytes in both directions.
//!
//! **The shell STATES the cwd**, with a marker norte puts in its prompt on
//! starting it (never touching its configuration). On attaching, the shell
//! follows the panel with a `cd`; on detaching, the panel can follow the
//! shell.
//!
//! **When norte exits, the shell goes with it** (`SIGHUP` from
//! `portable-pty`'s drop): leaving an orphaned shell talking to a pty
//! nobody reads anymore is a process nobody knows exists.

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};

use norte_frontend::subshell::{Nonce, install, scan_cwd};

/// This session's live subshell.
pub struct Subshell {
    /// Where it is written to.
    ///
    /// SHARED with the reader thread, which also writes: it is the one that
    /// answers the shell's terminal queries ([`Writer`]).
    write: Writer,
    /// The pty, which is also the one that resizes.
    maestro: Box<dyn portable_pty::MasterPty + Send>,
    /// The child. Kept so it can be killed and so we know whether it is
    /// still alive.
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// What the shell has written and has not been painted yet, plus the
    /// LAST cwd it announced. Filled by a reader thread.
    mailbox: Arc<Mutex<Mailbox>>,
    /// Which of the three it is, if it is one of the three. `None` = norte
    /// installed nothing in it and does not type anything into it.
    which: Option<norte_frontend::shell::Shell>,
    /// This session's MAILBOX: how the shell is told to change directory,
    /// instead of typing a `cd` into it (#363).
    ///
    /// A 0600 file with a nonce in its name, under the user's runtime
    /// directory (`$XDG_RUNTIME_DIR`, which is already 0700 of its own) or
    /// under `/tmp/norte-<uid>` if there is none — the same place and the
    /// same criterion as the daemon's socket. `None` = it could not be
    /// created, and then the panel simply does not drag the shell along:
    /// degrading this way is correct, and crashing or typing the `cd` again
    /// would not be.
    ///
    /// Deleted on releasing the subshell. If norte dies abruptly, a file of
    /// a few dozen bytes is left in a directory the system cleans up on
    /// logout.
    mailbox_file: Option<std::path::PathBuf>,
}

/// The pty's input, shared between whoever attaches and the reader thread.
///
/// Two writers, and both legitimate: the reader's keys come in through
/// [`Subshell::write`], and the RESPONSES to the shell's terminal queries
/// are sent by the thread that sees them go by. A modern shell asks what
/// terminal it has in front of it and STOPS until it is answered (fish 4
/// does it before its first prompt), so the response cannot wait for
/// someone to attach.
type Writer = Arc<Mutex<Box<dyn std::io::Write + Send>>>;

/// What the reader thread leaves for whoever attaches.
#[derive(Default)]
struct Mailbox {
    /// Bytes pending painting, already WITHOUT the markers.
    pending: Vec<u8>,
    /// The last cwd announced, in bytes (rule 1).
    cwd: Option<Vec<u8>>,
    /// A marker split between two reads, waiting for its end.
    cola: Vec<u8>,
    /// The pty closed: the shell is gone.
    closed: bool,
}

/// How much of what the shell wrote is kept while nobody is looking.
///
/// A `find /` launched and left running writes without end, and this lives
/// in memory: the TAIL is kept, which is what a reader would want to see on
/// returning, and what is further back is dropped.
const BUFFER_MAX: usize = 256 * 1024;

impl Subshell {
    /// Starts a shell in its own pty.
    ///
    /// `dir` is where it starts; `size` the terminal's size, which the
    /// child needs to know in order to paint.
    ///
    /// # Errors
    /// Whatever fails when opening the pty or launching the shell.
    pub fn start(dir: &std::path::Path, size: (u16, u16)) -> std::io::Result<Self> {
        Self::start_with(&norte_frontend::shell::login_shell(), &[], dir, size)
    }

    /// [`Self::start`] with the GIVEN program and arguments.
    ///
    /// Exists for the tests, and it is not a convenience: `start` uses
    /// `$SHELL`, i.e. the shell of whoever runs the suite, with its whole
    /// configuration behind it. On this machine that is a zsh whose first
    /// interactive start launches the powerlevel10k wizard and never reaches
    /// a prompt — a red test that says nothing about the code. A test that
    /// needs a shell needs A shell, not the one of whoever runs it.
    fn start_with(
        program: &std::path::Path,
        args: &[&str],
        dir: &std::path::Path,
        size: (u16, u16),
    ) -> std::io::Result<Self> {
        let system = portable_pty::native_pty_system();
        let pair = system
            .openpty(portable_pty::PtySize {
                rows: size.1,
                cols: size.0,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(std::io::Error::other)?;
        let shell = program.to_path_buf();
        let mut cmd = portable_pty::CommandBuilder::new(&shell);
        for a in args {
            cmd.arg(a);
        }
        cmd.cwd(dir);
        // The child knows it is INSIDE norte, like a suspension's does: it
        // is the same `NORTE_LEVEL` contract and the reader's prompt reads
        // it.
        cmd.env(
            norte_frontend::shell::LEVEL_VAR,
            norte_frontend::shell::next_norte_level(),
        );
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(std::io::Error::other)?;
        // The slave is RELEASED here: while norte keeps it open, closing the
        // shell would not close the pty and the reader would never see EOF.
        drop(pair.slave);
        let write: Writer = Arc::new(Mutex::new(
            pair.master.take_writer().map_err(std::io::Error::other)?,
        ));
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(std::io::Error::other)?;
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        // THIS session's nonce: what separates a marker the hook printed
        // from one that came from inside a file. See `Nonce`.
        let nonce = Nonce::new();
        launch_reader(
            reader,
            Arc::clone(&mailbox),
            Arc::clone(&write),
            nonce.clone(),
        );

        let which = shell_known(&shell);
        // The mailbox is created BEFORE the hook: the hook carries its path
        // inside.
        let mailbox_file = which.and_then(|_| create_mailbox(&nonce));
        let mut me = Self {
            write,
            maestro: pair.master,
            child,
            mailbox,
            which,
            mailbox_file,
        };
        // The prompt hook is sent as if the reader typed it: NONE of their
        // files is touched. A `.bashrc` norte edited would be a permanent
        // modification for a function that turns off on exit — and it would
        // survive a norte that is no longer there.
        //
        // With no mailbox, nothing is installed: the hook carries its path
        // inside, and one pointing at a file that does not exist would be
        // plumbing typed into the reader's face for nothing in return.
        if let (Some(which), Some(mailbox)) = (which, me.mailbox_file.clone()) {
            let _ = me.write(install(which, &nonce, &mailbox).as_bytes());
            // Ctrl+L: the `readline`/ZLE/fish command that clears the
            // screen and repaints the prompt. Without this, the first thing
            // the reader sees on their first Ctrl+O is the wall of plumbing
            // we just typed.
            let _ = me.write_key(b"\x0c");
        }
        Ok(me)
    }

    /// Writes to the shell.
    ///
    /// **norte no longer types COMMANDS into it** (#363): what goes out
    /// through here is the reader's keys and the startup plumbing, nothing
    /// else. Moving the shell to another directory is done through the
    /// mailbox and not through the line editor — see [`Self::ir_a`].
    ///
    /// # Errors
    /// Whatever the pty fails at.
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        write_raw(&self.write, bytes)
    }

    /// Writes ONE key from the reader to the shell.
    ///
    /// Today it is [`Self::write`] and nothing more. It stays as a
    /// separate door because what comes in through here was TYPED by
    /// someone and what comes in through the other one is sent by norte,
    /// and it is worth that showing at the call site. What used to be here
    /// —an exemption for Ctrl+L, which repaints the prompt without touching
    /// the line— existed to avoid lowering a permission that no longer
    /// exists: since #363 norte does not type commands, so there is no
    /// permission to look after.
    ///
    /// # Errors
    /// Whatever the pty fails at.
    pub fn write_key(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.write(bytes)
    }

    /// Sends the shell to directory `dir` (native bytes), IF it can be done.
    ///
    /// **Types nothing into it** (#363). Leaves the path in THIS session's
    /// MAILBOX —a 0600 file under the runtime directory— and the prompt hook
    /// picks it up and does the `cd` the next time the shell is between two
    /// commands. Returns whether it was left noted down.
    ///
    /// # Why it is not typed
    ///
    /// Typing a `cd` requires knowing the line editor is at an EMPTY prompt,
    /// and that cannot be known from outside. norte approximated it with "a
    /// prompt marker arrived and nobody has typed since then", which is a
    /// different statement, and there were at least two ways to satisfy the
    /// second without the first:
    ///
    /// - zsh's **buffer stack**. `push-line` (Ctrl+Q by default) sets the
    ///   line aside, the shell paints a new prompt —marker, permission— and
    ///   immediately returns it to the buffer. A `print -z` from a reader
    ///   function reaches the same place without touching a key;
    /// - **type-ahead**, in all three. What the reader types while the
    ///   shell is busy waits in the pty's queue. The marker arrives with
    ///   those bytes still unconsumed, and the `cd` got concatenated to
    ///   them.
    ///
    /// In both cases the shell EXECUTED a command nobody gave:
    /// `rm -rf tmpdir __norte_cd '...'`. With the mailbox there is nothing
    /// to concatenate onto —the injection channel into the line editor
    /// disappears— and the move happens exactly when the shell is
    /// demonstrably between commands, which is what had to be demonstrated.
    ///
    /// What changes for the reader: the `cd` applies at the next prompt and
    /// not instantly. It is the honest semantics, and it is what already
    /// happened every time this was refused.
    ///
    /// It is refused if the shell is not one of the three norte prepares:
    /// with no hook there is nobody to read the mailbox.
    ///
    /// # Errors
    /// Whatever fails when writing the mailbox.
    pub fn ir_a(&mut self, dir: &std::path::Path) -> std::io::Result<bool> {
        use std::os::unix::ffi::OsStrExt as _;
        if self.which.is_none() {
            return Ok(false);
        }
        let Some(mailbox) = self.mailbox_file.as_deref() else {
            return Ok(false);
        };
        // The trailing `_` is a sentinel and not decoration: the shell
        // reads the file with `$(<f)`, which eats trailing newlines, and a
        // directory CAN end in one.
        let mut bytes = dir.as_os_str().as_bytes().to_vec();
        bytes.push(b'_');
        write_mailbox(mailbox, &bytes)?;
        Ok(true)
    }

    /// What the shell has written since last time, taking it with it.
    ///
    /// # Panics
    /// If the reader thread panicked with the mailbox held. It deliberately
    /// does not recover: a poisoned mailbox means the reader died mid-write,
    /// so whatever was inside no longer describes the shell's screen — and
    /// painting it anyway is worse than crashing (rule 6).
    #[must_use]
    pub fn drain(&self) -> Vec<u8> {
        let mut b = mailbox_of(&self.mailbox);
        std::mem::take(&mut b.pending)
    }

    /// The last directory the shell announced, if it announced any.
    ///
    /// # Panics
    /// Same as [`Self::drain`]: poisoned mailbox.
    #[must_use]
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        use std::os::unix::ffi::OsStrExt as _;
        let b = mailbox_of(&self.mailbox);
        let bytes = b.cwd.as_ref()?;
        Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }

    /// Is the shell gone? (the reader saw EOF, or the child died).
    ///
    /// # Panics
    /// Same as [`Self::drain`]: poisoned mailbox.
    #[must_use]
    pub fn dead(&mut self) -> bool {
        let closed = mailbox_of(&self.mailbox).closed;
        closed || matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Tells the shell what size the terminal is now.
    pub fn resize(&self, size: (u16, u16)) {
        let _ = self.maestro.resize(portable_pty::PtySize {
            rows: size.1,
            cols: size.0,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Kills it. Called by norte's shutdown: an orphaned shell talking to a
    /// pty nobody reads anymore is a process nobody knows exists.
    pub fn matar(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The shell dies WITH norte, however norte exits.
///
/// In `Drop` and not only in the `app.quit` arm: the loop can also exit
/// through [`crate::event_loop::RunError`] —the terminal or the event
/// stream breaking—, and no orderly shutdown goes through there. A shell
/// that outlives its norte is left talking to a pty nobody reads anymore,
/// with the reader's terminal behind it.
impl Drop for Subshell {
    fn drop(&mut self) {
        self.matar();
        // And it takes its mailbox with it: it belongs to this session and
        // is of no use to anyone else. An error here does not count — the
        // shell is already gone and there is nobody to tell—, and what is
        // left if norte dies abruptly is a few dozen bytes in a directory
        // the system cleans up on logout.
        if let Some(p) = &self.mailbox_file {
            let _ = std::fs::remove_file(p);
            let _ = std::fs::remove_file(p.with_extension("cd.tmp"));
        }
    }
}

/// The executable name from a shell path, to choose the hook.
///
/// By BYTES (rule 1) and not by `to_string_lossy`: a `$SHELL` with a
/// component that is not UTF-8 turned into `\u{FFFD}`, `Shell::parse`
/// failed, and the reader was left with a subshell that never said where it
/// was — no hook, no `cd` and not a single message explaining it. Now a
/// non-UTF-8 name simply is not one of the three we know, which is the
/// truth.
fn shell_known(shell: &std::path::Path) -> Option<norte_frontend::shell::Shell> {
    use std::os::unix::ffi::OsStrExt as _;
    let name = shell.file_name()?;
    let text = std::str::from_utf8(name.as_bytes()).ok()?;
    norte_frontend::shell::Shell::parse(text)
}

/// The thread that reads from the pty non-stop and leaves what it read in
/// the mailbox.
///
/// A thread and not a task: `portable_pty` gives a BLOCKING reader, and
/// putting a blocking read into the executor is rule 2. The thread only
/// dies when the pty closes.
/// The mailbox, even if the reader thread died holding it.
///
/// `PoisonError::into_inner` and not an `expect` (rule 6): the poison means
/// the reader fell over mid-write, and the worst thing inside is a chunk of
/// output half-added. Crashing over that would be a panic in the main
/// thread WITH THE TERMINAL IN RAW MODE and the panels unpainted — a
/// disproportionate price for a few bytes of screen. And there is no
/// invariant that supports an `expect`: nobody can promise a thread will
/// not panic.
fn mailbox_of(mailbox: &Mutex<Mailbox>) -> std::sync::MutexGuard<'_, Mailbox> {
    mailbox
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Creates this session's mailbox: an EMPTY, 0600 file with the nonce in
/// its name (#363).
///
/// It lives where the daemon's socket lives and by the same criterion:
/// `$XDG_RUNTIME_DIR` if there is one —already 0700 of the user's— and if
/// not `/tmp/norte-<uid>`. The mode is set on CREATION and not afterward: a
/// file born 0644 and fixed up later has a window where another user can
/// open it, and what is written here sends a shell to a directory.
///
/// `None` if it cannot be done. The caller degrades: the panel does not
/// drag the shell along and the hook is not installed. That is preferable
/// to the two alternatives —crashing, or typing the `cd` again—.
fn create_mailbox(nonce: &Nonce) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(r) => std::path::PathBuf::from(r).join("norte"),
        // With no `XDG_RUNTIME_DIR`, the same place as the daemon's socket:
        // `/tmp/norte-<uid>`. The uid comes from the owner of a file we just
        // created, which is our euid with no `unsafe` (rule 5) — the same
        // trick `norte-client` uses to name that directory.
        None => std::path::PathBuf::from(format!("/tmp/norte-{}", uid_own()?)),
    };
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("subshell-{}.cd", nonce.as_str()));
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .ok()?;
    Some(path)
}

/// This process's uid WITHOUT `unsafe` (rule 5): the owner of a file we
/// just created is our euid.
///
/// Only used to NAME the `/tmp` directory, as in `norte-client`. What
/// truly protects it is that directory's 0700 mode and the mailbox's 0600,
/// not the number in the name.
fn uid_own() -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;
    let probe = std::env::temp_dir().join(format!(".norte-uid-{}", std::process::id()));
    std::fs::File::create(&probe).ok()?;
    let uid = std::fs::metadata(&probe).ok().map(|m| m.uid());
    let _ = std::fs::remove_file(&probe);
    uid
}

/// Leaves `bytes` in the mailbox, in one piece.
///
/// Via a temp file and `rename` and not by writing over it: the hook can
/// read it at any moment —it runs on EVERY prompt— and half a path is a
/// `cd` to somewhere it should not be. The `rename` within the same
/// directory is atomic, so the shell sees the whole path or the previous
/// one, never a fragment.
fn write_mailbox(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = path.with_extension("cd.tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
    }
    std::fs::rename(&tmp, path)
}

/// Writes to the pty.
///
/// Used by both writers. The distinction matters: typing a command puts
/// something on the line, and answering a terminal query does not — the
/// program that asked is waiting for those bytes, not `readline`.
fn write_raw(write: &Writer, bytes: &[u8]) -> std::io::Result<()> {
    let mut e = write
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    e.write_all(bytes)?;
    e.flush()
}

fn launch_reader(
    mut reader: PtyReader,
    mailbox: Arc<Mutex<Mailbox>>,
    write: Writer,
    nonce: norte_frontend::subshell::Nonce,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    mailbox_of(&mailbox).closed = true;
                    return;
                }
                Ok(n) => {
                    // Answering comes BEFORE anything else: the shell is
                    // STOPPED waiting for it. Outside the mailbox's lock,
                    // which does not matter here, and `write_raw`
                    // takes its own.
                    if let Some(r) = norte_frontend::subshell::terminal_reply(&buf[..n]) {
                        let _ = write_raw(&write, &r);
                    }
                    let mut b = mailbox_of(&mailbox);
                    // The previous chunk's tail goes FIRST: a marker split
                    // between two reads is reconstructed here, and without
                    // this the cwd would be lost and the escape sequence
                    // would show up on screen.
                    let mut chunk = std::mem::take(&mut b.cola);
                    chunk.extend_from_slice(&buf[..n]);
                    let s = scan_cwd(&chunk, &nonce);
                    b.cola = s.tail;
                    if let Some(c) = s.cwd {
                        // The prompt hook spoke: the shell has just
                        // finished whatever it had and is about to paint
                        // its `PS1`. And it has already picked up the
                        // mailbox if there was anything, because the `cd`
                        // goes INSIDE the hook and before the
                        // announcement: this is where it has ended up, not
                        // where it was.
                        b.cwd = Some(c);
                    }
                    b.pending.extend_from_slice(&s.visible);
                    // The TAIL of what was written is kept: a process
                    // writing without end while nobody is looking must not
                    // eat up norte's memory.
                    //
                    // The cut point is looked for at the next `\n`
                    // (terminal rule 1, not the names one): norte chooses
                    // the cut point, so cutting in the middle of a CSI
                    // would leave the terminal eating the bytes behind it
                    // as parameters — the dump's first line would come out
                    // broken.
                    if b.pending.len() > BUFFER_MAX {
                        let overflow = b.pending.len() - BUFFER_MAX;
                        let cut = b.pending[overflow..]
                            .iter()
                            .position(|c| *c == b'\n')
                            .map_or(b.pending.len(), |p| overflow + p + 1);
                        b.pending.drain(..cut);
                    }
                }
            }
        }
    });
}

/// The BLOCKING reader `portable_pty` returns, given its own name so the
/// thread's signature reads well.
type PtyReader = Box<dyn std::io::Read + Send>;

/// The bytes a key sends to a shell, or `None` if this key means nothing
/// there.
///
/// It is translated instead of copying the tty's raw stream, and that is
/// the decision that avoids the classic bug: with a thread reading
/// `/dev/tty` raw, on releasing the subshell that thread stays blocked
/// inside a `read` and eats the reader's NEXT key — the one that was
/// already meant for the panels. With a single reader (crossterm's, the one
/// the TUI already uses) that cannot happen.
///
/// The price is what is not in the table: mouse and bracketed paste do not
/// reach the shell. A shell does not ask for them, and the alternative was
/// the stolen key.
///
/// ```
/// use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
/// use norte_tui::subshell::key_to_bytes;
///
/// assert_eq!(key_to_bytes(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)), Some(b"a".to_vec()));
/// assert_eq!(key_to_bytes(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Some(b"\r".to_vec()));
/// // Ctrl+C travels as byte 3, which is what makes it interrupt.
/// assert_eq!(key_to_bytes(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(vec![3]));
/// ```
pub use crate::termpanel::key_to_bytes;

#[cfg(test)]
mod tests {
    use super::*;

    /// A bash with NOBODY's configuration, interactive.
    ///
    /// `--norc --noprofile` because what is tested is what norte installs,
    /// not whatever the test runner's `.bashrc` does; `-i` because a
    /// non-interactive shell has no prompt, and the prompt hook is exactly
    /// what needs to be seen working.
    fn bash(dir: &std::path::Path) -> Subshell {
        Subshell::start_with(
            std::path::Path::new("/bin/bash"),
            &["--norc", "--noprofile", "-i"],
            dir,
            (80, 24),
        )
        .expect("arranca bash")
    }

    /// **A real, live shell that remembers** (#142).
    ///
    /// Asserts on what ONLY the shell can produce (`echo $((6*7))`), not on
    /// the command's text: line discipline echo returns what was typed
    /// as-is, so looking for the command itself in the output does not
    /// prove anything was executed.
    #[test]
    fn a_subshell_lives_between_two_commands() {
        let dir = tempfile::tempdir().expect("tempdir");
        // `start` uses `$SHELL` (`login_shell`), i.e. the test runner's:
        // only what every POSIX shell does the same way is checked.
        let mut sh = bash(dir.path());

        sh.write(b"echo uno-$((6*7))\n").expect("escribe");
        assert!(
            wait_until(&sh, b"uno-42").is_some(),
            "the shell EXECUTES the first one"
        );
        sh.write(b"echo dos-$((6*7))\n").expect("escribe");
        assert!(
            wait_until(&sh, b"dos-42").is_some(),
            "and stays alive for the second: that is what makes it a subshell"
        );
        sh.matar();
    }

    /// **The shell STATES where it is, and norte understands it.**
    ///
    /// This is the test that was missing, and its absence let through a bug
    /// where the hook carried the `ESC` and the `BEL` RAW: the line editor
    /// ate the `ESC ]` as a meta prefix, so what ended up installed printed
    /// `777;norte-cwd;/home` with no OSC frame. `scan_cwd` recognized
    /// nothing, the panel NEVER followed the shell —#142's whole promise—
    /// and the reader saw that text on every prompt. With the shell started
    /// in `dir`, the first prompt already has to announce it.
    /// Runs for ALL THREE shells norte prepares, each with its own hook
    /// syntax: they are three different texts and a single test covered
    /// one. It skips whichever is not installed — it is an integration
    /// test, not a reason to redden someone's machine.
    #[test]
    fn the_shell_announces_where_it_is() {
        let mut tested = 0;
        for (bin, args) in [
            ("/bin/bash", &["--norc", "--noprofile", "-i"][..]),
            ("/usr/bin/zsh", &["-f", "-i"][..]),
            ("/usr/bin/fish", &["--no-config", "-i"][..]),
        ] {
            let path = std::path::Path::new(bin);
            if !path.exists() {
                continue;
            }
            tested += 1;
            let dir = tempfile::tempdir().expect("tempdir");
            let real = dir.path().canonicalize().expect("canonicalize");
            let sh = Subshell::start_with(path, args, &real, (80, 24)).expect("arranca");
            let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut announced = None;
            while std::time::Instant::now() < until && announced.is_none() {
                announced = sh.cwd();
                let _ = sh.drain();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert_eq!(
                announced.as_deref(),
                Some(real.as_path()),
                "{bin}: the hook did not arrive whole, or said a different place"
            );
        }
        assert!(tested > 0, "no known shell on this machine");
    }

    /// And FOLLOWS the panel: a `cd` over a directory with a hostile name
    /// does not turn into a command.
    ///
    /// The name carries bytes readline EXECUTES (`0x15` erases the whole
    /// line), which quoting did not stop: the quote protects a byte that
    /// enters the buffer, and that one does not enter it. `/tmp/…\x15id #`
    /// executed `id`.
    #[test]
    fn the_subshell_follows_the_pane_with_a_hostile_name() {
        use std::os::unix::ffi::OsStrExt as _;
        let root = tempfile::tempdir().expect("tempdir");
        let root = root.path().canonicalize().expect("canonicalize");
        let hostile = root.join(std::ffi::OsStr::from_bytes(
            b"a b; echo pwned\x15echo pwned #",
        ));
        std::fs::create_dir(&hostile).expect("mkdir");

        let sh = bash(&root);
        // The `cd` is only sent with the shell STOPPED at its prompt, which
        // is what this loop waits for (and what keeps it from being
        // concatenated with whatever the reader had left half-typed).
        let mut sh = sh;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut sent = false;
        // What is drained is KEPT: when this loop once ran out under load
        // (#360) the failure did not say what the shell had written up to
        // then, and without that it was impossible to tell "the prompt
        // never arrived" apart from "it arrived and the permission was
        // lost". That is the difference that cost the diagnosis.
        let mut seen = Vec::new();
        while std::time::Instant::now() < until && !sent {
            seen.extend_from_slice(&sh.drain());
            sent = sh.ir_a(&hostile).expect("cd");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            sent,
            "there was never a prompt to send the cd to; the shell wrote: {}",
            String::from_utf8_lossy(&seen)
        );

        // Checked by the MARKER, not by the echo: the shell says where it
        // is, and that is where it has to be.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut arrived = false;
        while std::time::Instant::now() < until {
            seen.extend_from_slice(&sh.drain());
            if sh.cwd().as_deref() == Some(hostile.as_path()) {
                arrived = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            arrived,
            "the shell did not end up inside the hostile directory"
        );
        // And nothing was executed. `pwned` appears in the directory's
        // name —and therefore in many shells' prompts—, so what is looked
        // for is an `echo`'s OUTPUT: the word alone on its own line.
        let text = String::from_utf8_lossy(&seen);
        assert!(
            !text.lines().any(|l| l.trim() == "pwned"),
            "something from the name was executed: {text}"
        );
        sh.matar();
    }

    /// **A Ctrl+L does not turn off the panel's following** (#360).
    ///
    /// Ctrl+L is not a command: `readline`/ZLE run it as a function that
    /// clears the screen and REPAINTS the prompt, leaving the line exactly
    /// as it was. It puts nothing into it.
    ///
    /// It used to turn it off all the same. When norte typed the `cd`, a
    /// permission was needed —"a marker arrived and nobody has typed since
    /// then"— and `write` lowered it for EVERYTHING that was sent. The
    /// repaint does not run `PROMPT_COMMAND` —bash runs that before reading
    /// a NEW command—, so no marker came after the Ctrl+L and the
    /// permission stayed down until the next Enter. In between, the panel
    /// did not follow the shell: #142's whole promise, turned off by the
    /// clear-screen key.
    ///
    /// Since #363 there is no permission to lower: the destination is
    /// noted in the mailbox no matter what, and the hook picks it up at
    /// the next prompt. The test stays because the property is still the
    /// same —a Ctrl+L must not leave the panel unable to drag the shell
    /// along— and because it is the race the suite once saw under load.
    ///
    /// The Enter in the middle is the READER's and not norte's: the hook
    /// runs between two commands, so the shell needs to reach one. That is
    /// the price of the change, and it is the one #363 accepted in
    /// exchange for closing the injection: the move applies at the next
    /// prompt and not instantly.
    #[test]
    fn a_ctrl_l_does_not_turn_off_tailing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = dir.path().canonicalize().expect("canonicalize");
        let other = dir.join("otro");
        std::fs::create_dir(&other).expect("mkdir");
        let mut sh = bash(&dir);
        wait_idle(&sh);
        sh.write_key(b"\x0c").expect("ctrl+l");
        let _ = sh.drain();

        assert!(
            sh.ir_a(&other).expect("cd"),
            "a Ctrl+L left the panel unable to follow the shell"
        );
        // The line is empty, so the reader's Enter executes nothing: it
        // only takes the shell to its next prompt, which is where the
        // hook picks up the mailbox.
        sh.write_key(b"\n").expect("intro");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut arrived = false;
        while std::time::Instant::now() < until {
            let _ = sh.drain();
            if sh.cwd().as_deref() == Some(other.as_path()) {
                arrived = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(arrived, "the `cd` after the Ctrl+L never got executed");
        sh.matar();
    }

    /// **A half-typed line does not turn into a command.**
    ///
    /// This is the worst bug this had: the `cd` was typed in on entering,
    /// so an `rm -rf tmpdir` the reader had typed and not executed turned
    /// into `rm -rf tmpdircd -- \'/other\'` as soon as they came back. And
    /// what the normal, not the rare, case does is that this subshell
    /// PROMISES to keep the half-typed line intact.
    ///
    /// Since #363 nothing is typed: the destination is left in the mailbox
    /// and the hook picks it up at the next prompt. So `ir_a` DOES accept —
    /// there is somewhere to note it down— and what is checked is that the
    /// reader's line stays intact and is not executed.
    #[test]
    fn a_half_typed_line_is_not_executed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = dir.path().canonicalize().expect("canonicalize");
        let other = dir.join("otro");
        std::fs::create_dir(&other).expect("mkdir");
        let mut sh = bash(&dir);
        // Wait for the shell to SETTLE, and only then leave something
        // half-typed: the install commands each produce a prompt, and
        // typing over one that has not arrived yet would be a test race.
        wait_idle(&sh);
        // `pwn''ed` and not `pwned`: what the pty ECHOES is the line
        // as-is, so looking for "pwned" would find the echo and not the
        // execution. With the quotes in the middle, the whole string only
        // appears if bash executed it.
        sh.write(b"echo pwn''ed").expect("media linea");
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = sh.drain();

        assert!(
            sh.ir_a(&other).expect("cd"),
            "there is a mailbox to note it in, so it is noted"
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        let text = String::from_utf8_lossy(&sh.drain()).into_owned();
        assert!(
            !text.contains("pwned"),
            "what the reader did not execute got executed: {text}"
        );
        // And the line is still there: Enter executes it WHOLE and alone.
        sh.write(b"\n").expect("intro");
        std::thread::sleep(std::time::Duration::from_millis(400));
        let text = String::from_utf8_lossy(&sh.drain()).into_owned();
        assert!(
            text.contains("pwned"),
            "the reader's line did not survive the move: {text}"
        );
        sh.matar();
    }

    /// **Type-ahead does not concatenate with anything** (#363).
    ///
    /// The case the `en_prompt` flag could not see, and that needs no
    /// particular shell: what the reader types while the shell is BUSY
    /// waits in the pty's queue. The prompt marker arrived with those bytes
    /// still unconsumed —"nobody typed since the marker" was true and "the
    /// line is empty" was false— and the `cd` got stuck onto it:
    /// `rm -rf tmpdir __norte_cd \'...\'`.
    ///
    /// With the mailbox there is nothing to concatenate onto. Reproduced
    /// here with a `sleep` in front, which is what keeps readline from
    /// reading.
    #[test]
    fn what_is_typed_while_the_shell_is_busy_does_not_drag_in_a_cd() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = dir.path().canonicalize().expect("canonicalize");
        let other = dir.join("otro");
        std::fs::create_dir(&other).expect("mkdir");
        let mut sh = bash(&dir);
        wait_idle(&sh);

        // The shell goes to sleep, and the reader types over it with no
        // Enter: those bytes stay in the pty's queue until `sleep` ends.
        sh.write(b"sleep 1\n").expect("sleep");
        std::thread::sleep(std::time::Duration::from_millis(150));
        // Same trick as the test next door: the echo cannot be confused
        // with the execution.
        sh.write(b"echo pwn''ed").expect("type-ahead");
        // And norte moves the panel exactly when the prompt comes back.
        assert!(sh.ir_a(&other).expect("cd"), "noted in the mailbox");
        std::thread::sleep(std::time::Duration::from_millis(1800));

        let text = String::from_utf8_lossy(&sh.drain()).into_owned();
        assert!(
            !text.contains("pwned"),
            "the reader's type-ahead ended up being executed: {text}"
        );
        // And the shell DID move: the hook picked up the mailbox at its
        // prompt.
        assert_eq!(
            sh.cwd().as_deref(),
            Some(other.as_path()),
            "the hook did not pick up the mailbox"
        );
        sh.matar();
    }

    /// The keys a shell needs arrive as the bytes it expects, and whatever
    /// means nothing there does not arrive.
    #[test]
    fn keys_arrive_as_terminal_bytes() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let t = |code, mods| key_to_bytes(&KeyEvent::new(code, mods));
        assert_eq!(t(KeyCode::Up, KeyModifiers::NONE), Some(b"\x1b[A".to_vec()));
        assert_eq!(t(KeyCode::Backspace, KeyModifiers::NONE), Some(vec![0x7f]));
        assert_eq!(t(KeyCode::Char('d'), KeyModifiers::CONTROL), Some(vec![4]));
        // Alt is ESC in front: that is what makes `alt+f` move a word.
        assert_eq!(
            t(KeyCode::Char('f'), KeyModifiers::ALT),
            Some(vec![0x1b, b'f'])
        );
        // The function ones arrive: an `htop` inside the subshell uses
        // them, and F10 is what closes it.
        assert_eq!(
            t(KeyCode::F(1), KeyModifiers::NONE),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            t(KeyCode::F(10), KeyModifiers::NONE),
            Some(b"\x1b[21~".to_vec())
        );
        // Control chords that are not letters are also bytes: Ctrl+\ is
        // SIGQUIT, not a backslash.
        assert_eq!(
            t(KeyCode::Char('\\'), KeyModifiers::CONTROL),
            Some(vec![28])
        );
        assert_eq!(t(KeyCode::Char(' '), KeyModifiers::CONTROL), Some(vec![0]));
        // And a key a shell does not use is not made up.
        assert_eq!(t(KeyCode::F(20), KeyModifiers::NONE), None);
        assert_eq!(t(KeyCode::CapsLock, KeyModifiers::NONE), None);
    }

    /// A non-ASCII character travels as whole UTF-8: typing `ñ` into the
    /// subshell must not send half a character.
    #[test]
    fn a_multibyte_character_travels_whole() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        assert_eq!(
            key_to_bytes(&KeyEvent::new(KeyCode::Char('ñ'), KeyModifiers::NONE)),
            Some("ñ".as_bytes().to_vec())
        );
    }

    /// Waits for what the shell wrote to contain `needle`, without hanging.
    ///
    /// Polls instead of sleeping a fixed while: a shell takes as long as it
    /// takes to start the test runner's `.bashrc`, and a `sleep` picked by
    /// eye is a test that goes red on someone else's loaded machine.
    /// Returns ALL that accumulated, not a `bool`: the caller usually wants
    /// to assert something about what arrived, and with a `bool` those
    /// bytes stayed inside this function and were dropped — the check
    /// afterward looked at an already-empty mailbox and could not fail no
    /// matter what it said.
    /// Waits for the shell to stop writing and to have announced a prompt.
    ///
    /// The install commands each produce a prompt —and a marker— ONE AT A
    /// TIME, and they arrive when they arrive: a test that types over one
    /// that has not arrived yet goes red from the race, not from the code.
    fn wait_idle(sh: &Subshell) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut still = 0;
        while std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if sh.drain().is_empty() && sh.cwd().is_some() {
                still += 1;
                if still >= 3 {
                    return;
                }
            } else {
                still = 0;
            }
        }
        panic!("the shell never settled");
    }

    fn wait_until(sh: &Subshell, needle: &[u8]) -> Option<Vec<u8>> {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen: Vec<u8> = Vec::new();
        while std::time::Instant::now() < until {
            seen.extend_from_slice(&sh.drain());
            if seen.windows(needle.len()).any(|w| w == needle) {
                return Some(seen);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        None
    }
}
