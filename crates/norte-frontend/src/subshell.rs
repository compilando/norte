//! The persistent SUBSHELL: the half that never touches a pty (#142).
//!
//! `app.toggle-panels` used to show the scrollback of the terminal norte
//! started from. Midnight Commander does something different that you notice
//! right away: it keeps a LIVE shell behind the panes, so the key leaves you
//! in a shell that remembers what you typed last time, and the pane's
//! directory and the shell's directory follow each other.
//!
//! Keeping a live shell asks for three things, and only the first is
//! plumbing:
//!
//! 1. a pty and a long-lived child — that lives in the frontend that owns the
//!    terminal (`norte-tui`), because it is the one that can hand it over;
//! 2. knowing WHERE that shell is, which is what lets the pane follow it. It
//!    is not guessed: the shell is asked to SAY it, by printing a marker on
//!    every prompt;
//! 3. sending it a `cd` when the pane moves, without a hostile file name
//!    turning into a command.
//!
//! Points 2 and 3 are text and bytes, not I/O, so they live here and are
//! tested without starting a shell.
//!
//! # The rule that governs this whole module
//!
//! **What gets written to a pty is NOT read by a shell parser: it is read by
//! the LINE EDITOR.** readline (bash), ZLE (zsh) and fish's reader see every
//! byte before anyone else, and control bytes are COMMANDS to them, not text:
//!
//! | byte | readline |
//! | --- | --- |
//! | `0x15` | `unix-line-discard` — erases the whole line |
//! | `0x01` | `beginning-of-line` — whatever follows is inserted IN FRONT |
//! | `0x7f` | `backward-delete-char` — deletes backward |
//! | `0x1b` | meta prefix — swallows whatever sequence comes next |
//!
//! Quoting defends against none of them: a quote protects a byte that ENTERS
//! the buffer, and these do not enter, they execute. A directory named
//! `<0x15>id #` — legal on any Unix, and creatable by any `tar` — turned a
//! quoted `cd -- '…'` into an executed `id`.
//!
//! Hence this module's two hard rules:
//!
//! - **Everything norte types is printable ASCII.** The real bytes travel as
//!   OCTAL escapes inside a `printf`, which is plain text to the line editor
//!   and exact bytes to the shell.
//! - **Nothing the shell prints is trusted without authentication.** The
//!   marker carries a session nonce: see [`Nonce`].
//!
//! # Why a marker and not OSC 7
//!
//! OSC 7 (`\e]7;file://host/path\e\\`) is what modern terminals emit to say
//! the cwd, and it would be the elegant choice. But it is emitted by whoever
//! chooses to: an unconfigured bash does not, and depending on it would give a
//! pane that follows the shell on some machines and not on others. The marker
//! is installed by norte itself in the prompt of the shell it STARTS, so it is
//! where it needs to be.

use crate::shell::Shell;

/// The prefix of the marker the subshell prints on every prompt.
///
/// A private OSC (`ESC ] 777 ; …`) because it is the range multiplexers let
/// through without interpreting it, with the name inside so a `77x` from
/// another program is not confused with this one.
pub const CWD_MARKER_PREFIX: &str = "\x1b]777;norte-cwd;";

/// The marker's terminator: BEL, which every shell knows how to write
/// without a fuss.
///
/// BEL is a LEGAL byte in a file name, so the shell escapes it before
/// printing it ([`DLE`]); otherwise, a directory with a BEL inside it would
/// have ended the marker halfway through, and the pane would have followed
/// the shell to a place the shell is not in.
pub const CWD_MARKER_END: u8 = 0x07;

/// The escape byte of the marker's PAYLOAD (`DLE`, "data link escape",
/// which is literally what it was invented for).
///
/// The shell doubles it (`DLE DLE` = one real `DLE`) and turns BEL into
/// `DLE G`. Any other pair is a MALFORMED marker and is discarded entirely:
/// better not to follow the shell than to follow it halfway.
pub const DLE: u8 = 0x10;

/// Cap on what is buffered while waiting for a marker to end.
///
/// Without it, a stream containing the prefix and NEVER a BEL — a `cat`-ed
/// binary, the reader's own `printf` — stopped being painted at all: every
/// byte the shell wrote from that point on piled up in the queue, without
/// limit and never reaching the screen. The shell looked hung and norte's
/// memory climbed at the pty's speed.
///
/// Past the cap it was not a marker, so it is painted as what it is: text.
pub const MARKER_MAX: usize = 8 * 1024;

/// The command that hands the terminal over to the subshell, and also asks
/// for it back.
pub const TOGGLE_COMMAND: &str = "app.toggle-panels";

