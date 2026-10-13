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
    pub fn new(history: Vec<String>) -> Self {
        // A history file is text anyone can edit: filtered like typing.
        let mut history: Vec<String> = history
            .into_iter()
            .map(|h| h.chars().filter(|c| allowed(*c)).collect::<String>())
            .filter(|h| !h.trim().is_empty())
            .collect();
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
        let clean: String = s.chars().filter(|c| allowed(*c)).collect();
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
                // Only a prefix of PLAIN bytes, which every shell reads
                // literally: anything else (a space, `$(`, a quote) would go
                // into the line live, and quoting half a word cannot be left
                // open safely. The reader types one more letter instead.
                let p = &many[0][..prefix];
                if p.len() <= word.len() || !p.iter().all(|b| plain(*b)) {
                    return false;
                }
                String::from_utf8_lossy(p).into_owned()
            }
        };
        let tail = self.buffer[at..].to_owned();
        let head = self.buffer[..start].to_owned();
        self.set(format!("{head}{replacement}"));
        self.buffer.push_str(&tail);
        true
    }

    /// [`Self::complete`] for the shell at `shell`: one norte cannot quote
    /// for (cmd, PowerShell…) only completes names made of characters no
    /// shell gives meaning to.
    pub fn complete_for(&mut self, names: &[&[u8]], shell: &Path) -> bool {
        if let Some(kind) = shell_quoting(shell) {
            return self.complete(names, kind);
        }
        let safe: Vec<&[u8]> = names
            .iter()
            .copied()
            .filter(|n| n.iter().all(|b| inert_everywhere(*b)))
            .collect();
        self.complete(&safe, None)
    }

    /// Inserts `name` quoted for the shell at `shell`. `false` (nothing
    /// inserted) for a shell norte cannot quote for and a name that would
    /// mean something to it.
    pub fn insert_name(&mut self, shell: &Path, name: &[u8]) -> bool {
        match shell_quoting(shell) {
            Some(kind) => self.insert(&quote_for(kind, name)),
            None if name.iter().all(|b| inert_everywhere(*b)) => {
                self.insert(&String::from_utf8_lossy(name));
            }
            None => return false,
        }
        true
    }

    /// The order to run, trimmed, recorded in the history unless it was
    /// typed with a leading space (`HISTCONTROL=ignorespace`); `None` when
    /// the line is blank. The line empties either way.
    pub fn take(&mut self) -> Option<String> {
        let private = self.buffer.starts_with(' ');
        let order = self.buffer.trim().to_owned();
        self.set(String::new());
        self.browsing = None;
        if order.is_empty() {
            return None;
        }
        if !private && self.history.last() != Some(&order) {
            self.history.push(order.clone());
            let excess = self.history.len().saturating_sub(HISTORY_MAX);
            self.history.drain(..excess);
        }
        Some(order)
    }
}

/// How names are quoted for `shell`: `Some(Some(_))` bash/zsh/fish,
/// `Some(None)` a POSIX sh, `None` a shell norte cannot quote for.
#[must_use]
pub fn shell_quoting(shell: &Path) -> Option<Option<Shell>> {
    let name = shell.file_name()?.to_string_lossy().to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    match name {
        "sh" | "dash" | "ash" | "ksh" | "mksh" | "busybox" => Some(None),
        other => Shell::parse(other).map(Some),
    }
}

/// Bytes every shell reads literally, unquoted (not `%`: fish expands
/// `%self` in some versions).
fn plain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"._,/:=+@-".contains(&b)
}

/// Bytes no shell gives meaning to, cmd.exe included (which splits on `,`,
/// `;` and `=`).
fn inert_everywhere(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"._-/:".contains(&b)
}

