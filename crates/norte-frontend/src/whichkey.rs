//! The which-key rows: while a chord sequence is PENDING, what can follow it.
//!
//! The model lives here, shared, so the TUI and the GUI paint the same rows in
//! the same order and word them the same way — only the pixels are each
//! frontend's own. It is built on [`Effective::continuations`], which is what
//! knows the map; this module is the part that turns a continuation into
//! something a reader can read.
//!
//! Two rules are not negotiable, and both are the point of the feature:
//!
//! - **No timers.** ADR 0006's resolution is timing-free, and a panel that
//!   appears after 400 ms would smuggle timing back in through the paint
//!   layer: the same keystrokes would show different things depending on how
//!   fast they were typed. The panel exists exactly while a prefix is pending.
//! - **A bare count does not open it.** The continuation of a count is any key
//!   at all, so the panel would be the whole keymap. A count typed BEHIND a
//!   prefix does show, in the title ([`WhichKeyRows::title`]), because "why is
//!   my `12` still there" is the state a reader most often cannot explain.

use norte_i18n::Lang;

use crate::keymap::{Availability, Chord, Effective, paint_chord, short_unavailable_message};

/// ONE row of the which-key panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhichKeyRow {
    /// The chord to press next, PAINTED: spelled for a reader (`f5` → `F5`)
    /// and masked, because a project `./.norte/keymap.toml` can bind any lone
    /// codepoint and this string goes to a terminal (see
    /// [`paint_chord`]).
    pub chord: String,
    /// What that key does, in the reader's language. Falls back to the command
    /// NAME when the catalogue has no label for it — which is the common case
    /// for a [`Availability::NotBuilt`] command, since nothing has written
    /// help text for a command that does not exist yet. Never the raw Fluent
    /// id: printing `help-cmd-pane-pack` at a reader is the failure mode this
    /// fallback exists to stop.
    pub label: String,
    /// Whether this build can run it. Always [`Availability::Here`] when
    /// [`Self::opens_sequence`] is set: the chord DOES do something (it opens
    /// the rest of the sequence), and what lies beyond is the next panel's
    /// business.
    pub avail: Availability,
    /// The chord opens ANOTHER sequence instead of running a command. The
    /// frontend marks these (a trailing `…`) rather than naming a command the
    /// key does not run.
    pub opens_sequence: bool,
    /// Why the key does nothing, already translated:
    /// [`short_unavailable_message`], shared with the reference sheet (K3b) so
    /// the two surfaces cannot word the same fact differently. EMPTY when the
    /// row is available — the long form, with the command in it, is
    /// [`unavailable_message`](crate::keymap::unavailable_message), which is
    /// what the status bar prints when the key is actually pressed.
    pub reason: String,
}

/// The whole panel: a title naming the state it describes, and the rows.
///
/// A value of this type is a SNAPSHOT of a resolver at one instant. It must be
/// rebuilt or dropped whenever that resolver's state changes — a frontend that
/// keeps it in a field past the end of the pending state is painting a panel
/// for keys that are no longer live.
///
/// **Build it on the transition, store it, render the stored value.** That is
/// what `norte-tui` does (`App::which_key`, written only by its
/// `show_pending`/`clear_pending` pair), and it is not tidiness:
/// [`Self::build`] costs several `String`s and one or two Fluent formats PER
/// ROW, so a frontend that calls it from inside a per-frame `render` pays a
/// twenty-row panel sixty times a second. That is the ~140-allocations-per-
/// keystroke pattern K2a deleted from the GUI's `means_command`, rebuilt in a
/// new place. Reading the resolver live in `render` is fine; building this
/// there is not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WhichKeyRows {
    /// The pending prefix as the reader typed it, painted, with the live count
    /// in front when one is in flight (`12 g`). Empty for an empty prefix.
    pub title: String,
    /// One row per key that can follow, in [`Effective::continuations`] order.
    pub rows: Vec<WhichKeyRow>,
}