/// The session's nonce: what distinguishes norte's shell from anything else
/// writing to the same pty.
///
/// The subshell's stream is, in part, controlled by whoever should not
/// control it: a file named `…\e]777;norte-cwd;/etc\a…` and an `ls` are
/// enough to make the marker appear without any prompt having printed it.
/// And the marker is not painted — it is OBEYED — so without authentication
/// it was a way to move the reader's pane to a directory chosen by someone
/// else, right before a copy or a delete points there.
///
/// The nonce is generated when the shell starts and typed into it inside the
/// hook, so the shell knows it and a file cannot guess it. It lives in the
/// reader's own scrollback, which is exactly the place where it does not
/// matter: whoever can read their terminal has already won.
///
/// ```
/// use norte_frontend::subshell::Nonce;
///
/// let a = Nonce::new();
/// let b = Nonce::new();
/// assert_ne!(a.as_str(), b.as_str(), "two sessions, two nonces");
/// assert!(a.as_str().chars().all(|c| c.is_ascii_hexdigit()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nonce(String);

impl Nonce {
    /// A new one, 128 bits in hexadecimal.
    ///
    /// The randomness comes from `RandomState`, the one the standard library
    /// uses to seed its `HashMap`s — no dependency to justify (rule 8) and no
    /// need for cryptographic quality: what it has to be is impossible to
    /// guess for a file that was written BEFORE this session existed.
    #[must_use]
    pub fn new() -> Self {
        use std::hash::BuildHasher as _;
        let s = std::collections::hash_map::RandomState::new();
        Self(format!(
            "{:016x}{:016x}",
            s.hash_one(0u64),
            s.hash_one(1u64)
        ))
    }

    /// The nonce as text: ASCII hexadecimal, fit to type into a shell.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Nonce {
    fn default() -> Self {
        Self::new()
    }
}

/// What has to be typed into the subshell right after starting it: the
/// prompt hook and the function that runs the `cd`s.
///
/// It is sent over the INPUT, as if the reader had typed it: no config file
/// of theirs is touched. A `~/.bashrc` that norte edited would be a permanent
/// modification for a function that turns off on exit — and it would survive
/// a norte that is no longer there.
///
/// Every line has a SPACE in front, and the first thing asked is that the
/// shell ignore anything starting with a space: that way the reader's history
/// does not fill up with its plumbing. Only that first line gets recorded
/// (fish already ignores the leading space out of the box, so none is
/// recorded there).
///
/// EVERYTHING that comes out of here is printable ASCII, per the module's
/// rule: the marker's control bytes are written as `printf` escapes (`\033`,
/// `\a`), not as bytes.
///
/// ```
/// use norte_frontend::shell::Shell;
/// use norte_frontend::subshell::{Nonce, install};
///
/// let n = Nonce::new();
/// let text = install(Shell::Bash, &n, std::path::Path::new("/run/user/1000/norte/b.cd"));
/// assert!(text.contains("PROMPT_COMMAND"));
/// // Not a single control byte except the newlines that send each command:
/// // anything else would be interpreted by the line editor, not the shell.
/// assert!(text.bytes().all(|b| b == b'\n' || (0x20..0x7f).contains(&b)));
/// ```
#[must_use]
pub fn install(shell: Shell, nonce: &Nonce, mailbox: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let (pre, n) = (marker_format_prefix(), nonce.as_str());
    // The mailbox path goes in ESCAPED as octal, the way a `cd`'s destination
    // used to: it is a path from the filesystem and can carry any byte,
    // including quotes. What is sent over the pty cannot carry a single
    // control byte (the line editor would interpret it), so it travels as a
    // `printf` format and the shell reconstructs it.
    let mut mailbox_esc = String::new();
    for b in mailbox.as_os_str().as_bytes() {
        use std::fmt::Write as _;
        // `write!` to a `String` cannot fail; the `let _` says so without
        // spending an `expect` (rule 6).
        let _ = write!(mailbox_esc, "\\{b:03o}");
    }
    // The hook's body: escapes DLE, then BEL, and prints.
    // `%s` and not `$PWD` interpolated into the format: a directory named
    // `%d` is not a format specifier, it is a directory.
    match shell {
        Shell::Bash | Shell::Zsh => {
            let body = concat!(
                "local p=${PWD//$'\\020'/$'\\020\\020'}; ",
                "p=${p//$'\\a'/$'\\020'G}; "
            );
            // The `cd` goes INSIDE the hook and before announcing the
            // directory: that way the marker says where the shell ended up,
            // not where it was. One function and one registration instead of
            // two, which also erases the ordering invariant that was needed
            // when there were two.
            //
            // `${d%_}` strips the sentinel: `$(<f)` eats the trailing
            // newlines and a directory CAN end in one.
            let move_cmd = format!(
                "local f d; f=$(printf '{mailbox_esc}'); \
                 if [ -s \"$f\" ]; then d=$(<\"$f\"); : > \"$f\"; cd -- \"${{d%_}}\" || true; fi; "
            );
            let hook =
                format!(" __norte_cwd() {{ {move_cmd}{body}printf '{pre}{n};%s\\a' \"$p\"; }}\n");
            match shell {
                Shell::Zsh => format!(
                    " setopt hist_ignore_space 2>/dev/null\n{hook} \
                     precmd_functions+=(__norte_cwd)\n"
                ),
                // `PROMPT_COMMAND` is ACCUMULATED with whatever was there: the
                // reader's prompt is theirs, and replacing it would strip
                // whatever git-status they had set up. And since bash 5.1 it
                // can be an ARRAY — `PROMPT_COMMAND=(__vte_prompt_command)` is
                // what GNOME Terminal ships — where string concatenation
                // overwrites element 0 and silently drops the rest.
                _ => format!(
                    " HISTCONTROL=ignorespace:${{HISTCONTROL}}\n{hook} \
                     if [[ ${{PROMPT_COMMAND@a}} == *a* ]]; \
                     then PROMPT_COMMAND+=(__norte_cwd); \
                     else PROMPT_COMMAND=\"__norte_cwd${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}\"; fi\n"
                ),
            }
        }
        // fish has no `PROMPT_COMMAND`: the hook is an event, which is also
        // what fish documents for this and does not touch `fish_prompt`,
        // which belongs to the reader. `string collect` preserves the
        // newlines inside a name that command substitution would split into
        // arguments.
        //
        // A single function, same as the other two: the `cd` goes inside and
        // before the announcement. When there were two, the order they were
        // defined in was an invariant to remember — in fish the hook is
        // registered when defined, so the other way round the first marker
        // came out with the `cd` line still unconsumed. There is no order to
        // remember anymore.
        Shell::Fish => format!(
            " function __norte_cwd --on-event fish_prompt; \
             set -l f (printf '{mailbox_esc}' | string collect); \
             if test -s \"$f\"; \
             set -l d (cat -- \"$f\" | string collect); \
             printf '' > \"$f\"; \
             cd -- (string sub -s 1 -e -1 -- \"$d\") 2>/dev/null; end; \
             set -l p (string replace -a -- \\x10 \\x10\\x10 $PWD | \
             string replace -a -- \\a \\x10G | string collect); \
             printf '{pre}{n};%s\\a' \"$p\"; end\n"
        ),
    }
}

