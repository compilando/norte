//! Editing the focused pane's location in place (`pane.edit-path`, ADR
//! 0190): the text a location is edited as, the line with its cursor, and
//! where Enter leads.
//!
//! Shared by both frontends. The window's `<input>` owns its own caret, so
//! it only uses [`initial_text`], [`target`] and [`complete`]; the terminal
//! keeps the whole [`PathEdit`].
//!
//! ```
//! use norte_frontend::path_edit::{PathEdit, initial_text};
//! # #[cfg(unix)] {
//! let dir = norte_proto::VPath::parse("file:///tmp").unwrap();
//! let mut edit = PathEdit::new(initial_text(&dir).unwrap());
//! assert!(edit.all_selected());
//! edit.insert("/etc"); // typing over the selection replaces the path
//! assert_eq!(edit.text(), "/etc");
//! # }
//! ```

use norte_proto::{Segment, VPath};
use unicode_normalization::UnicodeNormalization;

use crate::goto::{Action, K_PATH, action};

/// The text `dir` is edited as, or `None` when no text can name it
/// faithfully on screen.
///
/// A local folder whose native path is plain text is edited as that path
/// (`/home/x`, `C:\x`), which is what the reader types elsewhere. Anything
/// else — another scheme, another machine, a name that is not UTF-8 — is
/// edited in its wire form (`sftp://host/x`, `file:///caf%E9`), which
/// escapes the bytes text cannot carry, so Enter goes back to the SAME
/// folder and nothing is changed by being shown. What even the wire form
/// shows raw (a bidi override, a C1 control) is refused: painting it would
/// let a name rearrange the line, and masking it would edit another path.
#[must_use]
pub fn initial_text(dir: &VPath) -> Option<String> {
    let clean = |s: &String| !s.chars().any(norte_encoding::is_terminal_hazard);
    native_text(dir)
        .filter(clean)
        .or_else(|| Some(dir.to_wire()).filter(clean))
}

/// The native path of a local `dir`, only when it names `dir` back.
fn native_text(dir: &VPath) -> Option<String> {
    let native = norte_vfs::native::vpath_to_native(dir).ok()?;
    let text = without_verbatim(native.to_str()?);
    // The check is what sends a Windows `foo.`, `foo ` or `CON` to the
    // wire: resolving their native spelling strips or reroutes them.
    let back = norte_vfs::native::vpath_from_native(std::path::Path::new(&text)).ok()?;
    (back == *dir).then_some(text)
}

/// `vpath_to_native` adds Windows' verbatim prefix; nobody types it.
fn without_verbatim(native: &str) -> String {
    if cfg!(windows) {
        if let Some(rest) = native.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = native.strip_prefix(r"\\?\") {
            return rest.to_owned();
        }
    }
    native.to_owned()
}

/// Where Enter on `text` leads: the same resolution as a path typed into
/// go-anywhere (`~`, native absolute, or a wire with its scheme), so one
/// text never means two places depending on the box it was typed in. One
/// trailing `/` — what Tab leaves — is allowed on a wire, which does not
/// parse with it.
#[must_use]
pub fn target(text: &str) -> Action {
    let first = action(&format!("{K_PATH}{text}"));
    if matches!(first, Action::Ir(_)) {
        return first;
    }
    match text.strip_suffix('/') {
        Some(t) if !t.is_empty() && !t.ends_with('/') => match action(&format!("{K_PATH}{t}")) {
            ir @ Action::Ir(_) => ir,
            _ => first,
        },
        _ => first,
    }
}

/// The folders listed IN `dir`, the ones Tab completes: not the `..` row
/// (its path is the parent's) nor a results tab's hits from deeper down.
#[must_use]
pub fn folders_in<'a>(dir: &VPath, entries: &'a [norte_proto::Entry]) -> Vec<&'a [u8]> {
    entries
        .iter()
        .filter(|e| e.kind == norte_proto::EntryKind::Dir && e.path.parent().as_ref() == Some(dir))
        .filter_map(|e| e.path.file_name().map(Segment::as_bytes))
        .collect()
}

