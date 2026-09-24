//! A LIVE shell behind a [`Screen`], for whoever wants a terminal pane.
//!
//! It is behind the `pty` feature and off by default, so the crate stays
//! what its front cover says: a pure grid. Whoever only wants to parse
//! bytes compiles none of this.
//!
//! # Why it lives here and not in every frontend
//!
//! Both need it —the terminal paints it with `ratatui`, the window sends
//! the rows through the bridge— and it is exactly the same pty, the same
//! reader thread and the same delicate part: answering terminal queries
//! BEFORE anything else, because the shell stops until it is answered.
//! Writing it twice means two places to fix the same bug.
//!
//! # What this module does NOT decide
//!
//! **What program gets launched and with what environment.** That is
//! brought by the caller, in [`Startup`]: resolving the reader's shell and
//! the `NORTE_LEVEL` contract are norte's rules, not an emulator's, and
//! putting them here would tie this crate to the presentation one — exactly
//! backwards from how the layers go.
//!
//! The only thing from the environment that IS its own is [`TERM`], and for
//! one reason: whoever knows which sequences this grid understands is this
//! grid.

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};

use crate::Screen;

/// What the child on the other side is told.
///
/// `xterm-256color` since #366, and `vt100` before. The rule has not
/// changed —what is not honored is not announced, which is worse than
/// falling short— what has changed is what is honored:
///
/// - full SGR, with the 256 colors and true color;
/// - CUP, the four movements, CHA and VPA;
/// - ED, EL, ECH, ICH, DCH, IL, DL, SU, SD, REP;
/// - the ALTERNATE screen (`?1049`, and the old `?47`), which is what keeps
///   a `less` from leaving its last frame stuck on exit;
/// - scroll regions (`DECSTBM`), which is what a program with a pinned
///   status line takes for granted;
/// - saving and restoring the cursor with its style, `RIS`, and the
///   line-drawing character set.
///
/// What is still missing: the mouse, OSC 52 clipboard, queries that expect
/// a reply (`DA`, `DSR`), bracketed paste mode and DEC double widths. None
/// of them makes a program paint something coherent that does not match
/// its state —which was the criterion—: whoever asks and gets no answer
/// degrades, and whoever sends a sequence this grid ignores sees nothing
/// odd on screen.
///
/// It is a surface where someone READS and then types a command against
/// what they read, so if anything announced stopped being honored, this is
/// what has to come down, not the bar.
pub const TERM: &str = "xterm-256color";

/// How much of what the shell wrote and has not been flushed yet is kept.
///
/// A `find /` writing nonstop while nobody repaints must not eat up memory.
/// The TAIL is kept: what is further back would not be seen anyway, because
/// the grid has whatever height it has.
const BUFFER_MAX: usize = 256 * 1024;

/// What shell to start, where, and with what environment.
pub struct Startup<'a> {
    /// The program. An ABSOLUTE path: `portable_pty` searches by `cwd` if it
    /// is not one, and a file manager's `cwd` is the directory being looked
    /// at — a `bash` left there would get executed (#302, ADR 0082).
    pub program: &'a std::path::Path,
    /// Where it sits.
    pub dir: &'a std::path::Path,
    /// Columns and rows.
    pub tam: (u16, u16),
    /// Variables ADDED to the inherited environment.
    ///
    /// The rest is inherited on purpose: it is the environment the reader's
    /// own shell gave norte, and stripping it would leave a shell without a
    /// `PATH` or `HOME` that nobody asked for.
    pub env: &'a [(std::ffi::OsString, std::ffi::OsString)],
}

/// What the reader thread leaves for whoever repaints.
#[derive(Default)]
struct Buzon {
    /// Bytes read from the pty and not yet fed to the grid.
    pending: Vec<u8>,
    /// The pty closed: the shell is gone.
    closed: bool,
}