/// The marker's prefix written the way `printf` understands it, without a
/// single control byte: `\033]777;norte-cwd;`.
fn marker_format_prefix() -> String {
    let mut s = String::from("\\033");
    s.push_str(&CWD_MARKER_PREFIX[1..]);
    s
}

/// What [`scan_cwd`] returns.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// The stream WITHOUT the markers: what needs to be painted.
    pub visible: Vec<u8>,
    /// The last cwd announced and AUTHENTICATED, in bytes (rule 1).
    pub cwd: Option<Vec<u8>>,
    /// A marker split across two reads, waiting for its end.
    pub tail: Vec<u8>,
}

/// Pulls the complete markers out of the subshell's stream and returns the
/// LAST cwd announced, together with what needs to be PAINTED (the stream
/// without them).
///
/// A marker whose nonce is not `nonce`'s is DISCARDED — this session's hook
/// did not print it — but it is still pulled out of the stream: painting it
/// would show the reader the escape sequence someone slipped in.
///
/// It also returns the unfinished tail: a marker can be split across two pty
/// reads — the normal case when it arrives in the same block as a long
/// prompt — and the caller puts it back in front of the next chunk. Without
/// that, a cwd is lost every time the buffer falls in the middle. The tail is
/// BOUNDED by [`MARKER_MAX`].
///
/// The cwd's bytes travel AS THEY ARE: a directory does not have to be UTF-8
/// (rule 1), and what comes out of here is what the shell printed.
///
/// ```
/// use norte_frontend::subshell::{scan_cwd, Nonce, CWD_MARKER_PREFIX};
///
/// let n = Nonce::new();
/// let stream = format!("before{CWD_MARKER_PREFIX}{};/tmp\x07after", n.as_str());
/// let s = scan_cwd(stream.as_bytes(), &n);
/// assert_eq!(s.visible, b"beforeafter");
/// assert_eq!(s.cwd.as_deref(), Some(&b"/tmp"[..]));
/// assert!(s.tail.is_empty());
/// ```
#[must_use]
pub fn scan_cwd(bytes: &[u8], nonce: &Nonce) -> Scan {
    let pre = CWD_MARKER_PREFIX.as_bytes();
    let mut out = Scan {
        visible: Vec::with_capacity(bytes.len()),
        ..Scan::default()
    };
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i..].starts_with(pre) {
            // A HALFWAY prefix at the end of the chunk is tail, not text: if
            // it were painted, the marker would show up on screen the time
            // the buffer happened to fall inside it.
            if pre.starts_with(&bytes[i..]) {
                out.tail = bytes[i..].to_vec();
                return out;
            }
            out.visible.push(bytes[i]);
            i += 1;
            continue;
        }
        let from = i + pre.len();
        match bytes[from..].iter().position(|b| *b == CWD_MARKER_END) {
            Some(end) => {
                if let Some(dir) = payload_cwd(&bytes[from..from + end], nonce) {
                    out.cwd = Some(dir);
                }
                i = from + end + 1;
            }
            // Marker started and not finished: tail, as long as it fits.
            // Past the cap it was not a marker — it is painted and we move
            // on.
            None if bytes.len() - i <= MARKER_MAX => {
                out.tail = bytes[i..].to_vec();
                return out;
            }
            None => {
                out.visible.extend_from_slice(&bytes[i..]);
                return out;
            }
        }
    }
    out
}

