//! The shortcut editor: its rows, its capture state machine, and the words it
//! says (K3c c3/c4).
//!
//! Shared for the reason the rest of `norte-frontend` is: the TUI and the GUI
//! must not each decide what a row is, when a capture may be confirmed, or how
//! a refusal is worded. What stays per-frontend is the paint and the key
//! plumbing — a `crossterm` event on one side, a GPUI one on the other.
//!
//! Three things make this more than a list:
//!
//! - **It lists the keys AND the commands with no key.** The reference sheet
//!   (K3b, [`crate::keysheet`]) answers "what does this key do"; an editor also
//!   has to answer "how do I press X", and a command nobody bound is invisible
//!   to the sheet. So every runnable command of a screen that no binding names
//!   is a row too, with an empty chord.
//! - **The verdict comes BEFORE the confirmation.**
//!   [`crate::keymap::rebind_check`] is cheap enough to run on the captured
//!   chord, so the reader is told "free", "replaces `pane.copy`" or "refused,
//!   and why" while still holding the decision. A refusal cannot be confirmed
//!   at all ([`Rebind::is_refusal`]).
//! - **The captured chord still has to pass the door.**
//!   [`rebind_check`] reads the MERGED map and
//!   cannot see a project layer outranking the user's, nor a differently
//!   spelled twin in the very list being written; only
//!   [`rebind_dry_run`] can, because it models
//!   the write and re-runs the loader. This module deliberately does NOT wrap
//!   the door: it holds no config layers and would have to be handed them, and
//!   a second gate that could be reached without the first is exactly the
//!   defect the door exists to prevent. [`ShortcutsState::confirmable`] hands
//!   back the sequence and the command; the frontend runs the dry run and
//!   writes what IT returns.
//!
//! What a frontend must not do with any of this: write [`RebindWrite`]'s
//! chords by re-rendering the captured sequence, or pick the target list
//! itself. Both come out of the dry run, and both fail silently when guessed
//! (see [`norte_config::KeymapList`]).

use norte_i18n::Lang;

use crate::keymap::{
    Availability, Chord, Effective, KeymapFile, Rebind, RebindError, RebindSources, RebindWrite,
    Screen, Status, UnbindOutcome, UnbindWrite, catalogue, parse_keymap, presets, rebind_check,
    rebind_dry_run, short_unavailable_message, unbind_dry_run,
};
use crate::keysheet::sheet_of;
use crate::whichkey::command_label;

/// One screen of the editor: its map, and the commands a rebind may target
/// there.
#[derive(Debug, Clone, Copy)]
pub struct ScreenKeys<'a> {
    /// Which screen these keys belong to. Also decides the `keymap.toml`
    /// section a write lands in ([`Screen::section`]).
    pub screen: Screen,
    /// The map as it is RIGHT NOW — the one the resolver is using, not a copy
    /// taken at start-up. Borrowed: see [`sheet_of`].
    pub eff: &'a Effective,
    /// The commands this frontend runs ON THIS SCREEN. The unbound rows come
    /// from here, filtered to what the catalogue calls [`Status::Live`].
    ///
    /// It is NOT necessarily the `known_commands` the map was built with, and
    /// the difference matters in both directions. That set is deliberately
    /// wider for [`Screen::Dialog`] (the dialog map merges `[global]`, so it
    /// has to accept `app.quit` or the whole layer fails to validate), and
    /// listing every browse command as bindable in the viewer would answer
    /// "how do I press X" with a key that does nothing there. A frontend
    /// passes what it will actually dispatch.
    pub bindable: &'a [&'a str],
}

/// One row of the editor: a key and what it does, or a command and the fact
/// that nothing presses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutRow {
    /// Which screen's map this row belongs to; the renderer groups by it, and
    /// a write derives its section from it.
    pub screen: Screen,
    /// The sequence PAINTED, or EMPTY for an unbound command. Masked, like
    /// every chord that reaches a terminal — a project `./.norte/keymap.toml`
    /// can bind any lone codepoint.
    pub chord: String,
    /// The same sequence in chords, EMPTY for an unbound command
    /// ([`Self::is_bound`]). What an unbind hands the writer, spelled with
    /// `Display`.
    pub seq: Vec<Chord>,
    /// The command, raw — the name a write puts in `run`.
    pub command: String,
    /// What it does, in the reader's language ([`command_label`]), falling
    /// back to the command name. Never a raw Fluent id.
    pub label: String,
    /// Whether this build can run it. An unbound row is always
    /// [`Availability::Here`]: it is built from what the frontend says it
    /// dispatches, intersected with the Live catalogue.
    pub avail: Availability,
    /// Why the key does nothing, already translated
    /// ([`short_unavailable_message`]) — empty when it does something.
    pub reason: String,
    /// Whether this binding lives in `[global]` rather than [`Self::screen`]'s
    /// own section ([`Effective::is_global`]). Always `false` for an unbound
    /// row: it names no binding to have a section at all.
    ///
    /// The provenance comes from the LAYER the binding was read out of, not
    /// from [`Self::screen`] — a screen's map merges `[global]` into itself,
    /// so by the time a row exists the two are indistinguishable without this
    /// field. [`Self::is_editable`] is what the editor should actually ask.
    pub global: bool,
}

impl ShortcutRow {
    /// Whether a key presses this command today. `false` rows are the ones the
    /// sheet cannot show and an editor must.
    #[must_use]
    pub fn is_bound(&self) -> bool {
        !self.seq.is_empty()
    }