impl WhichKeyRows {
    /// The rows for `prefix` pending in `eff`, with `count` (if any) in the
    /// title.
    ///
    /// Allocates several `String`s and one or two Fluent formats PER ROW, so
    /// it belongs on the keystroke that leaves the resolver pending — the one
    /// that opens the sequence and each one that deepens it — never on every
    /// key event, and never inside a per-frame render (see the type's doc).
    ///
    /// ```
    /// use norte_frontend::keymap::{Effective, Screen, parse_chord, parse_keymap};
    /// use norte_frontend::whichkey::WhichKeyRows;
    /// use norte_i18n::Lang;
    ///
    /// let src = r#"
    /// [pane]
    /// keymap = [
    ///     { on = ["g", "g"], run = "cursor.top" },
    ///     { on = ["g", "h"], run = "cursor.bottom" },
    /// ]
    /// "#;
    /// let preset = parse_keymap(src).unwrap();
    /// let known = ["cursor.top", "cursor.bottom"];
    /// let eff = Effective::build_for(&preset, &[], &known, Screen::Browse).unwrap();
    ///
    /// let g = parse_chord("g").unwrap();
    /// let panel = WhichKeyRows::build(&eff, &[g], Some(12), Lang::En);
    /// assert_eq!(panel.title, "12 g", "the count in flight is part of the state");
    /// assert_eq!(panel.rows.len(), 2);
    /// assert_eq!(panel.rows[0].chord, "g");
    /// assert!(!panel.rows[0].opens_sequence);
    /// assert!(panel.rows[0].reason.is_empty(), "an available key has nothing to excuse");
    /// ```
    #[must_use]
    pub fn build(eff: &Effective, prefix: &[Chord], count: Option<u32>, lang: Lang) -> Self {
        let title = pending_title(prefix, count);
        let rows = eff
            .continuations(prefix)
            .into_iter()
            .map(|c| {
                let opens_sequence = c.seq_len > prefix.len() + 1;
                // A row that opens more keys names no command and excuses
                // nothing: the availability of one arbitrary branch behind it
                // is not the availability of the chord.
                let avail = if opens_sequence {
                    Availability::Here
                } else {
                    c.avail
                };
                WhichKeyRow {
                    chord: paint_chord(&c.next.to_string()),
                    label: if opens_sequence {
                        norte_i18n::t_in(lang, "whichkey-more-keys")
                    } else {
                        command_label(c.command, lang)
                    },
                    avail,
                    opens_sequence,
                    reason: short_unavailable_message(avail, lang),
                }
            })
            .collect();
        Self { title, rows }
    }