/// What may enter the line: no control character (one order, one line) and
/// no bidi override or zero-width character, so what is painted is what
/// runs.
fn allowed(c: char) -> bool {
    !c.is_control() && !norte_encoding::is_terminal_hazard(c)
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
                // Every byte that is not plain is escaped, `$`, `` ` `` and
                // `"` included: inside double quotes the reader opened,
                // `$'` is not special, and the word must stay inert there.
                if plain(b) {
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

    /// A shared prefix that is not plain text is NOT inserted raw: names
    /// `r$(x)a` and `r$(x)b` must not put `r$(x)` in the line.
    #[test]
    fn a_hostile_shared_prefix_is_not_inserted() {
        let mut b = CommandBar::new(Vec::new());
        b.insert("cat r");
        assert!(!b.complete(&[b"r$(curl x|sh)a", b"r$(curl x|sh)b"], Some(Shell::Bash)));
        assert_eq!(b.text(), "cat r");
        let names = norte_testkit::corpus::hostile_names();
        let pairs: Vec<Vec<u8>> = names
            .iter()
            .flat_map(|n| {
                let mut a = n.bytes.clone();
                a.push(b'1');
                let mut z = n.bytes.clone();
                z.push(b'2');
                [a, z]
            })
            .collect();
        for pair in pairs.chunks(2) {
            let mut b = CommandBar::new(Vec::new());
            b.insert("x ");
            let refs: Vec<&[u8]> = pair.iter().map(Vec::as_slice).collect();
            b.complete(&refs, Some(Shell::Bash));
            assert!(
                b.text().bytes().all(|c| plain(c) || c == b' '),
                "{:?}",
                b.text()
            );
        }
    }

    /// Bidi overrides and zero-width characters never reach the line: what
    /// is painted is what runs.
    #[test]
    fn terminal_hazards_never_reach_the_line() {
        let mut b = CommandBar::new(vec!["ok\u{202E}evil".into()]);
        b.insert("a\u{202E}b\u{200B}c");
        assert_eq!(b.text(), "abc");
        b.history_prev();
        assert_eq!(b.text(), "okevil");
    }

    /// `HISTCONTROL=ignorespace`: an order typed with a leading space runs
    /// but is not remembered.
    #[test]
    fn a_leading_space_keeps_an_order_out_of_the_history() {
        let mut b = CommandBar::new(Vec::new());
        b.insert(" mysql -psecret");
        assert_eq!(b.take().as_deref(), Some("mysql -psecret"));
        assert!(b.history().is_empty());
    }

    /// Inside double quotes the reader opened, a bash word is still inert:
    /// every byte that is not plain is an octal escape.
    #[test]
    fn a_bash_word_has_no_live_character_even_inside_double_quotes() {
        let q = quote_for(Some(Shell::Bash), b"a`id`$(id)\"b");
        assert!(!q[2..q.len() - 1].contains(['`', '$', '"']), "{q}");
    }

    /// For cmd.exe, a name with a separator or a special character is not
    /// inserted at all; a plain one is, as is.
    #[test]
    fn cmd_gets_only_names_that_mean_nothing_to_it() {
        let cmd = std::path::Path::new("C:\\Windows\\System32\\cmd.exe");
        let mut b = CommandBar::new(Vec::new());
        assert!(!b.insert_name(cmd, b"a&calc&b"));
        assert!(!b.insert_name(cmd, b"a b.txt"));
        assert_eq!(b.text(), "");
        assert!(b.insert_name(cmd, b"report-1.txt"));
        assert_eq!(b.text(), "report-1.txt");
    }

    /// The shells norte knows how to quote for; cmd and PowerShell are not.
    #[test]
    fn only_known_shells_get_names_quoted() {
        use std::path::Path;
        assert_eq!(
            shell_quoting(Path::new("/bin/bash")),
            Some(Some(Shell::Bash))
        );
        assert_eq!(
            shell_quoting(Path::new("/usr/bin/fish")),
            Some(Some(Shell::Fish))
        );
        assert_eq!(shell_quoting(Path::new("/bin/dash")), Some(None));
        assert_eq!(
            shell_quoting(Path::new("C:\\Windows\\System32\\cmd.exe")),
            None
        );
        assert_eq!(shell_quoting(Path::new("/usr/bin/pwsh")), None);
        assert_eq!(shell_quoting(Path::new("/usr/bin/nu")), None);
    }
}