/// What has to be REPLIED to the shell if it asked something a terminal
/// answers.
///
/// On the other side of that pty, the terminal is norte. A modern shell does
/// not assume what is in front of it: it ASKS and WAITS for the answer. fish
/// 4 sends `ESC [ c` (Device Attributes) and `ESC [ ? u` (do you support
/// kitty's keyboard protocol?) before painting its first prompt, and with no
/// answer it just sits there — the reader saw a hung shell, no prompt and no
/// explanation. Measured: answering those two, fish starts; without them, it
/// does not start in ten seconds.
///
/// The answers are deliberately POOR: "a VT100 with options" and "I don't
/// speak kitty". Promising capabilities that are not then honored is worse
/// than not promising them, and here there is not even a terminal behind it
/// while the panes are in front — whatever the shell writes then is stored in
/// a buffer, nobody paints it.
///
/// The cursor position (`ESC [ 6 n`) and the background color (`OSC 11`) are
/// not answered: fish reaches its prompt without them, and answering them
/// wrong means making up a value the program is going to use to place things.
///
/// ```
/// use norte_frontend::subshell::terminal_reply;
///
/// assert_eq!(terminal_reply(b"hello\x1b[c"), Some(b"\x1b[?1;2c".to_vec()));
/// assert_eq!(terminal_reply(b"\x1b[0c"), Some(b"\x1b[?1;2c".to_vec()));
/// assert_eq!(terminal_reply(b"\x1b[?u"), Some(b"\x1b[?0u".to_vec()));
/// assert_eq!(terminal_reply(b"ls -la\n"), None);
/// ```
#[must_use]
pub fn terminal_reply(output: &[u8]) -> Option<Vec<u8>> {
    let mut reply = Vec::new();
    for (query, answer) in [
        (&b"\x1b[c"[..], &b"\x1b[?1;2c"[..]),
        (&b"\x1b[0c"[..], &b"\x1b[?1;2c"[..]),
        (&b"\x1b[?u"[..], &b"\x1b[?0u"[..]),
    ] {
        if output
            .windows(query.len())
            .any(|w| w == query)
            // `ESC [ c` is a suffix of `ESC [ 0 c`: answering both would give
            // a double reply to a single question.
            && !reply.windows(answer.len()).any(|w| w == answer)
        {
            reply.extend_from_slice(answer);
        }
    }
    (!reply.is_empty()).then_some(reply)
}

/// The cwd from a payload `<nonce>;<escaped path>`, if the nonce is ours and
/// the escapes are well-formed.
fn payload_cwd(payload: &[u8], nonce: &Nonce) -> Option<Vec<u8>> {
    let cut = payload.iter().position(|b| *b == b';')?;
    if &payload[..cut] != nonce.as_str().as_bytes() {
        return None;
    }
    let mut dir = Vec::with_capacity(payload.len() - cut);
    let body = &payload[cut + 1..];
    let mut i = 0;
    while i < body.len() {
        if body[i] != DLE {
            dir.push(body[i]);
            i += 1;
            continue;
        }
        match body.get(i + 1) {
            Some(&DLE) => dir.push(DLE),
            Some(b'G') => dir.push(CWD_MARKER_END),
            // An escape the hook could not have produced: the whole payload
            // is not to be trusted, not just that byte.
            _ => return None,
        }
        i += 2;
    }
    // A RELATIVE path cannot come from the hook — `$PWD` is absolute — and
    // following it would let `std::path::absolute` resolve it against
    // norte's own process cwd, which has nothing to do with it.
    (dir.first() == Some(&b'/')).then_some(dir)
}