    /// Whether there are no ROWS — the only thing worth opening a panel for.
    /// A title with nothing under it describes a state the reader can already
    /// read in the status bar, so a frontend is right to paint nothing.
    ///
    /// A pending prefix always has at least one continuation — the resolver
    /// would not be pending otherwise — so this is a guard for the degenerate
    /// call, not a state a reader reaches.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The pending state as ONE string: the prefix painted and space-joined, with
/// the count in front when one is in flight (`12 g`). Empty when neither is.
///
/// Shared, and that is the point: the which-key title and the TUI's
/// status-bar segment are now the same string by construction. They used to be
/// two `match`es over the same two values, one of them painting the chords and
/// the other not — so `f5 g` appeared as `F5 g` in the box and `f5 g` in the
/// bar one line below, and only one of the two was masked, although both go to
/// a terminal and a project `./.norte/keymap.toml` carries no trust.
///
/// ```
/// use norte_frontend::keymap::parse_chord;
/// use norte_frontend::whichkey::pending_title;
///
/// let f5 = parse_chord("f5").unwrap();
/// assert_eq!(pending_title(&[f5], None), "F5");
/// assert_eq!(pending_title(&[f5], Some(12)), "12 F5");
/// // A bare count has no prefix, and no prefix with no count says nothing.
/// assert_eq!(pending_title(&[], Some(12)), "12");
/// assert_eq!(pending_title(&[], None), "");
/// ```
#[must_use]
pub fn pending_title(prefix: &[Chord], count: Option<u32>) -> String {
    let painted: Vec<String> = prefix.iter().map(|c| paint_chord(&c.to_string())).collect();
    let seq = painted.join(" ");
    match (count, seq.is_empty()) {
        (None, _) => seq,
        (Some(n), true) => n.to_string(),
        (Some(n), false) => format!("{n} {seq}"),
    }
}

/// A command's short label, translated: `dialog.*` verbs live in
/// `dialog-cmd-*` and everything else in `help-cmd-*`.
///
/// Routed by PREFIX and not by which context the caller is walking, because
/// the two do not agree: every screen's effective map merges the preset's
/// `[global]` section, so `app.quit` turns up while listing the dialog
/// context, and asking `dialog-cmd-*` for it finds nothing.
///
/// A miss falls back to the command NAME. `norte_i18n::t_in` answers a missing
/// message with the id itself, so the alternative is a column of literal
/// `help-cmd-…` painted at the reader — the failure mode that actually
/// shipped once in the F1 page. The miss is detected by testing for that echo,
/// which IS the failure mode, so the check cannot drift out of agreement with
/// it. The case that made it common —a `Planned` command with no help text,
/// because there was nothing to help with yet— is gone: #132 built the last of
/// them. What still falls back is a command from outside the catalogue.
///
/// A `lua:<name>` command is never in the catalogue — its registry is a
/// runtime one — so it always falls back to its own name, which is the most a
/// keymap can honestly say about it.
///
/// ```
/// use norte_frontend::whichkey::command_label;
/// use norte_i18n::Lang;
///
/// assert_eq!(command_label("app.quit", Lang::En), "quit norte");
/// // `dialog.*` is routed to the other catalogue, prefix stripped.
/// assert_eq!(command_label("dialog.approve", Lang::En), "approve");
/// // A command the catalogue knows is named in prose, built or not.
/// assert_eq!(command_label("pane.pack", Lang::En), "pack into an archive");
/// assert_eq!(command_label("lua:greet", Lang::En), "lua:greet");
/// ```
#[must_use]
pub fn command_label(command: &str, lang: Lang) -> String {
    let id = match command.strip_prefix("dialog.") {
        Some(verb) => format!("dialog-cmd-{}", verb.replace('.', "-")),
        None => format!("help-cmd-{}", command.replace('.', "-")),
    };
    let text = norte_i18n::t_in(lang, &id);
    if text == id { command.to_owned() } else { text }
}

#[cfg(test)]
mod tests {
    use super::{WhichKeyRows, command_label};
    use crate::keymap::{Availability, Effective, Screen, parse_chord, parse_keymap};
    use norte_i18n::Lang;

    fn eff() -> Effective {
        let src = r#"
counts = true

[pane]
keymap = [
    { on = ["g", "g"], run = "cursor.top" },
    { on = ["g", "p"], run = "pane.pack" },
    { on = ["g", "a", "b"], run = "mark.all" },
]
"#;
        let preset = parse_keymap(src).expect("fixture parses");
        let known = ["cursor.top", "mark.all"];
        Effective::build_for(&preset, &[], &known, Screen::Browse).expect("fixture builds")
    }