/// Whether the window's `<input>` sent a text the terminal's line would
/// have let in: no control, nothing that rearranges a screen.
#[must_use]
pub fn acceptable(text: &str) -> bool {
    text.chars().all(allowed)
}

/// Completes the last component before byte `at` against `dirs`, the
/// folders listed in `pane_dir`. It only completes where the text up to
/// that component IS `pane_dir`: the listing on screen is the only one at
/// hand, and offering its names under another folder would invent them.
/// `Some((text, cursor))` with the new line and its cursor (in bytes) when
/// it changed.
#[must_use]
pub fn complete(
    text: &str,
    at: usize,
    pane_dir: &VPath,
    dirs: &[&[u8]],
) -> Option<(String, usize)> {
    let at = (0..=at.min(text.len()))
        .rev()
        .find(|i| text.is_char_boundary(*i))?;
    let head = &text[..at];
    // In a wire only `/` separates: a `\` is a byte of a Unix name.
    let wire = !(crate::goto::typed_local(head) || head.starts_with('~'));
    let is_sep = |c: char| c == '/' || (!wire && cfg!(windows) && c == '\\');
    let sep = head.rfind(is_sep)?;
    let (parent, word) = (&head[..=sep], &head[sep + 1..]);
    if target(parent) != Action::Ir(pane_dir.clone()) {
        return None;
    }
    let word_nfc: String = word.nfc().collect();
    let names: Vec<String> = dirs
        .iter()
        .filter_map(|n| {
            let spelled = name_text(n, wire)?;
            // Matched in NFC (a macOS listing is NFD), inserted as listed.
            let nfc: String = spelled.nfc().collect();
            if !nfc.starts_with(&word_nfc) {
                return None;
            }
            // Only a spelling that resolves back to THIS name: Windows
            // strips `foo.`'s dot and reroutes `CON` when resolving.
            let own = pane_dir.join(Segment::new(n.to_vec()).ok()?);
            (target(&format!("{parent}{spelled}")) == Action::Ir(own)).then_some(spelled)
        })
        .collect();
    let replacement = match names.as_slice() {
        [] => return None,
        // The separator is added unless the text after the cursor already
        // starts with one.
        [one] if text[at..].starts_with(is_sep) => one.clone(),
        [one] => {
            let sep = if wire { '/' } else { std::path::MAIN_SEPARATOR };
            format!("{one}{sep}")
        }
        many => {
            let mut common = common_prefix(many);
            // Never stop inside a `%XX`: a half escape plus the next typed
            // letter is a byte neither name has.
            if wire
                && let Some(p) = common.rfind('%')
                && common.len() - p < 3
            {
                common = &common[..p];
            }
            if common.len() <= word.len() {
                return None;
            }
            common.to_owned()
        }
    };
    let mut out = String::with_capacity(text.len() + replacement.len());
    out.push_str(&head[..=sep]);
    out.push_str(&replacement);
    let cursor = out.len();
    out.push_str(&text[at..]);
    Some((out, cursor))
}

/// A folder name as it is written into the line: its wire form in a wire,
/// its UTF-8 in a native path (a name that is not text has no native
/// spelling and is not offered), never one with a character the line
/// refuses.
fn name_text(name: &[u8], wire: bool) -> Option<String> {
    let text = if wire {
        Segment::new(name.to_vec()).ok()?.to_wire()
    } else {
        std::str::from_utf8(name).ok()?.to_owned()
    };
    acceptable(&text).then_some(text)
}

fn common_prefix(names: &[String]) -> &str {
    let first = &names[0];
    let mut end = first.len();
    for n in &names[1..] {
        end = end.min(
            first
                .char_indices()
                .zip(n.chars())
                .take_while(|((_, a), b)| a == b)
                .last()
                .map_or(0, |((i, c), _)| i + c.len_utf8()),
        );
    }
    &first[..end]
}

/// What the line accepts: no control, nothing that rearranges a terminal.
fn allowed(c: char) -> bool {
    !c.is_control() && !norte_encoding::is_terminal_hazard(c)
}

