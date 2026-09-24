//! Does this terminal know how to paint graphics via kitty's protocol? The
//! same question [`crate::alt_menu`] asks the KEYBOARD protocol, for the
//! IMAGE protocol: asked once, at startup, and cached.
//!
//! T4 (WOW phase 5) adds what actually paints: [`escape_colocar`]/
//! [`escape_delete`] are the pure escapes — no I/O here, that runs in the run
//! loop, which owns the terminal — and [`mark_placed`]/[`delete_placed`]
//! keep count of which id is placed RIGHT NOW on the real terminal. That
//! count is PROCESS state, like `alt_menu::REQUESTED` (private, not linked
//! from here — the same reason [`query_support`] documents below):
//! yielding the terminal, an `Esc` that closes the viewer, and exiting all
//! need to be able to erase WITHOUT anyone passing them the `App` — the `App`
//! decides WHAT should be seen, this keeps count of what is really on
//! screen.
//!
//! The protocol is described at
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/>: an APC sequence
//! (`\x1b_G…\x1b\\`) that a terminal that does not speak it simply IGNORES,
//! answering nothing. That is why the probe sends a DA1 (`\x1b[c`) RIGHT
//! AFTER, which every VT100-compatible terminal does answer: without it there
//! would be nothing to wait for, and the probe would time out on every
//! terminal with no support.