    /// Whether this row may be rebound or unbound from here at all.
    ///
    /// A `[global]` row fails this — not because the binding cannot change,
    /// but because [`Screen::section`] never answers `"global"`: a write
    /// through a row that names ONE screen would land in a section that
    /// changes all three, or (today) find nothing to write to at all and
    /// silently do nothing (#141). The editor says where the binding lives
    /// instead of guessing at a write it must not make; the file itself is
    /// still the way to change it.
    #[must_use]
    pub fn is_editable(&self) -> bool {
        !self.global
    }
}

/// Every row of the editor: per screen, the bound keys in the map's own
/// precedence order (exactly [`sheet_of`]'s rows), then the runnable commands
/// nothing presses, in catalogue order.
///
/// Each screen's rows are contiguous and screens come in the order given, so a
/// renderer can group them without sorting anything. Within a screen the bound
/// rows keep the sheet's order for the reason the sheet documents: precedence
/// order IS what the key does. The unbound tail is in catalogue order, which is
/// stable across builds — a list that reshuffled between two openings would be
/// unusable.
///
/// A command bound to several keys is several bound rows and no unbound row.
///
/// ```
/// use norte_frontend::keymap::{Effective, Screen, parse_keymap};
/// use norte_frontend::shortcuts::{ScreenKeys, build_rows};
/// use norte_i18n::Lang;
///
/// let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
///     .unwrap();
/// let bindable = ["pane.copy", "pane.move"];
/// let eff = Effective::build_for(&preset, &[], &bindable, Screen::Browse).unwrap();
///
/// let rows = build_rows(
///     &[ScreenKeys { screen: Screen::Browse, eff: &eff, bindable: &bindable }],
///     Lang::En,
/// );
/// assert_eq!(rows.len(), 2);
/// assert_eq!((rows[0].chord.as_str(), rows[0].command.as_str()), ("F5", "pane.copy"));
/// // `pane.move` has no key at all — which is exactly why it needs a row.
/// assert!(!rows[1].is_bound());
/// assert_eq!(rows[1].command, "pane.move");
/// ```
#[must_use]
pub fn build_rows(screens: &[ScreenKeys<'_>], lang: Lang) -> Vec<ShortcutRow> {
    let mut out = Vec::new();
    for s in screens {
        for row in sheet_of(s.screen, s.eff) {
            out.push(ShortcutRow {
                screen: row.screen,
                chord: row.chord,
                seq: row.seq,
                label: command_label(&row.command, lang),
                command: row.command,
                avail: row.avail,
                reason: short_unavailable_message(row.avail, lang),
                global: row.global,
            });
        }
        let bound: Vec<&str> = s.eff.bindings_all_seq().iter().map(|b| b.1).collect();
        for def in catalogue::CATALOGUE {
            if def.status != Status::Live
                || !s.bindable.contains(&def.name)
                || bound.contains(&def.name)
            {
                continue;
            }
            out.push(ShortcutRow {
                screen: s.screen,
                chord: String::new(),
                seq: Vec::new(),
                command: def.name.to_owned(),
                label: command_label(def.name, lang),
                avail: Availability::Here,
                reason: String::new(),
                // No binding, no section it could live in: never global.
                global: false,
            });
        }
    }
    out
}

/// A rebind in progress: which row it is for, the chord captured so far, and
/// what the map says about it.
///
/// It carries the row's command and screen rather than only its index, so the
/// confirm reads one value instead of indexing back into a list that a
/// hot-reload may have replaced. It cannot outlive such a reload anyway —
/// [`ShortcutsState::refresh`] cancels it — but a capture that CANNOT dangle is
/// better than one that merely does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    row: usize,
    screen: Screen,
    command: String,
    seq: Vec<Chord>,
    verdict: Option<Rebind>,
}

impl Capture {
    /// Waiting for the key: nothing captured yet. The state a frontend paints
    /// as "press the new key".
    #[must_use]
    pub fn is_waiting(&self) -> bool {
        self.seq.is_empty()
    }

    /// REAL index (into [`ShortcutsState::rows`]) of the row being rebound.
    #[must_use]
    pub fn row(&self) -> usize {
        self.row
    }

    /// The command that will run, if this capture is confirmed.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// The screen whose map — and whose `keymap.toml` section — this is about.
    #[must_use]
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// What was captured. Empty while [`Self::is_waiting`].
    #[must_use]
    pub fn seq(&self) -> &[Chord] {
        &self.seq
    }

    /// What the map says about it: `None` while waiting.
    #[must_use]
    pub fn verdict(&self) -> Option<&Rebind> {
        self.verdict.as_ref()
    }
}

/// The editor: rows, a filter over them, a cursor, and at most one capture.
///
/// Pure. Nothing here touches disk, and nothing here writes a keymap: the
/// frontend runs [`rebind_dry_run`] on what
/// [`Self::confirmable`] hands back and persists the [`RebindWrite`] the door
/// returns.
#[derive(Debug, Clone)]
pub struct ShortcutsState {
    rows: Vec<ShortcutRow>,
    /// Folded haystack per row (command + label + chord).
    folds: Vec<String>,
    /// Filter bytes as typed (sanitized only when painted).
    query: Vec<u8>,
    /// REAL indices into `rows` that match.
    visible: Vec<usize>,
    /// Selection WITHIN `visible`.
    cursor: usize,
    capture: Option<Capture>,
}