/// How something is sent to the pty.
///
/// A CHANNEL, and not the shared writer behind a mutex this used to be.
/// There are two legitimate senders —keystrokes, and the RESPONSES to
/// terminal queries the shell stops for until it is answered— and sharing
/// the writer between the two was a deadlock waiting to happen: writing to
/// a pty BLOCKS when the child's input queue fills up (around 4 KiB) and
/// the child is not reading. With the mutex, the reader thread would sit
/// inside `write_all` holding it, and the next keystroke would block
/// whoever sent it — in the window, the task that serves EVERYTHING else.
/// The window would end up completely dead while looking alive.
///
/// Repro that showed it: `printf '\e[c%.0s' {1..1000000} > f; cat f`.
///
/// With the channel, the only one that writes to the pty is its own
/// thread, so there is nothing to share and nobody can hold anybody up.
type Entry = std::sync::mpsc::SyncSender<Vec<u8>>;

/// How many sends fit before dropping.
///
/// Filling it up requires the child to have stopped reading its input, and
/// then what is being dropped are keystrokes that child was not going to
/// read either. Dropping them is worse than delivering them and much
/// better than blocking whoever sends them.
const COLA_MAX: usize = 1024;

/// How a shell's terminal query gets answered.
///
/// Received from the caller instead of decided here because the honest
/// answer depends on what the emulator claims to be, and today that table
/// lives in `norte-frontend` next to the subshell's — which is where
/// someone will go looking for it.
///
/// **Watch out for what this is**: an UNREQUESTED write into the shell's
/// input, triggered by CONTENT that passes through the pty. A file with
/// `\x1b[c` inside it types the response wherever the line editor's cursor
/// is. It executes nothing —the responses are fixed constants with no
/// CR— and any real terminal does the same, but it is worth knowing.
pub type Responder = fn(&[u8]) -> Option<Vec<u8>>;

/// A live shell with its grid.
pub struct Shell {
    screen: Screen,
    entry: Entry,
    maestro: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    buzon: Arc<Mutex<Buzon>>,
    tam: (u16, u16),
}