/// The bytes a chord sends to a shell, or `None` if it means nothing there.
///
/// **Lives here because BOTH frontends need it**, and is built over
/// [`Chord`](crate::keymap::Chord) rather than any toolkit's event for the
/// same reason: the terminal builds it from `crossterm` and the window from
/// whatever the renderer sends, but the table is one — which byte `Ctrl+C` is
/// does not depend on who saw it. Writing it twice means having two places
/// where `F10` stops working inside an `htop`.
///
/// What is NOT here, and it shows: mouse and bracketed paste. A shell does
/// not ask for them, and the alternative in the terminal was stealing a key
/// from the reader.
///
/// ```
/// use norte_frontend::keymap::{Chord, KeyCode, Mods};
/// use norte_frontend::subshell::chord_a_bytes;
///
/// let ctrl_c = Chord::new(Mods { ctrl: true, ..Mods::default() }, KeyCode::Char('c'));
/// // Ctrl+C travels as byte 3, which is what makes it interrupt.
/// assert_eq!(chord_a_bytes(ctrl_c), Some(vec![3]));
/// // Enter is CR, not LF: that is what a terminal sends.
/// assert_eq!(chord_a_bytes(Chord::new(Mods::default(), KeyCode::Enter)), Some(b"\r".to_vec()));
/// ```
#[must_use]
pub fn chord_a_bytes(chord: crate::keymap::Chord) -> Option<Vec<u8>> {
    use crate::keymap::KeyCode;
    let (mods, code) = chord.parts();
    // A chord with Cmd/Super is NOT sent to a shell: there is no terminal
    // encoding for that modifier, so what came out was the bare letter — on
    // macOS, `cmd+w` typed a `w` instead of closing the window. Returning
    // `None` lets the key follow its own path and the keymap resolves it.
    if mods.cmd {
        return None;
    }
    let body: Vec<u8> = match code {
        // Ctrl+letter is the age-old control byte: `a`→1, `c`→3. Without
        // this, a Ctrl+C inside the pane interrupts nothing.
        KeyCode::Char(c) if mods.ctrl && c.is_ascii_alphabetic() => {
            vec![(c.to_ascii_lowercase() as u8) - b'a' + 1]
        }
        // The OTHER control chords, which are also bytes and not letters:
        // Ctrl+\ is SIGQUIT, Ctrl+space is the NUL `readline` uses for its
        // mark, Ctrl+[ is Escape. Without this branch the character traveled
        // as-is, so Ctrl+\ sent a plain backslash.
        KeyCode::Char(c)
            if mods.ctrl && matches!(c, '@' | ' ' | '[' | '\\' | ']' | '^' | '_' | '?') =>
        {
            vec![match c {
                '@' | ' ' => 0,
                '?' => 0x7f,
                other => (other as u8) & 0x1f,
            }]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        // The function keys, which an `htop` or an editor inside the pane do
        // use: without this, F10 did nothing at all. xterm codes, the ones
        // `terminfo` gives for `xterm`/`screen`/`tmux`.
        KeyCode::F(1) => b"\x1bOP".to_vec(),
        KeyCode::F(2) => b"\x1bOQ".to_vec(),
        KeyCode::F(3) => b"\x1bOR".to_vec(),
        KeyCode::F(4) => b"\x1bOS".to_vec(),
        // The jump to 16, 22 is not a bug: xterm never assigned them.
        KeyCode::F(n @ 5..=12) => {
            let num = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n) - 5];
            format!("\x1b[{num}~").into_bytes()
        }
        KeyCode::F(_) => return None,
        KeyCode::Enter => b"\r".to_vec(),
        // Shift+Tab is `CSI Z`, which is what a shell expects to go backward
        // through a completion.
        KeyCode::Tab if mods.shift => b"\x1b[Z".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        // DEL (127), not BS (8): that is what a modern terminal sends, and
        // what `readline` expects for backward delete.
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
    };
    // Alt is ESC in front, which is what makes `alt+f` move a word.
    if mods.alt {
        let mut with_esc = vec![0x1b];
        with_esc.extend_from_slice(&body);
        return Some(with_esc);
    }
    Some(body)
}

