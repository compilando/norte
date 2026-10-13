//! The command bar's model (ADR 0181): one line under the panels whose
//! orders run as entries of the terminal panel, never typed into a shell.
//!
//! Shared by both frontends: the buffer, its cursor, the history and the
//! quoting of a name inserted into the line. The frontends paint it and
//! start the order.

use std::ffi::OsString;
use std::path::Path;

use crate::shell::Shell;

/// How many orders the history keeps.
pub const HISTORY_MAX: usize = 200;

/// The bar's line, its cursor and its history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandBar {
    buffer: String,
    /// Cursor, in chars.
    cursor: usize,
    history: Vec<String>,
    /// Which history entry is shown while walking it (`None` = the line
    /// being typed).
    browsing: Option<usize>,
}

impl CommandBar {
    /// An empty line over `history` (oldest first), capped at
    /// [`HISTORY_MAX`].
    #[must_use]
    pub fn new(mut history: Vec<String>) -> Self {
        let excess = history.len().saturating_sub(HISTORY_MAX);
        history.drain(..excess);
        Self {
            history,
            ..Self::default()
        }
    }

    fn byte_at(&self, chars: usize) -> usize {
        self.buffer
            .char_indices()
            .nth(chars)
            .map_or(self.buffer.len(), |(b, _)| b)
    }

    fn len_chars(&self) -> usize {
        self.buffer.chars().count()
    }

    fn set(&mut self, text: String) {
        self.cursor = text.chars().count();
        self.buffer = text;
    }

    /// The line.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.buffer
    }

    /// The cursor, in chars.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The history, oldest first.
    #[must_use]
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Types `s` at the cursor. Control characters are dropped: the line is
    /// one order, and a newline in it would be two.
    pub fn insert(&mut self, s: &str) {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        let at = self.byte_at(self.cursor);
        self.buffer.insert_str(at, &clean);
        self.cursor += clean.chars().count();
        self.browsing = None;
    }

    /// Deletes the char before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let at = self.byte_at(self.cursor);
            self.buffer.remove(at);
        }
    }

    /// Deletes the char under the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.len_chars() {
            let at = self.byte_at(self.cursor);
            self.buffer.remove(at);
        }
    }

    /// Moves the cursor one char left.
    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor one char right.
    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len_chars());
    }

    /// To the start of the line.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// To the end of the line.
    pub fn end(&mut self) {
        self.cursor = self.len_chars();
    }

    /// The previous history entry.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let i = self
            .browsing
            .map_or(self.history.len() - 1, |i| i.saturating_sub(1));
        self.browsing = Some(i);
        self.set(self.history[i].clone());
    }

    /// The next history entry, back to an empty line past the newest.
    pub fn history_next(&mut self) {
        let Some(i) = self.browsing else {
            return;
        };
        if i + 1 < self.history.len() {
            self.browsing = Some(i + 1);
            self.set(self.history[i + 1].clone());
        } else {
            self.browsing = None;
            self.set(String::new());
        }
    }

    /// Completes the word before the cursor against `names` (the pane's
    /// entries): the longest common prefix of the matches, or the whole
    /// name quoted for `shell` and a space when only one matches. `true`
    /// when the line changed.
    pub fn complete(&mut self, names: &[&[u8]], shell: Option<Shell>) -> bool {
        let at = self.byte_at(self.cursor);
        let start = self.buffer[..at].rfind(' ').map_or(0, |i| i + 1);
        let word = &self.buffer.as_bytes()[start..at];
        let hits: Vec<&[u8]> = names
            .iter()
            .copied()
            .filter(|n| n.starts_with(word))
            .collect();
        let replacement = match hits.as_slice() {
            [] => return false,
            [one] => format!("{} ", quote_for(shell, one)),
            many => {
                let mut prefix = many[0].len();
                for n in &many[1..] {
                    prefix = prefix.min(
                        many[0]
                            .iter()
                            .zip(n.iter())
                            .take_while(|(a, b)| a == b)
                            .count(),
                    );
                }
                // Only what is plain, printable text: a prefix ending inside
                // a multibyte char or holding a control byte waits for the
                // reader to type one more letter.
                let Ok(p) = std::str::from_utf8(&many[0][..prefix]) else {
                    return false;
                };
                if p.chars().any(char::is_control) || p.len() <= word.len() {
                    return false;
                }
                p.to_owned()
            }
        };
        let tail = self.buffer[at..].to_owned();
        let head = self.buffer[..start].to_owned();
        self.set(format!("{head}{replacement}"));
        self.buffer.push_str(&tail);
        true
    }

    /// The order to run, trimmed, recorded in the history; `None` when the
    /// line is blank. The line empties either way.
    pub fn take(&mut self) -> Option<String> {
        let order = self.buffer.trim().to_owned();
        self.set(String::new());
        self.browsing = None;
        if order.is_empty() {
            return None;
        }
        if self.history.last() != Some(&order) {
            self.history.push(order.clone());
            let excess = self.history.len().saturating_sub(HISTORY_MAX);
            self.history.drain(..excess);
        }
        Some(order)
    }
}

/// Bytes every shell reads literally, unquoted.
fn plain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"._,/:=+@%-".contains(&b)
}