impl ShortcutsState {
    /// Opens the editor over `rows` (a [`build_rows`] snapshot).
    #[must_use]
    pub fn new(rows: Vec<ShortcutRow>) -> Self {
        let folds = Self::fold_rows(&rows);
        let mut s = Self {
            rows,
            folds,
            query: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
            capture: None,
        };
        s.recompute();
        s
    }

    fn fold_rows(rows: &[ShortcutRow]) -> Vec<String> {
        rows.iter()
            .map(|r| crate::nav::fold(format!("{} {} {}", r.command, r.label, r.chord).as_bytes()))
            .collect()
    }

    /// Replaces the rows with a fresh snapshot (a keymap hot-reload): CANCELS
    /// any capture, and RE-ANCHORS the cursor on the row it was on.
    ///
    /// Neither is bookkeeping, and the second is the dangerous one.
    ///
    /// The cancellation is why this is not
    /// [`crate::settings::SettingsState::refresh`], which keeps its edit
    /// buffer. A settings edit is text; a capture is a VERDICT, and the verdict
    /// was read off the map that has just been replaced. Keeping it would let a
    /// reader confirm "free" about a key another norte, or their own text
    /// editor, bound half a second ago.
    ///
    /// The re-anchor is because a CURSOR over these rows does assert something
    /// about the map — which binding the next destructive key acts on. The
    /// settings row set is a fixed schema whose values change; this one is
    /// DERIVED from the keymap and is reordered by exactly the reload the
    /// editor itself causes: a command that has just been bound leaves the
    /// unbound tail and joins the bound block in precedence order, and every
    /// index after it shifts. A cursor kept as a bare index would then sit on a
    /// different binding, and the next unbind would delete a row the reader
    /// never pointed at while the message named it confidently. So the
    /// selected row's identity (screen, command, sequence) is looked up in the
    /// new rows; when it is gone — the case a bare index cannot even detect —
    /// the cursor falls back to the clamp.
    ///
    /// The filter survives untouched: it asserts nothing about the map.
    pub fn refresh(&mut self, rows: Vec<ShortcutRow>) {
        let anchor = self.visible.get(self.cursor).map(|&i| self.rows[i].clone());
        self.folds = Self::fold_rows(&rows);
        self.rows = rows;
        self.capture = None;
        self.recompute();
        if let Some(anchor) = anchor
            && let Some(pos) = self.visible.iter().position(|&i| {
                let r = &self.rows[i];
                r.screen == anchor.screen && r.command == anchor.command && r.seq == anchor.seq
            })
        {
            self.cursor = pos;
        }
    }