use std::io::{self, IsTerminal, Read, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use base64::Engine as _;
use ratatui::layout::Rect;

/// The id used to ask. Arbitrary and ours alone: a response with another id
/// answers a different question and says nothing about ours.
const PROBE_ID: &str = "i=31";

/// The query: a 1x1 RGB image (`f=24`) transmitted inline (`t=d`), with
/// action `a=q` — "query", never draws anything — and a DA1 RIGHT AFTER to
/// have something to wait for on a terminal that does not answer the APC.
const QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";

/// How long to wait for the response before calling it "no".
const DEADLINE: Duration = Duration::from_millis(200);

/// Does the terminal's answer say it can paint graphics?
///
/// Looks for the protocol's APC response (`\x1b_G…;OK\x1b\\`) WITH OUR ID.
/// Anything else — only the DA1 answer, a declared error, nothing at all — is
/// "no": whoever does not know, stays silent.
fn response_says_yes(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    text.split("\x1b_G")
        .skip(1)
        .any(|rest| match rest.split_once("\x1b\\") {
            Some((body, _)) => is_our_ok(body),
            None => false,
        })
}

/// `body` is what is between `\x1b_G` and `\x1b\\`: comma-separated keys
/// (`i=31`, `I=2`…) and AFTER a `;` the message (`OK`, `ENOTSUPPORTED`…).
///
/// Review, finding 1: the first version checked `contains(PROBE_ID)`, a
/// substring — and `"i=311;OK".contains("i=31")` is true, so the answer to
/// ANOTHER query (id 311, not 31) counted as a yes for ours. Here the keys
/// field is split from the message on the first `;`, and EACH key is compared
/// by exact EQUALITY against [`PROBE_ID`]: no id that merely shares a prefix
/// sneaks through.
fn is_our_ok(body: &str) -> bool {
    let Some((keys, message)) = body.split_once(';') else {
        return false;
    };
    message == "OK" && keys.split(',').any(|key| key == PROBE_ID)
}

/// What the terminal answered, asked ONCE.
static SUPPORT: OnceLock<bool> = OnceLock::new();

/// Asks the terminal whether it can paint graphics, and caches the answer.
///
/// Called at STARTUP, with raw mode already set and before the loop raises
/// its event reader, for the same two reasons `alt_menu::query_support`
/// already documents: with the reader alive, that thread holds the lock and
/// the query gives up; and under `--pick` stdout is the caller's data pipe,
/// so with no terminal on stdout it does not ask.
///
/// It has NO test: what it does is write to the control terminal and read it
/// back with a deadline. What is testable is the parsing (`response_says_yes`,
/// private — no brackets: linking from here, which is public, to a private
/// item is a denied `rustdoc::private_intra_doc_links` in the gate), and that
/// is tested. A test of this would need a fake pty that answered like kitty,
/// and that is testing the pty.
///
/// The APC query is sent, and a DA1 RIGHT AFTER: a terminal that does not
/// speak the protocol silently ignores the first one, and without the second
/// there would be nothing to wait for — the probe would always time out, and
/// startup would pay that delay on every terminal without support.
pub fn query_support() -> bool {
    *SUPPORT.get_or_init(|| {
        if !io::stdout().is_terminal() {
            return false;
        }
        ask().unwrap_or(false)
    })
}

/// [`query_support`]'s answer, without asking. If it was never asked,
/// "no".
#[must_use]
pub fn supported() -> bool {
    SUPPORT.get().copied().unwrap_or(false)
}

/// How many RAW bytes (before base64) each chunk of an APC carries.
///
/// The protocol chunks by the ALREADY-base64 payload length (4096 characters
/// is kitty's limit), so it is chunked raw at a multiple of 3: 3 raw bytes are
/// exactly 4 base64 characters, with no padding mid-chunk. `3 * 1024` raw
/// bytes = 4096 characters, exactly the limit.
const CHUNK_RAW_BYTES: usize = 3 * 1024;

/// The escape that places the image in the viewer's slot.
///
/// `a=T` transmits AND displays at once, at the CURSOR's position — the
/// CALLER has to move the cursor to `rect`'s corner (e.g.
/// `crossterm::cursor::MoveTo`) right BEFORE writing this (T4, the run loop):
/// this function only builds the string, it does not touch the cursor. `C=1`
/// also asks that PLACING not move the cursor: without it, kitty leaves it
/// after the image once done, and if that lands on the last row the screen
/// SCROLLS — with the alternate screen and ratatui painting by diff, that
/// shifts the whole frame (review, CRITICAL 1).
///
/// `q=2` silences the terminal's response (success AND error): without it,
/// kitty answers `\x1b_Gi=<id>;OK\x1b\\` to every chunk carrying `i`, and
/// since nobody consumes it, it reaches crossterm's event reader, which does
/// not parse APC — `\x1b_` is read as `Alt+_` and the rest as loose
/// keystrokes that enter the keymap (review, CRITICAL 3). The startup probe
/// itself avoids this by READING its response by hand; here it is simpler to
/// ask for silence.
///
/// `f=100` is PNG, FIXED — and it is a promise the CALLER has to keep, not
/// something this function checks: the `thumbnail` kind (`plugin.thumbnail`,
/// ADR 0107) can return PNG, JPEG, or WebP (`PluginThumbnail::mimetype`), and
/// `thumb::reencode` in `norte-plugin-host` really does fall back to JPEG when
/// the PNG does not fit its cap. kitty's protocol has no `f=` key for JPEG or
/// WebP — only PNG (100) or raw raster (24/32) — so sending either of those
/// two with `f=100` does not fail with a readable error: kitty rejects it
/// silently. `viewer_open::imagen_from_thumbnail` is what filters BEFORE
/// `bytes` reaches here (branch review, finding 1): everything that passes
/// through this function is already PNG. The bytes go in base64 because an
/// APC ends in `\x1b\\` and a PNG perfectly normally contains that pair:
/// sending it raw would cut the image in half and leave the rest written on
/// screen as text.
///
/// `c`/`r` are CELLS, not pixels: the terminal is told the SLOT and it fits
/// the image in, which is what keeps the image inside the frame when the
/// terminal has cells of a different size than assumed. The caller passes it
/// the frame's INTERIOR (no borders) — `rect` is not clipped here.
///
/// If `bytes` exceeds `CHUNK_RAW_BYTES` (private, not linked) it is chunked
/// into several APCs in a row: the first carries the WHOLE header (`i`, `f`,
/// `c`, `r`, `C`, `q`) plus `m=1`; the following ones carry `m` (`1` while
/// more remain, `0` on the last) and also `q=2` — each chunk is its own
/// command and kitty can answer any that carries `i`, so silence is asked for
/// on all of them, not just the first. This is the normal case, because a
/// real thumbnail (up to 1920 px on a side) never fits a single chunk.
///
/// `crop` shows only a PIECE of the raster, in its own pixels (the protocol's
/// `x`, `y`, `w`, `h`). It is what the zoom-in does (spec 2026-09-20): the
/// cells stay the same and what shrinks is what is shown in them. `None`
/// shows the whole image, the usual case.
///
/// ```
/// use norte_tui::kitty_graphics::escape_colocar;
/// use ratatui::layout::Rect;
///
/// let esc = escape_colocar(7, b"PNGFALSO", Rect::new(1, 2, 40, 20), None);
/// assert!(esc.starts_with("\x1b_G") && esc.ends_with("\x1b\\"));
/// assert!(!esc.contains(",x="), "with no crop its keys are not sent");
/// ```
#[must_use]
pub fn escape_colocar(
    id: u32,
    bytes: &[u8],
    rect: Rect,
    crop: Option<crate::viewer_open::Crop>,
) -> String {
    let engine = base64::engine::general_purpose::STANDARD;
    // `chunks` on an empty slice produces no chunk at all, and a zero-byte
    // thumbnail still needs ONE (empty) APC for the terminal to recognize
    // it — hence the `[&[][..]]` fallback.
    let chunks: Vec<&[u8]> = if bytes.is_empty() {
        vec![&[]]
    } else {
        bytes.chunks(CHUNK_RAW_BYTES).collect()
    };
    let total = chunks.len();
    let mut out = String::new();
    for (i, chunk) in chunks.into_iter().enumerate() {
        use std::fmt::Write as _;
        let last = i + 1 == total;
        let more = u8::from(!last);
        out.push_str("\x1b_G");
        if i == 0 {
            // `write!` on a `String` never fails (rule 6: no
            // `unwrap`/`expect` outside tests, and here it is not even
            // needed).
            let _ = write!(
                out,
                "a=T,i={id},f=100,c={},r={},C=1,q=2",
                rect.width, rect.height
            );
            // The chunk goes BEFORE `m`, which closes the header. Its four
            // keys go together or none goes at all: kitty takes whichever
            // are missing as "from the origin" and "to the end", and half a
            // pair would show a piece nobody asked for.
            if let Some(r) = crop {
                let _ = write!(out, ",x={},y={},w={},h={}", r.x, r.y, r.w, r.h);
            }
            let _ = write!(out, ",m={more}");
        } else {
            let _ = write!(out, "m={more},q=2");
        }
        out.push(';');
        out.push_str(&engine.encode(chunk));
        out.push_str("\x1b\\");
    }
    out
}

/// The escape that erases ONLY this image — data AND placement.
///
/// `d=I` (uppercase) erases the placement AND frees the BYTES the terminal
/// keeps stored for this id; `d=i` (lowercase, what the original ticket
/// asked for) only erases the placement and leaves the data alive in the
/// terminal's memory — with `mint_image_id` never recycling an id, every file
/// that gets looked at would leave a copy of its PNG there for the rest of
/// the session (review, IMPORTANT 6; the ticket's test was fixed with it).
/// `i=<id>` still scopes the erase to THIS image: without it, every image on
/// the whole terminal would be erased, including another program's in
/// another tab. `q=2` silences the response, same reason as
/// [`escape_colocar`].
///
/// ```
/// use norte_tui::kitty_graphics::escape_delete;
/// assert!(escape_delete(7).contains("i=7"));
/// ```
#[must_use]
pub fn escape_delete(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}

/// The id of the image placed RIGHT NOW on the real terminal, or `0` if
/// there is none — PROCESS state, like [`crate::alt_menu`]'s
/// `REQUESTED`/`YIELDED`: `0` is not a valid id because
/// [`crate::viewer_open::ImagenPlaced::id`] starts at 1, so it serves as a
/// sentinel without wrapping an atomic in an `Option`.
static PLACED: AtomicU32 = AtomicU32::new(0);

/// Notes that `id` was just placed on the real terminal.
///
/// Called by the run loop right after successfully writing [`escape_colocar`]
/// — never before, or a write failure would leave this count believing an
/// image is up that the terminal never saw.
pub fn mark_placed(id: u32) {
    PLACED.store(id, Ordering::Relaxed);
}

/// Is `id` the image placed RIGHT NOW?
///
/// Review, IMPORTANT 4: the run loop uses this to avoid retransmitting the
/// whole PNG every frame when nothing changed — without this, a STILL viewer
/// resent its thumbnail (up to 1920 px on a side, in base64) on every
/// `session_tick` (once a second), with an erase+place flicker thrown in.
#[must_use]
pub fn ya_placed(id: u32) -> bool {
    PLACED.load(Ordering::Relaxed) == id
}

/// Erases the image placed RIGHT NOW, if any, and forgets which one it was.
///
/// Idempotent — calling it twice in a row writes nothing the second time —
/// and never fails toward the caller: an escape that could not be written is
/// swallowed with a `tracing::debug!` (the painting rule: an image that fails
/// to erase is a nuisance, not a reason to bring down the TUI or the
/// suspension).
///
/// This is the erase point SHARED by all four moments (T4): closing the
/// viewer or moving it to another file reach it through the diff the run loop
/// makes every frame (compares the desired id against this one); yielding the
/// terminal ([`crate::suspend::suspend_terminal`]) and exiting
/// ([`crate::tty::restore`]) call it here directly because neither of the two
/// is guaranteed a following frame to make that diff.
pub fn delete_placed(out: &mut impl Write) {
    let id = PLACED.swap(0, Ordering::Relaxed);
    if id == 0 {
        return;
    }
    match out
        .write_all(escape_delete(id).as_bytes())
        .and_then(|()| out.flush())
    {
        Ok(()) => {}
        Err(e) => {
            // MINOR 7: `write_all` can fail mid-APC — without closing it,
            // everything painted afterward would be read as its payload. The
            // terminator is ALWAYS written after a failure, best-effort.
            let _ = out.write_all(b"\x1b\\");
            tracing::debug!(error = %e, id, "could not erase the placed image");
        }
    }
}

/// Writes the query to `/dev/tty` and reads the response with a short
/// deadline, until seeing the `c` that closes the DA1 or until [`DEADLINE`]
/// runs out.
///
/// A failure opening or writing the control terminal reads as "no" from
/// [`query_support`]: a presentation probe must not bring down startup.
///
/// `/dev/tty` has no `read_timeout` like a socket (rule 5: a real `poll` is
/// `unsafe`, and that `unsafe` belongs only to `norte-vfs-local`), so the read
/// runs on a separate thread and the asker waits with
/// [`std::sync::mpsc::Receiver::recv_timeout`].
///
/// **Review, finding 2 — why the thread keeps reading past the deadline,
/// instead of giving up with it.** The first design checked, byte by byte,
/// whether the asker was still listening, and gave up the moment it was not.
/// That means a LATE response (SSH with real latency) got read as ONE byte —
/// the one that made the send fail — and the rest (`[?62;c`) was left
/// unconsumed on the terminal, waiting for crossterm's event reader to start
/// up and eat it as user keystrokes. Here, the thread, once launched, no
/// longer checks whether anyone is listening: it keeps reading until it sees
/// the `c` (or an error/EOF) no matter what, and only then tries to send the
/// result — which, if the deadline already passed, nobody picks up, and it
/// does not matter: the thread's job was never to notify whoever gave up, it
/// is to DRAIN the terminal's entire response before another reader confuses
/// it with keyboard input.
///
/// This does not close the window entirely. A real race still exists: if the
/// response takes so long that the event reader has already started (beyond
/// this startup, inside `run`) BEFORE this thread finishes reading it, the
/// two compete for the same bytes of the same fd, and which one gets each
/// byte is undefined. Closing it entirely would require either waiting here
/// indefinitely (losing the short-deadline guarantee that is this probe's
/// whole point) or flushing the terminal's input buffer (`tcflush`, which is
/// `unsafe`/`libc` — rule 5 forbids it outside `norte-vfs-local`). The
/// residual risk is accepted, bounded to terminals with latency much greater
/// than [`DEADLINE`] (200 ms) AND that also take so long to answer that they
/// manage to overlap with the event reader's startup — not observed in this
/// task's local tests (tmux, kitty).
fn ask() -> io::Result<bool> {
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    tty.write_all(QUERY)?;
    tty.flush()?;
    let mut reader = tty.try_clone()?;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match reader.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let c = byte[0];
                    buf.push(c);
                    if c == b'c' {
                        break;
                    }
                }
            }
        }
        // Best-effort: if the asker is no longer listening (the deadline
        // passed), the send fails and is ignored — by then the drain above
        // already did what mattered.
        let _ = tx.send(buf);
    });

    let read = rx.recv_timeout(DEADLINE).unwrap_or_default();
    let support = response_says_yes(&read);
    // Without this, a "no" and a terminal that answered nothing at all are
    // indistinguishable from outside. This is step 6's evidence (the gate):
    // what each real terminal actually answered, not just the final yes/no.
    tracing::debug!(
        response = %String::from_utf8_lossy(&read).escape_debug(),
        support,
        "kitty graphics probe"
    );
    Ok(support)
}