/// `name` as ONE word for `shell` (`None` = POSIX `sh`), and ASCII only,
/// whatever its bytes: what is typed into the bar is text, and a name's
/// bytes travel as escapes the shell itself turns back into them.
#[must_use]
pub fn quote_for(shell: Option<Shell>, name: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    match shell {
        // `$'…'` with every byte that is not plain printable ASCII as a
        // three-digit octal escape, so a following digit cannot join it.
        Some(Shell::Bash | Shell::Zsh) => {
            out.push_str("$'");
            for &b in name {
                if (0x20..0x7f).contains(&b) && b != b'\'' && b != b'\\' {
                    out.push(char::from(b));
                } else {
                    let _ = write!(out, "\\{b:03o}");
                }
            }
            out.push('\'');
        }
        // fish has no `$'…'`; unquoted, `\XHH` is one raw byte.
        Some(Shell::Fish) => {
            for &b in name {
                if plain(b) {
                    out.push(char::from(b));
                } else {
                    let _ = write!(out, "\\X{b:02X}");
                }
            }
        }
        // POSIX sh: plain printable ASCII goes in single quotes; anything
        // else becomes `printf` octal escapes inside a command substitution
        // (which drops trailing newlines — a POSIX limit, sh only).
        None if name.iter().all(|b| (0x20..0x7f).contains(b)) => {
            out.push('\'');
            for &b in name {
                if b == b'\'' {
                    out.push_str("'\\''");
                } else {
                    out.push(char::from(b));
                }
            }
            out.push('\'');
        }
        None => {
            out.push_str("\"$(printf '");
            for &b in name {
                let _ = write!(out, "\\{b:03o}");
            }
            out.push_str("')\"");
        }
    }
    out
}

/// The argv that runs `order` in `shell_path`: `shell::shell_command_argv`.
#[must_use]
pub fn argv(shell_path: &Path, order: &str) -> Vec<OsString> {
    crate::shell::shell_command_argv(shell_path, order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hostile_name_is_one_ascii_word_in_every_shell() {
        let name = b"it's\na caf\xE9";
        for shell in [Some(Shell::Bash), Some(Shell::Zsh), Some(Shell::Fish), None] {
            let q = quote_for(shell, name);
            assert!(
                !q.is_empty() && q.is_ascii() && !q.contains('\n'),
                "{shell:?}: {q}"
            );
        }
    }

    /// Runs the quoted word through the REAL shell and gets the bytes
    /// back. A shell that is not installed is skipped.
    #[cfg(unix)]
    #[test]
    fn the_shells_read_back_the_exact_bytes() {
        let names: Vec<Vec<u8>> = norte_testkit::corpus::hostile_names()
            .into_iter()
            .map(|n| n.bytes)
            .chain([b"it's\na caf\xE9 $HOME `x` *".to_vec()])
            .collect();
        for (shell, bin) in [
            (Some(Shell::Bash), "bash"),
            (Some(Shell::Zsh), "zsh"),
            (Some(Shell::Fish), "fish"),
            (None, "sh"),
        ] {
            let Some(path) = crate::openers::resolve_program(std::ffi::OsStr::new(bin)) else {
                continue;
            };
            for name in &names {
                // `sh` cannot carry a trailing newline through `$(...)`.
                if shell.is_none() && name.ends_with(b"\n") {
                    continue;
                }
                let order = format!("printf %s {}", quote_for(shell, name));
                let out = std::process::Command::new(&path)
                    .args(argv(&path, &order).into_iter().skip(1))
                    .output()
                    .expect("runs");
                assert_eq!(&out.stdout, name, "{bin}: {order}");
            }
        }
    }

    #[test]
    fn history_skips_blanks_and_consecutive_repeats() {
        let mut b = CommandBar::new(Vec::new());
        for o in ["ls", "ls", "  ", "make"] {
            b.insert(o);
            let _ = b.take();
        }
        assert_eq!(b.history(), ["ls", "make"]);
        b.history_prev();
        assert_eq!(b.text(), "make");
        b.history_prev();
        assert_eq!(b.text(), "ls");
        b.history_next();
        b.history_next();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn editing_moves_by_chars_not_bytes() {
        let mut b = CommandBar::new(Vec::new());
        b.insert("cañón");
        b.left();
        b.backspace();
        assert_eq!(b.text(), "cañn");
        b.home();
        b.delete();
        assert_eq!(b.text(), "añn");
        b.end();
        b.insert("!");
        assert_eq!(b.text(), "añn!");
        assert_eq!(b.cursor(), 4);
    }

    #[test]
    fn completion_extends_to_the_common_prefix_then_quotes_a_single_match() {
        let mut b = CommandBar::new(Vec::new());
        b.insert("cat rep");
        assert!(b.complete(
            &[b"report-a.txt", b"report-b.txt", b"other"],
            Some(Shell::Bash)
        ));
        assert_eq!(b.text(), "cat report-");
        b.insert("a");
        assert!(b.complete(&[b"report-a.txt", b"report-b.txt"], Some(Shell::Bash)));
        assert_eq!(
            b.text(),
            format!("cat {} ", quote_for(Some(Shell::Bash), b"report-a.txt"))
        );
        b.insert("q");
        assert!(
            !b.complete(&[b"zzz"], Some(Shell::Bash)),
            "nothing matches: no change"
        );
    }

    #[test]
    fn the_history_is_capped_keeping_the_newest() {
        let many: Vec<String> = (0..HISTORY_MAX + 5).map(|i| format!("echo {i}")).collect();
        let mut b = CommandBar::new(many);
        assert_eq!(b.history().len(), HISTORY_MAX);
        b.insert("make");
        let _ = b.take();
        assert_eq!(b.history().len(), HISTORY_MAX);
        assert_eq!(b.history().last().map(String::as_str), Some("make"));
        assert_eq!(b.history()[0], "echo 6");
    }

    #[test]
    fn a_pasted_newline_does_not_make_two_orders() {
        let mut b = CommandBar::new(Vec::new());
        b.insert("ls\nrm -rf x");
        assert_eq!(b.text(), "lsrm -rf x");
    }
}