impl Shell {
    /// Starts the shell.
    ///
    /// # Errors
    /// Whatever fails while opening the pty or launching the program.
    pub fn open(a: &Startup<'_>, responder: Responder) -> std::io::Result<Self> {
        let tam = (a.tam.0.max(1), a.tam.1.max(1));
        let system = portable_pty::native_pty_system();
        let par = system
            .openpty(portable_pty::PtySize {
                rows: tam.1,
                cols: tam.0,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(std::io::Error::other)?;
        let mut cmd = portable_pty::CommandBuilder::new(a.program);
        cmd.cwd(a.dir);
        for (k, v) in a.env {
            cmd.env(k, v);
        }
        cmd.env("TERM", TERM);
        // The inherited size is the OUTER terminal's, which has nothing to
        // do with this pane: whoever asks through there would get the
        // wrong width. The pty already reports the right one via
        // `TIOCGWINSZ`.
        cmd.env_remove("COLUMNS");
        cmd.env_remove("LINES");
        let child = par
            .slave
            .spawn_command(cmd)
            .map_err(std::io::Error::other)?;
        // The slave is DROPPED: while it is held open, closing the shell
        // does not close the pty and the reader would never see EOF.
        drop(par.slave);
        let escritor = par.master.take_writer().map_err(std::io::Error::other)?;
        let reader = par
            .master
            .try_clone_reader()
            .map_err(std::io::Error::other)?;
        let entry = launch_escritor(escritor);
        let buzon = Arc::new(Mutex::new(Buzon::default()));
        launch_reader(reader, Arc::clone(&buzon), entry.clone(), responder);
        Ok(Self {
            screen: Screen::new(tam.0, tam.1),
            entry,
            maestro: par.master,
            child,
            buzon,
            tam,
        })
    }

    /// Flushes into the grid whatever the shell wrote, and says whether
    /// anything changed.
    ///
    /// Returning whether there were bytes is what avoids repainting when
    /// the shell is quiet, which is almost always.
    pub fn bombear(&mut self) -> bool {
        let pending = {
            let mut b = buzon_de(&self.buzon);
            std::mem::take(&mut b.pending)
        };
        if pending.is_empty() {
            return false;
        }
        self.screen.alimentar(&pending);
        true
    }

    /// Sends bytes to the shell, NEVER blocking the caller.
    ///
    /// It is the difference that matters: writing to a pty blocks if the
    /// child stopped reading, and the caller here is the task that serves
    /// the rest of the window. It gets queued and returns; the pty's own
    /// thread writes it.
    ///
    /// With the queue full it gets DROPPED, and that is correct: filling it
    /// up requires the child to be a thousand sends behind on reading its
    /// input, so what gets dropped are keystrokes that child was not going
    /// to read either.
    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.entry.try_send(bytes.to_vec());
    }

    /// Adjusts the grid AND the pty to the pane's size.
    ///
    /// Both, and it matters that it is both: without telling the pty, a
    /// full-screen program keeps painting for the old size and what shows
    /// is garbage. Does nothing if it did not change.
    pub fn resize(&mut self, tam: (u16, u16)) {
        let tam = (tam.0.max(1), tam.1.max(1));
        if tam == self.tam {
            return;
        }
        self.tam = tam;
        self.screen.resize(tam.0, tam.1);
        let _ = self.maestro.resize(portable_pty::PtySize {
            rows: tam.1,
            cols: tam.0,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Is the shell gone?
    pub fn dead(&mut self) -> bool {
        buzon_de(&self.buzon).closed || matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// The grid, to paint it.
    #[must_use]
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Kills the shell and waits for it.
    ///
    /// Waiting after the `kill` is not courtesy: without reaping the child
    /// it stays a zombie until the process exits.
    pub fn matar(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// **The shell dies WITH the pane, however the process exits.**
///
/// Without this, closing the slot removed the node and left the shell
/// alive with its reader thread, its pty and its open directory —a busy
/// mount stayed busy— with no pane to see it in and no way back to it. On
/// exit it died by accident, from the kernel's `SIGHUP`, so anything that
/// ignored that signal survived the whole process.
impl Drop for Shell {
    fn drop(&mut self) {
        self.matar();
    }
}

fn buzon_de(buzon: &Mutex<Buzon>) -> std::sync::MutexGuard<'_, Buzon> {
    buzon
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The ONLY one that writes to the pty, on its own thread.
///
/// Being just one is what removes the deadlock from the middle: the
/// blocking `write_all` happens here and not in the task of whoever types,
/// and there is no shared mutex anyone could get stuck holding while
/// waiting.
///
/// Dies when the last sender is dropped, i.e. with the `Shell`.
fn launch_escritor(mut escritor: Box<dyn std::io::Write + Send>) -> Entry {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(COLA_MAX);
    std::thread::spawn(move || {
        while let Ok(bytes) = rx.recv() {
            if escritor.write_all(&bytes).is_err() || escritor.flush().is_err() {
                return;
            }
        }
    });
    tx
}

fn launch_reader(
    mut reader: Box<dyn std::io::Read + Send>,
    buzon: Arc<Mutex<Buzon>>,
    entry: Entry,
    responder: Responder,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    buzon_de(&buzon).closed = true;
                    return;
                }
                Ok(n) => {
                    // Answering comes BEFORE anything else: the shell is
                    // STOPPED waiting for it and the answer cannot wait for
                    // someone to repaint. See [`Responder`] for what this
                    // is. It is QUEUED, not written directly: this thread
                    // is the one that feeds the grid, and blocking it
                    // inside a `write_all` with a child that is not reading
                    // was half the jam the channel exists to prevent.
                    if let Some(r) = responder(&buf[..n]) {
                        let _ = entry.try_send(r);
                    }
                    let mut b = buzon_de(&buzon);
                    b.pending.extend_from_slice(&buf[..n]);
                    // The cut falls on WHATEVER byte, and that is accepted:
                    // an escape split there loses its `ESC [` and its tail
                    // gets painted as text. It is ugly and not dangerous
                    // —the grid's guarantee holds, a control byte never
                    // reaches a cell—, and it only happens if a program
                    // wrote 256 KiB while nobody was repainting. Looking
                    // for a sequence boundary here would force the parser
                    // into the reader thread just to drop bytes.
                    if b.pending.len() > BUFFER_MAX {
                        let extra = b.pending.len() - BUFFER_MAX;
                        b.pending.drain(..extra);
                    }
                }
            }
        }
    });
}