    /// An unavailable key is a ROW — dimmed and explained, never missing.
    ///
    /// It was the `Planned` commands of K2b's presets that made this the
    /// normal sight; #132 built the last of them, and what is left is the
    /// other unavailability — a live command this frontend does not run,
    /// which is what every GUI-only binding looks like from here. Same row,
    /// same requirement: it must read as an answer, not as a glitch.
    #[test]
    fn an_unavailable_row_carries_the_short_reason_and_its_issue() {
        let panel =
            WhichKeyRows::build(&eff(), &[parse_chord("g").expect("chord")], None, Lang::En);
        let p = panel
            .rows
            .iter()
            .find(|r| r.chord == "p")
            .expect("the pane.pack row");
        assert!(matches!(p.avail, Availability::NotHere), "{:?}", p.avail);
        assert!(!p.reason.is_empty(), "{:?}", p.reason);
        assert!(
            !p.reason.contains("keymap-"),
            "the reason is a Fluent id and must be TRANSLATED: {:?}",
            p.reason
        );
        // Y ahora SÍ tiene texto de ayuda: el comando existe, solo que este
        // build no lo ejecuta. La fila lo nombra en cristiano y explica por
        // qué la tecla no hará nada, que es más de lo que se podía decir
        // cuando la capacidad no estaba construida.
        // `t_in` y no `t`: el panel se construyó con `Lang::En` explícito, y
        // `t` traduce con el idioma GLOBAL —que sale del entorno—. Comparar
        // uno contra otro era verde solo donde `LANG` ya era inglés.
        assert_eq!(
            p.label,
            norte_i18n::t_in(norte_i18n::Lang::En, "help-cmd-pane-pack")
        );
    }

    /// A row that opens more keys claims nothing: not the command at the end
    /// of one arbitrary branch, and not that branch's availability either.
    #[test]
    fn a_row_that_opens_a_longer_sequence_names_no_command() {
        let panel =
            WhichKeyRows::build(&eff(), &[parse_chord("g").expect("chord")], None, Lang::En);
        let a = panel
            .rows
            .iter()
            .find(|r| r.chord == "a")
            .expect("the `g a` row");
        assert!(a.opens_sequence);
        assert_eq!(a.avail, Availability::Here, "the chord itself works");
        assert!(a.reason.is_empty());
        assert_ne!(a.label, "mark.all", "one branch of it is not what it does");
    }

    /// The count rides in the title and nowhere else: `12` then `g` is one
    /// state, and the panel is the only place that can explain it.
    #[test]
    fn the_title_carries_the_count_behind_the_prefix() {
        let g = parse_chord("g").expect("chord");
        let panel = WhichKeyRows::build(&eff(), &[g], Some(12), Lang::En);
        assert_eq!(panel.title, "12 g");
        let plain = WhichKeyRows::build(&eff(), &[g], None, Lang::En);
        assert_eq!(plain.title, "g");
    }

    /// The panel is painted, so it inherits `paint_chord`'s masking: a chord
    /// bound by an untrusted project layer cannot smuggle an escape sequence
    /// into a terminal through the title or a row.
    #[test]
    fn chords_are_painted_and_masked() {
        // U+202E RIGHT-TO-LEFT OVERRIDE, bound by a project layer that
        // carries no trust: a lone codepoint is a legal chord. Bound BOTH as
        // the prefix and as a continuation, because the title and the rows are
        // two different `paint_chord` call sites.
        let src =
            "[pane]\nkeymap = [{ on = [\"\u{202e}\", \"\u{202e}\"], run = \"cursor.top\" }]\n";
        let preset = parse_keymap(src).expect("fixture parses");
        let eff = Effective::build_for(&preset, &[], &["cursor.top"], Screen::Browse)
            .expect("fixture builds");
        let rlo = parse_chord("\u{202e}").expect("a lone codepoint is a chord");
        let panel = WhichKeyRows::build(&eff, &[rlo], None, Lang::En);
        assert!(
            !panel.title.chars().any(norte_encoding::is_terminal_hazard),
            "{:?}",
            panel.title
        );
        let row = panel.rows.first().expect("the continuation row");
        assert!(
            !row.chord.chars().any(norte_encoding::is_terminal_hazard),
            "{:?}",
            row.chord
        );
    }

    /// Both locales answer, and neither answers with the id.
    #[test]
    fn labels_are_translated_in_both_locales() {
        for lang in [Lang::En, Lang::Es] {
            let label = command_label("app.quit", lang);
            assert!(!label.starts_with("help-cmd-"), "{lang:?}: {label}");
            let dialog = command_label("dialog.approve", lang);
            assert!(!dialog.starts_with("dialog-cmd-"), "{lang:?}: {dialog}");
        }
    }
}