    fn recompute(&mut self) {
        self.visible = if self.query.is_empty() {
            (0..self.rows.len()).collect()
        } else {
            let q = crate::nav::fold(&self.query);
            self.folds
                .iter()
                .enumerate()
                .filter(|(_, f)| f.contains(&q))
                .map(|(i, _)| i)
                .collect()
        };
        if self.visible.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len() - 1;
        }
    }

    /// Appends a char to the filter. No-op while capturing — every key belongs
    /// to the capture then, and a filter that moved under it would change which
    /// row the confirm is about.
    pub fn push_char(&mut self, c: char) {
        if self.capture.is_some() {
            return;
        }
        let mut buf = [0u8; 4];
        self.query
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.recompute();
    }

    /// Removes the last complete UTF-8 char from the filter. No-op capturing.
    pub fn backspace(&mut self) {
        if self.capture.is_some() || self.query.is_empty() {
            return;
        }
        let mut cut = self.query.len() - 1;
        while cut > 0 && (self.query[cut] & 0b1100_0000) == 0b1000_0000 {
            cut -= 1;
        }
        self.query.truncate(cut);
        self.recompute();
    }

    /// Moves the selection up. No-op capturing.
    pub fn up(&mut self) {
        if self.capture.is_none() {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    /// Moves the selection down. No-op capturing.
    pub fn down(&mut self) {
        if self.capture.is_none() && self.cursor + 1 < self.visible.len() {
            self.cursor += 1;
        }
    }

    /// Moves the selection up `n`. No-op capturing.
    pub fn page_up(&mut self, n: usize) {
        if self.capture.is_none() {
            self.cursor = self.cursor.saturating_sub(n);
        }
    }

    /// Moves the selection down `n`. No-op capturing.
    pub fn page_down(&mut self, n: usize) {
        if self.capture.is_none() {
            self.cursor = (self.cursor + n).min(self.visible.len().saturating_sub(1));
        }
    }

    /// Selects the `idx`-th VISIBLE row, clamped (the GUI's hover/click
    /// primitive). No-op capturing.
    pub fn set_cursor(&mut self, idx: usize) {
        if self.capture.is_none() {
            self.cursor = idx.min(self.visible.len().saturating_sub(1));
        }
    }

    /// REAL indices into [`Self::rows`] that pass the filter.
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// All rows — `rows()[visible()[i]]` paints the `i`-th filtered row.
    #[must_use]
    pub fn rows(&self) -> &[ShortcutRow] {
        &self.rows
    }

    /// Selection position WITHIN [`Self::visible`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The row under the cursor, if any is visible.
    #[must_use]
    pub fn selected(&self) -> Option<&ShortcutRow> {
        self.visible.get(self.cursor).map(|&i| &self.rows[i])
    }

    /// Filter text ready to paint (lossy, masked — same contract as the
    /// palette's query display).
    #[must_use]
    pub fn query_display(&self) -> String {
        String::from_utf8_lossy(&self.query)
            .chars()
            .map(|c| {
                if norte_encoding::is_terminal_hazard(c) {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    }

    /// The capture in flight, if any.
    #[must_use]
    pub fn capture(&self) -> Option<&Capture> {
        self.capture.as_ref()
    }

    /// Whether a capture is in flight — while it is, the frontend must route
    /// every key to [`Self::capture_chord`] instead of to its own bindings.
    #[must_use]
    pub fn is_capturing(&self) -> bool {
        self.capture.is_some()
    }

    /// Starts capturing a new key for the row under the cursor. `false` when
    /// nothing is selected (an empty filter result), when the row is
    /// [`ShortcutRow::is_editable`]'s `false` (a `[global]` row: writing here
    /// would land in this screen's own section, not the one the row is
    /// actually bound in — #141), and a no-op when a capture is already in
    /// flight.
    pub fn begin_capture(&mut self) -> bool {
        if self.capture.is_some() {
            return false;
        }
        let Some(&real) = self.visible.get(self.cursor) else {
            return false;
        };
        let row = &self.rows[real];
        if !row.is_editable() {
            return false;
        }
        self.capture = Some(Capture {
            row: real,
            screen: row.screen,
            command: row.command.clone(),
            seq: Vec::new(),
            verdict: None,
        });
        true
    }

    /// Records the captured chord and reads the verdict off `eff`.
    ///
    /// `eff` must be the map of [`Capture::screen`] — the same one the row came
    /// from. A different screen's map would answer about a different keyboard
    /// (`Tab` is free in the viewer and reserved in the browser), so the
    /// mismatch is a caller bug and says so in debug.
    ///
    /// Called again, it REPLACES the chord: capturing is not additive, and a
    /// reader who pressed the wrong key presses another.
    ///
    /// No-op when no capture is in flight.
    pub fn capture_chord(&mut self, chord: Chord, eff: &Effective) {
        let Some(cap) = &mut self.capture else {
            return;
        };
        debug_assert_eq!(
            eff.screen(),
            cap.screen,
            "the verdict must be read off the map of the row being rebound"
        );
        cap.seq = vec![chord];
        cap.verdict = Some(rebind_check(eff, &cap.seq));
    }

    /// Throws the captured chord away and waits for another, WITHOUT leaving
    /// capture mode.
    pub fn recapture(&mut self) {
        if let Some(cap) = &mut self.capture {
            cap.seq.clear();
            cap.verdict = None;
        }
    }

    /// Leaves capture mode, writing nothing.
    pub fn cancel_capture(&mut self) {
        self.capture = None;
    }

    /// What to hand [`rebind_dry_run`]: the
    /// screen, the command and the sequence — and ONLY when a verdict exists
    /// and it is not a refusal.
    ///
    /// The refusal check is [`Rebind::is_refusal`] and not a match on the
    /// variants, so a rule the loader grows tomorrow refuses by default in both
    /// frontends instead of being treated as permission until someone extends
    /// two `match`es.
    ///
    /// It is a gate, not THE gate: the door is the dry run, which sees the
    /// layers this type does not hold. Nothing may reach `persist_keymap_bind`
    /// without passing it.
    #[must_use]
    pub fn confirmable(&self) -> Option<(Screen, &str, &[Chord])> {
        let cap = self.capture.as_ref()?;
        let verdict = cap.verdict.as_ref()?;
        if verdict.is_refusal() {
            return None;
        }
        Some((cap.screen, &cap.command, &cap.seq))
    }
}

/// The verdict as a sentence, translated: what the editor shows BEFORE asking
/// for a confirmation.
///
/// Every chord in a [`Rebind`] is already painted and every command in one is a
/// catalogue name or a `lua:` name that passed the charset (see the type's own
/// doc), so nothing here needs masking again.
#[must_use]
pub fn verdict_message(verdict: &Rebind, lang: Lang) -> String {
    match verdict {
        Rebind::Free => norte_i18n::t_in(lang, "shortcuts-verdict-free"),
        Rebind::Replaces { command, avail } => {
            let label = command_label(command, lang);
            let reason = short_unavailable_message(*avail, lang);
            if reason.is_empty() {
                norte_i18n::ta_in(lang, "shortcuts-verdict-replaces", &[("command", &label)])
            } else {
                norte_i18n::ta_in(
                    lang,
                    "shortcuts-verdict-replaces-unavailable",
                    &[("command", &label), ("reason", &reason)],
                )
            }
        }
        Rebind::PrefixClash { with, command } => norte_i18n::ta_in(
            lang,
            "shortcuts-verdict-prefix-clash",
            &[("chord", with), ("command", &command_label(command, lang))],
        ),
        Rebind::Sacred { reserved_for } => norte_i18n::ta_in(
            lang,
            "shortcuts-verdict-sacred",
            &[("command", &command_label(reserved_for, lang))],
        ),
        Rebind::DigitWithCounts => norte_i18n::t_in(lang, "shortcuts-verdict-digit"),
        Rebind::Empty => norte_i18n::t_in(lang, "shortcuts-verdict-empty"),
        Rebind::EscInSequence => norte_i18n::t_in(lang, "shortcuts-verdict-esc"),
        Rebind::Unwritable { chord } => {
            norte_i18n::ta_in(lang, "shortcuts-verdict-unwritable", &[("chord", chord)])
        }
    }
}

/// Why a captured chord did not become a write, decided BEFORE any I/O.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// The active preset name resolves to no bundled preset. Unreachable while
    /// an editor is open — the same lookup built the maps it is showing — and
    /// carried as a value rather than asserted away (rule 6).
    #[error("unknown keymap preset")]
    UnknownPreset,
    /// The door refused: the layer would not load, or the binding would load
    /// and never fire.
    #[error(transparent)]
    Door(#[from] RebindError),
}

/// THE DOOR: what an editor calls on confirm, and the only thing that may
/// decide a `keymap.toml` write is safe.
///
/// It lives here and not in a frontend because the one step that MUST NOT be
/// improvised is the cut: [`RebindSources::split_at`] is the only way to model
/// the write into the layer it will really land in, and its own documentation
/// lists the two ways of guessing that fail silently — one hands over the
/// project layer, the other the system layer. A second frontend re-deriving
/// this is the reader most likely to guess. So the frontends keep what is
/// genuinely theirs (which commands they dispatch, how a key event becomes a
/// [`Chord`]) and share the gate.
///
/// `kinds` and `layers` are
/// [`FrontendConfig::keymap_layer_kinds`](crate::config::FrontendConfig::keymap_layer_kinds)
/// and
/// [`FrontendConfig::keymap_layers`](crate::config::FrontendConfig::keymap_layers),
/// parallel; `known_commands` must be the set the frontend VALIDATES that
/// screen's map with (for a dialog map that is wider than what the screen
/// dispatches, because the map merges `[global]`), or the dry run rejects a
/// binding the loader accepts.
///
/// The verdict is about the layers AS THEY WERE LOADED: the writer takes the
/// file lock, this does not.
///
/// ```
/// use norte_config::Layer;
/// use norte_frontend::keymap::{Screen, parse_chord};
/// use norte_frontend::shortcuts::plan_rebind;
///
/// // A fresh install: no layer file anywhere, so the write creates the user's.
/// let w = plan_rebind(
///     "orthodox",
///     &[],
///     &[],
///     &["pane.copy", "pane.mkdir"],
///     Screen::Browse,
///     &[parse_chord("ctrl+alt+n").unwrap()],
///     "pane.mkdir",
/// )
/// .unwrap();
/// assert_eq!((w.section, &w.chords[..]), ("pane", &["ctrl+alt+n".to_owned()][..]));
/// ```
///
/// # Errors
/// [`PlanError`] — an unknown preset, or the door's own refusal.
pub fn plan_rebind(
    preset_name: &str,
    kinds: &[norte_config::Layer],
    layers: &[KeymapFile],
    known_commands: &[&str],
    screen: Screen,
    seq: &[Chord],
    command: &str,
) -> Result<RebindWrite, PlanError> {
    let Some(preset) = presets::source(preset_name).and_then(|src| parse_keymap(src).ok()) else {
        return Err(PlanError::UnknownPreset);
    };
    let split = RebindSources::split_at(&preset, kinds, layers, known_commands, screen);
    Ok(rebind_dry_run(&split.sources(), seq, command)?)
}

/// Why a plan did not become a write, in the reader's language.
#[must_use]
pub fn plan_error_message(e: &PlanError, lang: Lang) -> String {
    match e {
        PlanError::UnknownPreset => norte_i18n::t_in(lang, "shortcuts-refused-preset"),
        PlanError::Door(e) => write_error_message(e, lang),
    }
}

/// Why the door refused, translated — shown after a confirm that did not
/// write.
///
/// [`RebindError::Load`] is rendered as a category and never as its own
/// `Display`: that message may embed untrusted config text (a `./.norte` layer
/// arrives with a cloned repository) and this string goes to a status bar
/// (#73). What a reader can act on is not the diagnostic anyway — the file that
/// would not load is not the one they are editing.
#[must_use]
pub fn write_error_message(e: &RebindError, lang: Lang) -> String {
    match e {
        RebindError::Load(_) => norte_i18n::t_in(lang, "shortcuts-refused-load"),
        RebindError::Shadowed { by, avail } => {
            let label = command_label(by, lang);
            let reason = short_unavailable_message(*avail, lang);
            if reason.is_empty() {
                norte_i18n::ta_in(lang, "shortcuts-refused-shadowed", &[("command", &label)])
            } else {
                norte_i18n::ta_in(
                    lang,
                    "shortcuts-refused-shadowed-unavailable",
                    &[("command", &label), ("reason", &reason)],
                )
            }
        }
    }
}

/// The unbind's door, called the same way [`plan_rebind`] is: the active
/// preset's name, the loaded layers and kinds, and the screen a row belongs
/// to — [`RebindSources::split_at`] makes the same cut either way, so a
/// second frontend has exactly as little reason to redo it for a removal as
/// for a write.
///
/// A row this editor may not touch at all ([`ShortcutRow::is_editable`]'s
/// `false`, a `[global]` binding) is the CALLER's to refuse before this is
/// ever reached — [`ShortcutsState::begin_capture`] already refuses the
/// symmetric case for a rebind, and the row itself carries what a refusal
/// message needs (`shortcuts-row-global`). This function has no row to ask.
///
/// # Errors
/// [`PlanError`] — an unknown preset, or a rebuilt map that fails to load
/// (see [`unbind_dry_run`]'s own doc for why that is unreachable in practice
/// and still typed).
pub fn plan_unbind(
    preset_name: &str,
    kinds: &[norte_config::Layer],
    layers: &[KeymapFile],
    known_commands: &[&str],
    screen: Screen,
    seq: &[Chord],
) -> Result<UnbindWrite, PlanError> {
    let Some(preset) = presets::source(preset_name).and_then(|src| parse_keymap(src).ok()) else {
        return Err(PlanError::UnknownPreset);
    };
    let split = RebindSources::split_at(&preset, kinds, layers, known_commands, screen);
    unbind_dry_run(&split.sources(), seq).map_err(|e| PlanError::Door(RebindError::Load(e)))
}

/// What removing a binding did, in the reader's language — worded from
/// [`UnbindOutcome`], never from "removed from your keymap.toml": that
/// sentence is true and useless the moment anything else still binds the key
/// (#141).
///
/// [`UnbindOutcome::NotBound`] is deliberately absent from this function: it
/// is not a fact about the key, it is "nothing was written", which the caller
/// already has its own wording for (`msg-shortcut-nothing-to-unbind`) shared
/// with the byte-exact writer's own no-op.
#[must_use]
pub fn unbind_outcome_message(outcome: &UnbindOutcome, chord: &str, lang: Lang) -> String {
    match outcome {
        UnbindOutcome::NotBound => norte_i18n::t_in(lang, "msg-shortcut-nothing-to-unbind"),
        UnbindOutcome::Cleared => {
            norte_i18n::ta_in(lang, "msg-shortcut-unbound-cleared", &[("chord", chord)])
        }
        UnbindOutcome::Runs { command, .. } => {
            let label = command_label(command, lang);
            norte_i18n::ta_in(
                lang,
                "msg-shortcut-bound",
                &[("chord", chord), ("command", &label)],
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ScreenKeys, ShortcutsState, build_rows, verdict_message, write_error_message};
    use crate::keymap::{
        Availability, Effective, KeymapFile, Rebind, RebindError, RebindSources, Screen,
        parse_chord, parse_keymap, rebind_dry_run,
    };
    use norte_i18n::Lang;

    const BINDABLE: &[&str] = &[
        "pane.copy",
        "pane.move",
        "cursor.top",
        "cursor.down",
        "pane.switch",
    ];

    const SRC: &str = r#"
[pane]
keymap = [
    { on = ["f5"], run = "pane.copy" },
    { on = ["alt+f5"], run = "pane.pack" },
    { on = ["g", "g"], run = "cursor.top" },
    { on = ["tab"], run = "pane.switch" },
]
"#;

    fn eff() -> Effective {
        let preset = parse_keymap(SRC).expect("fixture parses");
        Effective::build_for(&preset, &[], BINDABLE, Screen::Browse).expect("fixture builds")
    }

    fn state(eff: &Effective) -> ShortcutsState {
        ShortcutsState::new(build_rows(
            &[ScreenKeys {
                screen: Screen::Browse,
                eff,
                bindable: BINDABLE,
            }],
            Lang::En,
        ))
    }

    fn c(s: &str) -> crate::keymap::Chord {
        parse_chord(s).expect("chord")
    }

    /// The sheet answers "what does this key do"; the editor also has to
    /// answer "how do I press X", and `pane.move` — runnable, bound by
    /// nothing — is exactly the row the sheet cannot have.
    #[test]
    fn a_runnable_command_with_no_key_is_a_row() {
        let eff = eff();
        let rows = build_rows(
            &[ScreenKeys {
                screen: Screen::Browse,
                eff: &eff,
                bindable: BINDABLE,
            }],
            Lang::En,
        );
        let unbound: Vec<&str> = rows
            .iter()
            .filter(|r| !r.is_bound())
            .map(|r| r.command.as_str())
            .collect();
        assert_eq!(unbound, ["pane.move", "cursor.down"], "in catalogue order");
        // And the bound ones keep the sheet's rows, unavailable included.
        let pack = rows
            .iter()
            .find(|r| r.command == "pane.pack")
            .expect("the preset binds it");
        assert!(pack.is_bound());
        // No ejecutable, y con su motivo escrito. Era una capacidad `Planned`
        // con número de issue hasta que #132 construyó la última; hoy la fila
        // no ejecutable es la del comando que este build no implementa.
        assert!(matches!(pack.avail, Availability::NotHere), "{:?}", pack.avail);
        assert!(!pack.reason.is_empty(), "{:?}", pack.reason);
        // A command NOT in `bindable` gets no unbound row: it would answer
        // "how do I press X" with a key that does nothing on this screen.
        assert!(!rows.iter().any(|r| r.command == "viewer.close"));
    }

    /// Enter on a row captures; the verdict is there before anything is
    /// confirmed, and the sequence handed out is the one that was judged.
    #[test]
    fn a_capture_carries_its_verdict_before_the_confirm() {
        let eff = eff();
        let mut s = state(&eff);
        assert!(s.begin_capture());
        assert!(s.is_capturing());
        assert!(s.capture().expect("capture").is_waiting());
        assert!(s.confirmable().is_none(), "nothing captured yet");

        s.capture_chord(c("ctrl+j"), &eff);
        assert_eq!(
            s.capture().and_then(super::Capture::verdict),
            Some(&Rebind::Free)
        );
        let (screen, command, seq) = s.confirmable().expect("free is confirmable");
        assert_eq!(screen, Screen::Browse);
        assert_eq!(command, "pane.copy");
        assert_eq!(seq, [c("ctrl+j")]);
    }

    /// A refusal cannot be confirmed AT ALL — the three shapes the loader
    /// would reject, and the one the specification reserves.
    #[test]
    fn a_refused_chord_is_never_confirmable() {
        let eff = eff();
        for chord in ["g", "tab"] {
            let mut s = state(&eff);
            assert!(s.begin_capture());
            s.capture_chord(c(chord), &eff);
            let verdict = s
                .capture()
                .and_then(super::Capture::verdict)
                .expect("a verdict")
                .clone();
            assert!(verdict.is_refusal(), "{chord}: {verdict:?}");
            assert!(
                s.confirmable().is_none(),
                "{chord} must not be writable: {verdict:?}"
            );
        }
    }

    /// The cursor follows its ROW across a hot reload, not its index.
    ///
    /// The reload the editor itself causes is the one that reorders the list:
    /// the command just bound leaves the unbound tail and joins the bound block
    /// in precedence order, so every index after it shifts. A cursor left as a
    /// bare index would sit on a different binding — and the next unbind would
    /// delete a row the reader never pointed at, while the message named it.
    #[test]
    fn the_cursor_follows_its_row_across_a_reload() {
        let before = eff();
        let mut s = state(&before);
        while s
            .selected()
            .is_some_and(|r| r.command != "cursor.down" || r.is_bound())
            && s.cursor() + 1 < s.visible().len()
        {
            s.down();
        }
        assert_eq!(
            s.selected().map(|r| r.command.as_str()),
            Some("cursor.down"),
            "the row this test is about is the unbound tail's"
        );
        let index_before = s.cursor();

        // The same map with ONE more bound key, ahead of the unbound tail:
        // `cursor.down` is the same row and no longer the same index.
        let after = SRC.replace(
            r#"{ on = ["tab"], run = "pane.switch" },"#,
            "{ on = [\"tab\"], run = \"pane.switch\" },\n    { on = [\"ctrl+x\"], run = \"pane.copy\" },",
        );
        let after = parse_keymap(&after).expect("fixture parses");
        let after =
            Effective::build_for(&after, &[], BINDABLE, Screen::Browse).expect("fixture builds");
        s.refresh(build_rows(
            &[ScreenKeys {
                screen: Screen::Browse,
                eff: &after,
                bindable: BINDABLE,
            }],
            Lang::En,
        ));
        assert_eq!(
            s.selected().map(|r| r.command.as_str()),
            Some("cursor.down"),
            "the selection is a ROW, not an index"
        );
        assert_ne!(
            s.cursor(),
            index_before,
            "and the test is worthless unless the index really moved"
        );
    }

    /// Capturing again replaces the chord: a reader who pressed the wrong key
    /// presses another one, and the verdict follows.
    #[test]
    fn a_second_chord_replaces_the_first() {
        let eff = eff();
        let mut s = state(&eff);
        s.begin_capture();
        s.capture_chord(c("g"), &eff);
        assert!(s.confirmable().is_none());
        s.capture_chord(c("f5"), &eff);
        assert!(
            matches!(
                s.capture().and_then(super::Capture::verdict),
                Some(Rebind::Replaces { .. })
            ),
            "f5 is taken by pane.copy"
        );
        s.recapture();
        assert!(s.is_capturing(), "recapture stays in the mode");
        assert!(s.capture().expect("capture").is_waiting());
        assert!(s.confirmable().is_none());
    }

    /// A hot reload replaced the map the verdict was read off: the capture
    /// goes with it. Anything else lets a reader confirm "free" about a key
    /// another norte bound half a second ago.
    #[test]
    fn a_refresh_cancels_the_capture() {
        let eff = eff();
        let mut s = state(&eff);
        s.push_char('c');
        s.begin_capture();
        s.capture_chord(c("ctrl+j"), &eff);
        assert!(s.confirmable().is_some());

        s.refresh(build_rows(
            &[ScreenKeys {
                screen: Screen::Browse,
                eff: &eff,
                bindable: BINDABLE,
            }],
            Lang::En,
        ));
        assert!(!s.is_capturing(), "the verdict did not survive its map");
        assert!(s.confirmable().is_none());
        assert_eq!(s.query_display(), "c", "the filter is not a verdict");
    }

    /// While capturing, every key belongs to the capture: a filter or a cursor
    /// that moved under it would change WHICH row the confirm is about.
    #[test]
    fn the_list_does_not_move_under_a_capture() {
        let eff = eff();
        let mut s = state(&eff);
        let before = s.cursor();
        s.begin_capture();
        s.push_char('z');
        s.down();
        s.page_down(5);
        s.set_cursor(3);
        assert_eq!(s.cursor(), before);
        assert!(s.query_display().is_empty());
        assert_eq!(s.capture().expect("capture").row(), s.visible()[before]);
    }

    /// Every verdict says something, in both locales, and never echoes a
    /// Fluent id at the reader.
    #[test]
    fn every_verdict_is_worded_in_both_locales() {
        let verdicts = [
            Rebind::Free,
            Rebind::Replaces {
                command: "pane.copy".to_owned(),
                avail: Availability::Here,
            },
            Rebind::Replaces {
                command: "pane.pack".to_owned(),
                avail: Availability::NotBuilt {
                    reason: "keymap-reason-archive-write",
                    issue: 132,
                },
            },
            Rebind::PrefixClash {
                with: "g g".to_owned(),
                command: "cursor.top".to_owned(),
            },
            Rebind::Sacred {
                reserved_for: "pane.switch",
            },
            Rebind::DigitWithCounts,
            Rebind::Empty,
            Rebind::EscInSequence,
            Rebind::Unwritable {
                chord: "f13".to_owned(),
            },
        ];
        for lang in [Lang::En, Lang::Es] {
            for v in &verdicts {
                let m = verdict_message(v, lang);
                assert!(!m.is_empty(), "{lang:?} {v:?}");
                assert!(!m.starts_with("shortcuts-"), "{lang:?} {v:?}: {m}");
                assert!(!m.contains("keymap-reason-"), "{lang:?} {v:?}: {m}");
                assert!(!m.contains("keymap-short-"), "{lang:?} {v:?}: {m}");
            }
            for e in [
                RebindError::Shadowed {
                    by: "cursor.top".to_owned(),
                    avail: Availability::Here,
                },
                RebindError::Shadowed {
                    by: "pane.pack".to_owned(),
                    avail: Availability::NotBuilt {
                        reason: "keymap-reason-archive-write",
                        issue: 132,
                    },
                },
            ] {
                let m = write_error_message(&e, lang);
                assert!(
                    !m.is_empty() && !m.starts_with("shortcuts-"),
                    "{lang:?}: {m}"
                );
            }
        }
    }

    /// The door's refusal never quotes the loader's own message: it may embed
    /// text from a `./.norte` layer that arrived with a cloned repository, and
    /// this string goes to a status bar (#73).
    #[test]
    fn a_load_refusal_is_a_category_not_the_diagnostic() {
        let preset = parse_keymap(SRC).expect("fixture parses");
        let none = KeymapFile::default();
        let src = RebindSources {
            preset: &preset,
            below: &[],
            target: &none,
            above: &[],
            known_commands: BINDABLE,
            screen: Screen::Browse,
        };
        // A command the loader does not know: the door refuses with `Load`.
        let e = rebind_dry_run(&src, &[c("ctrl+j")], "pane.definitely-not-a-command")
            .expect_err("an unknown command cannot be bound");
        let m = write_error_message(&e, Lang::En);
        assert!(
            !m.contains("definitely-not-a-command"),
            "the diagnostic must not travel: {m}"
        );
    }

    /// Rows are painted, so they inherit `paint_chord`'s masking: a chord
    /// bound by an untrusted project layer cannot smuggle an escape sequence
    /// into the editor's list.
    #[test]
    fn chords_are_painted_and_masked() {
        let src = "[pane]\nkeymap = [{ on = [\"\u{202e}\"], run = \"pane.copy\" }]\n";
        let preset = parse_keymap(src).expect("fixture parses");
        let eff =
            Effective::build_for(&preset, &[], BINDABLE, Screen::Browse).expect("fixture builds");
        let rows = build_rows(
            &[ScreenKeys {
                screen: Screen::Browse,
                eff: &eff,
                bindable: BINDABLE,
            }],
            Lang::En,
        );
        let row = rows.first().expect("one bound row");
        assert!(
            !row.chord.chars().any(norte_encoding::is_terminal_hazard),
            "{:?}",
            row.chord
        );
        // And the unpainted sequence is still there for the unbind to use.
        assert_eq!(row.seq, [c("\u{202e}")]);
    }

    /// K3c #141: a `[global]` binding merges into the screen's own map
    /// indistinguishably from one written in `[pane]` — that is the whole
    /// point of `[global]` — so the row's provenance can only come from the
    /// LAYER the binding was read out of ([`Effective::is_global`]), never
    /// from the screen it happens to be displayed under. A row built from it
    /// says so and refuses to be edited; an ordinary `[pane]` row does not.
    #[test]
    fn a_global_binding_is_a_row_marked_global_and_not_editable() {
        let src = "[global]\nkeymap = [{ on = [\"ctrl+p\"], run = \"pane.switch\" }]\n\
                    [pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n";
        let preset = parse_keymap(src).expect("fixture parses");
        let eff =
            Effective::build_for(&preset, &[], BINDABLE, Screen::Browse).expect("fixture builds");
        let rows = build_rows(
            &[ScreenKeys {
                screen: Screen::Browse,
                eff: &eff,
                bindable: BINDABLE,
            }],
            Lang::En,
        );
        let global_row = rows
            .iter()
            .find(|r| r.command == "pane.switch")
            .expect("the global binding is a row");
        assert!(global_row.global, "read from [global]");
        assert!(!global_row.is_editable(), "the editor may not write here");

        let pane_row = rows
            .iter()
            .find(|r| r.command == "pane.copy")
            .expect("the ordinary binding is a row too");
        assert!(!pane_row.global, "read from [pane], not [global]");
        assert!(pane_row.is_editable());

        // And an unbound row (no binding at all) is never global either.
        let unbound = rows
            .iter()
            .find(|r| !r.is_bound())
            .expect("BINDABLE names a command nothing here presses");
        assert!(!unbound.global);
        assert!(unbound.is_editable());
    }

    /// The state itself refuses to open a capture on a `[global]` row —
    /// checked here rather than trusted to every caller, so a frontend that
    /// forgets to ask [`ShortcutRow::is_editable`] first (the GUI's own
    /// `enter` arm ignores `begin_capture`'s return value entirely) still
    /// cannot write into a section that would change all three screens.
    #[test]
    fn begin_capture_refuses_a_global_row() {
        let src = "[global]\nkeymap = [{ on = [\"ctrl+p\"], run = \"pane.switch\" }]\n";
        let preset = parse_keymap(src).expect("fixture parses");
        let eff =
            Effective::build_for(&preset, &[], BINDABLE, Screen::Browse).expect("fixture builds");
        let mut s = state(&eff);
        assert!(
            s.selected()
                .is_some_and(|r| r.command == "pane.switch" && r.global),
            "the cursor starts on the one bound row"
        );
        assert!(!s.begin_capture(), "a global row cannot be captured into");
        assert!(!s.is_capturing());
    }
}