#[cfg(test)]
mod tests {
    use super::response_says_yes;

    #[test]
    fn a_kitty_response_is_a_yes() {
        // kitty answers the query with OK for the id it was sent.
        assert!(response_says_yes(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;c"));
    }

    #[test]
    fn only_the_da1_response_is_a_no() {
        // A terminal that does not speak the protocol ignores the APC and
        // only answers DA1. That is the case for xterm, for VTE, and for tmux
        // with no passthrough, and it is why DA1 is sent right after: without
        // it there would be nothing to wait for and the probe would hang
        // until the deadline.
        assert!(!response_says_yes(b"\x1b[?62;c"));
    }

    #[test]
    fn an_ok_for_another_id_does_not_count() {
        // If the response is to another query (an id that is not ours), it
        // says nothing about our question.
        assert!(!response_says_yes(b"\x1b_Gi=99;OK\x1b\\\x1b[?62;c"));
    }

    #[test]
    fn an_id_that_shares_a_prefix_does_not_count() {
        // Review, finding 1: "i=311" CONTAINS "i=31" as a substring and also
        // ends in ";OK" — a foreign id that happens to share a prefix must
        // not sneak in as if it were ours.
        assert!(!response_says_yes(b"\x1b_Gi=311;OK\x1b\\"));
    }

    #[test]
    fn a_declared_error_is_a_no() {
        assert!(!response_says_yes(b"\x1b_Gi=31;ENOTSUPPORTED\x1b\\"));
    }

    #[test]
    fn nothing_is_a_no() {
        assert!(!response_says_yes(b""));
    }
}