/// The line being edited in the terminal, with its cursor, starting with
/// all of it selected — as in a location bar, typing replaces the path and
/// a cursor key keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathEdit {
    buffer: String,
    /// Cursor, in chars.
    cursor: usize,
    all: bool,
}

impl PathEdit {
    /// The line `text`, all of it selected, the cursor at its end.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self {
            cursor: text.chars().count(),
            buffer: text,
            all: true,
        }
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

    /// Whether the whole line is selected.
    #[must_use]
    pub fn all_selected(&self) -> bool {
        self.all
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

    /// Drops the selection by deleting what it covers.
    fn take_selection(&mut self) -> bool {
        if self.all {
            self.all = false;
            self.buffer.clear();
            self.cursor = 0;
            return true;
        }
        false
    }

    /// Types `s` at the cursor, over the selection if there is one. Only
    /// its first line, and no character the line refuses: a pasted
    /// newline must not become part of a path.
    pub fn insert(&mut self, s: &str) {
        let clean: String = s
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| allowed(*c))
            .collect();
        self.take_selection();
        let at = self.byte_at(self.cursor);
        self.buffer.insert_str(at, &clean);
        self.cursor += clean.chars().count();
    }

    /// Deletes the selection, or the char before the cursor.
    pub fn backspace(&mut self) {
        if !self.take_selection() && self.cursor > 0 {
            self.cursor -= 1;
            let at = self.byte_at(self.cursor);
            self.buffer.remove(at);
        }
    }

    /// Deletes the selection, or the char under the cursor.
    pub fn delete(&mut self) {
        if !self.take_selection() && self.cursor < self.len_chars() {
            let at = self.byte_at(self.cursor);
            self.buffer.remove(at);
        }
    }

    /// One char left; on a selection, to its start.
    pub fn left(&mut self) {
        if std::mem::take(&mut self.all) {
            self.cursor = 0;
        } else {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    /// One char right; on a selection, to its end.
    pub fn right(&mut self) {
        if !std::mem::take(&mut self.all) {
            self.cursor = (self.cursor + 1).min(self.len_chars());
        }
    }

    /// To the start of the line.
    pub fn home(&mut self) {
        self.all = false;
        self.cursor = 0;
    }

    /// To the end of the line.
    pub fn end(&mut self) {
        self.all = false;
        self.cursor = self.len_chars();
    }

    /// [`complete`] at the cursor. `true` when the line changed.
    pub fn complete(&mut self, pane_dir: &VPath, dirs: &[&[u8]]) -> bool {
        self.all = false;
        let at = self.byte_at(self.cursor);
        let Some((text, cursor)) = complete(&self.buffer, at, pane_dir, dirs) else {
            return false;
        };
        self.cursor = text[..cursor].chars().count();
        self.buffer = text;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire")
    }

    /// A local folder is edited as the path the reader types everywhere
    /// else, and Enter on it unchanged goes back to the same folder.
    #[cfg(unix)]
    #[test]
    fn a_local_folder_is_edited_as_its_native_path() {
        let dir = vp("file:///home/ana/fotos");
        let text = initial_text(&dir).expect("plain text");
        assert_eq!(text, "/home/ana/fotos");
        assert_eq!(target(&text), Action::Ir(dir));
    }

    /// The bytes text cannot carry travel escaped, so showing the path
    /// does not change which folder Enter leads to (rule 1).
    #[test]
    fn a_name_that_is_not_text_is_edited_as_its_wire_and_round_trips() {
        let dir = vp("file:///tmp/caf%E9");
        let text = initial_text(&dir).expect("wire is text");
        assert_eq!(text, "file:///tmp/caf%E9");
        assert_eq!(target(&text), Action::Ir(dir));
    }

    /// A remote pane keeps its scheme and host, so an edit stays there.
    #[test]
    fn a_remote_folder_keeps_its_scheme_and_host() {
        let dir = vp("sftp://ana@host/srv/www");
        let text = initial_text(&dir).expect("wire");
        assert_eq!(text, "sftp://ana@host/srv/www");
        assert_eq!(target(&text), Action::Ir(dir));
    }

    /// An escape sequence in a local name is shown escaped, not raw.
    #[cfg(unix)]
    #[test]
    fn a_control_in_a_name_is_edited_escaped() {
        let dir = vp("file:///tmp/a%1B%5Db");
        let text = initial_text(&dir).expect("wire escapes ASCII controls");
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert_eq!(target(&text), Action::Ir(dir));
    }

    /// A bidi override survives even the wire form, so the line refuses it
    /// instead of painting a name that reorders it.
    #[test]
    fn a_bidi_override_cannot_be_edited() {
        let dir = vp("file:///tmp/a\u{202E}b");
        assert_eq!(initial_text(&dir), None);
    }

    #[test]
    fn typing_over_the_selection_replaces_the_path() {
        let mut e = PathEdit::new("/home/ana".into());
        assert!(e.all_selected());
        e.insert("/srv");
        assert_eq!((e.text(), e.cursor(), e.all_selected()), ("/srv", 4, false));
    }

    #[test]
    fn a_cursor_key_keeps_the_path_and_drops_the_selection() {
        let mut e = PathEdit::new("/home/ana".into());
        e.left();
        assert_eq!(
            (e.text(), e.cursor(), e.all_selected()),
            ("/home/ana", 0, false)
        );
        let mut e = PathEdit::new("/home/ana".into());
        e.right();
        assert_eq!((e.cursor(), e.all_selected()), (9, false));
        e.backspace();
        assert_eq!(e.text(), "/home/an");
    }

    #[test]
    fn backspace_on_the_selection_empties_the_line() {
        let mut e = PathEdit::new("/home/ana".into());
        e.backspace();
        assert_eq!((e.text(), e.cursor()), ("", 0));
    }

    /// A paste with a newline or an escape cannot smuggle either into the
    /// path.
    #[test]
    fn a_paste_keeps_its_first_line_and_no_controls() {
        let mut e = PathEdit::new(String::new());
        e.insert("/tmp/a\u{1b}b\n/etc");
        assert_eq!(e.text(), "/tmp/ab");
    }

    #[cfg(unix)]
    #[test]
    fn tab_completes_a_unique_folder_of_the_pane_with_a_separator() {
        let pane = vp("file:///home/ana");
        let mut e = PathEdit::new("/home/ana".into());
        e.end();
        e.insert("/fo");
        assert!(e.complete(&pane, &[b"fotos", b"docs"]));
        assert_eq!(e.text(), "/home/ana/fotos/");
        assert_eq!(e.cursor(), e.text().chars().count());
    }

    #[cfg(unix)]
    #[test]
    fn tab_extends_to_the_common_prefix_and_no_further() {
        let pane = vp("file:///home/ana");
        let mut e = PathEdit::new("/home/ana/f".into());
        e.end();
        assert!(e.complete(&pane, &[b"fotos", b"fondos"]));
        assert_eq!(e.text(), "/home/ana/fo");
        assert!(
            !e.complete(&pane, &[b"fotos", b"fondos"]),
            "nothing more to add"
        );
    }

    /// Only the listing on screen is known: under another folder, Tab
    /// does not offer its names.
    #[cfg(unix)]
    #[test]
    fn tab_completes_nothing_under_a_folder_that_is_not_the_pane() {
        let pane = vp("file:///home/ana");
        let mut e = PathEdit::new("/srv/fo".into());
        e.end();
        assert!(!e.complete(&pane, &[b"fotos"]));
        assert_eq!(e.text(), "/srv/fo");
    }

    /// In a wire, a name goes in escaped; in a native path, one that is not
    /// text is not offered at all.
    #[test]
    fn a_completed_name_is_spelled_for_the_line_it_goes_into() {
        let pane = vp("sftp://host/srv");
        let (text, _) = complete("sftp://host/srv/5", 17, &pane, &[b"50%"]).expect("one hit");
        assert_eq!(text, "sftp://host/srv/50%25/");
        #[cfg(unix)]
        {
            let pane = vp("file:///tmp");
            assert_eq!(complete("/tmp/caf", 8, &pane, &[b"caf\xE9"]), None);
        }
    }

    /// The text after the cursor is kept where it was.
    #[cfg(unix)]
    #[test]
    fn completing_in_the_middle_keeps_the_tail() {
        let pane = vp("file:///home");
        let (text, cursor) = complete("/home/an/x", 8, &pane, &[b"ana"]).expect("hit");
        assert_eq!((text.as_str(), cursor), ("/home/ana/x", 9));
    }

    /// What Tab leaves — a trailing `/` — is where Enter goes, in a wire
    /// too, where `VPath::parse` refuses the empty segment.
    #[test]
    fn enter_after_tab_goes_to_the_completed_folder() {
        let pane = vp("sftp://host/srv");
        let (text, _) = complete("sftp://host/srv/w", 17, &pane, &[b"www"]).expect("hit");
        assert_eq!(text, "sftp://host/srv/www/");
        assert_eq!(target(&text), Action::Ir(vp("sftp://host/srv/www")));
    }

    /// Two names sharing a half escape (`caf%E9`, `caf%E8`) stop BEFORE
    /// the `%`: `caf%E` plus a typed letter is a byte neither name has.
    #[test]
    fn a_common_prefix_never_ends_inside_an_escape() {
        let pane = vp("sftp://host/srv");
        let (text, _) =
            complete("sftp://host/srv/c", 17, &pane, &[b"caf\xE9", b"caf\xE8"]).expect("prefix");
        assert_eq!(text, "sftp://host/srv/caf");
    }

    /// The `..` row is a folder whose path is the PARENT, and a results
    /// tab lists hits from deeper down: neither is a folder of this one.
    #[test]
    fn only_the_folders_listed_in_the_pane_are_completed() {
        let entry = |wire: &str| norte_proto::Entry {
            attrs: std::collections::BTreeMap::new(),
            path: vp(wire),
            kind: norte_proto::EntryKind::Dir,
            size: None,
            mtime_ms: None,
        };
        let dir = vp("mem:///home/ana");
        let entries = [
            entry("mem:///home"),
            entry("mem:///home/ana/api"),
            entry("mem:///home/ana/web/deep"),
        ];
        assert_eq!(folders_in(&dir, &entries), [b"api".as_slice()]);
    }

    /// The canonical hostile corpus, as a pane's folder and as a folder
    /// IN a pane, locally and remotely: the unedited line goes back to the
    /// same folder (or does not open), and a completed name resolves to
    /// that exact name.
    #[test]
    fn every_hostile_name_round_trips_or_is_refused() {
        let roots = if cfg!(windows) {
            ["file:///C:/tmp", "sftp://host/srv"]
        } else {
            ["file:///tmp", "sftp://host/srv"]
        };
        for fixture in norte_testkit::corpus::hostile_names() {
            let Ok(seg) = Segment::new(fixture.bytes.clone()) else {
                continue;
            };
            for root in roots {
                let pane = vp(root);
                let dir = pane.join(seg.clone());
                if let Some(text) = initial_text(&dir) {
                    assert_eq!(
                        target(&text),
                        Action::Ir(dir.clone()),
                        "{} in {root}",
                        fixture.id
                    );
                    assert!(acceptable(&text), "{} in {root}: {text:?}", fixture.id);
                }
                let Some(start) = initial_text(&pane) else {
                    panic!("{root} opens");
                };
                let sep = if start.contains("://") {
                    '/'
                } else {
                    std::path::MAIN_SEPARATOR
                };
                let line = format!("{start}{sep}");
                let names = [fixture.bytes.as_slice()];
                if let Some((done, _)) = complete(&line, line.len(), &pane, &names) {
                    assert_eq!(
                        target(&done),
                        Action::Ir(dir),
                        "{} in {root}: {done:?}",
                        fixture.id
                    );
                }
            }
        }
    }
}