/// The chord the reader uses to GET the panes back.
///
/// It is the same one that took them away, and that is why it comes from the
/// keymap and not from a constant: presets do not bind it the same way
/// (`norton` and `far` put it on `Ctrl+O`, another preset can move it) and a
/// hardcoded `Ctrl+O` would leave the reader stuck inside the shell with no
/// way back — with the panes alive behind a screen that does not respond.
///
/// Only a LONE chord works. A two-key sequence — `g` `s`, say — cannot be
/// recognized here without putting the whole resolver inside the pty loop,
/// and above all it should NOT be: the sequence's first key would have to be
/// stolen from the shell, which is exactly where the reader is typing it. If
/// the preset binds the toggle to a sequence, this returns `None` and the
/// caller does not hand over the terminal, instead of handing it over with no
/// way out.
///
/// If the preset binds it to TWO lone chords, it sends the LAST one: that is
/// the one that wins in the effective keymap, so it is the one the reader has
/// on the reference sheet. Entering through the other one and not being able
/// to leave through it would be exactly what this function exists to
/// prevent, but that cannot be fixed here by something that only sees one
/// chord: the help says so.
///
/// ```
/// use norte_frontend::keymap::{Effective, Screen, parse_keymap};
/// use norte_frontend::subshell::{TOGGLE_COMMAND, detach_chord};
///
/// let src = r#"
/// [global]
/// keymap = [{ on = ["ctrl+o"], run = "app.toggle-panels" }]
/// "#;
/// let preset = parse_keymap(src).unwrap();
/// let eff =
///     Effective::build_for(&preset, &[], &[TOGGLE_COMMAND], Screen::Browse).unwrap();
/// assert!(detach_chord(&eff).is_some());
/// ```
#[must_use]
pub fn detach_chord(browse: &crate::keymap::Effective) -> Option<crate::keymap::Chord> {
    // The rule — a LONE chord or the keyboard is not handed over — lives in
    // `Effective::lone_chord`, because the terminal pane (#362) needs it just
    // the same: both hand the entire keyboard to another program and both
    // keep a single chord to come back.
    browse.lone_chord(TOGGLE_COMMAND)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Effective, Screen, parse_keymap};

    fn nonce() -> Nonce {
        Nonce::new()
    }

    /// A marker with the payload ALREADY escaped, the way the hook would
    /// print it.
    fn marker(n: &Nonce, dir: &[u8]) -> Vec<u8> {
        let mut v = CWD_MARKER_PREFIX.as_bytes().to_vec();
        v.extend_from_slice(n.as_str().as_bytes());
        v.push(b';');
        for b in dir {
            match *b {
                DLE => v.extend_from_slice(&[DLE, DLE]),
                CWD_MARKER_END => v.extend_from_slice(&[DLE, b'G']),
                other => v.push(other),
            }
        }
        v.push(CWD_MARKER_END);
        v
    }

    /// **Nothing norte types carries a control byte.**
    ///
    /// This is THE module's rule, and the test that backs it. The hook used
    /// to carry the raw `ESC` and `BEL`: readline ate the `ESC ]` as a meta
    /// prefix, so the `PROMPT_COMMAND` that ended up installed printed
    /// `777;norte-cwd;/home` WITHOUT the OSC frame — the pane never followed
    /// the shell, and the reader saw that text on every prompt.
    #[test]
    fn norte_never_types_a_control_byte() {
        let n = nonce();
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            // A mailbox path with hostile bytes: it travels the same octal
            // escape path a `cd`'s destination used to travel.
            let text = install(shell, &n, std::path::Path::new("/tmp/\x15\x01'b"));
            assert!(
                text.bytes()
                    .all(|b| b == b'\n' || (0x20..0x7f).contains(&b)),
                "{shell:?}: the hook carries a byte the line editor would execute"
            );
        }
    }

    /// **The hook does the `cd` BEFORE announcing the directory.**
    ///
    /// It is a single function and that order is its invariant: if it
    /// announced first, the marker would say where the shell was, not where
    /// it ended up, and the pane would stay one prompt behind itself.
    ///
    /// There used to be two — one for the `cd`, another to announce — and the
    /// order they were DEFINED in was the invariant: in fish the hook is
    /// registered when defined, so the other way round the first marker came
    /// out with the `cd` line still unconsumed. With one function that
    /// invariant disappears.
    #[test]
    fn the_hook_moves_before_announcing() {
        let n = nonce();
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let text = install(shell, &n, std::path::Path::new("/tmp/mailbox"));
            let moves = text.find("cd --").expect("the hook moves");
            let announces = text.find("777;norte-cwd;").expect("the hook announces");
            assert!(
                moves < announces,
                "{shell:?}: announces the directory before having moved"
            );
            assert_eq!(
                text.matches("--on-event fish_prompt").count()
                    + text.matches("precmd_functions").count()
                    + text.matches("PROMPT_COMMAND+=").count(),
                usize::from(shell != Shell::Bash) + usize::from(shell == Shell::Bash),
                "{shell:?}: a single registration, not two"
            );
        }
    }

    /// **The hook does not type any `cd`: it reads it from a file** (#363).
    ///
    /// Typing it required knowing that the line editor was at an empty
    /// prompt, and that cannot be known from outside: zsh's buffer stack
    /// (`push-line`, `print -z`) and all three shells' type-ahead reached "a
    /// marker was received and nobody typed" with a half-finished line from
    /// the reader waiting. The `cd` got concatenated and the shell EXECUTED a
    /// command nobody gave.
    #[test]
    fn the_hook_reads_the_destination_from_a_file_and_does_not_type_it() {
        let n = nonce();
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let text = install(shell, &n, std::path::Path::new("/run/u/norte/b.cd"));
            assert!(
                !text.contains("__norte_cd"),
                "{shell:?}: the function that used to be typed still exists"
            );
            // The mailbox path travels escaped in octal, like the destination
            // used to: `/` is `\057`, and its presence says the hook carries
            // it.
            assert!(
                text.contains("\\057\\162\\165\\156"),
                "{shell:?}: the hook does not carry the mailbox path: {text}"
            );
        }
    }

    /// The hook asks for the WHOLE marker, OSC prefix included: if the
    /// `\033` were lost, `scan_cwd` would recognize nothing.
    #[test]
    fn the_hook_prints_the_whole_marker() {
        let n = nonce();
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let text = install(shell, &n, std::path::Path::new("/tmp/mailbox"));
            assert!(
                text.contains("\\033]777;norte-cwd;"),
                "{shell:?}: without the OSC prefix there is no marker"
            );
            assert!(text.contains(n.as_str()), "{shell:?}: missing nonce");
        }
    }

    /// bash's hook PRESERVES whatever `PROMPT_COMMAND` was there, and knows
    /// it can be an ARRAY since 5.1: `PROMPT_COMMAND=(__vte_prompt_command)`
    /// is what GNOME Terminal ships, and a string assignment would have
    /// overwritten element 0, silently dropping the rest.
    #[test]
    fn bashs_hook_does_not_stomp_the_readers_prompt() {
        let text = install(Shell::Bash, &nonce(), std::path::Path::new("/tmp/mailbox"));
        assert!(text.contains("${PROMPT_COMMAND:+;$PROMPT_COMMAND}"));
        assert!(text.contains("PROMPT_COMMAND+=(__norte_cwd)"));
        assert!(text.contains("@a"), "missing the array check");
    }

    /// The marker is pulled out of the stream and NOT painted: if it were
    /// painted, the reader would see the escape sequence in their shell every
    /// time a prompt appears.
    #[test]
    fn the_marker_is_not_painted() {
        let n = nonce();
        let mut stream = b"$ ls".to_vec();
        stream.extend_from_slice(&marker(&n, b"/home"));
        stream.extend_from_slice(b"$ ");
        let s = scan_cwd(&stream, &n);
        assert_eq!(s.visible, b"$ ls$ ");
        assert_eq!(s.cwd.as_deref(), Some(&b"/home"[..]));
        assert!(s.tail.is_empty());
    }

    /// **A marker split across two reads is neither lost nor painted.**
    ///
    /// This is the normal case, not the rare one: the pty delivers whatever
    /// there is, and a long prompt lands mid-marker constantly. Without the
    /// tail, the cwd was lost every time the buffer fell in the middle.
    #[test]
    fn a_split_marker_is_reconstructed() {
        let n = nonce();
        let mut whole = b"antes".to_vec();
        whole.extend_from_slice(&marker(&n, b"/home/mine"));
        whole.extend_from_slice("despu\u{e9}s".as_bytes());
        for cut in 1..whole.len() {
            let a = scan_cwd(&whole[..cut], &n);
            let mut second = a.tail;
            second.extend_from_slice(&whole[cut..]);
            let b = scan_cwd(&second, &n);
            let mut visible = a.visible;
            visible.extend_from_slice(&b.visible);
            assert_eq!(visible, b"antesdespu\xc3\xa9s", "cut {cut}");
            assert_eq!(
                b.cwd.or(a.cwd).as_deref(),
                Some(&b"/home/mine"[..]),
                "cut {cut}"
            );
            assert!(b.tail.is_empty(), "cut {cut}");
        }
    }

    /// The cwd travels in BYTES: a directory does not have to be text
    /// (rule 1), and passing it through `String` would change what it is.
    #[test]
    fn a_cwd_that_is_not_utf8_survives() {
        let n = nonce();
        let s = scan_cwd(&marker(&n, b"/home/a\xffb"), &n);
        assert_eq!(s.cwd.as_deref(), Some(&b"/home/a\xffb"[..]));
    }

    /// **A BEL inside the name does not cut the marker short.**
    ///
    /// BEL is a legal byte in a file name and it is the terminator norte
    /// chose: without escaping it, `/tmp/a\x07b` was announced as `/tmp/a`
    /// and the pane followed the shell to a directory the shell was not in —
    /// silently, if `/tmp/a` existed.
    #[test]
    fn a_bel_in_the_name_does_not_truncate_the_marker() {
        let n = nonce();
        for dir in [&b"/tmp/a\x07b"[..], &b"/tmp/\x10\x07\x10"[..]] {
            let s = scan_cwd(&marker(&n, dir), &n);
            assert_eq!(s.cwd.as_deref(), Some(dir), "{dir:?}");
        }
    }

    /// **A marker that does not carry this session's nonce moves nothing.**
    ///
    /// The pty's stream is, in part, controlled by whoever should not
    /// control it: a file with the marker inside and a `cat` are enough.
    /// Without authentication, that moved the reader's pane to a directory
    /// chosen by someone else right before a copy or a delete points there.
    #[test]
    fn a_forged_marker_does_not_move_the_pane() {
        let n = nonce();
        let other = Nonce("0".repeat(32));
        let s = scan_cwd(&marker(&other, b"/etc"), &n);
        assert_eq!(s.cwd, None, "the nonce was not ours");
        // And it is NOT painted: showing the reader the sequence someone
        // slipped in would be the other half of the problem.
        assert!(s.visible.is_empty());
    }

    /// A RELATIVE path does not come from `$PWD`, and following it would
    /// resolve it against norte's own process cwd — a different directory,
    /// chosen by nobody.
    #[test]
    fn a_relative_path_is_not_followed() {
        let n = nonce();
        assert_eq!(scan_cwd(&marker(&n, b"etc"), &n).cwd, None);
        assert_eq!(scan_cwd(&marker(&n, b""), &n).cwd, None);
    }

    /// **A marker that never ends does not swallow the whole screen.**
    ///
    /// Without a cap, the first `\e]777;norte-cwd;` with no BEL after it — a
    /// `cat`-ed binary — stopped painting EVERYTHING the shell wrote from
    /// that point on, piling it up without limit: the shell looked hung and
    /// memory climbed at the pty's speed.
    #[test]
    fn a_marker_with_no_end_does_not_eat_the_screen() {
        let n = nonce();
        let mut stream = CWD_MARKER_PREFIX.as_bytes().to_vec();
        stream.extend(std::iter::repeat_n(b'x', MARKER_MAX + 10));
        let s = scan_cwd(&stream, &n);
        assert!(s.tail.is_empty(), "past the cap nothing is retained");
        assert_eq!(
            s.visible.len(),
            stream.len(),
            "and it is painted as the text it is"
        );
    }

    /// Only the LAST one counts: more than one prompt can have happened
    /// between two reads, and the shell's location is the final one.
    #[test]
    fn sends_the_last_marker() {
        let n = nonce();
        let mut stream = marker(&n, b"/one");
        stream.push(b'x');
        stream.extend_from_slice(&marker(&n, b"/two"));
        let s = scan_cwd(&stream, &n);
        assert_eq!(s.visible, b"x");
        assert_eq!(s.cwd.as_deref(), Some(&b"/two"[..]));
    }

    /// **The MAILBOX path does not let a command escape, and the defense is
    /// NOT quoting.**
    ///
    /// This test used to check the `cd` norte typed. Since #363 none is
    /// typed, but the mailbox path travels the same road and inside the
    /// hook, so the property is the same and is just as needed: it is a
    /// filesystem path and can carry any byte.
    ///
    /// Ordinary hostile names would be stopped by quoting. The three at the
    /// end would not: they are bytes readline EXECUTES (`0x15` erases the
    /// line, `0x01` jumps to the start, `0x7f` deletes backward), so they
    /// never reach the parser the quote protects. The defense is that the
    /// path travels in octal.
    #[test]
    fn the_mailbox_path_does_not_let_a_command_escape() {
        // The canonical CORPUS, not a list invented here (repo convention):
        // the three `readline_*` fixtures came from this, and a local list
        // would have left the bug outside the place the rest of norte looks
        // for it.
        let mut names: Vec<Vec<u8>> = norte_testkit::corpus::hostile_names()
            .into_iter()
            .map(|n| {
                let mut path = b"/tmp/".to_vec();
                path.extend_from_slice(&n.bytes);
                path
            })
            .collect();
        names.extend(
            [
                &b"/tmp/; rm -rf ~"[..],
                &b"/tmp/$(whoami)"[..],
                &b"/tmp/`id`"[..],
            ]
            .into_iter()
            .map(<[u8]>::to_vec),
        );
        let n = nonce();
        for name in &names {
            use std::os::unix::ffi::OsStrExt as _;
            let mailbox = std::path::Path::new(std::ffi::OsStr::from_bytes(name));
            for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
                let text = install(shell, &n, mailbox);
                assert!(
                    text.bytes()
                        .all(|b| b == b'\n' || (0x20..0x7f).contains(&b)),
                    "{name:?} traveled with a byte the line editor executes"
                );
                // And the path's bytes are ALL there, in octal and in order.
                let mut expected = String::new();
                for b in name {
                    use std::fmt::Write as _;
                    write!(expected, "\\{b:03o}").expect("String does not fail");
                }
                assert!(text.contains(&expected), "{name:?} in {shell:?}");
            }
        }
    }

    fn browse(src: &str) -> Effective {
        let preset = parse_keymap(src).expect("the fixture parses");
        Effective::build_for(&preset, &[], &[TOGGLE_COMMAND], Screen::Browse)
            .expect("the fixture builds")
    }

    /// A SEQUENCE does not work as a chord to come back, and saying so here
    /// is what keeps the frontend from handing over the terminal with no way
    /// out: the sequence's first key belongs to the shell, not to norte.
    #[test]
    fn a_sequence_does_not_work_to_go_back() {
        let eff = browse(
            r#"
[global]
keymap = [{ on = ["g", "s"], run = "app.toggle-panels" }]
"#,
        );
        assert_eq!(detach_chord(&eff), None);
    }

    /// And neither does a preset that does not bind it at all: there is no
    /// key, so there is nothing to hand over.
    #[test]
    fn with_no_binding_there_is_no_chord() {
        let eff = browse("[global]\nkeymap = []\n");
        assert_eq!(detach_chord(&eff), None);
    }

    /// The chord comes from the KEYMAP, not from a constant: presets do not
    /// bind it the same way, and a hardcoded `Ctrl+O` would leave the reader
    /// locked inside the shell of whichever preset moves it.
    #[test]
    fn the_chord_comes_from_the_rebound_keymap() {
        let eff = browse(
            r#"
[global]
keymap = [{ on = ["ctrl+u"], run = "app.toggle-panels" }]
"#,
        );
        let chord = detach_chord(&eff).expect("there is a chord");
        assert_eq!(chord, crate::keymap::parse_chord("ctrl+u").expect("chord"));
    }

    /// With TWO lone chords it sends the last one, the one that wins in the
    /// effective map: entering through one and not being able to leave
    /// through it would be exactly what `detach_chord` exists to prevent.
    #[test]
    fn with_two_chords_it_sends_the_one_that_wins_in_the_effective_map() {
        let eff = browse(
            r#"
[global]
keymap = [
    { on = ["ctrl+o"], run = "app.toggle-panels" },
    { on = ["ctrl+u"], run = "app.toggle-panels" },
]
"#,
        );
        let chord = detach_chord(&eff).expect("there is a chord");
        assert_eq!(chord, crate::keymap::parse_chord("ctrl+u").expect("chord"));
    }
}
